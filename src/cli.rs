use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "janus",
    version,
    about = "Manage a janus knowledge vault",
    long_about = None,
)]
pub struct Cli {
    #[arg(long, global = true, env = "JANUS_VAULT", value_name = "PATH")]
    pub vault: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create janus.toml and .janus/ for a vault
    Init,
}
