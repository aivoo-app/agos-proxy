//! Shared helpers for the CLI subcommands.

use anyhow::{Context as _, Result};

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
        crate::domain::ProviderKind::OpenAICompatible => "OpenAI-compatible",
        crate::domain::ProviderKind::Anthropic => "Anthropic",
        crate::domain::ProviderKind::Google => "Google (Gemini)",
        crate::domain::ProviderKind::Custom => "Custom",
    }
}

/// Best-effort password hash. MVP stores the plaintext behind a marker; real
/// argon2 lands with the crypto milestone.
pub fn hash_password(password: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"agos-password-marker:");
    hasher.update(password.as_bytes());
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

/// Find a profile by name, failing clearly if it doesn't exist.
pub fn require_profile(store: &Store, name: &str) -> Result<crate::domain::Profile> {
    store
        .get_profile_by_name(name)?
        .with_context(|| format!("no profile named {name:?}"))
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
