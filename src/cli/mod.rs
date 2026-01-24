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
//! agos-proxy
//! ├── serve                     start the proxy server
//! ├── profile                   manage profiles / API tokens
//! │   ├── create  list  show    ...
//! │   └── token rotate          rotate a profile's API token
//! ├── provider  add  list       manage providers for a profile
//! ├── proxy     create          manage proxies
//! ├── route     create  status  manage routes and their model chains
//! ├── chat                      test a proxy/route interactively
//! ├── setup                     guided terminal setup wizard
//! └── config    export  import  move a profile setup between machines
//! ```

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};

mod bootstrap;
mod chat;
mod config;
mod profile;
mod provider;
mod proxy;
mod route;
mod setup;
mod usage;
pub mod util;

/// Top-level entry point parsed from the command line.
#[derive(Debug, Parser)]
#[command(
    name = "agos-proxy",
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
    Serve {
        /// Address to bind, e.g. 0.0.0.0:8080.
        #[arg(long, default_value = "127.0.0.1:3000")]
        bind: String,
    },
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
    /// Test a proxy/route interactively before wiring up a client.
    Chat(chat::ChatArgs),
    /// Export or import a profile setup.
    #[command(subcommand)]
    Config(config::ConfigArgs),
    /// Show per-request usage logs and aggregates for a profile.
    #[command(subcommand)]
    Usage(usage::UsageArgs),
    /// Seed a profile setup non-interactively from a JSON document.
    #[command(subcommand)]
    Bootstrap(bootstrap::BootstrapArgs),
    /// Guided terminal setup wizard: profile -> providers -> proxies -> routes.
    Setup(setup::SetupArgs),
}

/// Where AGOS Proxy looks for its working files. Uses the platform config dir
/// (e.g. `proxy` on Linux) so nothing is scattered around the cwd.
pub fn data_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("AGOS_HOME") {
        return Ok(PathBuf::from(dir));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .context("cannot determine a home config directory")?;
    Ok(base.join("agos-proxy"))
}

/// Dispatch a parsed command line and run it to completion.
pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Serve { bind } => server_command(&bind),
        Command::Profile(args) => profile::run(args),
        Command::Provider(args) => provider::run(args),
        Command::Proxy(args) => proxy::run(args),
        Command::Route(args) => route::run(args),
        Command::Chat(args) => chat::run(args),
        Command::Config(args) => config::run(args),
        Command::Usage(args) => usage::run(args),
        Command::Bootstrap(args) => bootstrap::run(args),
        Command::Setup(args) => setup::run(args),
    }
}

fn server_command(bind: &str) -> Result<()> {
    // The runtime is intentionally small: open the store, build the router, and
    // hand off to tokio. All the interesting work happens inside the handlers.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let addr = bind.to_string();
    rt.block_on(async move { crate::server::serve(&addr).await })?;
    Ok(())
}
