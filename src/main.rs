use clap::Parser;

use agos::cli::Cli;
use agos::VERSION;

fn main() {
    let cli = Cli::parse();
    if let Err(err) = agos::cli::run(cli) {
        eprintln!("agos-proxy v{VERSION}: {err}");
        std::process::exit(1);
    }
}
