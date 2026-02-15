//! Proxy management: named groups of routes a profile exposes to callers.

use anyhow::Result;
use clap::Subcommand;

use crate::cli::util::{ensure_password_ok, open_store, pick_profile, pick_proxy};
use crate::storage::Store;

/// Subcommands under `agos-proxy proxy`.
#[derive(Debug, Subcommand)]
pub enum ProxyArgs {
    /// Create a new proxy under a profile (interactive wizard, or flag-driven).
    Create {
        /// Name of the owning profile.
        #[arg(long)]
        profile: Option<String>,
    },
    /// List the proxies under a profile.
    List {
        /// Name of the owning profile.
        #[arg(long)]
        profile: Option<String>,
    },
    /// Edit a proxy's name/description.
    Edit {
        /// Name of the owning profile.
        #[arg(long)]
        profile: Option<String>,
    },
    /// Remove a proxy (and its routes).
    Delete {
        /// Name of the owning profile.
        #[arg(long)]
        profile: Option<String>,
    },
}

/// Entry point for `agos-proxy proxy ...`.
pub fn run(args: ProxyArgs) -> Result<()> {
    let store = open_store()?;
    match args {
        ProxyArgs::Create { profile } => create(&store, profile),
        ProxyArgs::List { profile } => list(&store, profile),
        ProxyArgs::Edit { profile } => edit(&store, profile),
        ProxyArgs::Delete { profile } => delete(&store, profile),
    }
}

/// Resolve the owning profile from a flag, or let the user pick one.
fn resolve_profile(store: &Store, given: Option<String>) -> Result<crate::domain::Profile> {
    use crate::cli::util::require_profile;
    match given {
        Some(p) if !p.is_empty() => require_profile(store, &p),
        _ => pick_profile(store, "Profile"),
    }
}

fn create(store: &Store, profile: Option<String>) -> Result<()> {
    use dialoguer::{theme::ColorfulTheme, Input};
    let theme = ColorfulTheme::default();
    let profile = resolve_profile(store, profile)?;
    ensure_password_ok(&profile)?;

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

fn list(store: &Store, profile: Option<String>) -> Result<()> {
    let profile = resolve_profile(store, profile)?;
    let proxies = store.list_proxies(profile.id.as_str())?;
    if proxies.is_empty() {
        println!(
            "No proxies configured for {:?}. Create one with `agos-proxy proxy create`.",
            profile.name
        );
        return Ok(());
    }
    println!("{:<6} {:<24} DESCRIPTION", "ID", "NAME");
    for p in proxies {
        let desc = p.description.as_deref().unwrap_or("-");
        println!("{:<6} {:<24} {}", p.id, p.name, desc);
    }
    Ok(())
}

fn edit(store: &Store, profile: Option<String>) -> Result<()> {
    use dialoguer::{theme::ColorfulTheme, Input};
    let theme = ColorfulTheme::default();
    let profile = resolve_profile(store, profile)?;
    ensure_password_ok(&profile)?;
    let proxy = pick_proxy(store, &profile, "Proxy to edit")?;

    let name: String = Input::<String>::with_theme(&theme)
        .with_prompt("Proxy name")
        .default(proxy.name.clone())
        .interact_text()?;
    if name != proxy.name && store.get_proxy_named(profile.id.as_str(), &name)?.is_some() {
        anyhow::bail!(
            "a proxy named {name:?} already exists under {:?}",
            profile.name
        );
    }
    let description: String = Input::<String>::with_theme(&theme)
        .with_prompt("Description (optional)")
        .default(
            proxy
                .description
                .as_deref()
                .map(|d| d.to_string())
                .unwrap_or_default(),
        )
        .allow_empty(true)
        .interact_text()?;

    store.update_proxy(
        proxy.id,
        &name,
        if description.is_empty() {
            None
        } else {
            Some(description.as_str())
        },
    )?;
    println!("Updated proxy {:?} under profile {:?}.", name, profile.name);
    Ok(())
}

fn delete(store: &Store, profile: Option<String>) -> Result<()> {
    use dialoguer::{theme::ColorfulTheme, Confirm};
    let theme = ColorfulTheme::default();
    let profile = resolve_profile(store, profile)?;
    ensure_password_ok(&profile)?;
    let proxy = pick_proxy(store, &profile, "Proxy to delete")?;
    let sure = Confirm::with_theme(&theme)
        .with_prompt(format!("Delete proxy {:?} and all its routes?", proxy.name))
        .default(false)
        .interact()?;
    if !sure {
        println!("Cancelled.");
        return Ok(());
    }
    store.delete_proxy(proxy.id)?;
    println!("Deleted proxy {:?}.", proxy.name);
    Ok(())
}
