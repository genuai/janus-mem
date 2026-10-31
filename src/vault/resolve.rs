//! Metadata resolution: a discovered file becomes a [`Document`] (SPEC §6.2,
//! §6.3, §6.6.1, §6.12).
//!
//! Phase 3a turns bytes into a typed frontmatter view. This is the layer above
//! it: read the file once, apply the §6.3 fallback hierarchy to every metadata
//! field, and record a [`DocNote`] for each fallback a reader should know about.
//!
//! Identity (§6.4) is deliberately *not* decided here. The ladder needs the
//! whole scan's document set to answer "is this key unique?", so it runs as a
//! second pass over the resolved documents; until then a document carries the
//! ladder's weakest rung (`rel_path`) as a provisional identity.
//!
//! # Resolution order, in one place
//!
//! ```text
//! title     frontmatter title → filename stem (H1 arrives with M2)
//! type      frontmatter → [paths] hint → [defaults]; unknown keeps its name
//! status    frontmatter → [defaults]; unknown becomes `unknown`
//! date      frontmatter → filename `YYYY-MM-DD` prefix → mtime (never identity)
//! from/to   frontmatter only — never derived, never from mtime
//! scope     frontmatter, normalised; invalid → `global`
//! shared    frontmatter, deduplicated, order-preserving; never identity
//! timeless  frontmatter override → [decay] timeless_types
//! ```
//!
//! Every rule is fail-open. A document with nonsense frontmatter still becomes a
//! document, because the alternative — a scan that silently omits exactly the
//! files that need attention — is the worse failure (§6.3).

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::time::SystemTime;

use chrono::NaiveDate;

use crate::config::{Config, ResolvedType, TypeSpec};
use crate::document::{DateSource, DocNote, Document, Frontmatter, IdentityTier, Status};
use crate::error::JanusError;
use crate::id::DocId;
use crate::vault::frontmatter::{self, FrontmatterRead};
use crate::vault::{self, DiscoveredFile};

/// The scope a document belongs to when it declares none — and the fallback for
/// one that declares something invalid (§6.12 rule 1).
pub const GLOBAL_SCOPE: &str = "global";

/// Length of a `YYYY-MM-DD` prefix in bytes. ASCII by construction, so slicing
/// this far into a filename can never split a character.
const DATE_PREFIX_LEN: usize = 10;

/// A file the walk admitted but that could not become a document.
///
/// Reading is where a scan gives up on a file, and giving up is a *skip*, not a
/// failure: one unreadable note must not hide the rest of the corpus. Carrying
/// the [`DocNote`] keeps the reason in the same vocabulary as every other
/// irregularity, so a report can print `rel_path` plus one code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadSkip {
    /// Vault-relative path, `/`-separated, so a report needs no second lookup.
    pub rel_path: String,
    /// Why the file was skipped: unreadable, changed during read, or not UTF-8.
    pub note: DocNote,
}

impl ReadSkip {
    /// The stable machine-readable reason code (§9.4).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.note.code()
    }

    /// The one-line human explanation.
    #[must_use]
    pub fn message(&self) -> String {
        self.note.message()
    }
}

impl fmt::Display for ReadSkip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.rel_path, self.note.message())
    }
}

/// Reads one discovered file and resolves its metadata into a [`Document`].
///
/// The file is read exactly once and everything — hash, frontmatter, body — is
/// derived from those bytes, so the snapshot cannot disagree with itself
/// (§6.9). A file that cannot be read consistently, or is not UTF-8, comes back
/// as a [`ReadSkip`] rather than an error; the caller keeps scanning.
///
/// # Errors
///
/// [`ReadSkip`] for an unreadable file, a file that kept changing while being
/// read, or content that is not valid UTF-8.
pub fn resolve(file: &DiscoveredFile, config: &Config) -> Result<Document, ReadSkip> {
    let content = read_bytes(file)?;

    let text = std::str::from_utf8(&content.bytes).map_err(|_| ReadSkip {
        rel_path: file.rel_path.clone(),
        note: DocNote::NonUtf8Content,
    })?;

    let FrontmatterRead {
        span,
        raw,
        body,
        bom_len: _,
        frontmatter,
        mut notes,
    } = frontmatter::read(text, config);

    let title = resolve_title(frontmatter.title.as_deref(), &file.rel_path);
    let (type_name, type_spec) = resolve_type(&frontmatter, &file.rel_path, config, &mut notes);

    let timeless = frontmatter.timeless.unwrap_or(type_spec.timeless);
    let type_spec = TypeSpec {
        timeless,
        ..type_spec
    };

    let status = resolve_status(frontmatter.status.as_deref(), config, &mut notes);
    let (date, date_source) = resolve_date(&frontmatter, &file.rel_path, content.mtime);
    let scope = resolve_scope(frontmatter.scope.as_deref(), &mut notes);
    let shared_with = dedupe_shared_with(&frontmatter.shared_with);
    let (id, id_source) = provisional_identity(&file.rel_path);
    // §6.10 windows are derived from the supersession chain in M5; M1 parses
    // what the file says and nothing else (SPEC §6.3, phase 3 note).
    let effective_from = frontmatter.effective_from;
    let effective_to = frontmatter.effective_to;

    Ok(Document {
        id,
        id_source,
        path: file.path.clone(),
        rel_path: file.rel_path.clone(),
        content_hash: content.content_hash,
        frontmatter,
        raw_frontmatter: raw.map(str::to_owned),
        body: body.to_owned(),
        span,
        // The walk's `size`/`mtime` are its own snapshot; these are the ones that
        // belong to the bytes that were actually hashed and parsed.
        mtime: content.mtime,
        size: content.size,
        title,
        type_name,
        type_spec,
        status,
        date,
        date_source,
        effective_from,
        effective_to,
        scope,
        shared_with,
        notes,
    })
}

/// Resolves a whole walk's worth of files, separating documents from skips.
///
/// Order follows the input, which the walk has already sorted by `rel_path`, so
/// a report can print the result without sorting it again and two scans of an
/// unchanged tree agree exactly (§10). A file that cannot be read does not stop
/// the batch: it is returned as a [`ReadSkip`] alongside the documents.
#[must_use]
pub fn resolve_all(files: &[DiscoveredFile], config: &Config) -> (Vec<Document>, Vec<ReadSkip>) {
    let mut documents = Vec::with_capacity(files.len());
    let mut skips = Vec::new();

    for file in files {
        match resolve(file, config) {
            Ok(document) => documents.push(document),
            Err(skip) => skips.push(skip),
        }
    }

    (documents, skips)
}

/// Reads a file, translating both read failures into skips.
fn read_bytes(file: &DiscoveredFile) -> Result<vault::FileContent, ReadSkip> {
    vault::read_consistent(&file.path).map_err(|err| ReadSkip {
        rel_path: file.rel_path.clone(),
        note: match err {
            JanusError::ChangedDuringRead { .. } => DocNote::ChangedDuringRead,
            // Every other read failure is reported as unreadable, with the
            // path already in the message via the document's own location.
            other => DocNote::Unreadable {
                message: other.to_string(),
            },
        },
    })
}

/// Applies the title hierarchy: frontmatter title, else the filename stem.
///
/// The first `#` heading is the third rung, and it arrives with the Markdown
/// parser in M2. A declared-but-blank title counts as absent rather than as an
/// empty title, because an empty title is never what the author meant.
fn resolve_title(declared: Option<&str>, rel_path: &str) -> String {
    let declared = declared.map(str::trim).filter(|title| !title.is_empty());

    declared.map_or_else(
        || filename_stem(rel_path).unwrap_or_else(|| rel_path.to_owned()),
        str::to_owned,
    )
}

/// The filename without its extension, e.g. `notes/2024-11-02-pg.md` → `2024-11-02-pg`.
fn filename_stem(rel_path: &str) -> Option<String> {
    Path::new(rel_path)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
}

/// Applies the type hierarchy: frontmatter, else the path hint, else the default.
///
/// Types are an open registry (§6.6.1), so an unknown name is kept verbatim with
/// the fallback weight and a note — never replaced by the default type.
fn resolve_type(
    frontmatter: &Frontmatter,
    rel_path: &str,
    config: &Config,
    notes: &mut Vec<DocNote>,
) -> (String, TypeSpec) {
    let declared = frontmatter
        .r#type
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());

    let resolved = match declared.or_else(|| config.path_type_hint(rel_path)) {
        Some(name) => config.resolve_type(name),
        None => config.default_type(),
    };

    note_if_unknown_type(&resolved, notes);
    (resolved.name.into_owned(), resolved.spec)
}

/// Records a note for a canonical type the authority registry does not know.
fn note_if_unknown_type(resolved: &ResolvedType<'_>, notes: &mut Vec<DocNote>) {
    if !resolved.known {
        notes.push(DocNote::UnknownType {
            value: resolved.name.to_string(),
        });
    }
}

/// Applies the status hierarchy: frontmatter, else the configured default.
///
/// An unrecognised value becomes [`Status::Unknown`] rather than the default:
/// the file declared something, and pretending it said `active` would hide the
/// typo that the 0.8× factor and the note exist to surface (§6.3).
fn resolve_status(declared: Option<&str>, config: &Config, notes: &mut Vec<DocNote>) -> Status {
    let Some(raw) = declared.map(str::trim).filter(|value| !value.is_empty()) else {
        return config.defaults.status;
    };

    let status = Status::parse(raw);
    if status == Status::Unknown {
        notes.push(DocNote::UnknownStatus {
            value: raw.to_owned(),
        });
    }
    status
}

/// Applies the date hierarchy and reports which rung produced the date.
///
/// `mtime` is a legitimate last resort for display but never for identity
/// (§6.4), which is why the source travels with the value instead of being
/// re-derived by whoever needs it.
fn resolve_date(
    frontmatter: &Frontmatter,
    rel_path: &str,
    mtime: SystemTime,
) -> (Option<NaiveDate>, DateSource) {
    if let Some(date) = frontmatter.date {
        return (Some(date), DateSource::Frontmatter);
    }
    if let Some(date) = filename_date(rel_path) {
        return (Some(date), DateSource::Filename);
    }
    // Every file has an mtime today, so the `None` arm is currently
    // unreachable. `DateSource::None` stays defined for a source that genuinely
    // has no date (a synthetic document, a filesystem without mtimes) rather
    // than being removed and re-added later.
    (
        Some(chrono::DateTime::<chrono::Utc>::from(mtime).date_naive()),
        DateSource::Mtime,
    )
}

/// Parses a `YYYY-MM-DD` prefix from the filename stem, e.g.
/// `2024-11-02-postgres.md`.
///
/// The date must be a whole prefix: the character after it is a separator
/// (`-`, `_`, `.`) or the end of the stem. So `2024-11-02.md`,
/// `2024-11-02_postgres.md` and `2024-11-02.v2.md` all match, while
/// `2024_11_02.md` (wrong separators) and `2024-11-02notadate.md` do not — a
/// date glued to more text is a version string far more often than a date.
fn filename_date(rel_path: &str) -> Option<NaiveDate> {
    let stem = filename_stem(rel_path)?;
    // `get` rather than slicing: a stem shorter than the prefix, or one whose
    // byte 10 is mid-character, must return `None`, not panic.
    let prefix = stem.get(..DATE_PREFIX_LEN)?;
    let rest = stem.get(DATE_PREFIX_LEN..)?;

    match rest.chars().next() {
        None | Some('-' | '_' | '.') => NaiveDate::parse_from_str(prefix, "%Y-%m-%d").ok(),
        Some(_) => None,
    }
}

/// Normalises and validates a declared scope (§6.12 rule 1).
///
/// Absent or blank means global and needs no note — that is the documented
/// default, not a fallback. Invalid *content* is different: it falls back to
/// global and says so, because a scope that silently disappeared would make a
/// document look more widely applicable than its author intended.
fn resolve_scope(declared: Option<&str>, notes: &mut Vec<DocNote>) -> String {
    let Some(raw) = declared else {
        return GLOBAL_SCOPE.to_owned();
    };
    if raw.trim().is_empty() {
        return GLOBAL_SCOPE.to_owned();
    }

    let normalised = normalise_scope(raw);
    if is_valid_scope(&normalised) {
        normalised
    } else {
        notes.push(DocNote::InvalidScope {
            value: raw.to_owned(),
        });
        GLOBAL_SCOPE.to_owned()
    }
}

/// Trims, lowercases and collapses internal whitespace, in that order.
///
/// Only ASCII case is folded: the valid scope alphabet is ASCII, so anything
/// else is invalid whatever its case, and ASCII folding cannot expand one
/// character into two the way Unicode lowercasing can.
fn normalise_scope(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for word in raw.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&word.to_ascii_lowercase());
    }
    out
}

/// Whether a normalised scope matches `[a-z0-9][a-z0-9._:-]*`.
fn is_valid_scope(scope: &str) -> bool {
    let mut chars = scope.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | ':' | '-')
        })
}

/// Deduplicates `shared_with`, keeping the first occurrence of each tag.
///
/// These are retrieval tags, so duplicates only cost work; the *order* is kept
/// because it is the closest thing to author intent a set-of-strings has, and a
/// deterministic order keeps reports stable. Empty entries are dropped: a blank
/// tag matches nothing and denotes nothing.
fn dedupe_shared_with(declared: &[String]) -> Vec<String> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut out = Vec::with_capacity(declared.len());

    for tag in declared {
        let tag = tag.trim();
        if !tag.is_empty() && seen.insert(tag) {
            out.push(tag.to_owned());
        }
    }

    out
}

/// The identity a document carries before the §6.4 ladder runs.
///
/// Phase 3b resolves metadata only. The ladder needs the *whole* scan's document
/// set to decide whether a key is unique, so it runs as a second pass (phase
/// 3c) over every resolved document and overwrites both of these fields.
///
/// Until then a document reports the ladder's last rung, `rel_path` — always
/// well-defined, and exactly what the ladder would choose for a document with no
/// other evidence. It is deliberately the *weakest* answer rather than a guess
/// at a stronger one, so nothing here can later disagree with the ladder.
fn provisional_identity(rel_path: &str) -> (DocId, IdentityTier) {
    (
        DocId::from_match_key(rel_path.as_bytes()),
        IdentityTier::RelPath,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn config(toml: &str) -> Config {
        Config::from_toml(toml, Path::new("<test>")).expect("test config must parse")
    }

    /// Writes `text` at `rel` inside `dir` and returns the walk entry for it.
    fn write(dir: &Path, rel: &str, text: &str) -> DiscoveredFile {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("test directory");
        }
        fs::write(&path, text).expect("test file");
        DiscoveredFile {
            path,
            rel_path: rel.to_owned(),
            size: text.len() as u64,
            mtime: SystemTime::UNIX_EPOCH,
        }
    }

    /// Resolves `text` written at `rel` under the given config.
    fn resolved(rel: &str, text: &str, toml: &str) -> Document {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = write(dir.path(), rel, text);
        resolve(&file, &config(toml)).expect("resolve must succeed")
    }

    /// Resolves with the default config.
    fn doc(rel: &str, text: &str) -> Document {
        resolved(rel, text, "")
    }

    /// Wraps `text` in a frontmatter block with a closing fence.
    fn frontmatter(text: &str) -> String {
        format!("---\n{text}---\nbody\n")
    }

    // ───────────────────────────────── title ─────────────────────────────────

    #[test]
    fn title_comes_from_frontmatter_before_the_filename() {
        let document = doc(
            "notes/2024-11-02-postgres.md",
            &frontmatter("title: Postgres cache\n"),
        );
        assert_eq!(document.title, "Postgres cache");
    }

    #[test]
    fn title_falls_back_to_the_filename_stem() {
        assert_eq!(
            doc("notes/2024-11-02-postgres.md", "no frontmatter").title,
            "2024-11-02-postgres"
        );
        assert_eq!(doc("notes/top.md", "x").title, "top");
        assert_eq!(doc("top.markdown", "x").title, "top");
        assert_eq!(doc("notes/archive.v2.md", "x").title, "archive.v2");
        assert_eq!(doc("notes/no-extension", "x").title, "no-extension");
    }

    #[test]
    fn a_blank_declared_title_falls_back_to_the_filename() {
        assert_eq!(
            doc("notes/deep.md", &frontmatter("title: \"   \"\n")).title,
            "deep"
        );
    }

    #[test]
    fn the_filename_stem_is_the_basename_not_the_whole_path() {
        assert_eq!(doc("2026/Q4/plan.md", "x").title, "plan");
    }

    // ────────────────────────────────── type ─────────────────────────────────

    #[test]
    fn frontmatter_type_is_resolved_through_the_alias_registry() {
        let document = doc("a.md", &frontmatter("type: adr\n"));

        assert_eq!(document.type_name, "decision");
        assert_eq!(document.type_spec.authority, 1.5);
        assert!(document.type_spec.timeless);
        assert!(document.notes.is_empty(), "{:?}", document.notes);
    }

    #[test]
    fn a_path_hint_types_a_document_whose_frontmatter_is_silent() {
        let toml = "[paths]\n\"decisions/\" = \"decision\"\n";
        let document = resolved("decisions/one.md", &frontmatter("status: active\n"), toml);

        assert_eq!(document.type_name, "decision");
        assert_eq!(document.type_spec.authority, 1.5);
    }

    #[test]
    fn frontmatter_type_wins_over_a_path_hint() {
        let toml = "[paths]\n\"decisions/\" = \"decision\"\n";
        let document = resolved("decisions/one.md", &frontmatter("type: journal\n"), toml);

        assert_eq!(document.type_name, "journal");
    }

    #[test]
    fn a_path_hint_is_not_consulted_for_a_document_outside_it() {
        let toml = "[paths]\n\"decisions/\" = \"decision\"\n";
        let document = resolved("notes/one.md", "x", toml);

        assert_eq!(document.type_name, "note");
    }

    #[test]
    fn the_default_type_applies_when_nothing_declares_one() {
        let document = doc("a.md", "x");

        assert_eq!(document.type_name, "note");
        assert_eq!(document.type_spec.authority, 0.9);
        assert!(!document.type_spec.timeless);
        assert_eq!(document.type_spec.half_life_days, 180.0);
    }

    #[test]
    fn the_default_type_is_configurable() {
        let document = resolved("a.md", "x", "[defaults]\ntype = \"journal\"\n");

        assert_eq!(document.type_name, "journal");
        assert_eq!(document.type_spec.authority, 1.0);
    }

    #[test]
    fn an_unknown_type_keeps_its_name_gets_the_fallback_weight_and_a_note() {
        let document = doc("a.md", &frontmatter("type: runbook\n"));

        assert_eq!(document.type_name, "runbook");
        assert_eq!(document.type_spec.authority, 0.80);
        assert_eq!(
            document.notes,
            [DocNote::UnknownType {
                value: "runbook".to_owned()
            }]
        );
    }

    #[test]
    fn an_unknown_type_reached_through_an_alias_reports_the_canonical_name() {
        let document = resolved(
            "a.md",
            &frontmatter("type: adr\n"),
            "[types]\nadr = \"runbook\"\n",
        );

        assert_eq!(document.type_name, "runbook");
        assert_eq!(
            document.notes,
            [DocNote::UnknownType {
                value: "runbook".to_owned()
            }]
        );
    }

    #[test]
    fn a_blank_declared_type_falls_through_to_the_path_hint() {
        let toml = "[paths]\n\"decisions/\" = \"decision\"\n";
        let document = resolved("decisions/one.md", &frontmatter("type: \"\"\n"), toml);

        assert_eq!(document.type_name, "decision");
    }

    #[test]
    fn the_unknown_type_keeps_its_name_apart_from_its_spec() {
        let document = doc("a.md", &frontmatter("type: retro\n"));

        assert_eq!(document.type_name, "retro");
        assert!(
            document.type_spec.decays(),
            "an unknown type is not timeless"
        );
        assert_eq!(document.type_spec.half_life_days, 180.0);
    }

    // ───────────────────────────────── status ────────────────────────────────

    #[test]
    fn declared_status_is_parsed_case_insensitively() {
        assert_eq!(
            doc("a.md", &frontmatter("status: DRAFT\n")).status,
            Status::Draft
        );
        assert_eq!(
            doc("a.md", &frontmatter("status: superseded\n")).status,
            Status::Superseded
        );
        assert_eq!(
            doc("a.md", &frontmatter("status: deprecated\n")).status,
            Status::Deprecated
        );
        assert_eq!(
            doc("a.md", &frontmatter("status: active\n")).status,
            Status::Active
        );
    }

    #[test]
    fn a_missing_status_defaults_to_active() {
        let document = doc("a.md", "no frontmatter");

        assert_eq!(document.status, Status::Active);
        assert!(document.notes.is_empty(), "{:?}", document.notes);
    }

    #[test]
    fn the_default_status_is_configurable() {
        let document = resolved("a.md", "x", "[defaults]\nstatus = \"draft\"\n");

        assert_eq!(document.status, Status::Draft);
    }

    #[test]
    fn an_unknown_status_becomes_unknown_with_a_note_rather_than_the_default() {
        let document = doc("a.md", &frontmatter("status: archived\n"));

        assert_eq!(document.status, Status::Unknown);
        assert_eq!(
            document.notes,
            [DocNote::UnknownStatus {
                value: "archived".to_owned()
            }]
        );
    }

    #[test]
    fn a_blank_declared_status_falls_back_to_the_default_without_a_note() {
        let document = doc("a.md", &frontmatter("status: \"  \"\n"));

        assert_eq!(document.status, Status::Active);
        assert!(document.notes.is_empty(), "{:?}", document.notes);
    }

    // ────────────────────────────────── date ─────────────────────────────────

    #[test]
    fn a_frontmatter_date_wins_and_is_reported_as_such() {
        let document = doc("2024-01-01-other.md", &frontmatter("date: 2026-10-29\n"));

        assert_eq!(document.date, NaiveDate::from_ymd_opt(2026, 10, 29));
        assert_eq!(document.date_source, DateSource::Frontmatter);
    }

    #[test]
    fn a_date_alias_such_as_created_wins_over_the_filename() {
        let document = doc("2024-01-01-other.md", &frontmatter("created: 2026-10-29\n"));

        assert_eq!(document.date, NaiveDate::from_ymd_opt(2026, 10, 29));
        assert_eq!(document.date_source, DateSource::Frontmatter);
    }

    #[test]
    fn an_iso_timestamp_in_frontmatter_keeps_its_date_part() {
        let document = doc("a.md", &frontmatter("date: 2026-10-29T10:00:00Z\n"));

        assert_eq!(document.date, NaiveDate::from_ymd_opt(2026, 10, 29));
        assert_eq!(document.date_source, DateSource::Frontmatter);
    }

    #[test]
    fn a_filename_date_is_parsed_and_reported_as_such() {
        for rel in [
            "2024-11-02-postgres.md",
            "notes/2024-11-02-postgres.md",
            "2024-11-02.md",
            "2024-11-02_postgres.md",
            "2024-11-02.v2.md",
        ] {
            let document = doc(rel, "x");
            assert_eq!(
                document.date,
                NaiveDate::from_ymd_opt(2024, 11, 2),
                "{rel} should carry a filename date"
            );
            assert_eq!(document.date_source, DateSource::Filename, "{rel}");
        }
    }

    #[test]
    fn a_filename_with_the_wrong_separators_is_not_a_date() {
        // §6.3 asks for a `YYYY-MM-DD` prefix specifically.
        let document = doc("2024_11_02.md", "x");

        assert_eq!(document.date_source, DateSource::Mtime);
    }

    #[test]
    fn a_date_glued_to_more_text_is_not_a_date() {
        // `2024-11-02notadate` is a version string, not an authored date.
        assert_eq!(
            doc("2024-11-02notadate.md", "x").date_source,
            DateSource::Mtime
        );
        assert_eq!(
            doc("2024-13-45-post.md", "x").date_source,
            DateSource::Mtime
        );
        assert_eq!(doc("2024-11-0-post.md", "x").date_source, DateSource::Mtime);
    }

    #[test]
    fn a_multibyte_character_after_the_date_neither_matches_nor_panics() {
        let document = doc("2024-11-02-日本語.md", "x");

        assert_eq!(document.date, NaiveDate::from_ymd_opt(2024, 11, 2));
        assert_eq!(document.date_source, DateSource::Filename);
    }

    #[test]
    fn a_multibyte_filename_shorter_than_a_date_prefix_is_safe() {
        let document = doc("日本語.md", "x");

        assert_eq!(document.date_source, DateSource::Mtime);
    }

    #[test]
    fn mtime_is_the_last_resort_and_uses_the_utc_date() {
        let document = doc("undated.md", "x");
        let expected = chrono::DateTime::<chrono::Utc>::from(document.mtime).date_naive();

        assert_eq!(document.date, Some(expected));
        assert_eq!(document.date_source, DateSource::Mtime);
    }

    #[test]
    fn an_unparseable_frontmatter_date_falls_through_to_the_filename() {
        let document = doc("2024-11-02-postgres.md", &frontmatter("date: soon\n"));

        assert_eq!(document.date, NaiveDate::from_ymd_opt(2024, 11, 2));
        assert_eq!(document.date_source, DateSource::Filename);
    }

    #[test]
    fn effective_dates_are_parsed_but_never_derived() {
        let declared = doc(
            "a.md",
            &frontmatter("effective_from: 2026-11-01\neffective_to: 2027-01-01\n"),
        );
        assert_eq!(
            declared.effective_from,
            NaiveDate::from_ymd_opt(2026, 11, 1)
        );
        assert_eq!(declared.effective_to, NaiveDate::from_ymd_opt(2027, 1, 1));

        // `date` and `mtime` both exist here, and neither may fill the window:
        // §6.10 derivation belongs to M5, not to a read.
        let absent = doc("2024-11-02-postgres.md", "x");
        assert_eq!(absent.date_source, DateSource::Filename);
        assert_eq!(absent.effective_from, None);
        assert_eq!(absent.effective_to, None);
    }

    // ───────────────────────────────── scope ─────────────────────────────────

    #[test]
    fn an_absent_scope_is_global_without_a_note() {
        let document = doc("a.md", "x");

        assert_eq!(document.scope, GLOBAL_SCOPE);
        assert!(document.notes.is_empty(), "{:?}", document.notes);
    }

    #[test]
    fn scope_is_lowercased_and_trimmed() {
        assert_eq!(
            doc("a.md", &frontmatter("scope: \"  ORG:Acme  \"\n")).scope,
            "org:acme"
        );
    }

    #[test]
    fn scope_accepts_digits_dots_underscores_colons_and_dashes() {
        for scope in ["org:acme", "org:acme-q4.v2_1", "acme", "2026"] {
            let document = doc("a.md", &frontmatter(&format!("scope: {scope}\n")));
            assert_eq!(document.scope, scope);
            assert!(document.notes.is_empty(), "{scope}: {:?}", document.notes);
        }
    }

    #[test]
    fn an_invalid_scope_falls_back_to_global_and_is_reported_as_declared() {
        let document = doc("a.md", &frontmatter("scope: org/acme\n"));

        assert_eq!(document.scope, GLOBAL_SCOPE);
        assert_eq!(
            document.notes,
            [DocNote::InvalidScope {
                value: "org/acme".to_owned()
            }]
        );
    }

    #[test]
    fn a_scope_with_internal_whitespace_is_invalid_even_after_collapsing() {
        let document = doc("a.md", &frontmatter("scope: \"org   acme\"\n"));

        assert_eq!(document.scope, GLOBAL_SCOPE);
        assert_eq!(
            document.notes,
            [DocNote::InvalidScope {
                value: "org   acme".to_owned()
            }]
        );
    }

    #[test]
    fn a_scope_may_not_start_with_a_separator() {
        for scope in [":acme", "-acme", ".acme", "_acme"] {
            let document = doc("a.md", &frontmatter(&format!("scope: \"{scope}\"\n")));
            assert_eq!(document.scope, GLOBAL_SCOPE, "{scope}");
            assert!(
                document.notes.contains(&DocNote::InvalidScope {
                    value: scope.to_owned()
                }),
                "{scope}: {:?}",
                document.notes
            );
        }
    }

    #[test]
    fn an_uppercase_scope_is_valid_because_normalisation_lowercases_first() {
        let document = doc("a.md", &frontmatter("scope: Acme\n"));

        assert_eq!(document.scope, "acme");
        assert!(document.notes.is_empty(), "{:?}", document.notes);
    }

    #[test]
    fn a_blank_scope_is_global_without_a_note() {
        let document = doc("a.md", &frontmatter("scope: \"   \"\n"));

        assert_eq!(document.scope, GLOBAL_SCOPE);
        assert!(document.notes.is_empty(), "{:?}", document.notes);
    }

    #[test]
    fn scope_normalisation_is_idempotent() {
        for raw in ["ORG:Acme", "  org:acme ", "org:acme"] {
            assert_eq!(normalise_scope(raw), "org:acme");
        }
    }

    // ─────────────────────────────── shared_with ─────────────────────────────

    #[test]
    fn shared_with_is_deduplicated_preserving_first_occurrence_order() {
        let document = doc(
            "a.md",
            &frontmatter("shared_with: [shared:legal, shared:finance, shared:legal]\n"),
        );

        assert_eq!(document.shared_with, ["shared:legal", "shared:finance"]);
    }

    #[test]
    fn shared_with_accepts_a_scalar_and_drops_blank_entries() {
        let document = doc("a.md", &frontmatter("shared_with: shared:finance\n"));

        assert_eq!(document.shared_with, ["shared:finance"]);
        assert!(dedupe_shared_with(&["".to_owned(), "  ".to_owned()]).is_empty());
    }

    #[test]
    fn shared_with_is_trimmed_before_deduplicating() {
        assert_eq!(
            dedupe_shared_with(&[
                " shared:legal".to_owned(),
                "shared:legal ".to_owned(),
                "shared:finance".to_owned(),
            ]),
            ["shared:legal", "shared:finance"]
        );
    }

    #[test]
    fn an_absent_shared_with_is_empty() {
        assert!(doc("a.md", "x").shared_with.is_empty());
    }

    #[test]
    fn shared_with_does_not_change_the_provisional_identity() {
        let plain = doc("a.md", "x");
        let shared = doc("a.md", &frontmatter("shared_with: [shared:legal]\n"));

        assert_eq!(plain.id, shared.id);
    }

    // ──────────────────────────────── timeless ───────────────────────────────

    #[test]
    fn timeless_comes_from_the_type_registry_when_frontmatter_is_silent() {
        assert!(
            doc("a.md", &frontmatter("type: decision\n"))
                .type_spec
                .timeless
        );
        assert!(
            doc("a.md", &frontmatter("type: principle\n"))
                .type_spec
                .timeless
        );
        assert!(
            !doc("a.md", &frontmatter("type: journal\n"))
                .type_spec
                .timeless
        );
    }

    #[test]
    fn frontmatter_can_opt_a_timeless_type_out_of_timelessness() {
        let document = doc("a.md", &frontmatter("type: decision\ntimeless: false\n"));

        assert!(!document.type_spec.timeless);
    }

    #[test]
    fn frontmatter_can_opt_an_ordinary_type_into_timelessness() {
        let document = doc("a.md", &frontmatter("type: journal\ntimeless: true\n"));

        assert!(document.type_spec.timeless);
        assert!(!document.type_spec.decays());
    }

    #[test]
    fn a_frontmatter_timeless_override_does_not_rewrite_the_frontmatter_view() {
        let document = doc("a.md", &frontmatter("type: decision\ntimeless: false\n"));

        assert_eq!(document.frontmatter.timeless, Some(false));
        assert_eq!(document.frontmatter.r#type.as_deref(), Some("decision"));
    }

    // ──────────────────────────── file and content ────────────────────────────

    #[test]
    fn content_body_and_span_survive_resolution() {
        let text = "\u{feff}---\r\ntitle: T\r\n---\r\nbody line\r\n";
        let document = doc("a.md", text);

        assert_eq!(
            document.raw_frontmatter.as_deref(),
            Some("---\r\ntitle: T\r\n---\r\n")
        );
        assert_eq!(document.body, "body line\r\n");
        assert_eq!(document.span.start, 3);
        assert_eq!(document.span.end, 3 + "---\r\ntitle: T\r\n---\r\n".len());
        assert_eq!(document.title, "T");
    }

    #[test]
    fn a_file_without_frontmatter_keeps_its_whole_text_as_body() {
        let document = doc("a.md", "# heading\n\ntext\n");

        assert!(document.raw_frontmatter.is_none());
        assert_eq!(document.body, "# heading\n\ntext\n");
        assert!(document.span.is_empty());
    }

    #[test]
    fn the_hash_size_and_mtime_describe_the_bytes_that_were_parsed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let text = frontmatter("title: T\n");
        let file = write(dir.path(), "a.md", &text);

        let document = resolve(&file, &config("")).expect("resolve must succeed");
        let on_disk = fs::metadata(&file.path).expect("metadata");

        assert_eq!(document.size, text.len() as u64);
        assert_eq!(document.size, on_disk.len());
        assert_eq!(
            document.content_hash,
            crate::id::Blake3Hash::of_bytes(text.as_bytes())
        );
        assert_eq!(document.mtime, on_disk.modified().expect("mtime"));
        // The walk's own snapshot is stale by construction here; the document
        // reports the timestamp that belongs to the bytes it hashed.
        assert_ne!(document.mtime, file.mtime);
    }

    #[test]
    fn path_and_rel_path_are_carried_through_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = write(dir.path(), "notes/deep/one.md", "x");
        let expected = file.path.clone();

        let document = resolve(&file, &config("")).expect("resolve must succeed");

        assert_eq!(document.path, expected);
        assert_eq!(document.rel_path, "notes/deep/one.md");
    }

    #[test]
    fn the_provisional_identity_is_the_rel_path_rung_of_the_ladder() {
        let document = doc(
            "notes/deep.md",
            &frontmatter("title: T\ndate: 2026-01-01\n"),
        );

        assert_eq!(document.id_source, IdentityTier::RelPath);
        assert_eq!(document.id, DocId::from_match_key(b"notes/deep.md"));
    }

    #[test]
    fn frontmatter_is_preserved_verbatim_next_to_the_resolved_fields() {
        let document = doc(
            "a.md",
            &frontmatter("type: adr\nstatus: archived\nscope: org/acme\n"),
        );

        assert_eq!(document.frontmatter.r#type.as_deref(), Some("adr"));
        assert_eq!(document.frontmatter.status.as_deref(), Some("archived"));
        assert_eq!(document.frontmatter.scope.as_deref(), Some("org/acme"));
    }

    // ────────────────────────────────── skips ────────────────────────────────

    #[test]
    fn non_utf8_content_is_skipped_with_a_note() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = write(dir.path(), "bad.md", "placeholder");
        fs::write(&file.path, [0xff, 0xfe, 0x00]).expect("write bytes");

        let skip = resolve(&file, &config("")).expect_err("invalid UTF-8 must be skipped");

        assert_eq!(skip.rel_path, "bad.md");
        assert_eq!(skip.note, DocNote::NonUtf8Content);
        assert_eq!(skip.code(), "non_utf8_content");
    }

    #[test]
    fn an_unreadable_file_is_skipped_not_failed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = DiscoveredFile {
            path: dir.path().join("gone.md"),
            rel_path: "gone.md".to_owned(),
            size: 0,
            mtime: SystemTime::UNIX_EPOCH,
        };

        let skip = resolve(&missing, &config("")).expect_err("missing file must be skipped");

        assert_eq!(skip.code(), "unreadable");
        assert_eq!(skip.rel_path, "gone.md");
        assert!(!skip.message().is_empty());
        assert!(skip.to_string().contains("gone.md"));
    }

    #[test]
    fn a_directory_in_place_of_a_file_is_skipped_as_unreadable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.md");
        fs::create_dir_all(&path).expect("directory");
        let file = DiscoveredFile {
            path,
            rel_path: "a.md".to_owned(),
            size: 0,
            mtime: SystemTime::UNIX_EPOCH,
        };

        assert_eq!(
            resolve(&file, &config(""))
                .expect_err("directory is not a document")
                .code(),
            "unreadable"
        );
    }

    // ─────────────────────────────── note plumbing ───────────────────────────

    #[test]
    fn frontmatter_notes_are_kept_and_resolution_notes_are_appended() {
        // Each of these declarations is legal YAML but needs a resolution note;
        // they must arrive in hierarchy order, after any phase 3a notes.
        let document = doc(
            "a.md",
            "---\ntype: runbook\nstatus: archived\nscope: org/acme\n---\nbody\n",
        );

        let codes: Vec<&str> = document.notes.iter().map(DocNote::code).collect();
        assert_eq!(codes, ["unknown_type", "unknown_status", "invalid_scope"]);
    }

    #[test]
    fn a_document_with_no_frontmatter_has_no_notes() {
        let document = doc("plain.md", "# nothing declared\n");

        assert!(document.notes.is_empty(), "{:?}", document.notes);
        assert_eq!(document.title, "plain");
        assert_eq!(document.type_name, "note");
        assert_eq!(document.status, Status::Active);
        assert_eq!(document.scope, GLOBAL_SCOPE);
    }

    #[test]
    fn yaml_notes_from_phase_3a_are_carried_through() {
        let document = doc("a.md", "---\ntype: [unclosed\n---\nbody\n");

        assert!(
            document
                .notes
                .iter()
                .any(|note| note.code() == "yaml_unparseable"),
            "{:?}",
            document.notes
        );
        // Fail-open: the document still resolved, from the filename and defaults.
        assert_eq!(document.type_name, "note");
        assert_eq!(document.title, "a");
    }

    #[test]
    fn an_empty_file_still_becomes_a_document() {
        let document = doc("empty.md", "");

        assert_eq!(document.title, "empty");
        assert!(document.body.is_empty());
        assert_eq!(document.date_source, DateSource::Mtime);
    }

    #[test]
    fn every_doc_note_reason_renders_a_message() {
        for note in [
            DocNote::NonUtf8Content,
            DocNote::ChangedDuringRead,
            DocNote::NoClosingFence,
        ] {
            assert!(!note.message().is_empty());
        }
    }

    #[test]
    fn helper_paths_stay_total() {
        // Exhaustive rather than sampled: these helpers must never panic on any
        // input a vault can hand them.
        for rel in [
            "",
            ".md",
            ".",
            "..",
            "a",
            "a/",
            "2024-11-02",
            "2024-11-02--",
            "notes/2024-11-02-ü.md",
        ] {
            let _ = filename_date(rel);
            let _ = filename_stem(rel).unwrap_or_default();
            let _ = resolve_title(None, rel);
        }
        for scope in ["", " ", "-", ":", "org acme", "ORG", "org:ACME"] {
            let _ = is_valid_scope(&normalise_scope(scope));
        }
    }

    #[test]
    fn skip_display_names_the_path_and_the_reason() {
        let skip = ReadSkip {
            rel_path: "notes/a.md".to_owned(),
            note: DocNote::ChangedDuringRead,
        };

        assert!(skip.to_string().starts_with("notes/a.md: "));
        assert_eq!(skip.code(), "changed_during_read");
    }
}
