//! Developer-only doc generator: man pages, shell completions, CLI reference.
//!
//! Run explicitly when cutting a release — it is *not* part of the default
//! build and never ships to users:
//!
//! ```sh
//! cargo run --release --features gen-docs --bin gen-docs
//! ```
//!
//! Output lands in `target/docs/`:
//! - `man/agos-proxy.1` (+ one page per subcommand)
//! - `completions/agos-proxy.{bash,fish,zsh,_agos-proxy.ps1}`
//! - `CLI.md` (full command reference, also copied to `docs/CLI.md`)
//!
//! This is a thin wrapper over the `gen-docs` logic also exposed as the
//! `agos-proxy gen-docs --all` subcommand.

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/docs");
    agos::cli::gen_docs::generate(&root, true, true)?;
    Ok(())
}
