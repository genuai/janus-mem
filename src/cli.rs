//! Command-line surface: argument parsing and exit-code wiring (SPEC §9.1).
//!
//! This module only describes *what the user asked for*. It contains no
//! behaviour: `main` matches on [`Command`] and calls into the library. That
//! keeps the CLI a thin adapter, so the same operations can be exposed later
//! over MCP and JSON-RPC without duplicating a single decision (§9.5).

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// The parsed command line.
///
/// Every subcommand accepts `--vault` because a corpus root is ambient
/// configuration, not an argument: requiring it on each invocation would make
/// the tool useless from an agent host that spawns a fresh process per turn.
#[derive(Debug, Parser)]
#[command(
    name = "janus",
    version,
    about = "Manage a janus knowledge vault",
    long_about = None,
)]
pub struct Cli {
    /// Corpus root. Falls back to `JANUS_VAULT`, then the current directory.
    #[arg(long, global = true, env = "JANUS_VAULT", value_name = "PATH")]
    pub vault: Option<PathBuf>,

    /// The operation to perform.
    #[command(subcommand)]
    pub command: Command,
}

/// The operations `janus` currently supports.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create janus.toml and .janus/ for a vault
    Init,
}
