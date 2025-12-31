//! Provider management: the upstreams (base URL + credentials) a profile talks to.

use anyhow::Result;
use clap::Subcommand;
use dialoguer::{theme::ColorfulTheme, Input, Select};

use crate::cli::util::{kind_label, open_store, prompt_headers, require_profile};
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
}

/// Entry point for `agos-proxy provider ...`.
pub fn run(args: ProviderArgs) -> Result<()> {
    let store = open_store()?;
    match args {
        ProviderArgs::Add { profile } => add(&store, profile),
        ProviderArgs::List { profile } => list(&store, profile),
    }
}

fn add(store: &crate::storage::Store, profile: Option<String>) -> Result<()> {
    let theme = ColorfulTheme::default();
    let profile_name = match profile {
        Some(p) if !p.is_empty() => p,
        _ => Input::<String>::with_theme(&theme)
            .with_prompt("Profile name")
            .interact_text()?,
    };
    let profile = require_profile(store, &profile_name)?;

    let name: String = Input::<String>::with_theme(&theme)
        .with_prompt("Provider name (e.g. deepseek)")
        .interact_text()?;
    let base_url: String = Input::<String>::with_theme(&theme)
        .with_prompt("Base URL")
        .default("https://api.deepseek.com".into())
        .interact_text()?;
    let auth_token = dialoguer::Password::with_theme(&theme)
        .with_prompt("API token")
        .interact()?;
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
    let theme = ColorfulTheme::default();
    let profile_name = match profile {
        Some(p) if !p.is_empty() => p,
        _ => Input::<String>::with_theme(&theme)
            .with_prompt("Profile name")
            .interact_text()?,
    };
    let profile = require_profile(store, &profile_name)?;
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
