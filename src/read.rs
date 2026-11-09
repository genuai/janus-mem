//! `read_document`: the one path that returns document text (M6 §3.3, M6-12).
//!
//! The read walks and parses through the same functions `search` uses, so it
//! cannot disagree with the index about which files exist or where the body
//! starts. It loads a generation only when a chunk id asks for a span.
//! `content_hash` comes from the bytes this call read, never from `DocMeta`, so
//! a skipped refresh cannot label old bytes with a new hash. `max_chars` counts
//! characters, never bytes, so a cut cannot split one (M6-10).

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::chunk::dump::resolve_argument;
use crate::config::Config;
use crate::document::{Document, Frontmatter};
use crate::error::JanusError;
use crate::id::{Blake3Hash, ChunkId};
use crate::index::Snapshot;
use crate::vault::{self, WalkOptions, resolve::resolve};

/// One read, as the engine resolved its parameters.
pub struct Options {
    /// The path the caller gave, confined against the corpus root.
    pub path: PathBuf,
    /// The text cap in characters; `None` means `[mcp] max_chars`.
    pub max_chars: Option<usize>,
    /// One chunk's id, when a span was asked for instead of the whole body.
    pub chunk_id: Option<String>,
}

/// The answer to one read: the file, its text, and the facts about them.
/// Fields are private: an adapter renders the object, so none can drift.
#[derive(Debug, Serialize)]
pub struct Report {
    path: String,
    rel_path: String,
    frontmatter: Frontmatter,
    body: String,
    content_hash: String,
    truncated: bool,
    /// First and last byte of the chunk inside the body, as `search` reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    span: Option<(u32, u32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    breadcrumb: Option<Vec<String>>,
}

/// Reads one document and returns its text.
///
/// `generation` is the loaded index, and only a chunk id needs it.
///
/// # Errors
///
/// [`JanusError::PathEscape`] for a path outside the corpus or inside a
/// forbidden directory, [`JanusError::DocumentNotInCorpus`] for a path the walk
/// does not admit, [`JanusError::DocumentChanged`] when the generation does not
/// describe the bytes this call read, and [`JanusError::Usage`] for a chunk id
/// that is not one.
pub fn compute(
    root: &Path,
    config: &Config,
    options: &Options,
    generation: Option<&Snapshot>,
) -> Result<Report, JanusError> {
    let document = open(root, config, &options.path)?;
    let (text, span, breadcrumb) = match &options.chunk_id {
        None => (&document.body[..], None, None),
        Some(id) => {
            let chunk = chunk_text(&document, id, generation)?;
            (chunk.text, Some(chunk.span), Some(chunk.breadcrumb))
        }
    };
    // The configured cap, unless the caller named one. `0` is no limit.
    let (body, truncated) = cut(text, options.max_chars.unwrap_or(config.mcp.max_chars));
    Ok(Report {
        path: document.path.to_string_lossy().into_owned(),
        rel_path: document.rel_path.clone(),
        frontmatter: document.frontmatter,
        body,
        content_hash: document.content_hash.to_hex(),
        truncated,
        span,
        breadcrumb,
    })
}

/// The admitted file at `given`, parsed as one document.
///
/// The walk decides admission, so an ignored, hidden or non-UTF-8 file is "no
/// document" here exactly as it is in every other report.
fn open(root: &Path, config: &Config, given: &Path) -> Result<Document, JanusError> {
    // Confinement compares components, so both sides must be canonical: a
    // corpus reached through a symlinked path would otherwise refuse every
    // file, which on macOS is `/var` against `/private/var`.
    let root = dunce::canonicalize(root).map_err(|source| JanusError::VaultResolve {
        path: root.to_path_buf(),
        source,
    })?;
    let walk = vault::walk(&root, config, WalkOptions::default())?;
    // One rule for every `PATH` argument, so read refuses an escape exactly as
    // `chunks` does, and accepts the same spellings.
    let rels = resolve_argument(&root, given)?;
    // A path the workspace pruned belongs to the corpus that owns the tree, so
    // the answer names that corpus instead of reporting a missing document the
    // caller can see on disk (case 5.3, M6b-26).
    if let Some(owner) = rels.iter().find_map(|rel| config.prune_owner(rel)) {
        return Err(JanusError::Usage {
            message: format!(
                "{} is in corpus {owner:?}, not this one; read it with --corpus {owner}",
                rels.join(" or ")
            ),
        });
    }
    let absent = || JanusError::DocumentNotInCorpus {
        path: given.to_path_buf(),
    };
    let file = rels
        .iter()
        .find_map(|rel| walk.files.iter().find(|file| &file.rel_path == rel))
        .ok_or_else(absent)?;
    resolve(file, config).map_err(|_| absent())
}

/// One chunk's text and the facts that place it in the body.
struct Chunk<'a> {
    text: &'a str,
    span: (u32, u32),
    breadcrumb: Vec<String>,
}

/// The span text of one chunk of `document`, from the current generation.
///
/// The span describes the bytes the generation saw. When the body moved it
/// describes nothing this call can return, so there is no answer.
fn chunk_text<'a>(
    document: &'a Document,
    id: &str,
    generation: Option<&Snapshot>,
) -> Result<Chunk<'a>, JanusError> {
    let chunk_id: ChunkId = id.parse().map_err(|error| JanusError::Usage {
        message: format!("chunk_id {id:?}: {error}"),
    })?;
    let stale = || JanusError::DocumentChanged {
        path: PathBuf::from(&document.rel_path),
    };
    let snapshot = generation.ok_or_else(stale)?;
    let row = snapshot
        .docs
        .iter()
        .find(|row| snapshot.strings.get(row.rel_path) == Some(document.rel_path.as_str()))
        .ok_or_else(stale)?;
    if row.body_hash != Blake3Hash::of_bytes(document.body.as_bytes()) {
        return Err(stale());
    }
    let chunk = snapshot
        .chunks
        .get(row.chunk_range.0 as usize..row.chunk_range.1 as usize)
        .unwrap_or_default()
        .iter()
        .find(|chunk| chunk.id == chunk_id)
        .ok_or_else(stale)?;
    let text = document
        .body
        .get(chunk.span.0 as usize..chunk.span.1 as usize)
        .ok_or_else(stale)?;
    let breadcrumb = chunk
        .heading_path
        .iter()
        .filter_map(|id| snapshot.strings.get(*id))
        .map(str::to_owned)
        .collect();
    Ok(Chunk {
        text,
        span: chunk.span,
        breadcrumb,
    })
}

/// Cuts `text` to `limit` characters, and says whether it cut.
///
/// `0` is no limit. A character outside ASCII is one character and several
/// bytes, so the cut comes from `char_indices`, never from `len`.
fn cut(text: &str, limit: usize) -> (String, bool) {
    if limit == 0 {
        return (text.to_owned(), false);
    }
    match text.char_indices().nth(limit) {
        Some((byte, _)) => (text[..byte].to_owned(), true),
        None => (text.to_owned(), false),
    }
}

/// Writes the human form of one answer: the text, and a note when it was cut.
///
/// The value is the object the protocol carries, so the two forms cannot
/// disagree. A closed pipe is not a read failure, so the caller may ignore it.
pub fn render(value: &Value, out: &mut dyn Write) -> io::Result<()> {
    let body = value["body"].as_str().unwrap_or_default();
    write!(out, "{body}")?;
    if value["truncated"].as_bool().unwrap_or_default() {
        writeln!(out)?;
        writeln!(
            out,
            "[{} characters; the max_chars cap]",
            body.chars().count()
        )?;
    }
    Ok(())
}
