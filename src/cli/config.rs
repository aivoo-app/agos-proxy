//! Config export/import: moving a profile setup between machines.

use anyhow::{bail, Result};
use clap::Subcommand;

/// Subcommands under `agos config`.
#[derive(Debug, Subcommand)]
pub enum ConfigArgs {
    /// Export a profile's setup (providers, proxies, routes) to a portable file.
    Export {
        /// Name of the profile to export.
        profile: Option<String>,
    },
    /// Import a profile setup previously written by `export`.
    Import {
        /// Path to the portable profile file.
        path: Option<std::path::PathBuf>,
    },
}

/// Entry point for `agos config ...`.
pub fn run(args: ConfigArgs) -> Result<()> {
    match args {
        ConfigArgs::Export { .. } | ConfigArgs::Import { .. } => {
            bail!("config export/import is not implemented yet; it arrives with a later milestone")
        }
    }
}
