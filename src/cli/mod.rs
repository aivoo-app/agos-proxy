//! Command-line interface for AGOS Proxy.
//!
//! The CLI is the primary way to configure and operate the gateway: profiles,
//! providers, proxies and routes are created here, and `serve` runs the HTTP
//! server. Alongside each wizard there is a flag-only form so setup can be
//! scripted and committed to source control.
//!
//! The command tree:
//!
//! ```text
//! agos
//! ├── serve                     start the proxy server
//! ├── profile                   manage profiles / API tokens
//! │   ├── create  list  show    ...
//! │   └── token rotate          rotate a profile's API token
//! ├── provider  add  list       manage providers for a profile
//! ├── proxy     create          manage proxies
//! ├── route     create  status  manage routes and their model chains
//! └── config    export  import  move a profile setup between machines
//! ```

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};

mod config;
mod profile;
mod provider;
mod proxy;
mod route;
pub mod util;

/// Top-level entry point parsed from the command line.
#[derive(Debug, Parser)]
#[command(
    name = "agos",
    version,
    about = "Self-hosted OpenAI-compatible AI gateway with multi-provider failover"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// The set of top-level commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start the proxy server.
    Serve,
    /// Manage profiles (tenants) and their API tokens.
    #[command(subcommand)]
    Profile(profile::ProfileArgs),
    /// Manage providers (upstreams + credentials) for a profile.
    #[command(subcommand)]
    Provider(provider::ProviderArgs),
    /// Manage proxies for a profile.
    #[command(subcommand)]
    Proxy(proxy::ProxyArgs),
    /// Manage routes and their ordered model chains.
    #[command(subcommand)]
    Route(route::RouteArgs),
    /// Export or import a profile setup.
    #[command(subcommand)]
    Config(config::ConfigArgs),
}

/// Where AGOS Proxy looks for its working files. Uses the platform config dir
/// (e.g. `~/.config/agos` on Linux) so nothing is scattered around the cwd.
pub fn data_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("AGOS_HOME") {
        return Ok(PathBuf::from(dir));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .context("cannot determine a home config directory")?;
    Ok(base.join("agos"))
}

/// Dispatch a parsed command line and run it to completion.
pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Serve => server_command(),
        Command::Profile(args) => profile::run(args),
        Command::Provider(args) => provider::run(args),
        Command::Proxy(args) => proxy::run(args),
        Command::Route(args) => route::run(args),
        Command::Config(args) => config::run(args),
    }
}

fn server_command() -> Result<()> {
    // The runtime is intentionally small: open the store, build the router, and
    // hand off to tokio. All the interesting work happens inside the handlers.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async { crate::server::serve("127.0.0.1:3000").await })?;
    Ok(())
}
