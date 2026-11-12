//! `janus doctor`: aggregate every visible problem into one report (M5 §3.10).
//!
//! The report has two halves, and they read two different generations:
//!
//! * **Scan checks** read the walk. A document note is a statement about the
//!   file, so `unknown_type`, `invalid_scope` and their siblings come from the
//!   parsed document. `duplicate_identity` is a warning: an ambiguous key falls
//!   through to `rel_path`, so it costs move continuity and never correctness
//!   (M5-57), and `one_sided_supersession` compares the two declarations in the
//!   walk.
//! * **Graph checks** read the published generation. A link lives in the
//!   snapshot, so cycles, dangling rows, inert edges and windows are resolved
//!   from `Snapshot::edges`, `Snapshot::dangling_links` and the snapshot's own
//!   metadata. The walk is never mixed into this half: a new file would change
//!   the id set, and every edge would then be looked up in the wrong one.
//!
//! A diagnostic never mutates what it diagnoses. `doctor` takes no lock, builds
//! nothing, refreshes nothing and writes nothing (M5-12). A missing generation
//! is one `index_absent` warning, and a stale one is one `index_stale` warning
//! followed by the graph checks of the last build.
//!
//! The exit code is 1 when the report holds at least one error, and 0 otherwise.
//! A warning never changes it (M5-13).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write as _};
use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;

use crate::clock::Clock;
use crate::config::{self, Config};
use crate::document::{DocNote, Document};
use crate::error::JanusError;
use crate::graph::{EffectiveStatus, Graph, Window};
use crate::id::DocId;
use crate::index::refresh::Witness;
use crate::index::store::{self, LoadOptions, Policy};
use crate::index::{DanglingReason, Snapshot};
use crate::link::{Corpora, Corpus};
use crate::scan::API_VERSION;
use crate::score::ScopeFilter;
use crate::vault::{self, WalkOptions, identity, resolve};
use crate::workspace::Workspace;

/// The severity of a report check. A warning never changes the exit code.
const ERROR: &str = "error";
/// A tolerated irregularity. It is reported, and it never fails the command.
const WARNING: &str = "warning";

/// How `janus doctor` should be rendered.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Emit one JSON object on stdout instead of one line per check.
    pub json: bool,
}

/// Runs `janus doctor`.
///
/// `workspace` is the workspace whose file names this corpus, when one does. A
/// qualified link resolves only through it: the policy and the corpora a link
/// can name live in that file (M7-37). Without one, every qualified link is
/// reported, as it was before `promote` (case 7.4).
///
/// # Errors
///
/// Propagates corpus, config and walk failures. A missing or stale generation is
/// reported as a warning, not an error: the scan half still has an answer.
pub fn run(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    options: Options,
    workspace: Option<&Workspace>,
) -> Result<ExitCode, JanusError> {
    let walk = vault::walk(root, config, WalkOptions::default())?;
    let (mut documents, _skips) = resolve::resolve_all(&walk.files, config);
    // The scan half is one walk. Identity is derived without a basis so a
    // duplicate key stays visible after a build: carry-forward would adopt both
    // paths and suppress the note the duplicate produced (M5-44).
    identity::derive_identity(&mut documents, None, config);

    let mut checks = scan_checks(&documents, config);
    // One load per corpus this workspace puts in reach, and none when no
    // workspace names this corpus.
    let reach = reach(workspace, root, clock);
    let selected: Vec<Corpus<'_>> = reach
        .iter()
        .map(|(id, own, snapshot)| Corpus {
            id,
            snapshot,
            config: own,
        })
        .collect();
    let known: Vec<&str> = workspace
        .map(|found| {
            found
                .corpora
                .iter()
                .map(|corpus| corpus.id.as_str())
                .collect()
        })
        .unwrap_or_default();
    let corpora = Corpora {
        allowed: workspace.is_some_and(|found| found.search.allow_cross_corpus_links),
        selected,
        known,
    };
    match generation(root, config, clock, &walk.files) {
        Some(found) => {
            checks.extend(graph_checks(&found.snapshot, config, &corpora));
            if found.stale {
                checks.push(Check::new(
                    "index_stale",
                    WARNING,
                    index_label(config, Some(store::MANIFEST_FILE)),
                    "",
                    "the corpus changed after this generation was built; graph checks describe the last build",
                ));
            }
        }
        None => checks.push(Check::new(
            "index_absent",
            WARNING,
            index_label(config, None),
            "",
            "no validated generation; run `janus index` to enable the graph checks",
        )),
    }

    checks.sort_by(|left, right| {
        (left.code, left.path.as_str(), left.target.as_str()).cmp(&(
            right.code,
            right.path.as_str(),
            right.target.as_str(),
        ))
    });
    checks.dedup_by(|left, right| {
        left.code == right.code
            && left.path == right.path
            && left.target == right.target
            && left.message == right.message
    });

    let errors = checks
        .iter()
        .filter(|check| check.severity == ERROR)
        .count();
    let warnings = checks.len() - errors;
    let report = Report {
        api: API_VERSION,
        ok: errors == 0,
        summary: Summary { errors, warnings },
        checks,
    };

    let mut out = io::stdout().lock();
    if options.json {
        let json = serde_json::to_string_pretty(&report)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        // A closed pipe is not a doctor failure (`janus doctor --json | head`).
        let _ = writeln!(out, "{json}");
    } else {
        write_human(&mut out, &report);
    }

    Ok(if report.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

// ───────────────────────────────── the report ──────────────────────────────

/// The `--json` report: one stable object, like every other command's.
#[derive(Debug, Serialize)]
struct Report {
    /// Contract version; see [`API_VERSION`].
    api: u32,
    /// True when the report holds no error.
    ok: bool,
    /// The count of each severity.
    summary: Summary,
    /// Every check, sorted by `(code, path, target)`.
    checks: Vec<Check>,
}

/// The two severity counts, so a consumer need not recount the list.
#[derive(Debug, Serialize)]
struct Summary {
    /// Checks that make `ok` false and the exit code 1.
    errors: usize,
    /// Checks that are reported and change nothing.
    warnings: usize,
}

/// One check. `target` is the second sort key and is never emitted.
#[derive(Debug, Serialize)]
struct Check {
    /// The stable machine-readable code (§3.10). Public API.
    code: &'static str,
    /// `error` or `warning`.
    severity: &'static str,
    /// The vault-relative path the check describes.
    path: String,
    /// The sort key after `path`; a link target, an id or a key.
    #[serde(skip)]
    target: String,
    /// One human sentence naming the offending value.
    message: String,
}

impl Check {
    fn new(
        code: &'static str,
        severity: &'static str,
        path: impl Into<String>,
        target: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity,
            path: path.into(),
            target: target.into(),
            message: message.into(),
        }
    }
}

// ─────────────────────────────── scan half ─────────────────────────────────

/// Every check that reads the walk.
fn scan_checks(documents: &[Document], config: &Config) -> Vec<Check> {
    let mut checks = Vec::new();
    for document in documents {
        for note in &document.notes {
            // Duplicates are aggregated below, one row per shared key.
            if matches!(note, DocNote::DuplicateIdentity { .. }) {
                continue;
            }
            let Some(severity) = note_severity(note.code()) else {
                continue;
            };
            checks.push(Check::new(
                note.code(),
                severity,
                document.rel_path.clone(),
                "",
                note.message(),
            ));
        }
    }
    checks.extend(duplicate_checks(documents));
    checks.extend(one_sided_checks(documents, config));
    checks
}

/// The document notes that §3.10 promotes, and the severity each carries.
///
/// The list is closed: a note that §3.10 does not name stays a note. Adding one
/// here is additive, because a code is public API (§9.5).
fn note_severity(code: &str) -> Option<&'static str> {
    match code {
        "unknown_type" | "invalid_scope" | "yaml_unparseable" | "no_closing_fence"
        | "oversized_block" | "symlink_escape" => Some(WARNING),
        _ => None,
    }
}

/// One error per identity key that more than one document carries.
///
/// The note is emitted per document, but the defect is the shared key. One row
/// names every path, so a reader sees both sides without counting duplicates.
fn duplicate_checks(documents: &[Document]) -> Vec<Check> {
    let mut keys: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for document in documents {
        for note in &document.notes {
            if let DocNote::DuplicateIdentity { key } = note {
                keys.entry(key.as_str())
                    .or_default()
                    .insert(document.rel_path.as_str());
            }
        }
    }
    keys.into_iter()
        .map(|(key, paths)| {
            let paths: Vec<&str> = paths.into_iter().collect();
            let message = if paths.len() == 1 {
                format!("identity key `{key}` is ambiguous in this scan")
            } else {
                format!("identity key `{key}` is shared by {}", paths.join(", "))
            };
            Check::new("duplicate_identity", WARNING, paths[0], key, message)
        })
        .collect()
}

/// One warning per `superseded_by` declaration that the target does not answer.
///
/// A `supersedes` declaration is complete on its own: the successor states that
/// it replaces the predecessor. A `superseded_by` declaration is the replaced
/// side speaking for the successor, so it is one-sided unless the target also
/// declares `supersedes` back (M5-42). That keeps the five-link chain silent and
/// reports `bad/one-sided.md`, whose target declares nothing.
fn one_sided_checks(documents: &[Document], config: &Config) -> Vec<Check> {
    let paths: BTreeMap<&str, &Document> = documents
        .iter()
        .map(|document| (document.rel_path.as_str(), document))
        .collect();
    let ids: BTreeMap<&str, &Document> = documents
        .iter()
        .filter_map(|document| document.frontmatter.id.as_deref().map(|id| (id, document)))
        .collect();

    let mut checks = Vec::new();
    for document in documents {
        for target in &document.frontmatter.superseded_by {
            let Some(target_document) = lookup(target, &paths, &ids, &config.corpus_id) else {
                continue;
            };
            let answered = target_document
                .frontmatter
                .supersedes
                .iter()
                .filter_map(|back| lookup(back, &paths, &ids, &config.corpus_id))
                .any(|back| back.rel_path == document.rel_path);
            if answered {
                continue;
            }
            checks.push(Check::new(
                "one_sided_supersession",
                WARNING,
                document.rel_path.clone(),
                target.trim(),
                format!(
                    "`superseded_by: {}` has no matching `supersedes: {}` on its target",
                    target.trim(),
                    document.rel_path
                ),
            ));
        }
    }
    checks
}

/// Resolves a link text against the walk: current path, then declared id.
fn lookup<'a>(
    text: &str,
    paths: &BTreeMap<&'a str, &'a Document>,
    ids: &BTreeMap<&'a str, &'a Document>,
    corpus_id: &str,
) -> Option<&'a Document> {
    let text = local_link(text, corpus_id)?;
    paths.get(text).copied().or_else(|| ids.get(text).copied())
}

/// The local part of a link text, or `None` when it names another corpus.
fn local_link<'a>(text: &'a str, corpus_id: &str) -> Option<&'a str> {
    let text = text.trim();
    if let Some(rest) = text
        .strip_prefix(corpus_id)
        .and_then(|rest| rest.strip_prefix(':'))
    {
        return Some(rest);
    }
    if let Some((head, _)) = text.split_once(':')
        && head.len() > 1
        && !head.contains('/')
        && !head.contains('\\')
        && !head.chars().any(char::is_whitespace)
    {
        // A qualifier that is not this corpus: the graph half owns the report.
        return None;
    }
    Some(text)
}

// ─────────────────────────────── graph half ───────────────────────────────

/// The published generation, and whether the corpus has moved on from it.
struct Generation {
    snapshot: Snapshot,
    stale: bool,
}

/// Reads the validated generation without a lock, a build or a refresh.
fn generation(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    files: &[vault::DiscoveredFile],
) -> Option<Generation> {
    let load = store::load(&LoadOptions {
        policy: Policy::Refuse,
        ..LoadOptions::new(root, config, clock.today())
    })
    .ok()?;
    let dir = config::resolve_index_dir(root, &config.index_dir);
    // The mtime is only the pre-M4D fallback input. A manifest with a digest
    // compares digests, and the file is not opened for its timestamp.
    let index_mtime = fs::metadata(dir.join(store::INDEX_FILE))
        .and_then(|metadata| metadata.modified())
        .ok();
    // `None`: this report judges the corpus. A config edit is not a graph fact,
    // so `doctor` keeps the freshness fields it had before M7-1.
    let stale = Witness::of(files, &load.manifest, index_mtime, None).stale;
    Some(Generation {
        snapshot: load.snapshot,
        stale,
    })
}

/// The corpora of the workspace a qualified link may reach, with the generation
/// each one holds (M7-37).
///
/// The corpus this report describes is left out: a link that names it resolves
/// locally and is no cross-corpus link at all. A corpus whose config or index
/// cannot be read is left out too, and stays in the workspace's id list, so its
/// links are reported rather than guessed.
fn reach(
    workspace: Option<&Workspace>,
    own: &Path,
    clock: &dyn Clock,
) -> Vec<(String, Config, Snapshot)> {
    let Some(workspace) = workspace else {
        return Vec::new();
    };
    workspace
        .corpora
        .iter()
        .filter(|corpus| corpus.root != own)
        .filter_map(|corpus| {
            let mut config = workspace.config_of(corpus).ok()?;
            // The workspace owns each corpus's index location (M6b-11).
            config.index_dir = corpus.index_dir.clone();
            let load = store::load(&LoadOptions {
                policy: Policy::Refuse,
                ..LoadOptions::new(&corpus.root, &config, clock.today())
            })
            .ok()?;
            Some((corpus.id.clone(), config, load.snapshot))
        })
        .collect()
}

/// Every check that reads the published generation.
fn graph_checks(snapshot: &Snapshot, config: &Config, corpora: &Corpora<'_>) -> Vec<Check> {
    let (graph, ignored) = Graph::for_snapshot_linked(snapshot, config, corpora);
    let scopes = ScopeFilter::new(&[], false);
    let mut checks = Vec::new();

    for &(from, to) in graph.ignored() {
        checks.push(Check::new(
            "supersession_cycle",
            ERROR,
            path_of(&graph, from),
            path_of(&graph, to),
            format!(
                "the cycle breaker ignored this edge to {}",
                path_of(&graph, to)
            ),
        ));
    }

    for &(from, to) in graph.inert() {
        checks.push(Check::new(
            "cross_scope_supersession",
            ERROR,
            path_of(&graph, from),
            path_of(&graph, to),
            format!(
                "the edge to {} is inert: the two scope sets do not intersect",
                path_of(&graph, to)
            ),
        ));
    }

    for (id, window) in graph.windows(&scopes) {
        if window.invalid {
            checks.push(invalid_window_check(&graph, id, window));
        }
    }

    let statuses = graph.effective_statuses(&scopes);
    for document in &snapshot.docs {
        let Some(effective) = statuses.get(&document.id) else {
            continue;
        };
        let declared = EffectiveStatus::from(document.status);
        if *effective == declared {
            continue;
        }
        checks.push(Check::new(
            "status_mismatch",
            WARNING,
            text(snapshot, document.rel_path),
            document.id.to_hex(),
            format!(
                "declared `{}`, effective `{}`",
                declared.as_str(),
                effective.as_str()
            ),
        ));
    }

    // A qualified link that resolved is a node in this graph and earns no row.
    // One that did not is reported under the code its reason earns (M7-37).
    for row in &ignored {
        checks.push(Check::new(
            row.code(),
            ERROR,
            path_of(&graph, row.declarer),
            row.target.clone(),
            row.message(&corpora.known),
        ));
    }

    for link in snapshot
        .dangling_links
        .iter()
        .filter(|link| link.reason == DanglingReason::NotFound)
    {
        checks.push(Check::new(
            "dangling_link",
            ERROR,
            path_of(&graph, link.from),
            link.target.clone(),
            format!("a link target does not resolve: {}", link.target),
        ));
    }

    checks
}

/// One `invalid_validity` row for an empty window.
fn invalid_window_check(graph: &Graph, id: DocId, window: Window) -> Check {
    let from = window
        .valid_from
        .map_or_else(|| "none".to_owned(), |date| date.to_string());
    let to = window
        .valid_to
        .map_or_else(|| "none".to_owned(), |date| date.to_string());
    Check::new(
        "invalid_validity",
        ERROR,
        path_of(graph, id),
        format!("{from}/{to}"),
        format!("the window is empty: valid_from {from} is on or after valid_to {to}"),
    )
}

/// A node's vault-relative path, or the empty path when the graph holds no row.
fn path_of(graph: &Graph, id: DocId) -> String {
    graph
        .endpoint(id)
        .map_or_else(String::new, |endpoint| endpoint.facts().rel_path.clone())
}

/// One interned string from the snapshot's pool.
fn text(snapshot: &Snapshot, id: crate::index::StringId) -> String {
    snapshot.strings.get(id).unwrap_or_default().to_owned()
}

/// The report path of the index directory, or one file inside it.
fn index_label(config: &Config, file: Option<&str>) -> String {
    let dir = config.index_dir.to_string_lossy().replace('\\', "/");
    match file {
        Some(file) => format!("{dir}/{file}"),
        None => dir,
    }
}

// ───────────────────────────────── human ───────────────────────────────────

/// Prints one line per check, then the summary.
fn write_human(out: &mut impl io::Write, report: &Report) {
    for check in &report.checks {
        let _ = writeln!(
            out,
            "{}  {}  {}  {}",
            check.severity, check.code, check.path, check.message
        );
    }
    let _ = writeln!(
        out,
        "{} error(s), {} warning(s)",
        report.summary.errors, report.summary.warnings
    );
}
