//! Process entry point: argument parsing, tracing, exit codes.
//!
//! Everything here is wiring. Deliberately no behaviour lives in this file, so
//! the same operations can be exposed over MCP and JSON-RPC without duplicating
//! decisions (§9.5).

use std::io::IsTerminal as _;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use janus_mem::cli::{Cli, Command, Progress};
use janus_mem::clock::SystemClock;
use janus_mem::config::{self, Config};
use janus_mem::error::JanusError;
use janus_mem::index::embed::{self, Embedder, ModelDirs, Resolved};
use janus_mem::{chunk, doctor, ids, index, lineage, moves, scan, search, suggest, vault};
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
/// Resolution itself is silent; each caller decides what to say. `janus index`
/// announces a degraded resolution (§3.6 row 1), a read leaves it to P2's
/// `degraded` field, and `--no-embeddings` is an explicit choice that earns no
/// line at all.
fn resolve_embedder(config: &Config) -> Resolved {
    embed::resolve(config, &model_dirs(config))
}

/// Resolves the embedder for a read, keeping the reason for `degraded`.
///
/// A read stays silent about a degraded resolution: the response's `degraded`
/// field is where that reason belongs, and printing it twice would make the
/// response and stderr disagree about the same fact. `--no-embeddings` resolves
/// nothing at all, so a machine without the model can still run it (V-14).
fn read_embedder(
    config: &Config,
    no_embeddings: bool,
) -> (Option<Box<dyn Embedder>>, Option<String>) {
    if no_embeddings {
        return (None, None);
    }
    match resolve_embedder(config) {
        Resolved::Ready(embedder) => (Some(embedder), None),
        Resolved::Degraded(reason, detail) => (None, Some(degraded_reason(reason, &detail))),
    }
}

/// The §3.6 row-1 wording for a reason `resolve` returned.
fn degraded_reason(reason: &str, detail: &str) -> String {
    match reason {
        "disabled" => "embeddings disabled in janus.toml; lexical only".to_owned(),
        "not compiled" => "embeddings not compiled; lexical only".to_owned(),
        other => format!("{other}: {detail}; lexical only"),
    }
}

/// Resolves `--progress` once, at the edge (M4D §3.6).
///
/// `auto` asks the terminal; every other value is a plain answer. This is the
/// only place that reads the terminal, so a captured stderr is silent on any
/// machine and no library path depends on whether a test runs in a TTY.
fn progress_enabled(mode: Progress) -> bool {
    match mode {
        Progress::Always => true,
        Progress::Never => false,
        Progress::Auto => std::io::stderr().is_terminal(),
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

fn run(cli: Cli) -> Result<ExitCode, JanusError> {
    let result: Result<(), JanusError> = match cli.command {
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
            scan::run(
                &root,
                &config,
                &SystemClock,
                scan::Options { json, verbose },
            )
        }

        // A diagnostic reads the walk and the published generation. It takes
        // no lock, builds nothing and writes nothing (M5 §3.10, M5-12).
        Command::Doctor { json } => {
            let root = vault::resolve_corpus(cli.vault)?;
            tracing::info!(vault = %root.display(), "corpus resolved");

            let config = Config::load(&root)?;
            return doctor::run(&root, &config, &SystemClock, doctor::Options { json });
        }

        // Identity is a read like `scan`: it answers from the last published
        // generation and never builds one (M5 §3.1).
        Command::Ids { paths, json } => {
            let root = vault::resolve_corpus(cli.vault)?;
            tracing::info!(vault = %root.display(), "corpus resolved");

            let config = Config::load(&root)?;
            ids::run(&root, &config, &SystemClock, ids::Options { json, paths })
        }

        // Moves are a read like `ids`: the published generation already holds
        // the path history and the hints, so nothing is built or refreshed.
        Command::Moves { since, json } => {
            let root = vault::resolve_corpus(cli.vault)?;
            tracing::info!(vault = %root.display(), "corpus resolved");

            let since = since.as_deref().map(moves::parse_since).transpose()?;
            let config = Config::load(&root)?;
            moves::run(&root, &config, &SystemClock, moves::Options { json, since })
        }

        // Suggestions are a read: the pairs are scored from the published rows,
        // so nothing is built, refreshed or written (M5 §3.12).
        Command::SuggestLinks {
            threshold,
            no_embeddings,
            json,
        } => {
            let root = vault::resolve_corpus(cli.vault)?;
            tracing::info!(vault = %root.display(), "corpus resolved");

            let threshold = threshold
                .as_deref()
                .map_or(Ok(suggest::DEFAULT_THRESHOLD), suggest::parse_threshold)?;
            let config = Config::load(&root)?;
            let (embedder, _reason) = read_embedder(&config, no_embeddings);
            suggest::run(
                &root,
                &config,
                &SystemClock,
                embedder.as_deref(),
                suggest::Options { threshold, json },
            )
        }

        // The chain is a read like `search`: it refreshes, never builds under
        // `--no-rebuild`, and never writes a vault file (M5 §3.9).
        Command::Lineage {
            path,
            topic,
            no_rebuild,
            no_embeddings,
            progress,
            json,
        } => {
            let root = vault::resolve_corpus(cli.vault)?;
            tracing::info!(vault = %root.display(), "corpus resolved");

            let config = Config::load(&root)?;
            let progress = progress_enabled(progress);
            // A read passes the embedder so a rebuild it triggers still
            // produces the generation this process would build (V-21 row 5).
            let (embedder, _reason) = read_embedder(&config, no_embeddings);
            lineage::run(
                &root,
                &config,
                &SystemClock,
                embedder.as_deref(),
                lineage::Options {
                    path,
                    topic,
                    json,
                    no_rebuild,
                    no_embeddings,
                    progress,
                },
            )
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
                &SystemClock,
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
            progress,
            paths,
        } => {
            let root = vault::resolve_corpus(cli.vault)?;
            tracing::info!(vault = %root.display(), "corpus resolved");

            let config = Config::load(&root)?;
            let progress = progress_enabled(progress);
            // `--no-embeddings` is a statement about the generation, so it skips
            // resolution entirely: no model is loaded for a lexical build, and
            // the flag is an explicit choice, so it prints nothing.
            let embedder = if no_embeddings {
                None
            } else {
                match resolve_embedder(&config) {
                    Resolved::Ready(embedder) => Some(embedder),
                    Resolved::Degraded(reason, detail) => {
                        tracing::warn!("embeddings unavailable ({reason}): {detail}; lexical only");
                        None
                    }
                }
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
                    progress,
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
            as_of,
            no_rebuild,
            no_embeddings,
            progress,
            json,
        } => {
            let root = vault::resolve_corpus(cli.vault)?;
            tracing::info!(vault = %root.display(), "corpus resolved");

            // Parsed and expanded at the edge, so the answer already holds the
            // first day of a short form and the echo cannot disagree (§3.8).
            let as_of = as_of.as_deref().map(search::parse_as_of).transpose()?;
            let config = Config::load(&root)?;
            let progress = progress_enabled(progress);
            // A read passes the embedder so a rebuild it triggers still produces
            // the generation this process would build (V-21 row 5); whether it
            // asks for the rows is `search`'s decision from α.
            let (embedder, embedding_reason) = read_embedder(&config, no_embeddings);
            search::run(
                &root,
                &config,
                &SystemClock,
                embedder.as_deref(),
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
                    as_of,
                    no_rebuild,
                    no_embeddings,
                    embedding_reason,
                    progress,
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
            no_embeddings,
            progress,
            json,
        } => {
            let root = vault::resolve_corpus(cli.vault)?;
            tracing::info!(vault = %root.display(), "corpus resolved");

            let config = Config::load(&root)?;
            let progress = progress_enabled(progress);
            let (embedder, embedding_reason) = read_embedder(&config, no_embeddings);
            search::facts::run(
                &root,
                &config,
                &SystemClock,
                embedder.as_deref(),
                &search::facts::Options {
                    query,
                    tags,
                    scopes: scope,
                    scope_only,
                    limit,
                    no_rebuild,
                    no_embeddings,
                    embedding_reason,
                    progress,
                    json,
                },
            )
        }
    };
    result.map(|()| ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    init_tracing();

    let cli = Cli::parse();

    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(err.exit_code())
        }
    }
}
