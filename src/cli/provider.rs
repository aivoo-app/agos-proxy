//! Provider management: the upstreams (base URL + credentials) a profile talks to.

use anyhow::Result;
use clap::Subcommand;
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};

use crate::cli::util::{
    ensure_password_ok, kind_label, open_store, pick_profile, pick_provider, prompt_headers,
    require_profile,
};
use crate::domain::ProviderKind;
use crate::storage::NewProvider;

/// Subcommands under `agos-proxy provider`.
#[derive(Debug, Subcommand)]
pub enum ProviderArgs {
    /// Add a provider to a profile (interactive wizard, or flag-driven).
    Add {
        /// Name of the owning profile.
        #[arg(long)]
        profile: Option<String>,
    },
    /// List the providers configured on a profile.
    List {
        /// Name of the owning profile.
        #[arg(long)]
        profile: Option<String>,
    },
    /// Edit a provider's settings interactively.
    Edit {
        /// Name of the owning profile.
        #[arg(long)]
        profile: Option<String>,
    },
    /// Remove a provider.
    Delete {
        /// Name of the owning profile.
        #[arg(long)]
        profile: Option<String>,
    },
}

/// Entry point for `agos-proxy provider ...`.
pub fn run(args: ProviderArgs) -> Result<()> {
    let store = open_store()?;
    match args {
        ProviderArgs::Add { profile } => add(&store, profile),
        ProviderArgs::List { profile } => list(&store, profile),
        ProviderArgs::Edit { profile } => edit(&store, profile),
        ProviderArgs::Delete { profile } => delete(&store, profile),
    }
}

/// Resolve the owning profile from a flag, or let the user pick one.
fn resolve_profile(
    store: &crate::storage::Store,
    given: Option<String>,
) -> Result<crate::domain::Profile> {
    match given {
        Some(p) if !p.is_empty() => require_profile(store, &p),
        _ => pick_profile(store, "Profile"),
    }
}

fn add(store: &crate::storage::Store, profile: Option<String>) -> Result<()> {
    let theme = ColorfulTheme::default();
    let profile = resolve_profile(store, profile)?;
    ensure_password_ok(&profile)?;

    let name: String = Input::<String>::with_theme(&theme)
        .with_prompt("Provider name (e.g. deepseek)")
        .interact_text()?;
    let base_url: String = Input::<String>::with_theme(&theme)
        .with_prompt("Base URL")
        .default("https://api.deepseek.com".into())
        .interact_text()?;
    let auth_token = crate::cli::util::prompt_token(&theme, "API token", false)?;
    let kind = pick_kind(&theme)?;
    let description: String = Input::<String>::with_theme(&theme)
        .with_prompt("Description (optional)")
        .allow_empty(true)
        .interact_text()?;
    let extra_headers = prompt_headers()?;

    let provider = store.create_provider(
        profile.id.as_str(),
        NewProvider {
            name,
            description: if description.is_empty() {
                None
            } else {
                Some(description)
            },
            base_url,
            auth_token,
            kind,
            extra_headers,
        },
    )?;
    println!(
        "Added provider {:?} ({}) under profile {:?}.",
        provider.name,
        kind_label(&provider.kind),
        profile.name
    );
    Ok(())
}

fn list(store: &crate::storage::Store, profile: Option<String>) -> Result<()> {
    let profile = resolve_profile(store, profile)?;
    let providers = store.list_providers(profile.id.as_str())?;
    if providers.is_empty() {
        println!(
            "No providers configured for {:?}. Add one with `agos-proxy provider add`.",
            profile.name
        );
        return Ok(());
    }
    println!("{:<16} {:<14} {:<30} BASE URL", "ID", "KIND", "NAME");
    for p in providers {
        println!(
            "{:<16} {:<14} {:<30} {}",
            p.id,
            kind_label(&p.kind),
            p.name,
            p.base_url
        );
    }
    Ok(())
}

/// Interactively edit an existing provider: pick it, then re-enter its fields
/// with the current values pre-filled.
fn edit(store: &crate::storage::Store, profile: Option<String>) -> Result<()> {
    let theme = ColorfulTheme::default();
    let profile = resolve_profile(store, profile)?;
    ensure_password_ok(&profile)?;
    let provider = pick_provider(store, &profile, "Provider to edit")?;

    let name: String = Input::<String>::with_theme(&theme)
        .with_prompt("Provider name")
        .default(provider.name.clone())
        .interact_text()?;
    let base_url: String = Input::<String>::with_theme(&theme)
        .with_prompt("Base URL")
        .default(provider.base_url.clone())
        .interact_text()?;
    let auth_token =
        crate::cli::util::prompt_token(&theme, "API token (empty to keep current)", true)?;
    let final_token = if auth_token.is_empty() {
        provider.auth_token.clone()
    } else {
        auth_token
    };
    let kind = pick_kind(&theme)?;
    let description: String = Input::<String>::with_theme(&theme)
        .with_prompt("Description (optional)")
        .default(
            provider
                .description
                .as_deref()
                .map(|d| d.to_string())
                .unwrap_or_default(),
        )
        .allow_empty(true)
        .interact_text()?;
    let extra_headers = prompt_headers()?;

    store.update_provider(
        provider.id,
        NewProvider {
            name: name.clone(),
            description: if description.is_empty() {
                None
            } else {
                Some(description)
            },
            base_url,
            auth_token: final_token,
            kind,
            extra_headers,
        },
    )?;
    println!(
        "Updated provider {:?} under profile {:?}.",
        name, profile.name
    );
    Ok(())
}

/// Remove a provider after a confirmation.
fn delete(store: &crate::storage::Store, profile: Option<String>) -> Result<()> {
    let theme = ColorfulTheme::default();
    let profile = resolve_profile(store, profile)?;
    ensure_password_ok(&profile)?;
    let provider = pick_provider(store, &profile, "Provider to delete")?;
    let sure = Confirm::with_theme(&theme)
        .with_prompt(format!(
            "Delete provider {:?} ({})?",
            provider.name, provider.base_url
        ))
        .default(false)
        .interact()?;
    if !sure {
        println!("Cancelled.");
        return Ok(());
    }
    store.delete_provider(provider.id)?;
    println!("Deleted provider {:?}.", provider.name);
    Ok(())
}

fn pick_kind(theme: &ColorfulTheme) -> Result<ProviderKind> {
    let kinds = [
        ProviderKind::OpenAICompatible,
        ProviderKind::Anthropic,
        ProviderKind::Google,
        ProviderKind::Custom,
    ];
    let labels: Vec<&str> = kinds.iter().map(|k| kind_label(k)).collect();
    let idx = Select::with_theme(theme)
        .with_prompt("Provider kind")
        .items(&labels)
        .default(0)
        .interact()?;
    Ok(kinds[idx])
}
