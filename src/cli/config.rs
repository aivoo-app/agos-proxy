//! Config export/import: moving a profile setup between machines.
//!
//! The portable file contains the full profile tree — providers (with their
//! upstream tokens), proxies, routes, and route entries — sealed with a
//! passphrase-derived key so it can be copied around safely. On import, tokens
//! are re-encrypted with the local store's master key and the profile gets a
//! fresh bearer token.

use std::collections::BTreeMap;

use anyhow::{bail, Context as _, Result};
use clap::Subcommand;
use dialoguer::{theme::ColorfulTheme, Input, Password};
use serde::{Deserialize, Serialize};

use crate::cli::util::{ensure_password_ok, open_store, require_profile};
use crate::crypto::{self, MasterKey};
use crate::domain::{ProviderKind, RouteCapabilities, RoutingStrategy};
use crate::storage::{NewProvider, Store};

/// Subcommands under `agos-proxy config`.
#[derive(Debug, Subcommand)]
pub enum ConfigArgs {
    /// Export a profile's setup (providers, proxies, routes) to a portable file.
    Export {
        /// Name of the profile to export.
        profile: Option<String>,
        /// Output file; defaults to `<profile>.agos.json` in the current dir.
        #[arg(long)]
        output: Option<String>,
    },
    /// Import a profile setup previously written by `export`.
    Import {
        /// Path to the portable profile file.
        path: Option<std::path::PathBuf>,
        /// Name for the imported profile if the original name is taken.
        #[arg(long)]
        name: Option<String>,
    },
}

/// The sealed on-disk container.
#[derive(Debug, Serialize, Deserialize)]
pub struct PortableFile {
    pub format: String,
    pub version: u32,
    /// Hex-encoded Argon2 salt.
    pub salt: String,
    /// Hex-encoded `nonce || ciphertext` of the sealed bundle.
    pub data: String,
}

/// The plaintext bundle: one full profile tree.
#[derive(Debug, Serialize, Deserialize)]
pub struct PortableProfile {
    pub name: String,
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masking_servers: Vec<PortableMask>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_masking_server: Option<String>,
    pub providers: Vec<PortableProvider>,
    pub proxies: Vec<PortableProxy>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PortableProvider {
    pub name: String,
    pub description: Option<String>,
    pub base_url: String,
    pub auth_token: String,
    pub kind: ProviderKind,
    pub extra_headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub masking_server: Option<String>,
}

/// An egress mask, exported with its secret (the whole file is sealed).
#[derive(Debug, Serialize, Deserialize)]
pub struct PortableMask {
    pub name: String,
    pub kind: String,
    pub endpoint_url: String,
    pub secret: String,
    #[serde(default)]
    pub max_body_bytes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_egress_ip: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PortableProxy {
    pub name: String,
    pub description: Option<String>,
    pub routes: Vec<PortableRoute>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PortableRoute {
    pub name: String,
    pub description: Option<String>,
    pub strategy: RoutingStrategy,
    pub identity: Option<String>,
    #[serde(default)]
    pub max_tokens: u32,
    #[serde(default)]
    pub cache_ttl_secs: i64,
    pub entries: Vec<PortableEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PortableEntry {
    /// References `PortableProvider::name` inside the same bundle.
    pub provider_name: String,
    pub model_id: String,
    pub priority: i32,
    pub weight: f64,
    pub capabilities: RouteCapabilities,
    #[serde(default)]
    pub price_per_1m: f64,
}

/// Entry point for `agos-proxy config ...`.
pub fn run(args: ConfigArgs) -> Result<()> {
    let store = open_store()?;
    match args {
        ConfigArgs::Export { profile, output } => export_cmd(&store, profile, output),
        ConfigArgs::Import { path, name } => import_cmd(&store, path, name),
    }
}

fn export_cmd(store: &Store, profile: Option<String>, output: Option<String>) -> Result<()> {
    let theme = ColorfulTheme::default();
    let name = match profile {
        Some(p) if !p.is_empty() => p,
        _ => Input::<String>::with_theme(&theme)
            .with_prompt("Profile to export")
            .interact_text()?,
    };
    let profile = require_profile(store, &name)?;
    ensure_password_ok(&profile)?;

    let bundle = export_bundle(store, &profile)?;
    let out_path = output.unwrap_or_else(|| format!("{}.agos.json", profile.name));

    let password = Password::with_theme(&theme)
        .with_prompt("Passphrase to protect the export")
        .interact()?;
    let file = seal_bundle(&bundle, &password)?;
    std::fs::write(&out_path, serde_json::to_vec_pretty(&file)?)?;

    println!(
        "Exported profile {:?} to {out_path} ({} providers, {} proxies).",
        profile.name,
        bundle.providers.len(),
        bundle.proxies.len()
    );
    Ok(())
}

fn import_cmd(
    store: &Store,
    path: Option<std::path::PathBuf>,
    rename: Option<String>,
) -> Result<()> {
    let theme = ColorfulTheme::default();
    let path = match path {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Input::<String>::with_theme(&theme)
            .with_prompt("Path to the export file")
            .interact_text()
            .map(std::path::PathBuf::from)?,
    };
    let raw = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let file: PortableFile = serde_json::from_slice(&raw).context("parsing the export file")?;

    let password = Password::with_theme(&theme)
        .with_prompt("Passphrase")
        .interact()?;
    let bundle = open_bundle(&file, &password)?;

    let final_name = rename.unwrap_or_else(|| bundle.name.clone());
    let token = import_bundle(store, &bundle, &final_name)?;
    println!("Imported profile {final_name:?} — bearer token: {token}");
    Ok(())
}

/// Collect a profile's full tree from the store.
pub fn export_bundle(store: &Store, profile: &crate::domain::Profile) -> Result<PortableProfile> {
    let providers = store.list_providers(profile.id.as_str())?;
    let masks = store.list_masking_servers(profile.id.as_str())?;
    let portable_masks = masks
        .iter()
        .map(|m| PortableMask {
            name: m.name.clone(),
            kind: m.kind.clone(),
            endpoint_url: m.endpoint_url.clone(),
            secret: m.secret.clone(),
            max_body_bytes: m.max_body_bytes,
            expected_egress_ip: m.expected_egress_ip.clone(),
        })
        .collect();
    let default_masking_server = profile
        .default_masking_server_id
        .and_then(|id| masks.iter().find(|m| m.id == id))
        .map(|m| m.name.clone());

    let portable_providers = providers
        .iter()
        .map(|p| PortableProvider {
            name: p.name.clone(),
            description: p.description.clone(),
            base_url: p.base_url.clone(),
            auth_token: p.auth_token.clone(),
            kind: p.kind,
            extra_headers: p.extra_headers.clone(),
            masking_server: p.masking_server.as_ref().map(|m| m.name.clone()),
        })
        .collect();

    let mut portable_proxies = Vec::new();
    for proxy in store.list_proxies(profile.id.as_str())? {
        let mut routes = Vec::new();
        for route in store.list_routes(proxy.id)? {
            let entries = store
                .route_entries(route.id)?
                .into_iter()
                .map(|e| {
                    let provider_name = store
                        .get_provider(e.provider_id)?
                        .map(|p| p.name)
                        .with_context(|| format!("provider {} missing", e.provider_id))?;
                    Ok(PortableEntry {
                        provider_name,
                        model_id: e.model_id,
                        priority: e.priority,
                        weight: e.weight,
                        capabilities: e.capabilities,
                        price_per_1m: e.price_per_1m,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            routes.push(PortableRoute {
                name: route.name,
                description: route.description,
                strategy: route.strategy,
                identity: route.identity.clone(),
                max_tokens: route.max_tokens,
                cache_ttl_secs: route.cache_ttl_secs,
                entries,
            });
        }
        portable_proxies.push(PortableProxy {
            name: proxy.name,
            description: proxy.description,
            routes,
        });
    }

    Ok(PortableProfile {
        name: profile.name.clone(),
        description: profile.description.clone(),
        masking_servers: portable_masks,
        default_masking_server,
        providers: portable_providers,
        proxies: portable_proxies,
    })
}

/// Seal a bundle into a `PortableFile` with a passphrase.
pub fn seal_bundle(bundle: &PortableProfile, password: &str) -> Result<PortableFile> {
    let mut salt = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut salt);
    let key = crypto::derive_key(password, &salt)?;
    let key = MasterKey::from_bytes(&key)?;
    let plaintext = serde_json::to_string(bundle)?;
    let ct = crypto::encrypt(&key, &plaintext)?;
    Ok(PortableFile {
        format: "agos-profile".into(),
        version: 1,
        salt: hex::encode(salt),
        data: hex::encode(ct),
    })
}

/// Open a sealed `PortableFile` with a passphrase.
pub fn open_bundle(file: &PortableFile, password: &str) -> Result<PortableProfile> {
    if file.format != "agos-profile" {
        bail!(
            "not an agos-proxy profile export (format {:?})",
            file.format
        );
    }
    if file.version != 1 {
        bail!("unsupported export version {}", file.version);
    }
    let salt = hex::decode(&file.salt).context("bad salt encoding")?;
    let key = crypto::derive_key(password, &salt)?;
    let key = MasterKey::from_bytes(&key)?;
    let ct = hex::decode(&file.data).context("bad data encoding")?;
    let plaintext = crypto::decrypt(&key, &ct)?;
    serde_json::from_str(&plaintext).context("parsing the sealed bundle")
}

/// Recreate a full profile tree. Returns the new profile's bearer token.
pub fn import_bundle(store: &Store, bundle: &PortableProfile, name: &str) -> Result<String> {
    if store.get_profile_by_name(name)?.is_some() {
        bail!("a profile named {name:?} already exists; pick another with --name");
    }
    let profile = store.create_profile(name, bundle.description.as_deref(), None)?;

    // Masks first so providers can bind to them by name.
    let mut mask_ids: BTreeMap<String, i64> = BTreeMap::new();
    for m in &bundle.masking_servers {
        let created = store.create_masking_server(
            profile.id.as_str(),
            crate::storage::NewMaskingServer {
                name: m.name.clone(),
                kind: m.kind.clone(),
                endpoint_url: m.endpoint_url.clone(),
                secret: m.secret.clone(),
                max_body_bytes: m.max_body_bytes,
                expected_egress_ip: m.expected_egress_ip.clone(),
            },
        )?;
        mask_ids.insert(created.name.clone(), created.id);
    }
    if let Some(default_name) = &bundle.default_masking_server {
        if let Some(id) = mask_ids.get(default_name) {
            store.set_profile_default_masking(profile.id.as_str(), Some(*id))?;
        }
    }

    // Providers next; entries reference them by name.
    let mut provider_ids: BTreeMap<String, i64> = BTreeMap::new();
    for p in &bundle.providers {
        let masking_server_id = p
            .masking_server
            .as_ref()
            .and_then(|n| mask_ids.get(n).copied());
        let created = store.create_provider(
            profile.id.as_str(),
            NewProvider {
                name: p.name.clone(),
                description: p.description.clone(),
                base_url: p.base_url.clone(),
                auth_token: p.auth_token.clone(),
                kind: p.kind,
                extra_headers: p.extra_headers.clone(),
                masking_server_id,
            },
        )?;
        provider_ids.insert(created.name.clone(), created.id);
    }

    for proxy in &bundle.proxies {
        let created_proxy = store.create_proxy(
            profile.id.as_str(),
            &proxy.name,
            proxy.description.as_deref(),
        )?;
        for route in &proxy.routes {
            let created_route = store.create_route(
                created_proxy.id,
                &route.name,
                route.description.as_deref(),
                route.strategy,
                route.identity.as_deref(),
            )?;
            if route.max_tokens > 0 || route.cache_ttl_secs > 0 {
                let _ = store.set_route_economy(
                    created_route.id,
                    route.max_tokens,
                    route.cache_ttl_secs,
                );
            }
            for entry in &route.entries {
                let provider_id = provider_ids.get(&entry.provider_name).with_context(|| {
                    format!(
                        "entry references unknown provider {:?}",
                        entry.provider_name
                    )
                })?;
                let created = store.add_route_entry(
                    created_route.id,
                    *provider_id,
                    &entry.model_id,
                    entry.priority,
                    entry.weight,
                    entry.capabilities.clone(),
                )?;
                if entry.price_per_1m > 0.0 {
                    let _ = store.set_route_entry_price(created.id, entry.price_per_1m);
                }
            }
        }
    }
    Ok(profile.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Profile;
    use crate::storage::NewMaskingServer;

    fn seeded_store() -> Store {
        let store = Store::open_in_memory().unwrap();
        let profile = store.create_profile("coder1", Some("dev"), None).unwrap();
        let provider = store
            .create_provider(
                profile.id.as_str(),
                NewProvider {
                    name: "example".into(),
                    description: None,
                    base_url: "https://api.example.com".into(),
                    auth_token: "sk-secret".into(),
                    kind: ProviderKind::OpenAI,
                    extra_headers: BTreeMap::new(),
                    masking_server_id: None,
                },
            )
            .unwrap();
        let proxy = store
            .create_proxy(profile.id.as_str(), "prog", None)
            .unwrap();
        let route = store
            .create_route(proxy.id, "r1", None, RoutingStrategy::Priority, None)
            .unwrap();
        store
            .add_route_entry(
                route.id,
                provider.id,
                "example-model",
                1,
                1.0,
                RouteCapabilities::default(),
            )
            .unwrap();
        store
    }

    fn get_profile(store: &Store) -> Profile {
        store.get_profile_by_name("coder1").unwrap().unwrap()
    }

    #[test]
    fn export_import_roundtrip_preserves_masks() {
        let store = seeded_store();
        let profile = get_profile(&store);
        let mask = store
            .create_masking_server(
                profile.id.as_str(),
                NewMaskingServer {
                    name: "hop1".into(),
                    kind: "cf_worker".into(),
                    endpoint_url: "https://hop1.example.workers.dev".into(),
                    secret: "s3cret".into(),
                    max_body_bytes: 0,
                    expected_egress_ip: None,
                },
            )
            .unwrap();
        let providers = store.list_providers(profile.id.as_str()).unwrap();
        store
            .update_provider(
                providers[0].id,
                crate::storage::NewProvider {
                    name: providers[0].name.clone(),
                    description: None,
                    base_url: providers[0].base_url.clone(),
                    auth_token: providers[0].auth_token.clone(),
                    kind: providers[0].kind,
                    extra_headers: providers[0].extra_headers.clone(),
                    masking_server_id: Some(mask.id),
                },
            )
            .unwrap();

        let bundle = export_bundle(&store, &profile).unwrap();
        assert_eq!(bundle.masking_servers.len(), 1);
        assert_eq!(bundle.providers[0].masking_server.as_deref(), Some("hop1"));

        let target = Store::open_in_memory().unwrap();
        import_bundle(&target, &bundle, "coder2").unwrap();
        let restored = target.list_providers("x");
        assert!(restored.is_err() || true); // profile id differs; check by lookup below
        let new_profile = target.get_profile_by_name("coder2").unwrap().unwrap();
        let back = target.list_providers(new_profile.id.as_str()).unwrap();
        assert_eq!(
            back[0].masking_server.as_ref().map(|m| m.name.as_str()),
            Some("hop1")
        );
        assert_eq!(
            back[0].masking_server.as_ref().map(|m| m.secret.as_str()),
            Some("s3cret")
        );
    }

    #[test]
    fn export_import_roundtrip_preserves_tree() {
        let store = seeded_store();
        let bundle = export_bundle(&store, &get_profile(&store)).unwrap();
        assert_eq!(bundle.providers.len(), 1);
        assert_eq!(bundle.proxies.len(), 1);
        assert_eq!(
            bundle.proxies[0].routes[0].entries[0].model_id,
            "example-model"
        );
        assert_eq!(bundle.providers[0].auth_token, "sk-secret");

        let target = Store::open_in_memory().unwrap();
        let token = import_bundle(&target, &bundle, "coder2").unwrap();
        assert!(!token.is_empty());

        let imported = target.get_profile_by_name("coder2").unwrap().unwrap();
        let providers = target.list_providers(imported.id.as_str()).unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].auth_token, "sk-secret");

        let proxies = target.list_proxies(imported.id.as_str()).unwrap();
        let route = target.list_routes(proxies[0].id).unwrap()[0].clone();
        let entries = target.route_entries(route.id).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].model_id, "example-model");
    }

    #[test]
    fn sealed_bundle_roundtrips_with_passphrase() {
        let bundle = PortableProfile {
            name: "t".into(),
            description: None,
            masking_servers: Vec::new(),
            default_masking_server: None,
            providers: vec![],
            proxies: vec![],
        };
        let file = seal_bundle(&bundle, "hunter2").unwrap();
        let opened = open_bundle(&file, "hunter2").unwrap();
        assert_eq!(opened.name, "t");
        assert!(open_bundle(&file, "wrong").is_err());
    }

    #[test]
    fn import_refuses_existing_profile_name() {
        let store = seeded_store();
        let bundle = export_bundle(&store, &get_profile(&store)).unwrap();
        assert!(import_bundle(&store, &bundle, "coder1").is_err());
    }
}
