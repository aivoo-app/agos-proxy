//! Provider management: the upstreams (base URL + credentials) a profile talks to.

use anyhow::{bail, Result};
use clap::Subcommand;

/// Subcommands under `agos provider`.
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

/// Entry point for `agos provider ...`.
pub fn run(args: ProviderArgs) -> Result<()> {
    match args {
        ProviderArgs::Add { .. } | ProviderArgs::List { .. } => {
            bail!("provider management is not implemented yet; it arrives with the MVP milestone")
        }
    }
}
