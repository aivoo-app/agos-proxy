//! Profile management: the tenants that own providers, proxies and routes.

use anyhow::{bail, Result};
use clap::Subcommand;

/// Subcommands under `agos profile`.
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

/// Subcommands under `agos profile token`.
#[derive(Debug, Subcommand)]
pub enum TokenArgs {
    /// Generate a new API token for the profile.
    Rotate {
        /// Name of the profile.
        name: String,
    },
}

/// Entry point for `agos profile ...`.
pub fn run(args: ProfileArgs) -> Result<()> {
    match args {
        ProfileArgs::Create { .. }
        | ProfileArgs::List
        | ProfileArgs::Show { .. }
        | ProfileArgs::Token(_) => {
            bail!("profile management is not implemented yet; it arrives with the MVP milestone")
        }
    }
}
