//! Documentation generation for the AGOS Proxy CLI.
//!
//! This backs the `agos-proxy gen-docs` subcommand and the standalone
//! `gen-docs` binary so there is a single source of truth for the generated
//! man page, shell completions and markdown command reference.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::CommandFactory as _;
use clap::ValueEnum;
use clap_complete::{generate_to, Shell};

use super::Cli;

/// Generate CLI documentation into `output_dir` (created if missing).
///
/// Always writes the full markdown command reference (`CLI.md`). When `all` is
/// set it also emits a roff man page under `man/` and shell completions under
/// `completions/`. If `copy_to_docs` is set, the markdown reference is also
/// copied to `docs/CLI.md` (used by the developer-only `gen-docs` binary).
pub fn generate(output_dir: &Path, all: bool, copy_to_docs: bool) -> Result<PathBuf> {
    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("creating doc dir {}", output_dir.display()))?;

    // Full command reference (always generated; complete CLI docs for every
    // subcommand).
    let cli_md = output_dir.join("CLI.md");
    let markdown = clap_markdown::help_markdown::<Cli>();
    std::fs::write(&cli_md, &markdown)
        .with_context(|| format!("writing CLI reference to {}", cli_md.display()))?;
    println!("CLI reference -> {}", cli_md.display());

    if all {
        let mut root_cmd = Cli::command();
        root_cmd.set_bin_name("agos-proxy");

        // Roff man page, section 1.
        let man_dir = output_dir.join("man");
        std::fs::create_dir_all(&man_dir)?;
        let man = clap_mangen::Man::new(root_cmd.clone())
            .title("AGOS-PROXY")
            .section("1")
            .source("AGOS Proxy")
            .manual("AGOS Proxy Manual");
        man.generate_to(&man_dir)?;
        println!("man pages -> {}", man_dir.display());

        // Shell completions for every supported shell.
        let comp_dir = output_dir.join("completions");
        std::fs::create_dir_all(&comp_dir)?;
        for shell in Shell::value_variants() {
            generate_to(*shell, &mut root_cmd, "agos-proxy", &comp_dir)?;
        }
        println!("completions -> {}", comp_dir.display());
    }

    if copy_to_docs {
        let docs_md = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/CLI.md");
        std::fs::write(&docs_md, &markdown)
            .with_context(|| format!("writing {}", docs_md.display()))?;
        println!("CLI reference -> {} (+ docs/CLI.md)", cli_md.display());
    }

    Ok(cli_md)
}

/// Run the `gen-docs` subcommand, writing into the requested output directory.
pub fn run(output_dir: PathBuf, all: bool) -> Result<()> {
    generate(&output_dir, all, false)?;
    Ok(())
}
