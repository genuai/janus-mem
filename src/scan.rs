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

use std::collections::BTreeMap;
use std::io::{self, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;

use crate::config::{CONFIG_FILE, Config};
use crate::document::Document;
use crate::error::JanusError;
use crate::vault::resolve::{self, ReadSkip};
use crate::vault::{self, SkipReason, WalkOptions, WalkResult};

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
pub fn run(root: &Path, config: &Config, options: Options) -> Result<(), JanusError> {
    let started = Instant::now();
    let walk = vault::walk(root, config, WalkOptions::default())?;
    let (documents, read_skips) = resolve::resolve_all(&walk.files, config);
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
    /// Walk skips per rule, always all rules so the shape is stable.
    skipped: BTreeMap<&'static str, usize>,
    /// Markdown files hidden by `.gitignore`; `null` when not attributed.
    ignored_by_git: Option<usize>,
    /// Directories or files the walker could not read at all.
    errors: Vec<&'a str>,
    /// Files that were admitted but could not be read, hashed or decoded.
    read_skips: Vec<ReadSkipDto<'a>>,
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
            skipped,
            ignored_by_git: walk.ignored_by_git,
            errors: walk.errors.iter().map(|e| e.message.as_str()).collect(),
            read_skips: read_skips.iter().map(ReadSkipDto::new).collect(),
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
            notes: document
                .notes
                .iter()
                .map(|note| NoteDto {
                    code: note.code(),
                    message: note.message(),
                })
                .collect(),
        }
    }
}

/// A tolerated irregularity: machine code plus a human sentence, never content.
#[derive(Debug, Serialize)]
struct NoteDto {
    code: &'static str,
    message: String,
}

/// One walk skip, for `--verbose`.
#[derive(Debug, Serialize)]
struct SkipDto<'a> {
    rel_path: &'a str,
    is_dir: bool,
    reason: &'static str,
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

    let summary = skip_summary(walk);
    if !summary.is_empty() {
        let _ = writeln!(out, "skipped: {}", summary.join(", "));
    }
    if !read_skips.is_empty() {
        let _ = writeln!(out, "unreadable: {}", read_skip_summary(read_skips));
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

/// Renders the per-rule skip counts, omitting rules that never fired.
fn skip_summary(walk: &WalkResult) -> Vec<String> {
    SkipReason::ORDER
        .iter()
        .filter_map(|reason| {
            let count = walk.count(*reason);
            (count > 0).then(|| format!("{count} {reason}"))
        })
        .collect()
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
        resolve::resolve(&file, &config()).expect("resolve must succeed")
    }

    #[test]
    fn skip_summary_is_ordered_and_omits_zero_counts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("target")).expect("dir");
        fs::write(root.join("keep.md"), "# k").expect("file");
        fs::write(root.join("notes.txt"), "no").expect("file");

        let walk = vault::walk(root, &config(), WalkOptions::hermetic()).expect("walk");
        let summary = skip_summary(&walk);

        assert_eq!(
            summary,
            ["1 always_excluded".to_owned(), "1 not_markdown".to_owned()]
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
        assert!(json.get("skipped_entries").is_none(), "{json}");
    }

    #[test]
    fn mtime_is_rendered_as_utc_rfc3339() {
        assert_eq!(rfc3339(SystemTime::UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn an_unresolved_identity_is_not_reported() {
        // Phase 4 has no ladder yet; a `rel_path`-tier placeholder must not leak
        // into a report and claim every document fell through to the last rung.
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

        assert!(doc.get("id").is_none(), "{doc}");
        assert!(doc.get("id_source").is_none(), "{doc}");
    }
}
