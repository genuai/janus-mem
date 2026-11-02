//! Process entry point: argument parsing, tracing, exit codes.
//!
//! Everything here is wiring. Deliberately no behaviour lives in this file, so
//! the same operations can be exposed over MCP and JSON-RPC without duplicating
//! decisions (§9.5).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use janus_mem::cli::{Cli, Command};
use janus_mem::clock::SystemClock;
use janus_mem::config::{self, Config};
use janus_mem::error::JanusError;
use janus_mem::index::embed::{self, Embedder, ModelDirs, Resolved};
use janus_mem::{chunk, index, scan, search, vault};
use tracing_subscriber::EnvFilter;

/// The directories a model may be read from, highest precedence first (V-25).
///
/// Built here, at the edge, so nothing below `main` reads `std::env`. The cache
/// is the same well-known location the model download of a later phase writes,
/// and it is never opened by `resolve` unless the configured name needs it.
fn model_dirs(config: &Config) -> ModelDirs {
    ModelDirs {
        config: config.embedding.model_dir.clone(),
        env: std::env::var_os("JANUS_MODEL_DIR").map(PathBuf::from),
        cache: model_cache(),
    }
}

/// The platform's model cache, without a dependency just to find it (§3.2).
fn model_cache() -> PathBuf {
    let base = match std::env::var_os("XDG_CACHE_HOME")
        .or_else(|| std::env::var_os("LOCALAPPDATA"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
    {
        Some(base) => base,
        None => std::env::temp_dir(),
    };
    base.join("fastembed")
}

/// Resolves config × feature × model availability once, at the edge (§3.2).
///
/// A degraded resolution returns `None` and the caller publishes a lexical
/// generation. The reason is not printed here: P2 puts it in every response's
/// `degraded` field, and P1b's only embedding line on stderr is the one a
/// dropped payload earns.
fn resolve_embedder(config: &Config) -> Option<Box<dyn Embedder>> {
    match embed::resolve(config, &model_dirs(config)) {
        Resolved::Ready(embedder) => Some(embedder),
        Resolved::Degraded(..) => None,
    }
}

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
        Command::Scan { json, verbose } => {
            let root = vault::resolve_corpus(cli.vault)?;
            tracing::info!(vault = %root.display(), "corpus resolved");

            let config = Config::load(&root)?;
            scan::run(&root, &config, scan::Options { json, verbose })
        }

        // Chunking is a read command too: it shares the discovery, resolution and
        // identity pipeline with `scan` and stops one step later (M2 phase 3).
        Command::Chunks {
            json,
            text,
            limit,
            paths,
        } => {
            let root = vault::resolve_corpus(cli.vault)?;
            tracing::info!(vault = %root.display(), "corpus resolved");

            let config = Config::load(&root)?;
            chunk::dump::run(
                &root,
                &config,
                chunk::dump::Options {
                    json,
                    text,
                    limit,
                    paths,
                },
            )
        }

        // The only command that writes the cache. Discovery matches `scan`
        // exactly, so the index cannot disagree with the report (§3.4).
        Command::Index {
            rebuild,
            no_embeddings,
            dry_run,
            json,
            paths,
        } => {
            let root = vault::resolve_corpus(cli.vault)?;
            tracing::info!(vault = %root.display(), "corpus resolved");

            let config = Config::load(&root)?;
            // `--no-embeddings` is a statement about the generation, so it skips
            // resolution entirely: no model is loaded for a lexical build.
            let embedder = if no_embeddings {
                None
            } else {
                resolve_embedder(&config)
            };
            index::cmd::run(
                &root,
                &config,
                &SystemClock,
                embedder.as_deref(),
                &index::cmd::Options {
                    rebuild,
                    dry_run,
                    json,
                    paths,
                },
            )
        }

        // Reads load or rebuild a validated generation and print at most one
        // stale diagnostic; the ranking decisions all live in `search` (§9.5).
        Command::Search {
            query,
            intent,
            scope,
            scope_only,
            limit,
            types,
            tags,
            decay_half_life,
            include_superseded,
            no_rebuild,
            json,
        } => {
            let root = vault::resolve_corpus(cli.vault)?;
            tracing::info!(vault = %root.display(), "corpus resolved");

            let config = Config::load(&root)?;
            search::run(
                &root,
                &config,
                &SystemClock,
                search::Options {
                    query,
                    intent,
                    scopes: scope,
                    scope_only,
                    limit,
                    types,
                    tags,
                    decay_half_life,
                    include_superseded,
                    no_rebuild,
                    json,
                },
            )
        }

        Command::Facts {
            query,
            tags,
            scope,
            scope_only,
            limit,
            no_rebuild,
            json,
        } => {
            let root = vault::resolve_corpus(cli.vault)?;
            tracing::info!(vault = %root.display(), "corpus resolved");

            let config = Config::load(&root)?;
            search::facts::run(
                &root,
                &config,
                &SystemClock,
                &search::facts::Options {
                    query,
                    tags,
                    scopes: scope,
                    scope_only,
                    limit,
                    no_rebuild,
                    json,
                },
            )
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
