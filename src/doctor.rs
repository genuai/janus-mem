//! `janus doctor`: aggregate every visible problem into one report (M5 §3.10).
//!
//! This half reads the walk. A document note is a statement about the file, so
//! `unknown_type`, `invalid_scope` and their siblings come from the parsed
//! document. `duplicate_identity` is promoted from a note to an error, and
//! `one_sided_supersession` compares the two declarations in the walk.
//!
//! A diagnostic never mutates what it diagnoses. `doctor` takes no lock, builds
//! nothing, refreshes nothing and writes nothing (M5-12). The graph half and the
//! index freshness warnings arrive with the next change.
//!
//! The exit code is 1 when the report holds at least one error, and 0 otherwise.
//! A warning never changes it (M5-13).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write as _};
use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;

use crate::clock::Clock;
use crate::config::Config;
use crate::document::{DocNote, Document};
use crate::error::JanusError;
use crate::scan::API_VERSION;
use crate::vault::{self, WalkOptions, identity, resolve};

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
/// # Errors
///
/// Propagates corpus, config and walk failures. A missing or stale generation is
/// reported as a warning, not an error: the scan half still has an answer.
pub fn run(
    root: &Path,
    config: &Config,
    _clock: &dyn Clock,
    options: Options,
) -> Result<ExitCode, JanusError> {
    let walk = vault::walk(root, config, WalkOptions::default())?;
    let (mut documents, _skips) = resolve::resolve_all(&walk.files, config);
    // The scan half is one walk. Identity is derived without a basis so a
    // duplicate key stays visible after a build: carry-forward would adopt both
    // paths and suppress the note the duplicate produced (M5-44).
    identity::derive_identity(&mut documents, None, config);

    let mut checks = scan_checks(&documents, config);

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
            Check::new("duplicate_identity", ERROR, paths[0], key, message)
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
