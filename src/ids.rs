//! `janus ids`: the identity, the rung and the match keys of each document.
//!
//! Identity continuity is invisible in every other report: a `search` result
//! names a path, and a `scan` row names a tier. This command answers the question
//! a corpus owner actually asks after a move — *did the document keep its id, and
//! if so, which key proved it?* (M5 §3.1).
//!
//! A read, like `scan`: it loads the validated generation as the identity basis,
//! never builds, never refreshes and never writes. With no generation it derives
//! ids statelessly and says nothing, exactly as `search` stays silent about a
//! first build.
//!
//! A path that names a tombstoned document prints one row with `retired: true`
//! and the title it had when it left. That row is how a reader learns that a
//! document is gone rather than misspelled.

use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::clock::Clock;
use crate::config::Config;
use crate::document::Document;
use crate::error::JanusError;
use crate::index::store;
use crate::index::{PathEntry, TombstoneMeta};
use crate::vault::{self, WalkOptions, identity, resolve};

use super::scan::API_VERSION;

/// How `janus ids` should be rendered.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Emit one JSON object on stdout instead of the table.
    pub json: bool,
    /// Paths to report; empty reports every live document.
    pub paths: Vec<PathBuf>,
}

/// Runs `janus ids`.
///
/// Results go to stdout, diagnostics to stderr, so `janus ids --json | jq`
/// always parses (§8.2).
///
/// # Errors
///
/// Propagates corpus, config, walk and path failures. A path that names neither a
/// live nor a retired document is a named error, never a silent empty report.
pub fn run(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    options: Options,
) -> Result<(), JanusError> {
    let walk = vault::walk(root, config, WalkOptions::default())?;
    let (mut documents, _skips) = resolve::resolve_all(&walk.files, config);
    let basis = store::validated_snapshot(root, config, clock.today());
    // The same resolution the last build would have published, so the ids agree
    // with `search` and with `scan` (cases 1.10 and 1.11).
    let tables = identity::resolve(&mut documents, basis.as_ref(), config, clock.today());

    for warning in &config.warnings {
        eprintln!("warning: {warning}");
    }

    let rows = report(&documents, &tables, config, &options.paths)?;
    let mut out = io::stdout().lock();
    if options.json {
        let report = Report {
            api: API_VERSION,
            vault: root.display().to_string(),
            rows,
        };
        let json = serde_json::to_string_pretty(&report)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        // A closed pipe is not a failure (`janus ids --json | head`).
        let _ = writeln!(out, "{json}");
    } else {
        write_human(&mut out, &rows);
    }
    Ok(())
}

// ───────────────────────────────── rows ────────────────────────────────────

/// The `--json` report: one stable object, like every other command's.
#[derive(Debug, Serialize)]
struct Report {
    /// Contract version; see [`API_VERSION`].
    api: u32,
    /// Corpus root, absolute.
    vault: String,
    /// One row per document, sorted by path.
    rows: Vec<Row>,
}

/// One document's identity. A live row and a retired row share one shape, so a
/// consumer never has to guess which fields exist.
#[derive(Debug, Serialize)]
struct Row {
    /// Vault-relative path, `/`-separated.
    path: String,
    /// The id, in hex.
    id: String,
    /// The rung that produced or matched the id; `null` for a retired document.
    tier: Option<&'static str>,
    /// Every match key, in ladder order, rendered for inspection.
    keys: Vec<String>,
    /// True when this build took the id from the previous generation.
    inherited: bool,
    /// The path the id held before this build, or `null`.
    previous_path: Option<String>,
    /// True when the document is no longer in the corpus.
    retired: bool,
    /// The title a retired document had when it left.
    title: Option<String>,
}

/// Builds one row per selected document, sorted by path.
fn report(
    documents: &[Document],
    tables: &identity::Resolution,
    config: &Config,
    paths: &[PathBuf],
) -> Result<Vec<Row>, JanusError> {
    if paths.is_empty() {
        let mut rows: Vec<Row> = documents
            .iter()
            .map(|document| live_row(document, tables, config))
            .collect();
        rows.sort_by(|left, right| left.path.cmp(&right.path));
        return Ok(rows);
    }

    let mut rows = Vec::new();
    for given in paths {
        let rel = rel_path(given)?;
        if let Some(document) = documents.iter().find(|document| document.rel_path == rel) {
            rows.push(live_row(document, tables, config));
            continue;
        }
        let Some(meta) = retired(tables, &rel) else {
            return Err(JanusError::DocumentNotInCorpus {
                path: given.clone(),
            });
        };
        rows.push(Row {
            path: rel,
            id: meta.id.to_hex(),
            tier: None,
            keys: Vec::new(),
            inherited: false,
            previous_path: None,
            retired: true,
            title: Some(meta.title.clone()),
        });
    }
    Ok(rows)
}

/// One live document's row.
fn live_row(document: &Document, tables: &identity::Resolution, config: &Config) -> Row {
    Row {
        path: document.rel_path.clone(),
        id: document.id.to_hex(),
        tier: Some(document.id_source.as_str()),
        keys: identity::key_report(document, config),
        inherited: tables.inherited.contains(&document.id),
        previous_path: previous_path(tables, document.id, &document.rel_path),
        retired: false,
        title: None,
    }
}

/// The path an id held before this build, or `None` when it held no other.
///
/// The last entry is the path this build recorded, so the one before it is the
/// path the id left.
fn previous_path(tables: &identity::Resolution, id: crate::id::DocId, now: &str) -> Option<String> {
    let entries: &Vec<PathEntry> = tables.path_history.get(&id)?;
    let last = entries.len().checked_sub(2)?;
    entries
        .get(last)
        .map(|entry| entry.rel_path.clone())
        .filter(|path| path != now)
}

/// The tombstone whose last path is `rel`, if any.
fn retired<'a>(tables: &'a identity::Resolution, rel: &str) -> Option<&'a TombstoneMeta> {
    tables
        .tombstones
        .iter()
        .find(|meta| last_path(tables, meta.id).is_some_and(|path| path == rel))
}

/// The last path an id occupied.
fn last_path(tables: &identity::Resolution, id: crate::id::DocId) -> Option<&str> {
    tables
        .path_history
        .get(&id)?
        .last()
        .map(|entry| entry.rel_path.as_str())
}

/// The corpus-relative form of a `PATH` argument, read without touching a file.
///
/// A retired path no longer exists, so it cannot be canonicalized. The argument
/// is normalised lexically and refused when it escapes the corpus (§9.3).
fn rel_path(given: &Path) -> Result<String, JanusError> {
    let mut parts: Vec<&str> = Vec::new();
    for component in given.components() {
        match component {
            Component::Normal(name) => {
                let Some(name) = name.to_str() else {
                    return Err(JanusError::DocumentNotInCorpus {
                        path: given.to_path_buf(),
                    });
                };
                parts.push(name);
            }
            Component::CurDir => {}
            _ => {
                return Err(JanusError::PathEscape {
                    path: given.to_path_buf(),
                });
            }
        }
    }
    Ok(parts.join("/"))
}

// ───────────────────────────────── human ───────────────────────────────────

/// Prints the aligned table. A retired row says so instead of printing a tier.
fn write_human(out: &mut impl io::Write, rows: &[Row]) {
    for row in rows {
        if row.retired {
            let title = row.title.as_deref().unwrap_or_default();
            let _ = writeln!(out, "{}  {}  retired: {title}", row.path, row.id);
            continue;
        }
        let tier = row.tier.unwrap_or_default();
        let _ = writeln!(out, "{}  {}  {tier}", row.path, row.id);
    }
}
