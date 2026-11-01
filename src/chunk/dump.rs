//! The `janus chunks` command (SPEC §6.5, §8.5, §9.1, §9.4).
//!
//! The chunker's only visible surface in M2 and the seam M3 builds its index
//! from. It runs exactly the pipeline [`crate::scan`] runs — discover, resolve,
//! derive identity — then chunks every admitted body, so the two commands cannot
//! disagree about what a document is. Nothing is written (§8.3).
//!
//! Chunk *text* is not in the report by default: text is content, and content
//! needs an explicit flag (§8.5). `--text` is that flag. Everything else is
//! metadata, and chunk ids are content-addressed, so the report is byte-identical
//! between runs and machines. Two renderings, one report: a document header with
//! one row per chunk, or one object on stdout versioned with `"api": 1`.

use std::collections::BTreeSet;
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::config::Config;
use crate::document::{DocNote, Document, NoteDto};
use crate::error::JanusError;
use crate::vault::resolve::{self, ReadSkip};
use crate::vault::{self, WalkOptions, identity};

use super::split::{Chunk, chunk_body};

// One contract version for every `--json` response (AGENTS.md versioning table),
// not one per command: a consumer that pins it needs a single number, and two
// constants in two modules would eventually disagree.
use crate::scan::API_VERSION;

/// Characters of a chunk's text shown by `--text`.
const PREVIEW_CHARS: usize = 120;

/// How `janus chunks` should be rendered.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Emit one JSON object on stdout instead of the human output.
    pub json: bool,
    /// Include a truncated preview of each chunk's text (§8.5: opt-in).
    pub text: bool,
    /// Cap the chunks shown per document; `None` means no cap.
    pub limit: Option<usize>,
    /// Paths to restrict the report to; empty means every document.
    pub paths: Vec<PathBuf>,
}

/// Runs `janus chunks`.
///
/// Results go to stdout, diagnostics to stderr, so `janus chunks --json | jq`
/// always parses (§8.2).
///
/// # Errors
///
/// Propagates corpus, config, walk and path failures. A file that cannot be read
/// is *not* an error: it is reported as a read skip and the report continues.
pub fn run(root: &Path, config: &Config, options: Options) -> Result<(), JanusError> {
    let walk = vault::walk(root, config, WalkOptions::default())?;
    let (mut documents, read_skips) = resolve::resolve_all(&walk.files, config);
    // Identity is a corpus-wide decision, so it runs before any selection: a
    // chunk id must not depend on which paths the caller happened to name (D24).
    identity::derive_identity(&mut documents, config);

    let selected = select(root, &documents, &options.paths)?;
    let chunked: Vec<ChunkedDoc<'_>> = selected
        .iter()
        .map(|index| ChunkedDoc::new(&documents[*index], config, options.limit))
        .collect();

    for warning in &config.warnings {
        eprintln!("warning: {warning}");
    }

    let mut out = io::stdout().lock();
    if options.json {
        let report = ChunksReport::new(root, config, &chunked, &read_skips, options.text);
        let json = serde_json::to_string_pretty(&report).map_err(serialize_error)?;
        // A closed pipe is not a failure (`janus chunks --json | head`).
        let _ = writeln!(out, "{json}");
    } else {
        write_human(&mut out, root, config, &chunked, &read_skips, &options);
    }

    Ok(())
}

/// Wraps a serialization failure as an IO error.
///
/// Serializing this report cannot fail — no non-string map keys, no non-finite
/// floats — but `unwrap` is not allowed in library code and a `JanusError`
/// variant for an impossible case would be public API for nothing.
fn serialize_error(err: serde_json::Error) -> JanusError {
    JanusError::Io(io::Error::new(io::ErrorKind::InvalidData, err))
}

// ─────────────────────────────── selection ─────────────────────────────────

/// Resolves the `PATH` arguments to indices into `documents`, in `rel_path` order;
/// empty selects everything. Every path is confined *before* anything is matched,
/// so an escape is refused (exit 4) whatever the argument order and a
/// legal-but-absent path is a named error (exit 1), never a silently empty report.
fn select(
    root: &Path,
    documents: &[Document],
    paths: &[PathBuf],
) -> Result<Vec<usize>, JanusError> {
    if paths.is_empty() {
        return Ok((0..documents.len()).collect());
    }

    let mut selected = BTreeSet::new();

    for given in paths {
        // Every spelling is confined first, so an escape is refused whatever the
        // argument order and whichever reading of a relative path matches.
        let rels = resolve_argument(root, given)?;

        let mut found = false;
        for (index, document) in documents.iter().enumerate() {
            if rels.contains(&document.rel_path) {
                selected.insert(index);
                found = true;
            }
        }
        if !found {
            return Err(JanusError::DocumentNotInCorpus {
                path: given.clone(),
            });
        }
    }

    Ok(selected.into_iter().collect())
}

/// Every `rel_path` a `PATH` argument can legitimately name, or the error that
/// refused it.
///
/// A relative path is read **twice**, because both readings are things a caller
/// means: against the working directory (the conventional reading, and what
/// `vault::confine` resolves) and against the corpus root (a `rel_path` copied out
/// of a report must work from anywhere). Both are confined, so neither is a way
/// out. Each spelling contributes two candidates: its own (how a symlinked
/// document is identified) and the resolved target's (`a/../b.md`).
fn resolve_argument(root: &Path, given: &Path) -> Result<Vec<String>, JanusError> {
    let cwd = std::env::current_dir()?;
    let spellings: Vec<PathBuf> = if given.is_absolute() {
        vec![given.to_path_buf()]
    } else {
        vec![cwd.join(given), root.join(given)]
    };

    let mut rels: Vec<String> = Vec::new();
    let mut escape = None;
    for spelling in spellings {
        match vault::confine(root, &spelling) {
            Ok(resolved) => {
                for path in [&spelling, &resolved] {
                    if let Some(rel) = rel_path(root, path)
                        && !rels.contains(&rel)
                    {
                        rels.push(rel);
                    }
                }
            }
            // Kept, not returned: the other reading may still be the intended
            // one. Reported only if no reading is usable.
            Err(err) => {
                if matches!(err, JanusError::PathEscape { .. }) {
                    escape.get_or_insert(err);
                }
            }
        }
    }

    if rels.is_empty() {
        // An escape outranks a typo when both readings failed: "no document at
        // this path" for a file just outside the vault sends the caller hunting
        // for a typo that is not there.
        return Err(escape.unwrap_or(JanusError::DocumentNotInCorpus {
            path: given.to_path_buf(),
        }));
    }

    Ok(rels)
}

/// The corpus-relative, `/`-separated form of an absolute path.
///
/// `None` when the path is outside `root` or has a component with no `rel_path`
/// form (`.`, `..`, a prefix, a non-UTF-8 name): an unnormalised path reports "no
/// document" rather than guessing which one was meant.
fn rel_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(name) => parts.push(name.to_str()?),
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// One selected document plus the chunks it produced.
///
/// The intermediate both renderers consume, so chunking runs once per document
/// whatever is printed, and resolve notes travel with chunk notes: "this document
/// has an indivisible fence" is a property of the document (CH-13).
struct ChunkedDoc<'a> {
    document: &'a Document,
    chunks: Vec<Chunk>,
    notes: Vec<DocNote>,
    /// True when `--limit` hid chunks that exist.
    truncated: bool,
}

impl<'a> ChunkedDoc<'a> {
    fn new(document: &'a Document, config: &Config, limit: Option<usize>) -> Self {
        let (mut chunks, chunk_notes) = chunk_body(document.id, &document.body, &config.chunking);

        let truncated = limit.is_some_and(|limit| chunks.len() > limit);
        if let Some(limit) = limit {
            chunks.truncate(limit);
        }

        let mut notes = document.notes.clone();
        notes.extend(chunk_notes);

        Self {
            document,
            chunks,
            notes,
            truncated,
        }
    }
}

// ── JSON ────────────────────────────────────────────────────────────────────

/// The `--json` report: one stable, versioned object.
///
/// No timestamps anywhere, which is why it needs no redactor: ids are
/// content-addressed, spans are body-relative and order is `rel_path` order.
#[derive(Debug, Serialize)]
struct ChunksReport<'a> {
    /// Contract version; see [`API_VERSION`].
    api: u32,
    /// Corpus root, absolute.
    vault: String,
    /// Declared corpus identity (§6.11).
    corpus_id: &'a str,
    /// The parameters the chunker ran with, as configured.
    chunking: ChunkingDto,
    /// Every selected document, in `rel_path` order.
    docs: Vec<ChunkedDocDto<'a>>,
    /// Chunks actually emitted, i.e. the sum of `docs[*].chunks`: with `--limit`
    /// the number reported, not the number that exists — `truncated` says which.
    total_chunks: usize,
    /// Files that were admitted but could not become documents. They are never
    /// chunked, so listing them is how a caller learns *why* a path is missing.
    read_skips: Vec<ReadSkipDto<'a>>,
}

/// The chunking parameters as configured (§6.5).
///
/// Declared values, not clamped ones (C-10): the splitter clamps at the point of
/// use (CH-8) and a clamped run says so through a note on the document.
#[derive(Debug, Serialize)]
struct ChunkingDto {
    max_chars: usize,
    overlap: f64,
}

impl<'a> ChunksReport<'a> {
    fn new(
        root: &Path,
        config: &'a Config,
        docs: &[ChunkedDoc<'a>],
        read_skips: &'a [ReadSkip],
        text: bool,
    ) -> Self {
        Self {
            api: API_VERSION,
            vault: root.display().to_string(),
            corpus_id: &config.corpus_id,
            chunking: ChunkingDto {
                max_chars: config.chunking.max_chars,
                overlap: config.chunking.overlap,
            },
            docs: docs
                .iter()
                .map(|doc| ChunkedDocDto::new(doc, text))
                .collect(),
            total_chunks: docs.iter().map(|doc| doc.chunks.len()).sum(),
            read_skips: read_skips
                .iter()
                .map(|skip| ReadSkipDto {
                    rel_path: &skip.rel_path,
                    reason: skip.code(),
                    message: skip.message(),
                })
                .collect(),
        }
    }
}

/// One document's chunks and notes. No body, no frontmatter (§8.5).
#[derive(Debug, Serialize)]
struct ChunkedDocDto<'a> {
    /// Resolved `DocId` (§6.4).
    id: String,
    /// Vault-relative, `/`-separated.
    rel_path: &'a str,
    title: &'a str,
    /// Chunks in source order, at most `--limit` of them.
    chunks: Vec<ChunkDto>,
    /// Present only when `--limit` hid chunks.
    #[serde(skip_serializing_if = "is_false")]
    truncated: bool,
    /// Resolve and chunk notes, in that order.
    notes: Vec<NoteDto>,
}

impl<'a> ChunkedDocDto<'a> {
    fn new(doc: &ChunkedDoc<'a>, text: bool) -> Self {
        let body = &doc.document.body;
        Self {
            id: doc.document.id.to_hex(),
            rel_path: &doc.document.rel_path,
            title: &doc.document.title,
            chunks: doc
                .chunks
                .iter()
                .map(|chunk| ChunkDto::new(chunk, body, text))
                .collect(),
            truncated: doc.truncated,
            notes: doc.notes.iter().map(DocNote::dto).collect(),
        }
    }
}

/// One chunk: span and breadcrumb, never text unless `--text` asked for it.
#[derive(Debug, Serialize)]
struct ChunkDto {
    /// Content-addressed chunk id (§6.4, D24).
    id: String,
    /// Display order within the document; never identity (§6.5).
    ordinal: u16,
    /// First byte of the chunk within the document body.
    start: u32,
    /// One past the last byte of the chunk.
    end: u32,
    /// Length in characters, counted as the limit counts (CRLF's `\r` excluded).
    chars: usize,
    /// Ancestor headings, outermost first, excluding the title (§6.5 rule 4).
    heading_path: Vec<String>,
    /// A collapsed, truncated preview; present only with `--text` (§8.5).
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

impl ChunkDto {
    fn new(chunk: &Chunk, body: &str, text: bool) -> Self {
        Self {
            id: chunk.id.to_hex(),
            ordinal: chunk.ordinal,
            start: chunk.span.0,
            end: chunk.span.1,
            chars: chunk.char_count(body),
            heading_path: chunk.heading_path.clone(),
            text: text.then(|| preview(chunk.text(body))),
        }
    }
}

/// One file that was admitted but could not become a document.
#[derive(Debug, Serialize)]
struct ReadSkipDto<'a> {
    rel_path: &'a str,
    reason: &'static str,
    message: String,
}

/// Omit `"truncated": false` from an untruncated report.
fn is_false(value: &bool) -> bool {
    !*value
}

// ── human ───────────────────────────────────────────────────────────────────

/// Writes the header, one block per document, and a summary line.
fn write_human(
    out: &mut impl io::Write,
    root: &Path,
    config: &Config,
    docs: &[ChunkedDoc<'_>],
    read_skips: &[ReadSkip],
    options: &Options,
) {
    // A write error here means the consumer closed the pipe, not a failure worth
    // a non-zero exit.
    let _ = writeln!(out, "corpus:  {}", root.display());
    let _ = writeln!(
        out,
        "chunking: max_chars = {}, overlap = {}",
        config.chunking.max_chars, config.chunking.overlap
    );
    let _ = writeln!(out);

    for doc in docs {
        let suffix = if doc.truncated { ", truncated" } else { "" };
        let _ = writeln!(
            out,
            "{}  ({} chunk(s){suffix})",
            doc.document.rel_path,
            doc.chunks.len()
        );
        for chunk in &doc.chunks {
            let body = &doc.document.body;
            let hex = chunk.id.to_hex();
            let breadcrumb = chunk.breadcrumb();
            let _ = writeln!(
                out,
                "{:>4}  {}  {:>6} bytes {:>6} chars  {}",
                chunk.ordinal,
                hex.get(..8).unwrap_or(&hex),
                chunk.text(body).len(),
                chunk.char_count(body),
                if breadcrumb.is_empty() {
                    "-"
                } else {
                    &breadcrumb
                }
            );
            if options.text {
                let _ = writeln!(out, "      │ {}", preview(chunk.text(body)));
            }
        }
        for note in &doc.notes {
            let _ = writeln!(out, "      ! {}: {}", note.code(), note.message());
        }
        let _ = writeln!(out);
    }

    let total: usize = docs.iter().map(|doc| doc.chunks.len()).sum();
    let _ = writeln!(out, "{total} chunk(s) in {} document(s)", docs.len());
    for skip in read_skips {
        let _ = writeln!(out, "unreadable: {} ({})", skip.rel_path, skip.code());
    }
}

/// A one-line preview of chunk text: whitespace collapsed, then capped.
///
/// `--text` is a debugging aid, not a way to read a document. Collapsing keeps one
/// chunk on one terminal row, and the cap is marked with `…`: a 3 000-character
/// fence cannot flood a shell, and a cut is never mistaken for the whole chunk.
fn preview(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= PREVIEW_CHARS {
        return collapsed;
    }
    let mut out: String = collapsed.chars().take(PREVIEW_CHARS).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_collapses_whitespace_and_keeps_short_text_whole() {
        assert_eq!(
            preview("## Decision\n\nUse\r\nPostgres."),
            "## Decision Use Postgres."
        );
        assert_eq!(preview("   "), "");
    }

    #[test]
    fn preview_marks_a_truncated_text_with_an_ellipsis() {
        let long = "a".repeat(PREVIEW_CHARS + 50);
        let rendered = preview(&long);

        assert_eq!(rendered.chars().count(), PREVIEW_CHARS + 1);
        assert!(rendered.ends_with('…'), "{rendered}");
        assert!(rendered.starts_with('a'));
    }

    #[test]
    fn preview_counts_characters_not_bytes() {
        let cjk = "文".repeat(PREVIEW_CHARS + 1);
        let rendered = preview(&cjk);

        // 121 CJK characters are 363 bytes: a byte-based cap would cut at 40.
        assert_eq!(rendered.chars().count(), PREVIEW_CHARS + 1);
        assert!(rendered.ends_with('…'));
    }

    #[test]
    fn rel_path_is_slash_separated_and_rejects_unrepresentable_components() {
        let root = Path::new("/vault");
        assert_eq!(
            rel_path(root, Path::new("/vault/notes/a.md")),
            Some("notes/a.md".to_owned())
        );
        assert_eq!(rel_path(root, Path::new("/vault")), None, "the root itself");
        assert_eq!(rel_path(root, Path::new("/vault-evil/a.md")), None);
        assert_eq!(rel_path(root, Path::new("/vault/../a.md")), None);
    }

    #[test]
    fn select_without_paths_keeps_every_document_in_order() {
        assert_eq!(
            select(Path::new("/vault"), &[], &[]).unwrap(),
            Vec::<usize>::new(),
            "no documents, no paths, no error"
        );
    }

    #[test]
    fn a_relative_path_falls_back_to_the_corpus_root_reading() {
        // The working-directory reading cannot be asserted here: `current_dir` is
        // process-wide and other tests run in parallel. What is asserted is the
        // fallback, which is the reading that makes a `rel_path` copied out of a
        // report work from anywhere.
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(dir.path()).expect("canonical");
        std::fs::create_dir_all(root.join("notes")).expect("directory");
        std::fs::write(root.join("notes/a.md"), "# A\n").expect("file");

        let rels = resolve_argument(&root, Path::new("notes/a.md")).expect("must resolve");
        assert!(rels.contains(&"notes/a.md".to_owned()), "{rels:?}");
    }

    #[test]
    fn a_missing_path_is_reported_as_the_path_the_caller_typed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(dir.path()).expect("canonical");

        let err = resolve_argument(&root, Path::new("notes/missing.md")).expect_err("must fail");
        assert!(
            matches!(&err, JanusError::DocumentNotInCorpus { path } if path == Path::new("notes/missing.md")),
            "{err:?}"
        );
        assert_eq!(err.exit_code(), 1);
    }

    #[test]
    fn a_path_outside_the_corpus_is_a_refusal_that_outranks_a_typo() {
        let outside = tempfile::tempdir().expect("tempdir");
        let inside = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(inside.path()).expect("canonical");
        let file = outside.path().join("secret.md");
        std::fs::write(&file, "# secret\n").expect("file");
        let absolute = dunce::canonicalize(&file).expect("canonical");

        let err = resolve_argument(&root, &absolute).expect_err("must fail");
        assert!(matches!(err, JanusError::PathEscape { .. }), "{err:?}");
        assert_eq!(err.exit_code(), 4);
    }

    #[test]
    fn a_false_truncated_flag_is_omitted_from_json() {
        let dto = ChunkedDocDto {
            id: "00".to_owned(),
            rel_path: "a.md",
            title: "A",
            chunks: Vec::new(),
            truncated: false,
            notes: Vec::new(),
        };
        let json = serde_json::to_string(&dto).expect("serialize");

        assert!(!json.contains("truncated"), "{json}");
    }

    #[test]
    fn a_chunk_without_text_has_no_text_key() {
        let chunk = Chunk {
            id: crate::id::ChunkId::from_bytes([7; 32]),
            doc_id: crate::id::DocId::from_bytes([1; crate::id::DOC_ID_LEN]),
            heading_path: vec!["Decision".to_owned()],
            span: (11, 28),
            ordinal: 0,
        };
        let body = "## Secret\n\nCONFIDENTIAL BODY";

        let hidden = serde_json::to_string(&ChunkDto::new(&chunk, body, false)).expect("serialize");
        let shown = serde_json::to_string(&ChunkDto::new(&chunk, body, true)).expect("serialize");

        assert!(!hidden.contains("CONFIDENTIAL"), "{hidden}");
        assert!(!hidden.contains("Secret"), "{hidden}");
        assert!(!hidden.contains("\"text\""), "{hidden}");
        // The span is body-relative, so `--text` shows exactly `body[11..28]`.
        assert!(shown.contains("\"text\":\"CONFIDENTIAL BODY\""), "{shown}");
    }
}
