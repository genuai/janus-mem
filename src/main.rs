//! Process entry point: argument parsing, tracing, exit codes.
//!
//! Everything here is wiring. Deliberately no behaviour lives in this file, so
//! the same operations can be exposed over MCP and JSON-RPC without duplicating
//! decisions (§9.5).

use std::io::{IsTerminal as _, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use janus_mem::api::{Call, Failure, Tool};
use janus_mem::cli::{Cli, Command, Progress};
use janus_mem::clock::SystemClock;
use janus_mem::config::{self, Config};
use janus_mem::engine::{Engine, Fleet};
use janus_mem::error::JanusError;
use janus_mem::index::embed::{self, Embedder, ModelDirs, Resolved};
use janus_mem::search::federate;
use janus_mem::{
    chunk, doctor, ids, index, lineage, mcp, moves, read, rpc, scan, search, status, suggest,
    vault, workspace,
};
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

/// The platform config directory for janus (§6.11): `$XDG_CONFIG_HOME/janus`,
/// `%APPDATA%\janus`, else `~/.config/janus`. Read here, once.
fn workspace_config_dir() -> PathBuf {
    let env = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    let base = env("XDG_CONFIG_HOME")
        .or_else(|| env("APPDATA"))
        .map_or_else(
            || {
                env("HOME")
                    .map_or_else(std::env::temp_dir, PathBuf::from)
                    .join(".config")
            },
            PathBuf::from,
        );
    base.join("janus")
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
    // One flag for every command, so a command that reads one corpus must refuse
    // it here, before any work, rather than parse it and ignore it.
    let workspace = cli.workspace;
    refuse_workspace(&cli.command, workspace.as_deref())?;

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

        // A read answers through the fleet, so the CLI tests the function the
        // rpc loop and the MCP server call (§3.3).
        Command::Read { path, corpus, json } => {
            let fleet = resolve_fleet(workspace, cli.vault, false)?;
            let mut params = serde_json::json!({ "path": path.to_string_lossy() });
            if let Some(corpus) = corpus {
                params["corpus"] = serde_json::Value::String(corpus);
            }
            call_tool(&fleet, Tool::ReadDocument, params, json, read::render)
        }

        // A status call answers from the manifest and one walk. It builds
        // nothing and takes no lock, so it works while a writer holds the index.
        // In a workspace it also lists every corpus the workspace names.
        Command::Status { json } => {
            let fleet = resolve_fleet(workspace, cli.vault, false)?;
            call_tool(
                &fleet,
                Tool::Status,
                serde_json::json!({}),
                json,
                status::render,
            )
        }

        // The first command that reads a workspace. Resolution happens here,
        // at the edge, and arrives below as a value (§5.1 of SPEC, §6.11).
        Command::Corpora { json } => {
            let path = workspace::resolve(
                workspace,
                std::env::var_os("JANUS_WORKSPACE").map(PathBuf::from),
                &workspace_config_dir(),
            )?;
            tracing::info!(workspace = %path.display(), "workspace resolved");

            let resolved = workspace::Workspace::load(&path)?;
            workspace::run(&resolved, workspace::Options { json })
        }

        // The rpc loop answers tool calls over stdin and stdout (§3.5). A
        // startup failure exits before the loop starts; every failure after
        // that is a response line, so a caller never loses its session.
        Command::Rpc { allow_write } => {
            let fleet = resolve_fleet(workspace, cli.vault, false)?;
            rpc::run(
                &fleet,
                allow_write,
                &mut std::io::stdin().lock(),
                &mut std::io::stdout().lock(),
            );
            Ok(())
        }

        // The MCP server is the same corpora behind a second protocol. A startup
        // failure exits before the loop starts; every failure after that is an
        // answer, so a client never loses its session (§3.6).
        Command::Mcp { allow_write } => {
            let fleet = resolve_fleet(workspace, cli.vault, false)?;
            // This release serves no write, so the flag cannot grant one. It is
            // accepted anyway, so a client config does not change at M7, and it
            // says once on stderr what it did (§3.8 case 8.3). Stdout stays
            // protocol only (§3.10).
            if allow_write {
                eprintln!("writes are not implemented yet; --allow-write has no effect");
            }
            mcp::run(
                &fleet,
                allow_write,
                &mut std::io::stdin().lock(),
                &mut std::io::stdout().lock(),
            );
            Ok(())
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

            let mut config = Config::load(&root)?;
            // A corpus that is a parent in the ambient workspace must not
            // publish the nested corpus's documents again: the rebuild reads the
            // same prune a query reads (case 5.4, M6b-26).
            if let Some(found) = ambient_workspace()? {
                for warning in &found.warnings {
                    eprintln!("warning: {warning}");
                }
                if let Some(corpus) = found.corpora.iter().find(|corpus| corpus.root == root) {
                    found.apply_prunes(&corpus.id, &mut config);
                }
            }
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
            corpus,
            no_rebuild,
            no_embeddings,
            progress,
            json,
        } => {
            // Parsed and expanded at the edge, so the answer already holds the
            // first day of a short form and the echo cannot disagree (§3.8).
            let as_of = as_of.as_deref().map(search::parse_as_of).transpose()?;
            let progress = progress_enabled(progress);
            let options = |embedding_reason: Option<String>| search::Options {
                query: query.clone(),
                intent: intent.clone(),
                scopes: scope.clone(),
                scope_only,
                limit,
                types: types.clone(),
                tags: tags.clone(),
                decay_half_life,
                include_superseded,
                as_of,
                no_rebuild,
                no_embeddings,
                embedding_reason,
                progress,
                json,
            };
            let choice = federate::Choice::parse(corpus.as_deref());
            match query_workspace(workspace, cli.vault.is_some())? {
                Some(found) => {
                    let fleet = workspace_fleet(&found, no_embeddings)?;
                    let (targets, notes) = fleet.scope(&choice)?;
                    federate::run_search(&targets, &SystemClock, options(None), notes)
                }
                None => {
                    choice.require_single()?;
                    let root = vault::resolve_corpus(cli.vault)?;
                    tracing::info!(vault = %root.display(), "corpus resolved");

                    let config = Config::load(&root)?;
                    // A read passes the embedder so a rebuild it triggers still
                    // produces the generation this process would build (V-21
                    // row 5); whether it asks for the rows is `search`'s
                    // decision from α.
                    let (embedder, embedding_reason) = read_embedder(&config, no_embeddings);
                    search::run(
                        &root,
                        &config,
                        &SystemClock,
                        embedder.as_deref(),
                        options(embedding_reason),
                    )
                }
            }
        }

        Command::Facts {
            query,
            tags,
            types,
            scope,
            scope_only,
            limit,
            corpus,
            no_rebuild,
            no_embeddings,
            progress,
            json,
        } => {
            let progress = progress_enabled(progress);
            let options = |embedding_reason: Option<String>| search::facts::Options {
                query: query.clone(),
                tags: tags.clone(),
                types: types.clone(),
                scopes: scope.clone(),
                scope_only,
                limit,
                no_rebuild,
                no_embeddings,
                embedding_reason,
                progress,
                json,
            };
            let choice = federate::Choice::parse(corpus.as_deref());
            match query_workspace(workspace, cli.vault.is_some())? {
                Some(found) => {
                    let fleet = workspace_fleet(&found, no_embeddings)?;
                    let (targets, notes) = fleet.scope(&choice)?;
                    federate::run_facts(&targets, &SystemClock, &options(None), notes)
                }
                None => {
                    choice.require_single()?;
                    let root = vault::resolve_corpus(cli.vault)?;
                    tracing::info!(vault = %root.display(), "corpus resolved");

                    let config = Config::load(&root)?;
                    let (embedder, embedding_reason) = read_embedder(&config, no_embeddings);
                    search::facts::run(
                        &root,
                        &config,
                        &SystemClock,
                        embedder.as_deref(),
                        &options(embedding_reason),
                    )
                }
            }
        }
    };
    result.map(|()| ExitCode::SUCCESS)
}

/// Answers one tool call as the CLI does: the envelope as JSON, or the human
/// form. A failure comes back as an error, so `main` maps it to an exit code.
///
/// The fleet is built here, at the edge, with the clock and the embedders this
/// process resolved, so a rebuild a call triggers publishes this process's
/// generation (V-21 row 5).
fn call_tool(
    fleet: &Fleet,
    tool: Tool,
    params: serde_json::Value,
    json: bool,
    render: fn(&serde_json::Value, &mut dyn Write) -> std::io::Result<()>,
) -> Result<(), JanusError> {
    let answer = fleet.answer(&Call::new(tool, params))?;
    if json {
        emit_json(&answer);
        return Ok(());
    }
    // A closed pipe is not a failure (`janus read | head`).
    let _ = render(&answer, &mut std::io::stdout().lock());
    Ok(())
}

/// The corpora one command answers from (§6.11).
///
/// One resolution path for every surface: a workspace when one names the
/// command, otherwise the one corpus the command already read.
fn resolve_fleet(
    explicit: Option<PathBuf>,
    root: Option<PathBuf>,
    no_embeddings: bool,
) -> Result<Fleet, JanusError> {
    match query_workspace(explicit, root.is_some())? {
        Some(found) => workspace_fleet(&found, no_embeddings),
        None => single_fleet(root, no_embeddings),
    }
}

/// Every corpus one workspace names, with the workspace's warnings printed.
///
/// The embedder stays unresolved until a corpus is read, so a query that names
/// the primary does not load a model for an archived corpus.
fn workspace_fleet(found: &workspace::Workspace, no_embeddings: bool) -> Result<Fleet, JanusError> {
    for warning in &found.warnings {
        eprintln!("warning: {warning}");
    }
    Fleet::of(
        found,
        Arc::new(SystemClock),
        Arc::new(move |config: &Config| read_embedder(config, no_embeddings)),
    )
}

/// One corpus, as every command read it before M6b.
fn single_fleet(root: Option<PathBuf>, no_embeddings: bool) -> Result<Fleet, JanusError> {
    let root = vault::resolve_corpus(root)?;
    tracing::info!(vault = %root.display(), "corpus resolved");

    let config = Config::load(&root)?;
    let (embedder, degraded) = read_embedder(&config, no_embeddings);
    Ok(Fleet::single(Engine::new(
        root,
        config,
        Box::new(SystemClock),
        embedder,
        degraded,
    )))
}

/// The workspace a query reads, when one names it (§6.11).
///
/// `--workspace`, then `JANUS_WORKSPACE`, then the platform file. A corpus root
/// named by `--vault` or `JANUS_VAULT` is a more specific subject than the
/// platform file, so it selects single-corpus mode; naming a root and a
/// workspace together is a usage error, because one query cannot read both.
fn query_workspace(
    explicit: Option<PathBuf>,
    root_named: bool,
) -> Result<Option<workspace::Workspace>, JanusError> {
    let env = std::env::var_os("JANUS_WORKSPACE").map(PathBuf::from);
    if root_named {
        if explicit.is_some() || env.is_some() {
            return Err(JanusError::Usage {
                message: "a corpus root (--vault) and a workspace (--workspace) cannot both \
                          name this query; pass one"
                    .to_owned(),
            });
        }
        return Ok(None);
    }
    match workspace::resolve(explicit, env, &workspace_config_dir()) {
        Ok(path) => Ok(Some(workspace::Workspace::load(&path)?)),
        // A query that names no workspace reads one corpus, as it always has.
        Err(JanusError::Usage { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The workspace the environment names, when one does (§6.11).
///
/// A command that works on one corpus refuses `--workspace` (M6b-24), so only the
/// variable and the platform file can name a workspace here. The prune such a
/// workspace derives still applies: a rebuild it triggers must not publish a
/// nested corpus's documents a second time (M6b-26).
fn ambient_workspace() -> Result<Option<workspace::Workspace>, JanusError> {
    let env = std::env::var_os("JANUS_WORKSPACE").map(PathBuf::from);
    match workspace::resolve(None, env, &workspace_config_dir()) {
        Ok(path) => Ok(Some(workspace::Workspace::load(&path)?)),
        // No workspace anywhere is the documented single-corpus mode.
        Err(JanusError::Usage { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Refuses `--workspace` on a command that works on one corpus (§6.11).
///
/// The flag is global, so the parser accepts it everywhere. A command that
/// parsed it and ignored it would answer about one corpus while looking like it
/// read the workspace, and `janus index --workspace w.toml` would publish one
/// corpus and report success. The refusal is a `usage` failure, so the exit code
/// is the one a caller already knows (M6b-24).
fn refuse_workspace(command: &Command, named: Option<&Path>) -> Result<(), JanusError> {
    let Some(path) = named.filter(|_| !federates(command)) else {
        return Ok(());
    };
    Err(JanusError::Usage {
        message: format!(
            "`janus {}` works on one corpus and cannot federate, so `--workspace` does not \
             apply to it; drop the flag (given {})",
            command_name(command),
            path.display()
        ),
    })
}

/// The commands that read a workspace (§6.11).
///
/// The list names the commands that do, not the ones that do not: a command
/// added later refuses the flag until someone writes it to read a workspace, so
/// the default is the safe one.
fn federates(command: &Command) -> bool {
    matches!(
        command,
        Command::Search { .. }
            | Command::Facts { .. }
            | Command::Read { .. }
            | Command::Status { .. }
            | Command::Corpora { .. }
            | Command::Rpc { .. }
            | Command::Mcp { .. }
    )
}

/// The word a user types to name a command that works on one corpus.
///
/// A federating command never reaches a message, so it shares the fallback name
/// rather than repeating the list above.
fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Init => "init",
        Command::Scan { .. } => "scan",
        Command::Doctor { .. } => "doctor",
        Command::Ids { .. } => "ids",
        Command::Moves { .. } => "moves",
        Command::SuggestLinks { .. } => "suggest-links",
        Command::Lineage { .. } => "lineage",
        Command::Chunks { .. } => "chunks",
        Command::Index { .. } => "index",
        _ => "command",
    }
}

/// Writes one object to stdout as pretty JSON. A closed pipe is not a command
/// failure (`… --json | head`).
fn emit_json(value: &impl serde::Serialize) {
    let Ok(text) = serde_json::to_string_pretty(value) else {
        return;
    };
    let _ = writeln!(std::io::stdout().lock(), "{text}");
}

/// Whether a failure of this command carries the envelope on stdout.
///
/// Only the tool commands do (§2.7). A receipt such as `janus scan --json`
/// keeps its shape: one `error:` line on stderr and nothing on stdout, so a
/// caller can tell "no report" from "a report with no rows".
fn failure_envelope_wanted(cli: &Cli) -> bool {
    matches!(
        &cli.command,
        Command::Search { json: true, .. }
            | Command::Facts { json: true, .. }
            | Command::Lineage { json: true, .. }
            | Command::Read { json: true, .. }
            | Command::Status { json: true, .. }
    )
}

fn print_failure(err: &JanusError) {
    emit_json(&Failure::from_error(err));
}

fn main() -> ExitCode {
    init_tracing();

    let cli = Cli::parse();
    // Read before `run` takes ownership: the error type cannot know whether the
    // caller asked for JSON, and neither can a renderer that never ran.
    let json = failure_envelope_wanted(&cli);

    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            if json {
                print_failure(&err);
            }
            eprintln!("error: {err}");
            ExitCode::from(err.exit_code())
        }
    }
}
