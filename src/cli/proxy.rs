//! Proxy management: named groups of routes a profile exposes to callers.

use anyhow::{bail, Result};
use clap::Subcommand;

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
    match args {
        ProxyArgs::Create { .. } => {
            bail!("proxy management is not implemented yet; it arrives with the MVP milestone")
        }
    }
}
