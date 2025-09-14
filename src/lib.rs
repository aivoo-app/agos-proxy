//! AGOS Proxy — a self-hosted AI gateway with OpenAI-compatible endpoints and
//! automatic multi-provider failover.
//!
//! The crate is organised as a small set of coherent modules:
//!
//! - [`domain`]  — the core data model: profiles, providers, proxies and routes.
//! - [`cli`]     — the interactive CLI (`clap` command tree and wizards).
//! - [`storage`] — persistence of the domain model to a local store.
//! - [`server`]  — the HTTP surface exposed to callers (work in progress).
//! - [`crypto`]  — handling of secrets at rest (work in progress).

pub mod cli;
pub mod crypto;
pub mod domain;
pub mod server;
pub mod storage;

/// Version of the crate, surfaced to the CLI and HTTP `Server` header.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
