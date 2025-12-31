//! Profile management: the tenants that own providers, proxies and routes.

use anyhow::{bail, Context as _, Result};
use clap::Subcommand;
use dialoguer::{theme::ColorfulTheme, Confirm, Input};

use crate::cli::util::{hash_password, open_store};
use crate::storage::Store;

/// Subcommands under `agos-proxy profile`.
#[derive(Debug, Subcommand)]
pub enum ProfileArgs {
    /// Create a new profile (interactive wizard, or flag-driven).
    Create {
        /// Name of the profile, e.g. `coder1`.
        #[arg(long)]
        name: Option<String>,
    },
    /// List existing profiles and their API token identifiers.
    List,
    /// Show a single profile in full.
    Show {
        /// Name of the profile.
        name: String,
    },
    /// Manage a profile's API token.
    #[command(subcommand)]
    Token(TokenArgs),
}

/// Subcommands under `agos-proxy profile token`.
#[derive(Debug, Subcommand)]
pub enum TokenArgs {
    /// Generate a new API token for the profile.
    Rotate {
        /// Name of the profile.
        name: String,
    },
}

/// Entry point for `agos-proxy profile ...`.
pub fn run(args: ProfileArgs) -> Result<()> {
    let store = open_store()?;
    match args {
        ProfileArgs::Create { name } => create(&store, name),
        ProfileArgs::List => list(&store),
        ProfileArgs::Show { name } => show(&store, &name),
        ProfileArgs::Token(TokenArgs::Rotate { name }) => rotate(&store, &name),
    }
}

fn create(store: &Store, name: Option<String>) -> Result<()> {
    let theme = ColorfulTheme::default();
    let name = match name {
        Some(n) if !n.is_empty() => n,
        _ => Input::<String>::with_theme(&theme)
            .with_prompt("Profile name")
            .interact_text()?,
    };
    if store.get_profile_by_name(&name)?.is_some() {
        bail!("a profile named {name:?} already exists");
    }
    let description: Option<String> = Input::<String>::with_theme(&theme)
        .with_prompt("Description (optional)")
        .allow_empty(true)
        .interact_text()
        .map(|s| if s.is_empty() { None } else { Some(s) })?;
    let wants_password = Confirm::with_theme(&theme)
        .with_prompt("Protect this profile with a password?")
        .default(false)
        .interact()?;
    let password_hash = if wants_password {
        let pw = dialoguer::Password::with_theme(&theme)
            .with_prompt("Password")
            .interact()?;
        Some(hash_password(&pw)?)
    } else {
        None
    };
    let profile = store.create_profile(&name, description.as_deref(), password_hash.as_deref())?;
    println!("Created profile {:?}.", profile.name);
    println!("API token: {}", profile.id);
    println!("Store: {}", store.path().display());
    Ok(())
}

fn list(store: &Store) -> Result<()> {
    let profiles = store.list_profiles()?;
    if profiles.is_empty() {
        println!("No profiles yet. Create one with `agos-proxy profile create`.");
        return Ok(());
    }
    println!("{:<16} {:<20} DESCRIPTION", "TOKEN", "NAME");
    for p in profiles {
        let desc = p.description.as_deref().unwrap_or("-");
        let token_preview = if p.id.len() > 12 {
            format!("{}…", &p.id[..12])
        } else {
            p.id.clone()
        };
        println!("{:<16} {:<20} {}", token_preview, p.name, desc);
    }
    Ok(())
}

fn show(store: &Store, name: &str) -> Result<()> {
    let profile = store
        .get_profile_by_name(name)?
        .with_context(|| format!("no profile named {name:?}"))?;
    println!("Profile: {}", profile.name);
    println!("  token:       {}", profile.id);
    println!(
        "  description: {}",
        profile.description.as_deref().unwrap_or("-")
    );
    println!(
        "  password:    {}",
        if profile.password_hash.is_some() {
            "set"
        } else {
            "none"
        }
    );
    println!("  created:     {}", profile.created_at);
    println!("  updated:     {}", profile.updated_at);
    Ok(())
}

fn rotate(store: &Store, name: &str) -> Result<()> {
    let profile = store
        .get_profile_by_name(name)?
        .with_context(|| format!("no profile named {name:?}"))?;
    let token = store.rotate_profile_token(&profile.id)?;
    println!("Rotated token for {:?}.", profile.name);
    println!("New API token: {token}");
    Ok(())
}
