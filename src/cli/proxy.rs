//! Proxy management: named groups of routes a profile exposes to callers.

use anyhow::Result;
use clap::Subcommand;
use dialoguer::{theme::ColorfulTheme, Input};

use crate::cli::util::{open_store, require_profile};

/// Subcommands under `agos proxy`.
#[derive(Debug, Subcommand)]
pub enum ProxyArgs {
    /// Create a new proxy under a profile (interactive wizard, or flag-driven).
    Create {
        /// Name of the owning profile.
        #[arg(long)]
        profile: Option<String>,
    },
}

/// Entry point for `agos proxy ...`.
pub fn run(args: ProxyArgs) -> Result<()> {
    let store = open_store()?;
    match args {
        ProxyArgs::Create { profile } => create(&store, profile),
    }
}

fn create(store: &crate::storage::Store, profile: Option<String>) -> Result<()> {
    let theme = ColorfulTheme::default();
    let profile_name = match profile {
        Some(p) if !p.is_empty() => p,
        _ => Input::<String>::with_theme(&theme)
            .with_prompt("Profile name")
            .interact_text()?,
    };
    let profile = require_profile(store, &profile_name)?;

    let name: String = Input::<String>::with_theme(&theme)
        .with_prompt("Proxy name (e.g. Programmer)")
        .interact_text()?;
    if store.get_proxy_named(profile.id.as_str(), &name)?.is_some() {
        anyhow::bail!(
            "a proxy named {name:?} already exists under {:?}",
            profile.name
        );
    }
    let description: String = Input::<String>::with_theme(&theme)
        .with_prompt("Description (optional)")
        .allow_empty(true)
        .interact_text()?;

    let proxy = store.create_proxy(
        profile.id.as_str(),
        &name,
        if description.is_empty() {
            None
        } else {
            Some(description.as_str())
        },
    )?;
    println!(
        "Created proxy {:?} under profile {:?} (id {}).",
        proxy.name, profile.name, proxy.id
    );
    Ok(())
}
