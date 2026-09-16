//! Shared helpers for the CLI subcommands.

use anyhow::{bail, Context as _, Result};

use crate::cli::data_dir;
use crate::storage::Store;

/// Resolve the on-disk store path (creating the parent dir if needed) and open it.
pub fn open_store() -> Result<Store> {
    let home = data_dir()?;
    std::fs::create_dir_all(&home).context("creating the config directory")?;
    let path = Store::default_path(&home);
    Store::open(path).context("opening the store")
}

/// Render a human-friendly provider-kind label.
pub fn kind_label(kind: &crate::domain::ProviderKind) -> &'static str {
    match kind {
        crate::domain::ProviderKind::OpenAI => "OpenAI",
        crate::domain::ProviderKind::Anthropic => "Anthropic",
        crate::domain::ProviderKind::Google => "Google",
        crate::domain::ProviderKind::OpenAIResponses => "OpenAI Responses",
        crate::domain::ProviderKind::Custom => "Custom",
    }
}

/// Hash a profile password with Argon2id and a random salt.
///
/// The stored form is `argon2id:<salt-hex>:<derived-key-hex>`; verification
/// re-derives the key from the recorded salt and compares in constant time.
pub fn hash_password(password: &str) -> Result<String> {
    use rand::RngCore;
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    let key = crate::crypto::derive_key(password, &salt)?;
    Ok(format!(
        "argon2id:{}:{}",
        hex::encode(salt),
        hex::encode(key)
    ))
}

/// Check a candidate password against a stored hash.
///
/// Accepts the current `argon2id:` format and the older unsalted `sha256:`
/// marker hashes so profiles created before the crypto milestone keep working.
pub fn verify_password(stored: &str, candidate: &str) -> Result<bool> {
    use sha2::{Digest, Sha256};
    if let Some(rest) = stored.strip_prefix("argon2id:") {
        let (salt_hex, key_hex) = rest.split_once(':').context("malformed argon2 hash")?;
        let salt = hex::decode(salt_hex).context("bad hash salt")?;
        let key = crate::crypto::derive_key(candidate, &salt)?;
        let stored_key = hex::decode(key_hex).context("bad hash digest")?;
        // Constant-time-ish compare; lengths differ only on tampering.
        if stored_key.len() != key.len() {
            return Ok(false);
        }
        let diff = stored_key
            .iter()
            .zip(key.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b));
        return Ok(diff == 0);
    }
    if let Some(digest_hex) = stored.strip_prefix("sha256:") {
        let mut hasher = Sha256::new();
        hasher.update(b"agos-password-marker:");
        hasher.update(candidate.as_bytes());
        return Ok(hex::encode(hasher.finalize()) == digest_hex);
    }
    bail!("unrecognised password hash format")
}

/// Gate a management operation on the profile's password.
///
/// Profiles without a password pass straight through; protected profiles are
/// prompted up to three times before the operation is refused.
pub fn ensure_password_ok(profile: &crate::domain::Profile) -> Result<()> {
    use dialoguer::{theme::ColorfulTheme, Password};
    let Some(hash) = profile.password_hash.as_deref() else {
        return Ok(());
    };
    let theme = ColorfulTheme::default();
    for attempt in 1..=3 {
        let candidate = Password::with_theme(&theme)
            .with_prompt(format!("Password for {:?}", profile.name))
            .interact()
            .context("reading the profile password")?;
        if verify_password(hash, &candidate)? {
            return Ok(());
        }
        eprintln!("incorrect password (attempt {attempt} of 3)");
    }
    bail!(
        "password check failed; refusing to modify {:?}",
        profile.name
    )
}

/// Find a profile by name, failing clearly if it doesn't exist.
pub fn require_profile(store: &Store, name: &str) -> Result<crate::domain::Profile> {
    store
        .get_profile_by_name(name)?
        .with_context(|| format!("no profile named {name:?}"))
}

/// Mask a token for display: keep the first 6 and last 4 characters, hide the
/// rest. Short tokens are fully masked.
pub fn mask_token(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() <= 12 {
        return "*".repeat(chars.len());
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// Prompt for an API token with paste verification.
///
/// The value is read hidden (so bystanders don't see it), trimmed (pasted
/// tokens often carry a trailing newline), shown masked with its length, and
/// then confirmed — with an option to reveal it in full — before it is
/// accepted. Re-enters the loop until the user confirms.
///
/// With `allow_empty` the user may submit an empty value (used by flows that
/// treat "empty" as "keep the current token").
pub fn prompt_token(
    theme: &dialoguer::theme::ColorfulTheme,
    prompt: &str,
    allow_empty: bool,
) -> Result<String> {
    use dialoguer::{Confirm, Password, Select};
    loop {
        let raw: String = Password::with_theme(theme)
            .with_prompt(prompt)
            .allow_empty_password(allow_empty)
            .interact()?;
        let token = raw.trim().to_string();
        if token.is_empty() {
            if allow_empty {
                return Ok(token);
            }
            println!("The token is empty — please paste it again.");
            continue;
        }
        println!(
            "Token: {} ({} characters, trailing whitespace stripped)",
            mask_token(&token),
            token.len()
        );
        let choices = ["Yes — save it", "Reveal the full token", "No — re-enter it"];
        let idx = Select::with_theme(theme)
            .with_prompt("Is this the token you meant to paste?")
            .items(choices)
            .default(0)
            .interact()?;
        match idx {
            0 => return Ok(token),
            1 => {
                println!("{token}");
                let sure = Confirm::with_theme(theme)
                    .with_prompt("Save this token?")
                    .default(true)
                    .interact()?;
                if sure {
                    return Ok(token);
                }
            }
            _ => continue,
        }
    }
}

/// Fetch the model catalogue from an OpenAI provider
/// (`GET {base}/models` with the provider's bearer token). Returns sorted
/// model IDs.
pub fn fetch_provider_models(provider: &crate::domain::Provider) -> Result<Vec<String>> {
    use anyhow::Context as _;
    let provider = provider.clone();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let ids = rt.block_on(async move {
        let base = crate::adapter::outbound::normalize_base(&provider.base_url);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .context("building the HTTP client")?;
        let mut req = client.get(format!("{base}/models"));
        if !provider.auth_token.is_empty() {
            req = req.bearer_auth(&provider.auth_token);
        }
        let resp = req.send().await.context("reaching the provider")?;
        let status = resp.status();
        let json: serde_json::Value = resp
            .json()
            .await
            .context("parsing the provider's model list")?;
        anyhow::ensure!(status.is_success(), "provider returned {status}");
        let mut ids: Vec<String> = json
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        ids.sort();
        Ok(ids)
    })?;
    Ok(ids)
}

/// Pick a model ID for `provider`: either browse the provider's live model
/// catalogue with a searchable menu, or type an ID manually. Falls back to
/// manual input when the catalogue cannot be fetched (offline, bad token, or a
/// provider kind that does not expose an OpenAI-style model list).
pub fn prompt_model_id(
    theme: &dialoguer::theme::ColorfulTheme,
    provider: &crate::domain::Provider,
    prompt: &str,
) -> Result<String> {
    use dialoguer::{FuzzySelect, Input, Select};
    let browseable = matches!(
        provider.kind,
        crate::domain::ProviderKind::OpenAI | crate::domain::ProviderKind::Custom
    );
    loop {
        if browseable {
            let choices = [
                format!(
                    "Browse models from {:?} (fetch from the API)",
                    provider.name
                ),
                "Type the model ID manually".to_string(),
            ];
            let idx = Select::with_theme(theme)
                .with_prompt(prompt)
                .items(choices)
                .default(0)
                .interact()?;
            if idx == 0 {
                println!("Fetching models from {} …", provider.base_url);
                match fetch_provider_models(provider) {
                    Ok(ids) if !ids.is_empty() => {
                        let sel = FuzzySelect::with_theme(theme)
                            .with_prompt(format!("Search models from {:?}", provider.name))
                            .items(&ids)
                            .interact()?;
                        return Ok(ids[sel].clone());
                    }
                    Ok(_) => println!("The provider returned an empty model list."),
                    Err(e) => println!("Could not fetch the model list: {e:#}"),
                }
                continue;
            }
        }
        let model_id: String = Input::<String>::with_theme(theme)
            .with_prompt("Model ID (e.g. vendor/model-id)")
            .interact_text()?;
        let model_id = model_id.trim().to_string();
        if model_id.is_empty() {
            println!("The model ID is empty — please enter it again.");
            continue;
        }
        return Ok(model_id);
    }
}

/// Collect extra headers interactively; empty input finishes the loop.
pub fn prompt_headers() -> Result<std::collections::BTreeMap<String, String>> {
    use dialoguer::{theme::ColorfulTheme, Input};
    let theme = ColorfulTheme::default();
    let mut headers = std::collections::BTreeMap::new();
    println!("Add optional headers (empty name to finish):");
    loop {
        let name: String = Input::<String>::with_theme(&theme)
            .with_prompt("Header name")
            .allow_empty(true)
            .interact_text()?;
        if name.is_empty() {
            break;
        }
        let value: String = Input::<String>::with_theme(&theme)
            .with_prompt("Value")
            .interact_text()?;
        headers.insert(name, value);
    }
    Ok(headers)
}

/// Pick a profile from an arrow-key select menu of the existing records.
///
/// Profiles are the only entity users have to type a name for from scratch, so
/// the picker prefers a menu whenever at least one exists.
pub fn pick_profile(store: &Store, prompt: &str) -> Result<crate::domain::Profile> {
    use dialoguer::{theme::ColorfulTheme, Select};
    let theme = ColorfulTheme::default();
    let all = store.list_profiles()?;
    if all.is_empty() {
        bail!("no profiles configured yet; create one first");
    }
    if all.len() == 1 {
        return Ok(all[0].clone());
    }
    let labels: Vec<String> = all.iter().map(|p| p.name.clone()).collect();
    let idx = Select::with_theme(&theme)
        .with_prompt(prompt)
        .items(&labels)
        .interact()?;
    Ok(all[idx].clone())
}

/// Pick a provider belonging to `profile` from a select menu.
///
/// If only one provider exists it is returned directly, so "pick provider"
/// flows stay fast and never feel like a dead end.
pub fn pick_provider(
    store: &Store,
    profile: &crate::domain::Profile,
    prompt: &str,
) -> Result<crate::domain::Provider> {
    use dialoguer::{theme::ColorfulTheme, Select};
    let theme = ColorfulTheme::default();
    let all = store.list_providers(profile.id.as_str())?;
    if all.is_empty() {
        bail!(
            "no providers configured for {:?} yet; add one first",
            profile.name
        );
    }
    if all.len() == 1 {
        return Ok(all[0].clone());
    }
    let labels: Vec<String> = all
        .iter()
        .map(|p| format!("{}  ({})", p.name, p.base_url))
        .collect();
    let idx = Select::with_theme(&theme)
        .with_prompt(prompt)
        .items(&labels)
        .interact()?;
    Ok(all[idx].clone())
}

/// Pick a proxy belonging to `profile` from a select menu.
pub fn pick_proxy(
    store: &Store,
    profile: &crate::domain::Profile,
    prompt: &str,
) -> Result<crate::domain::Proxy> {
    use dialoguer::{theme::ColorfulTheme, Select};
    let theme = ColorfulTheme::default();
    let all = store.list_proxies(profile.id.as_str())?;
    if all.is_empty() {
        bail!(
            "no proxies configured for {:?} yet; create one first",
            profile.name
        );
    }
    if all.len() == 1 {
        return Ok(all[0].clone());
    }
    let labels: Vec<String> = all
        .iter()
        .map(|p| {
            if let Some(desc) = p.description.as_deref() {
                format!("{}  ({})", p.name, desc)
            } else {
                p.name.clone()
            }
        })
        .collect();
    let idx = Select::with_theme(&theme)
        .with_prompt(prompt)
        .items(&labels)
        .interact()?;
    Ok(all[idx].clone())
}

/// Pick a route belonging to `proxy` from a select menu.
pub fn pick_route(
    store: &Store,
    proxy: &crate::domain::Proxy,
    prompt: &str,
) -> Result<crate::domain::Route> {
    use dialoguer::{theme::ColorfulTheme, Select};
    let theme = ColorfulTheme::default();
    let all = store.list_routes(proxy.id)?;
    if all.is_empty() {
        bail!(
            "no routes under proxy {:?} yet; create one first",
            proxy.name
        );
    }
    if all.len() == 1 {
        return Ok(all[0].clone());
    }
    let labels: Vec<String> = all
        .iter()
        .map(|r| {
            if let Some(desc) = r.description.as_deref() {
                format!("{}  ({})", r.name, desc)
            } else {
                r.name.clone()
            }
        })
        .collect();
    let idx = Select::with_theme(&theme)
        .with_prompt(prompt)
        .items(&labels)
        .interact()?;
    Ok(all[idx].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argon2_hash_roundtrips() {
        let stored = hash_password("s3cret").unwrap();
        assert!(stored.starts_with("argon2id:"));
        assert!(verify_password(&stored, "s3cret").unwrap());
        assert!(!verify_password(&stored, "wrong").unwrap());
    }

    #[test]
    fn hashes_are_salted() {
        let a = hash_password("same").unwrap();
        let b = hash_password("same").unwrap();
        assert_ne!(a, b, "same password must produce different hashes");
        assert!(verify_password(&a, "same").unwrap());
        assert!(verify_password(&b, "same").unwrap());
    }

    #[test]
    fn legacy_sha256_hashes_still_verify() {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(b"agos-password-marker:");
        hasher.update(b"oldpw");
        let legacy = format!("sha256:{}", hex::encode(hasher.finalize()));
        assert!(verify_password(&legacy, "oldpw").unwrap());
        assert!(!verify_password(&legacy, "nope").unwrap());
    }

    #[test]
    fn unknown_hash_format_is_rejected() {
        assert!(verify_password("plaintext", "x").is_err());
    }

    #[test]
    fn mask_token_hides_middle() {
        assert_eq!(mask_token("sk-or-v1-abcdef1234567890"), "sk-or-…7890");
        assert_eq!(mask_token("short"), "*****");
    }
}
