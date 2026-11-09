//! The `janus scan` command (SPEC §9.1, §9.4).
//!
//! Scan answers two questions about a corpus without writing anything: *what is
//! in it* (every document with its resolved metadata) and *what was left out and
//! why* (per-rule skip counts, plus unreadable files). The vault is never
//! mutated; the whole command is a read.
//!
//! Two output modes, one report:
//!
//! * **human** — an aligned table plus a one-line skip summary, for looking at.
//! * **`--json`** — one object on stdout, versioned with `"api": 1`, for scripts
//!   and for the M6 adapters to reuse. Never contains document *content*: no
//!   body, no raw frontmatter (§8.5, §13). A scan report is metadata only.
//!
//! Diagnostics (config warnings, tracing) go to stderr in both modes, so
//! `janus scan --json | jq` always parses (§8.2).
//!
//! # Identity is not in the report yet
//!
//! `id` and `id_source` arrive with phase 3c. Until the §6.4 ladder exists, a
//! document only carries a provisional `rel_path` identity, and emitting it
//! would report a wrong `id_source` for every document that has stronger
//! evidence. Adding two fields later is additive under the versioning policy,
//! so the report simply omits them today.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;

use crate::clock::Clock;
use crate::config::{CONFIG_FILE, Config};
use crate::document::{DocNote, Document, NoteDto};
use crate::error::JanusError;
use crate::index::store;
use crate::vault::resolve::{self, ReadSkip};
use crate::vault::{self, SkipReason, WalkOptions, WalkResult, identity};

/// The version stamped into every `--json` response.
///
/// Consumers pin this. Adding a field is additive and keeps the version;
/// renaming or removing one bumps it (AGENTS.md versioning table).
pub const API_VERSION: u32 = 1;

/// How a scan should be rendered.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Emit one JSON object on stdout instead of the human table.
    pub json: bool,
    /// Include the per-entry skip list, not just per-rule counts.
    pub verbose: bool,
}

/// Runs `janus scan`.
///
/// Results go to stdout, diagnostics to stderr, matching the rule that keeps
/// MCP's protocol stream clean later (§8.2).
///
/// # Errors
///
/// Propagates corpus, config and walk failures. A file that cannot be read is
/// *not* an error: it is reported as a skip and the scan continues.
pub fn run(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    options: Options,
) -> Result<(), JanusError> {
    let started = Instant::now();
    let walk = vault::walk(root, config, WalkOptions::default())?;
    let (mut documents, read_skips) = resolve::resolve_all(&walk.files, config);
    // Identity is a corpus-wide decision, so it runs after every file is parsed.
    // It resolves against the last published generation, or the `id` this prints
    // would disagree with `search` after an in-place edit (case 1.10). The read
    // never builds: no generation, no basis (M5 DoD 9).
    let basis = store::validated_snapshot(root, config, clock.today());
    identity::derive_identity(&mut documents, basis.as_ref(), config);
    let elapsed = started.elapsed();

    // Diagnostics never share a stream with results (§8.2). In human mode this
    // is the same terminal; in `--json` mode it is what keeps stdout parseable.
    for warning in &config.warnings {
        eprintln!("warning: {warning}");
    }

    let mut out = io::stdout().lock();

    if options.json {
        let report = ScanReport::new(
            root,
            config,
            &walk,
            &documents,
            &read_skips,
            elapsed,
            options.verbose,
        );
        let json = serde_json::to_string_pretty(&report).map_err(serialize_error)?;
        // A closed pipe is not a scan failure (`janus scan --json | head`).
        let _ = writeln!(out, "{json}");
    } else {
        write_human(
            &mut out,
            root,
            config,
            &walk,
            &documents,
            &read_skips,
            options.verbose,
        );
    }

    Ok(())
}

/// Wraps a serialization failure as an IO error.
///
/// Serializing this report cannot fail in practice — no floats, no non-string
/// map keys — but `unwrap` is not allowed in library code, and a distinct
/// `JanusError` variant for an impossible case would be public API for nothing.
fn serialize_error(err: serde_json::Error) -> JanusError {
    JanusError::Io(io::Error::new(io::ErrorKind::InvalidData, err))
}

// ───────────────────────────────── totals ──────────────────────────────────

/// How the walk's admissions and refusals add up, per rule.
///
/// Both outputs render this one value, so the table and the JSON cannot
/// disagree. Every rule is present, at zero when it never fired, in
/// [`SkipReason::ORDER`].
#[derive(Debug)]
struct WalkTotals {
    /// Markdown files the walk admitted. Each is a document or a read skip.
    files: usize,
    /// Paths the walk refused, one per path.
    entries: usize,
    /// Refused paths that were directories, so their contents were not visited.
    pruned_directories: usize,
    /// One row per rule, in report order.
    per_reason: Vec<ReasonTotal>,
}

/// One rule's share of the walk's refusals.
#[derive(Debug)]
struct ReasonTotal {
    reason: SkipReason,
    entries: usize,
    pruned_directories: usize,
}

impl WalkTotals {
    /// The rules that fired, as `2 config_exclude, 3 hidden`, in report order.
    fn breakdown(&self) -> String {
        joined(&self.per_reason, |row| row.entries)
    }

    /// The rules that pruned a directory, as `1 always_excluded`.
    fn pruned_breakdown(&self) -> String {
        joined(&self.per_reason, |row| row.pruned_directories)
    }
}

/// Renders one count per rule, dropping the rules whose count is zero.
fn joined(rows: &[ReasonTotal], count: impl Fn(&ReasonTotal) -> usize) -> String {
    rows.iter()
        .filter(|row| count(row) > 0)
        .map(|row| format!("{} {}", count(row), row.reason))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Counts the walk's entries once, for both outputs.
fn walk_totals(walk: &WalkResult) -> WalkTotals {
    WalkTotals {
        files: walk.files.len(),
        entries: walk.skipped_total(),
        pruned_directories: walk.pruned_total(),
        per_reason: SkipReason::ORDER
            .iter()
            .map(|reason| ReasonTotal {
                reason: *reason,
                entries: walk.count(*reason),
                pruned_directories: walk.pruned(*reason),
            })
            .collect(),
    }
}

// ───────────────────────────────── JSON ────────────────────────────────────

/// The `--json` report: one stable, versioned object.
#[derive(Debug, Serialize)]
struct ScanReport<'a> {
    /// Contract version; see [`API_VERSION`].
    api: u32,
    /// Corpus root, absolute.
    vault: String,
    /// Declared corpus identity (§6.11).
    corpus_id: &'a str,
    /// The config file that was read.
    config: String,
    /// Wall-clock duration. The only non-deterministic field besides `mtime`.
    taken_ms: u64,
    /// Every document, in `rel_path` order.
    docs: Vec<DocumentDto<'a>>,
    /// What the walk read and refused, with the unit and the pruned counts.
    walk: WalkDto,
    /// Walk skips per rule, as a map from rule code to entry count.
    ///
    /// The compatibility view: this field keeps the type, the keys and the
    /// meaning that `v0.1.0` published, so a consumer reading
    /// `report.skipped.hidden` keeps working and `api` stays 1. The pruned
    /// counts live under [`WalkDto::per_reason`].
    skipped: BTreeMap<&'static str, usize>,
    /// Markdown files hidden by `.gitignore`; `null` when not attributed.
    ignored_by_git: Option<usize>,
    /// Directories or files the walker could not read at all.
    errors: Vec<&'a str>,
    /// Files that were admitted but could not be read, hashed or decoded.
    read_skips: Vec<ReadSkipDto<'a>>,
    /// Names that look like a date but are refused (M4B §3.5), sorted and unique.
    ///
    /// The list carries the names, not a formatted sentence: a consumer can count
    /// and locate them without parsing prose. The human output prints one line
    /// with the count.
    date_near_misses: Vec<&'a str>,
    /// Per-entry walk skips; present only with `--verbose`.
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped_entries: Option<Vec<SkipDto<'a>>>,
}

impl<'a> ScanReport<'a> {
    fn new(
        root: &Path,
        config: &'a Config,
        walk: &'a WalkResult,
        documents: &'a [Document],
        read_skips: &'a [ReadSkip],
        elapsed: Duration,
        verbose: bool,
    ) -> Self {
        // One computation, so the JSON totals and the human table cannot
        // disagree.
        let totals = walk_totals(walk);

        let mut skipped = BTreeMap::new();
        for reason in SkipReason::ORDER {
            // Every rule is present even at zero: a consumer should never have to
            // distinguish "no hidden files" from "this build does not count them".
            skipped.insert(reason.as_str(), walk.count(reason));
        }

        Self {
            api: API_VERSION,
            vault: root.display().to_string(),
            corpus_id: &config.corpus_id,
            config: root.join(CONFIG_FILE).display().to_string(),
            taken_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            docs: documents.iter().map(DocumentDto::new).collect(),
            walk: WalkDto {
                files: totals.files,
                entries: totals.entries,
                directories_pruned: totals.pruned_directories,
                per_reason: totals
                    .per_reason
                    .iter()
                    .map(|row| SkipTotalDto {
                        reason: row.reason.as_str(),
                        entries: row.entries,
                        pruned_directories: row.pruned_directories,
                    })
                    .collect(),
            },
            skipped,
            ignored_by_git: walk.ignored_by_git,
            errors: walk.errors.iter().map(|e| e.message.as_str()).collect(),
            read_skips: read_skips.iter().map(ReadSkipDto::new).collect(),
            date_near_misses: date_near_misses(documents),
            skipped_entries: verbose.then(|| {
                walk.skipped
                    .iter()
                    .map(|entry| SkipDto {
                        rel_path: &entry.rel_path,
                        is_dir: entry.is_dir,
                        reason: entry.reason.as_str(),
                    })
                    .collect()
            }),
        }
    }
}

/// One document's resolved metadata. Deliberately excludes `body` and
/// `raw_frontmatter` (§8.5).
#[derive(Debug, Serialize)]
struct DocumentDto<'a> {
    id: String,
    id_source: &'static str,
    path: String,
    rel_path: &'a str,
    title: &'a str,
    #[serde(rename = "type")]
    type_name: &'a str,
    status: &'a str,
    date: Option<String>,
    date_source: &'static str,
    effective_from: Option<String>,
    effective_to: Option<String>,
    scope: &'a str,
    shared_with: &'a [String],
    tags: &'a [String],
    author: Option<&'a str>,
    doc_version: Option<&'a str>,
    schema_version: Option<u32>,
    supersedes: &'a [String],
    superseded_by: &'a [String],
    timeless: bool,
    content_hash: String,
    size: u64,
    mtime: String,
    has_frontmatter: bool,
    notes: Vec<NoteDto>,
}

impl<'a> DocumentDto<'a> {
    fn new(document: &'a Document) -> Self {
        Self {
            id: document.id.to_hex(),
            id_source: document.id_source.as_str(),
            path: document.path.to_string_lossy().into_owned(),
            rel_path: &document.rel_path,
            title: &document.title,
            type_name: &document.type_name,
            status: document.status.as_str(),
            date: document.date.map(iso_date),
            date_source: document.date_source.as_str(),
            effective_from: document.effective_from.map(iso_date),
            effective_to: document.effective_to.map(iso_date),
            scope: &document.scope,
            shared_with: &document.shared_with,
            tags: &document.frontmatter.tags,
            author: document.frontmatter.author.as_deref(),
            doc_version: document.frontmatter.doc_version.as_deref(),
            schema_version: document.frontmatter.schema_version,
            supersedes: &document.frontmatter.supersedes,
            superseded_by: &document.frontmatter.superseded_by,
            timeless: document.type_spec.timeless,
            content_hash: document.content_hash.to_hex(),
            size: document.size,
            mtime: rfc3339(document.mtime),
            has_frontmatter: document.raw_frontmatter.is_some(),
            notes: document.notes.iter().map(DocNote::dto).collect(),
        }
    }
}

/// One walk skip, for `--verbose`.
#[derive(Debug, Serialize)]
struct SkipDto<'a> {
    rel_path: &'a str,
    is_dir: bool,
    reason: &'static str,
}

/// What the walk read and refused, with the unit named.
///
/// `skipped` stays a map for compatibility; these fields are the new data.
/// `per_reason` repeats the map's counts and adds the directories pruned for
/// each rule.
#[derive(Debug, Serialize)]
struct WalkDto {
    /// Markdown files the walk admitted. Each is a document or a read skip.
    files: usize,
    /// Paths the walk refused. Equals the sum of `per_reason[].entries`.
    entries: usize,
    /// Directories the walk refused without descending into them.
    directories_pruned: usize,
    /// One row per rule, in [`SkipReason::ORDER`], whatever its count.
    per_reason: Vec<SkipTotalDto>,
}

/// One rule's share of the walk's refusals.
#[derive(Debug, Serialize)]
struct SkipTotalDto {
    /// The rule's stable code (§9.4).
    reason: &'static str,
    /// Paths refused. A file is one entry; a pruned directory is one entry.
    entries: usize,
    /// How many of `entries` were directories whose contents were not visited.
    pruned_directories: usize,
}

/// One file that was admitted but could not become a document.
#[derive(Debug, Serialize)]
struct ReadSkipDto<'a> {
    rel_path: &'a str,
    reason: &'static str,
    message: String,
}

impl<'a> ReadSkipDto<'a> {
    fn new(skip: &'a ReadSkip) -> Self {
        Self {
            rel_path: &skip.rel_path,
            reason: skip.code(),
            message: skip.message(),
        }
    }
}

/// The names in a vault that look like a date but are refused.
///
/// Both the file name and every folder name are tested, because both are rungs of
/// the date ladder. A name counts once however many documents sit under it.
fn date_near_misses(documents: &[Document]) -> Vec<&str> {
    let mut names: BTreeSet<&str> = BTreeSet::new();
    for document in documents {
        names.extend(
            document
                .rel_path
                .split('/')
                .filter(|part| resolve::is_date_near_miss(part)),
        );
    }
    names.into_iter().collect()
}

/// The one line that tells a user their names are not in ISO order.
fn near_miss_line(count: usize) -> String {
    format!(
        "{count} name(s) look like a date but are not ISO order; \
         if you write dates day-first, rename them"
    )
}

/// `YYYY-MM-DD`, the only date form in output (§9.4).
fn iso_date(date: chrono::NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// UTC, second precision, `Z` suffix — stable and sortable.
fn rfc3339(time: std::time::SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Secs, true)
}

// ───────────────────────────────── human ───────────────────────────────────

/// Writes the aligned table and the summary lines.
fn write_human(
    out: &mut impl io::Write,
    root: &Path,
    config: &Config,
    walk: &WalkResult,
    documents: &[Document],
    read_skips: &[ReadSkip],
    verbose: bool,
) {
    // A write error here means the consumer closed the pipe (`janus scan |
    // head`), which is not a failure worth a non-zero exit.
    let _ = writeln!(out, "corpus:  {}", root.display());
    let _ = writeln!(
        out,
        "config:  {} (corpus_id = {:?})",
        root.join(CONFIG_FILE).display(),
        config.corpus_id
    );
    let _ = writeln!(out);

    write_table(out, documents);

    let _ = writeln!(out);
    let bytes_total: u64 = documents.iter().map(|document| document.size).sum();
    let _ = writeln!(
        out,
        "{} document(s), {} bytes",
        documents.len(),
        bytes_total
    );

    // Two lines that reconcile, both read from `walk_totals`: every path the
    // walk yielded is admitted once or skipped once, and a pruned directory is
    // one entry whose contents were never visited (R1-1).
    let totals = walk_totals(walk);
    let skipped = if totals.entries == 0 {
        "0 skipped".to_owned()
    } else {
        format!("{} path(s) skipped: {}", totals.entries, totals.breakdown())
    };
    let _ = writeln!(out, "read:    {} file(s) admitted, {skipped}", totals.files);

    let count = totals.pruned_directories;
    let directories = if count == 1 {
        "1 directory".to_owned()
    } else {
        format!("{count} directories")
    };
    let pruned = if count == 0 {
        directories
    } else {
        format!("{directories} ({})", totals.pruned_breakdown())
    };
    let _ = writeln!(
        out,
        "pruned:  {pruned}; one entry each, contents never visited and not counted"
    );

    if !read_skips.is_empty() {
        let _ = writeln!(out, "unreadable: {}", read_skip_summary(read_skips));
    }
    // Both outputs derive this from the same pure function, so the table line
    // and the JSON list always agree.
    let near_misses = date_near_misses(documents);
    if !near_misses.is_empty() {
        let _ = writeln!(out, "note: {}", near_miss_line(near_misses.len()));
    }
    if let Some(ignored) = walk.ignored_by_git {
        let _ = writeln!(out, "ignored by .gitignore: {ignored}");
    }
    for error in &walk.errors {
        let _ = writeln!(out, "error: {}", error.message);
    }

    if verbose {
        for entry in &walk.skipped {
            let _ = writeln!(out, "  skipped {} ({})", entry.rel_path, entry.reason);
        }
        for skip in read_skips {
            let _ = writeln!(out, "  skipped {}", skip);
        }
    }
}

/// Prints `path`, `type`, `status` and `date (source)` as an aligned table.
///
/// Column widths come from the content, so a long path widens the table rather
/// than being truncated: this output is read by a human diagnosing a corpus, and
/// a truncated path is useless for that.
fn write_table(out: &mut impl io::Write, documents: &[Document]) {
    const HEADERS: [&str; 4] = ["path", "type", "status", "date (source)"];

    let mut rows: Vec<[String; 4]> = Vec::with_capacity(documents.len() + 1);
    rows.push(HEADERS.map(str::to_owned));
    for document in documents {
        rows.push([
            document.rel_path.clone(),
            document.type_name.clone(),
            document.status.as_str().to_owned(),
            date_cell(document),
        ]);
    }

    let mut widths = [0_usize; 4];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }

    for row in &rows {
        let _ = writeln!(
            out,
            "{:<w0$}  {:<w1$}  {:<w2$}  {}",
            row[0],
            row[1],
            row[2],
            row[3],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
        );
    }
}

/// `2026-10-29 (frontmatter)`, or `-` when the document has no date at all.
fn date_cell(document: &Document) -> String {
    document.date.map_or_else(
        || "-".to_owned(),
        |date| format!("{date} ({})", document.date_source),
    )
}

/// Renders read skips as `2 non_utf8_content, 1 unreadable`.
fn read_skip_summary(read_skips: &[ReadSkip]) -> String {
    // Codes sorted rather than in first-seen order: the summary is asserted on
    // by tests and read by humans, so it must not depend on file order.
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for skip in read_skips {
        *counts.entry(skip.code()).or_default() += 1;
    }

    counts
        .into_iter()
        .map(|(code, count)| format!("{count} {code}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocNote;
    use std::fs;
    use std::time::SystemTime;

    fn config() -> Config {
        Config::from_toml("", Path::new("<test>")).expect("default config must parse")
    }

    fn document(rel_path: &str, text: &str) -> Document {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(rel_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("directory");
        }
        fs::write(&path, text).expect("file");
        let file = vault::DiscoveredFile {
            path,
            rel_path: rel_path.to_owned(),
            size: text.len() as u64,
            mtime: SystemTime::UNIX_EPOCH,
        };
        let config = config();
        let mut document = resolve::resolve(&file, &config).expect("resolve must succeed");
        // Identity is a corpus-wide pass; one document is a one-element corpus.
        identity::derive_identity(std::slice::from_mut(&mut document), None, &config);
        document
    }

    /// The case 1.1 vault: one hidden directory holding three documents, one
    /// visible document, and one non-markdown file.
    ///
    /// No `janus.toml` is written: these tests call the walk directly, so the
    /// tree holds only the entries under test.
    fn accounting_vault() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join(".hidden")).expect("dir");
        for name in ["a.md", "b.md", "c.md"] {
            fs::write(root.join(".hidden").join(name), "# hidden").expect("file");
        }
        fs::write(root.join("visible.md"), "# visible").expect("file");
        fs::write(root.join("notes.txt"), "no").expect("file");
        dir
    }

    /// The JSON report and the human table for one vault, from one walk.
    fn report_and_table(root: &Path) -> (serde_json::Value, String) {
        let config = config();
        let walk = vault::walk(root, &config, WalkOptions::hermetic()).expect("walk");
        let (documents, read_skips) = resolve::resolve_all(&walk.files, &config);
        let report = ScanReport::new(
            root,
            &config,
            &walk,
            &documents,
            &read_skips,
            Duration::ZERO,
            false,
        );
        let json = serde_json::to_value(&report).expect("serialize");

        let mut out = Vec::new();
        write_human(
            &mut out,
            root,
            &config,
            &walk,
            &documents,
            &read_skips,
            false,
        );
        (json, String::from_utf8(out).expect("utf-8"))
    }

    /// One rule's row in `walk.per_reason`.
    fn skip_row<'a>(report: &'a serde_json::Value, reason: &str) -> &'a serde_json::Value {
        report["walk"]["per_reason"]
            .as_array()
            .expect("per_reason list")
            .iter()
            .find(|row| row["reason"] == reason)
            .unwrap_or_else(|| panic!("no {reason} row:\n{report}"))
    }

    #[test]
    fn a_pruned_directory_is_one_entry_not_the_documents_inside_it() {
        let dir = accounting_vault();
        let (json, _) = report_and_table(dir.path());

        // Hand-derived. `find . -name '*.md'` counts 4 documents: 3 inside
        // `.hidden/` and the visible one. The walk admits 1, because the
        // hidden directory is pruned as one entry and never entered.
        assert_eq!(json["walk"]["files"], 1, "{json}");
        assert_eq!(json["walk"]["directories_pruned"], 1, "{json}");
        assert_eq!(json["docs"].as_array().map(Vec::len), Some(1), "{json}");
        assert_eq!(skip_row(&json, "hidden")["entries"], 1, "{json}");
        assert_eq!(skip_row(&json, "hidden")["pruned_directories"], 1, "{json}");
        assert_eq!(skip_row(&json, "not_markdown")["entries"], 1, "{json}");
        assert_eq!(
            skip_row(&json, "not_markdown")["pruned_directories"],
            0,
            "a file is never a pruned directory:\n{json}"
        );
    }

    #[test]
    fn the_table_line_and_the_json_totals_agree() {
        let dir = accounting_vault();
        let (json, table) = report_and_table(dir.path());

        let rows = json["walk"]["per_reason"]
            .as_array()
            .expect("per_reason list");
        let sum = |field: &str| -> u64 {
            rows.iter()
                .map(|row| row[field].as_u64().expect("a count"))
                .sum()
        };
        let files = json["walk"]["files"].as_u64().expect("files");
        let entries = sum("entries");
        let pruned = sum("pruned_directories");

        // The walk yields each path once: 1 admitted + 1 pruned directory +
        // 1 `.txt` = 3. The three documents inside `.hidden/` are not paths the
        // walk saw.
        assert_eq!(files + entries, 3, "{json}");
        assert_eq!(json["walk"]["entries"].as_u64(), Some(entries), "{json}");
        assert_eq!(
            json["walk"]["directories_pruned"].as_u64(),
            Some(pruned),
            "the total equals the sum of its rows:\n{json}"
        );
        // The compatibility map reports the same entry counts as the new rows.
        for row in rows {
            let reason = row["reason"].as_str().expect("reason");
            assert_eq!(json["skipped"][reason], row["entries"], "{json}");
        }
        // The table prints the same numbers the JSON carries, because both read
        // `walk_totals`.
        assert!(
            table.contains(&format!(
                "{files} file(s) admitted, {entries} path(s) skipped"
            )),
            "{table}"
        );
        assert!(
            table.contains(&format!("pruned:  {pruned} directory")),
            "{table}"
        );
    }

    #[test]
    fn date_cell_reports_the_source_next_to_the_date() {
        let dated = document("2024-11-02-postgres.md", "x");
        assert_eq!(date_cell(&dated), "2024-11-02 (filename)");

        let declared = document("a.md", "---\ndate: 2026-10-29\n---\n");
        assert_eq!(date_cell(&declared), "2026-10-29 (frontmatter)");
    }

    #[test]
    fn table_pads_columns_to_the_widest_cell() {
        let documents = [
            document("a.md", "x"),
            document("a/much/longer/path.md", "x"),
        ];
        let mut out = Vec::new();
        write_table(&mut out, &documents);
        let text = String::from_utf8(out).expect("utf-8");

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[0].starts_with("path"));
        // Every row's `type` column starts at the same offset.
        let offset = lines[0].find("type").expect("header");
        for line in &lines[1..] {
            assert_eq!(&line[offset..offset + 4], "note", "{line}");
        }
    }

    #[test]
    fn read_skip_summary_is_sorted_by_code() {
        let skips = [
            ReadSkip {
                rel_path: "a.md".to_owned(),
                note: DocNote::NonUtf8Content,
            },
            ReadSkip {
                rel_path: "b.md".to_owned(),
                note: DocNote::Unreadable {
                    message: "nope".to_owned(),
                },
            },
            ReadSkip {
                rel_path: "c.md".to_owned(),
                note: DocNote::NonUtf8Content,
            },
        ];

        assert_eq!(
            read_skip_summary(&skips),
            "2 non_utf8_content, 1 unreadable"
        );
    }

    #[test]
    fn the_json_report_never_contains_content() {
        let text = "---\ntitle: Secret\n---\n\nCONFIDENTIAL BODY\n";
        let documents = [document("a.md", text)];
        let walk = WalkResult::default();
        let config = config();
        let report = ScanReport::new(
            Path::new("/vault"),
            &config,
            &walk,
            &documents,
            &[],
            Duration::from_millis(7),
            false,
        );
        let json = serde_json::to_string(&report).expect("serialize");

        assert!(!json.contains("CONFIDENTIAL BODY"), "{json}");
        assert!(!json.contains("raw_frontmatter"), "{json}");
        assert!(!json.contains("\"body\""), "{json}");
        assert!(json.contains("\"has_frontmatter\":true"), "{json}");
        assert!(json.contains("\"api\":1"), "{json}");
    }

    #[test]
    fn the_json_report_carries_every_skip_rule_even_at_zero() {
        let walk = WalkResult::default();
        let config = config();
        let report = ScanReport::new(
            Path::new("/vault"),
            &config,
            &walk,
            &[],
            &[],
            Duration::ZERO,
            false,
        );
        let json = serde_json::to_value(&report).expect("serialize");
        let skipped = json["skipped"].as_object().expect("skipped map");

        assert_eq!(skipped.len(), SkipReason::ORDER.len());
        assert!(skipped.values().all(|count| count == 0));

        // The new data is a list under `walk`, always all seven rules.
        let rows = json["walk"]["per_reason"]
            .as_array()
            .expect("per_reason list");
        let reasons: Vec<&str> = rows
            .iter()
            .map(|row| row["reason"].as_str().expect("reason"))
            .collect();
        assert_eq!(
            reasons,
            SkipReason::ORDER.map(|reason| reason.as_str()),
            "every rule, in fixed order"
        );
        assert!(
            rows.iter()
                .all(|row| row["entries"] == 0 && row["pruned_directories"] == 0),
            "{json}"
        );
        assert_eq!(json["walk"]["files"], 0, "{json}");
        assert_eq!(json["walk"]["entries"], 0, "{json}");
        assert_eq!(json["walk"]["directories_pruned"], 0, "{json}");
        assert!(json.get("skipped_entries").is_none(), "{json}");
    }

    #[test]
    fn mtime_is_rendered_as_utc_rfc3339() {
        assert_eq!(rfc3339(SystemTime::UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn the_report_carries_the_derived_identity() {
        let documents = [document("a.md", "x")];
        let walk = WalkResult::default();
        let config = config();
        let report = ScanReport::new(
            Path::new("/vault"),
            &config,
            &walk,
            &documents,
            &[],
            Duration::ZERO,
            false,
        );
        let json = serde_json::to_value(&report).expect("serialize");
        let doc = &json["docs"][0];

        // A single undated document: the unique title is the strongest rung.
        assert_eq!(doc["id_source"], "title");
        assert_eq!(doc["id"].as_str().unwrap().len(), 32);
        assert_eq!(doc["id"], documents[0].id.to_hex());
    }
}
