use std::process::ExitCode;

use clap::Parser;
use janus_mem::cli::{Cli, Command};
use janus_mem::config;
use janus_mem::error::JanusError;
use tracing_subscriber::EnvFilter;

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("janus=info")),
        )
        .with_writer(std::io::stderr)
        .init();
}

fn run(cli: Cli) -> Result<(), JanusError> {
    let vault_path = config::resolve_vault(cli.vault)?;
    tracing::info!(vault = %&vault_path.display(), "vault resolved");

    match cli.command {
        Command::Init => {
            let config_path = config::init_vault(&vault_path)?;
            println!("Initialized empty Janus vault in {}", config_path.display());
            println!("Hint: Edit janus.toml to customize vault settings.");
        }
    }

    Ok(())
}

fn main() -> std::process::ExitCode {
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
