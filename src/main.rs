//! Process entry point: argument parsing, tracing, exit codes.
//!
//! Everything here is wiring. Deliberately no behaviour lives in this file, so
//! the same operations can be exposed over MCP and JSON-RPC without duplicating
//! decisions (§9.5).

use std::process::ExitCode;

use clap::Parser;
use janus_mem::cli::{Cli, Command};
use janus_mem::config::{self, Config};
use janus_mem::error::JanusError;
use janus_mem::{scan, vault};
use tracing_subscriber::EnvFilter;

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("janus=info")),
        )
        // stderr, never stdout: stdout is the results stream, and later the MCP
        // protocol stream, which must not be polluted by log lines (§8.2).
        .with_writer(std::io::stderr)
        .init();
}

fn run(cli: Cli) -> Result<(), JanusError> {
    match cli.command {
        // `init` creates a corpus, so it must *not* search for an existing one:
        // running it in a subdirectory of a vault should not silently adopt the
        // parent. It uses the M0 rule — explicit path, else the working
        // directory.
        Command::Init => {
            let root = config::resolve_vault(cli.vault)?;
            tracing::info!(vault = %root.display(), "vault resolved");

            let config_path = config::init_vault(&root)?;
            println!("Initialized empty Janus vault in {}", config_path.display());
            println!("Hint: Edit janus.toml to customize vault settings.");
            Ok(())
        }

        // Read commands discover the corpus upwards, so `janus scan` works from
        // anywhere inside a vault (§6.1).
        Command::Scan => {
            let root = vault::resolve_corpus(cli.vault)?;
            tracing::info!(vault = %root.display(), "corpus resolved");

            let config = Config::load(&root)?;
            scan::run(&root, &config)
        }
    }
}

fn main() -> ExitCode {
    init_tracing();

    let cli = Cli::parse();

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(err.exit_code())
        }
    }
}
