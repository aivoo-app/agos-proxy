//! Profile management: the tenants that own providers, proxies and routes.

use anyhow::{bail, Context as _, Result};
use clap::Subcommand;
use dialoguer::{theme::ColorfulTheme, Confirm, Input};

use crate::cli::util::{ensure_password_ok, hash_password, open_store, pick_profile};
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
    /// View or change a profile's requests-per-minute limit (0 = unlimited).
    /// Note: rate limiting is per-process; multiple proxy instances each enforce
    /// their own limit independently.
    Limit {
        /// Name of the profile.
        name: String,
        /// New requests-per-minute ceiling; omit to just show the current one.
        rpm: Option<i64>,
    },
    /// Edit a profile's name, description and password.
    Edit {
        /// Name of the profile to edit.
        #[arg(long)]
        name: Option<String>,
    },
    /// Delete a profile and everything under it.
    Delete {
        /// Name of the profile to delete.
        #[arg(long)]
        name: Option<String>,
        /// Skip the confirmation prompt (for scripts/CI).
        #[arg(long)]
        yes: bool,
    },
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
        ProfileArgs::Limit { name, rpm } => limit(&store, &name, rpm),
        ProfileArgs::Edit { name } => edit(&store, name),
        ProfileArgs::Delete { name, yes } => delete(&store, name, yes),
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
    let wants_limit = Confirm::with_theme(&theme)
        .with_prompt("Limit API requests per minute? (rate limiting)")
        .default(false)
        .interact()?;
    let rpm_limit = if wants_limit {
        Input::<i64>::with_theme(&theme)
            .with_prompt("Requests per minute")
            .default(60)
            .interact()?
    } else {
        0
    };
    let profile = store.create_profile(&name, description.as_deref(), password_hash.as_deref())?;
    if rpm_limit > 0 {
        store.set_profile_rpm_limit(&profile.id, rpm_limit)?;
    }
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
    println!("{:<16} {:<20} {:<24} DESCRIPTION", "TOKEN", "NAME", "LIMIT");
    for p in profiles {
        let desc = p.description.as_deref().unwrap_or("-");
        let token_preview = if p.id.len() > 12 {
            format!("{}…", &p.id[..12])
        } else {
            p.id.clone()
        };
        let limit = if p.rpm_limit > 0 {
            format!("{} rpm", p.rpm_limit)
        } else {
            "unlimited".to_string()
        };
        println!(
            "{:<16} {:<20} {:<24} {}",
            token_preview, p.name, limit, desc
        );
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
        "  rate limit:  {}",
        if profile.rpm_limit > 0 {
            format!("{} req/min", profile.rpm_limit)
        } else {
            "unlimited".to_string()
        }
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
    ensure_password_ok(&profile)?;
    let token = store.rotate_profile_token(&profile.id)?;
    println!("Rotated token for {:?}.", profile.name);
    println!("New API token: {token}");
    Ok(())
}

fn limit(store: &Store, name: &str, rpm: Option<i64>) -> Result<()> {
    let profile = store
        .get_profile_by_name(name)?
        .with_context(|| format!("no profile named {name:?}"))?;
    let current = if profile.rpm_limit > 0 {
        format!("{} req/min", profile.rpm_limit)
    } else {
        "unlimited".to_string()
    };
    match rpm {
        None => {
            println!("Rate limit for {:?}: {current}", profile.name);
        }
        Some(rpm) => {
            if rpm < 0 {
                bail!("the requests-per-minute limit cannot be negative");
            }
            store.set_profile_rpm_limit(&profile.id, rpm)?;
            let updated = if rpm == 0 {
                "unlimited".to_string()
            } else {
                format!("{rpm} req/min")
            };
            println!("Rate limit for {:?}: {current} -> {updated}", profile.name);
        }
    }
    Ok(())
}

/// Resolve a profile by name, or let the user pick one from a menu.
fn resolve_profile(store: &Store, given: Option<String>) -> Result<crate::domain::Profile> {
    use crate::cli::util::require_profile;
    match given {
        Some(n) if !n.is_empty() => require_profile(store, &n),
        _ => pick_profile(store, "Profile"),
    }
}

/// Interactively edit a profile: name, description and password.
fn edit(store: &Store, name: Option<String>) -> Result<()> {
    use dialoguer::{theme::ColorfulTheme, Confirm, Input};
    let theme = ColorfulTheme::default();
    let profile = resolve_profile(store, name)?;
    ensure_password_ok(&profile)?;

    let new_name: String = Input::<String>::with_theme(&theme)
        .with_prompt("Profile name")
        .default(profile.name.clone())
        .interact_text()?;
    if new_name != profile.name && store.get_profile_by_name(&new_name)?.is_some() {
        anyhow::bail!("a profile named {new_name:?} already exists");
    }
    let description: String = Input::<String>::with_theme(&theme)
        .with_prompt("Description (optional)")
        .default(
            profile
                .description
                .as_deref()
                .map(|d| d.to_string())
                .unwrap_or_default(),
        )
        .allow_empty(true)
        .interact_text()?;
    let wants_password = Confirm::with_theme(&theme)
        .with_prompt("Protect this profile with a password?")
        .default(profile.password_hash.is_some())
        .interact()?;

    if new_name != profile.name {
        store.rename_profile(profile.id.as_str(), &new_name)?;
    }
    let new_desc: Option<&str> = if description.is_empty() {
        None
    } else {
        Some(description.as_str())
    };
    store.set_profile_description(profile.id.as_str(), new_desc)?;
    if wants_password {
        let has_changed = Confirm::with_theme(&theme)
            .with_prompt("Set a new password now?")
            .default(false)
            .interact()?;
        if has_changed {
            let pw = dialoguer::Password::with_theme(&theme)
                .with_prompt("New password")
                .interact()?;
            store.set_profile_password(profile.id.as_str(), &hash_password(&pw)?)?;
        }
    } else if profile.password_hash.is_some() {
        store.clear_profile_password(profile.id.as_str())?;
    }
    println!("Updated profile {:?}.", new_name);
    Ok(())
}

/// Delete a profile and everything under it, after a confirmation.
fn delete(store: &Store, name: Option<String>, yes: bool) -> Result<()> {
    use dialoguer::{theme::ColorfulTheme, Confirm};
    let theme = ColorfulTheme::default();
    let profile = resolve_profile(store, name)?;
    ensure_password_ok(&profile)?;
    if !yes {
        let sure = Confirm::with_theme(&theme)
            .with_prompt(format!(
                "Delete profile {:?} and ALL its providers, proxies and routes?",
                profile.name
            ))
            .default(false)
            .interact()?;
        if !sure {
            println!("Cancelled.");
            return Ok(());
        }
    }
    store.delete_profile(profile.id.as_str())?;
    println!("Deleted profile {:?}.", profile.name);
    Ok(())
}
