//! Route management: addressable fallback chains of models inside a proxy.

use anyhow::{bail, Result};
use clap::Subcommand;

/// Subcommands under `agos route`.
#[derive(Debug, Subcommand)]
pub enum RouteArgs {
    /// Create a new route under a proxy (interactive wizard, or flag-driven).
    Create {
        /// Name of the owning proxy.
        #[arg(long)]
        proxy: Option<String>,
    },
    /// Show the live health state of every model in a route's chain.
    Status {
        /// Name of the route.
        #[arg(long)]
        route: Option<String>,
    },
}

/// Entry point for `agos route ...`.
pub fn run(args: RouteArgs) -> Result<()> {
    match args {
        RouteArgs::Create { .. } | RouteArgs::Status { .. } => {
            bail!("route management is not implemented yet; it arrives with the MVP milestone")
        }
    }
}
