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

use std::path::PathBuf;

use agos::cli::Cli;
use clap::{CommandFactory as _, ValueEnum as _};
use clap_complete::{generate_to, Shell};

fn out_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/docs")
}

fn main() -> anyhow::Result<()> {
    let root = out_dir();
    let man_dir = root.join("man");
    let comp_dir = root.join("completions");
    std::fs::create_dir_all(&man_dir)?;
    std::fs::create_dir_all(&comp_dir)?;

    let mut cmd = Cli::command();
    cmd.set_bin_name("agos-proxy");

    // Man pages: one per command, section 1.
    let man = clap_mangen::Man::new(cmd.clone())
        .title("AGOS-PROXY")
        .section("1")
        .source("AGOS Proxy")
        .manual("AGOS Proxy Manual");
    man.generate_to(&man_dir)?;
    println!("man pages -> {}", man_dir.display());

    // Shell completions.
    for shell in Shell::value_variants() {
        let mut cmd = Cli::command();
        cmd.set_bin_name("agos-proxy");
        let path = generate_to(*shell, &mut cmd, "agos-proxy", &comp_dir)?;
        println!("completion ({shell}) -> {}", path.display());
    }

    // Markdown CLI reference.
    let md = clap_markdown::help_markdown::<Cli>();
    let cli_md = root.join("CLI.md");
    std::fs::write(&cli_md, &md)?;
    let docs_md = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/CLI.md");
    std::fs::write(&docs_md, &md)?;
    println!("CLI reference -> {} (+ docs/CLI.md)", cli_md.display());

    Ok(())
}
