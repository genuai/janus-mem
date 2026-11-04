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

/// When a long build prints its progress (M4D §3.6).
///
/// `auto` asks the terminal, once, at the edge. Nothing below `main` reads the
/// terminal, so a captured stderr — a test, CI, a pipe — is silent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Progress {
    /// Print only when stderr is a terminal.
    #[default]
    Auto,
    /// Print even when stderr is a file or a pipe.
    Always,
    /// Print nothing.
    Never,
}

/// The operations `janus` currently supports.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create janus.toml and .janus/ for a vault
    Init,

    /// Report which Markdown files the corpus contains and what was skipped
    Scan {
        /// Emit one JSON object on stdout instead of a table
        #[arg(long)]
        json: bool,

        /// Include the per-entry skip list, not just per-rule counts
        #[arg(long)]
        verbose: bool,
    },

    /// Report each document's id, the rung behind it, and its match keys
    Ids {
        /// Restrict the report to these corpus paths
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,

        /// Emit one JSON object on stdout instead of the table
        #[arg(long)]
        json: bool,
    },

    /// Report the chunks the corpus produces, without building an index
    Chunks {
        /// Emit one JSON object on stdout instead of the table
        #[arg(long)]
        json: bool,

        /// Append a truncated preview of each chunk's text
        #[arg(long)]
        text: bool,

        /// Cap the chunks reported per document
        #[arg(long, value_name = "N")]
        limit: Option<usize>,

        /// Restrict the report to these corpus paths
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,
    },

    /// Build or refresh the lexical index (M3)
    Index {
        /// Re-tokenize every chunk instead of reusing tokens (§3.4)
        #[arg(long)]
        rebuild: bool,

        /// Index lexically and drop any vectors, even when a model resolves
        #[arg(long)]
        no_embeddings: bool,

        /// Report the receipt without writing anything
        #[arg(long)]
        dry_run: bool,

        /// Emit one JSON receipt on stdout instead of the summary
        #[arg(long)]
        json: bool,

        /// Report embedding progress: auto, always or never
        #[arg(long, value_name = "WHEN", value_enum, default_value = "auto")]
        progress: Progress,

        /// Re-tokenize these documents only; the index still covers the corpus
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,
    },

    /// Search the index: one result per document at its best chunk (§3.1)
    Search {
        /// The query text
        #[arg(value_name = "QUERY")]
        query: String,

        /// Ranking intent: current (default), history, evolution, exhaustive
        #[arg(long, value_name = "INTENT")]
        intent: Option<String>,

        /// Add a scope to the query set; may be repeated (§6.12)
        #[arg(long, value_name = "SCOPE")]
        scope: Vec<String>,

        /// Exclude global and shared-only matches; requires --scope (§9 gap 10)
        #[arg(long)]
        scope_only: bool,

        /// Maximum documents returned, after grouping
        #[arg(long, value_name = "N", default_value_t = crate::search::DEFAULT_LIMIT)]
        limit: usize,

        /// Restrict to these canonical types or configured aliases
        #[arg(long, value_name = "T,..", value_delimiter = ',')]
        types: Vec<String>,

        /// Restrict to documents carrying any of these tags
        #[arg(long, value_name = "T,..", value_delimiter = ',')]
        tags: Vec<String>,

        /// Override the per-type decay half-life in days (§9 gap 9)
        #[arg(long, value_name = "DAYS")]
        decay_half_life: Option<f64>,

        /// Include declared-superseded documents under intent=current (§3.2)
        #[arg(long)]
        include_superseded: bool,

        /// Select the valid-time window at this date: YYYY-MM-DD, YYYY-MM or YYYY
        #[arg(long, value_name = "DATE")]
        as_of: Option<String>,

        /// Refuse instead of building or rebuilding; a read stays a read
        #[arg(long)]
        no_rebuild: bool,

        /// Rank lexically without resolving or loading an embedding model
        #[arg(long)]
        no_embeddings: bool,

        /// Report embedding progress: auto, always or never
        #[arg(long, value_name = "WHEN", value_enum, default_value = "auto")]
        progress: Progress,

        /// Emit one JSON object on stdout instead of the table
        #[arg(long)]
        json: bool,
    },

    /// List authoritative facts: active decisions, specs and principles (§3.6)
    Facts {
        /// Narrow the listing by relevance and report the score
        #[arg(long, value_name = "Q")]
        query: Option<String>,

        /// Restrict to documents carrying any of these tags
        #[arg(long, value_name = "T,..", value_delimiter = ',')]
        tags: Vec<String>,

        /// Add a scope to the query set; may be repeated (§6.12)
        #[arg(long, value_name = "SCOPE")]
        scope: Vec<String>,

        /// Exclude global and shared-only matches; requires --scope (§9 gap 10)
        #[arg(long)]
        scope_only: bool,

        /// Maximum facts returned
        #[arg(long, value_name = "N", default_value_t = crate::search::DEFAULT_LIMIT)]
        limit: usize,

        /// Refuse instead of building or rebuilding; a read stays a read
        #[arg(long)]
        no_rebuild: bool,

        /// List lexically without resolving or loading an embedding model
        #[arg(long)]
        no_embeddings: bool,

        /// Report embedding progress: auto, always or never
        #[arg(long, value_name = "WHEN", value_enum, default_value = "auto")]
        progress: Progress,

        /// Emit one JSON object on stdout instead of the table
        #[arg(long)]
        json: bool,
    },
}
