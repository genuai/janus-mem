//! `janus_status`: is this corpus fresh, and what can this process do?
//!
//! The answer comes from the manifest, one walk, and the metadata the resolved
//! documents carry. A status call never builds and never takes `.janus/lock`
//! (M6-6), so an agent can ask "is this fresh?" while a writer holds the index.
//!
//! Two embedding facts answer two questions and may disagree (§2.6):
//! `available` is this process, from the embedder it resolved; `model` is the
//! generation, from its manifest.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::config::{self, Config};
use crate::engine::CacheStats;
use crate::error::JanusError;
use crate::index::refresh::Signature;
use crate::index::store::{self, Manifest};
use crate::metadata::Metadata;
use crate::vault::resolve;
use crate::vault::{self, WalkOptions};
use crate::workspace::{self, Workspace};

/// The answer to one status call (§2.6).
///
/// Fields are private: an adapter renders the object, so none can drift.
#[derive(Debug, Serialize)]
pub struct Report {
    vault: String,
    corpus_id: String,
    writes_enabled: bool,
    embeddings: Embeddings,
    index: Index,
    metadata: MetadataBlock,
    cache: Cache,
    /// The workspace file that named these corpora (M6b §3.3). Absent in
    /// single-corpus mode, so that answer keeps its shape byte for byte.
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<String>,
    /// Every corpus the workspace names, with its role and freshness. Absent in
    /// single-corpus mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    corpora: Option<Vec<workspace::Row>>,
}

impl Report {
    /// Adds the workspace's corpora to this answer (case 3.3).
    ///
    /// The rows are the ones `janus corpora` prints, read the same way: one
    /// manifest per corpus and one walk only for the digest. Nothing is built
    /// and no lock is taken, so a status call still runs beside a writer.
    #[must_use]
    pub fn in_workspace(mut self, found: &Workspace) -> Self {
        self.workspace = Some(found.path.display().to_string());
        self.corpora = Some(workspace::rows(found));
        self
    }
}

/// What this process can embed, and what produced the stored vectors.
#[derive(Debug, Serialize)]
struct Embeddings {
    available: bool,
    model: Option<String>,
    degraded: Option<String>,
}

/// The generation on disk, or the fact that there is none.
///
/// Every field below `present` is `null` before the first build (case 9.1).
#[derive(Debug, Serialize)]
struct Index {
    present: bool,
    stale: Option<bool>,
    docs: Option<usize>,
    chunks: Option<usize>,
    built_at: Option<String>,
    format_version: Option<u16>,
    index_sha: Option<String>,
}

impl Index {
    /// No build has run.
    fn absent() -> Self {
        Self {
            present: false,
            stale: None,
            docs: None,
            chunks: None,
            built_at: None,
            format_version: None,
            index_sha: None,
        }
    }

    /// The facts one manifest states, and whether this walk still matches it.
    fn of(manifest: &Manifest, digest: &str) -> Self {
        Self {
            present: true,
            stale: Some(manifest.corpus_sig.as_deref() != Some(digest)),
            docs: Some(manifest.docs),
            chunks: Some(manifest.chunks),
            built_at: Some(manifest.built_at.clone()),
            format_version: Some(manifest.format_version),
            index_sha: Some(manifest.index_sha.clone()),
        }
    }
}

/// How many documents carry a date, so an agent can ask "is this corpus
/// metadata-poor?" without a second command.
///
/// The share's denominator is `dated + undated`, the documents this walk
/// resolved. Not `index.docs`: the manifest is null before the first build, and
/// a stale manifest describes an older corpus. The question is about the corpus
/// as it is now.
#[derive(Debug, Serialize)]
struct MetadataBlock {
    /// Documents whose date resolved to any source.
    dated: usize,
    /// Documents whose date resolved to nothing.
    undated: usize,
    /// `undated / (dated + undated)`, rounded to whole percent and divided by
    /// one hundred, so it equals the percentage `scan` prints.
    share_undated: f64,
}

/// The one-load cache counters of this process (§2.6).
#[derive(Debug, Serialize)]
struct Cache {
    hits: u64,
    misses: u64,
}

/// Reads the manifest and walks the corpus. Builds nothing and locks nothing.
///
/// `available` and `degraded` are the embedder this process resolved, and they
/// arrive as arguments because nothing below `main` may read the environment.
///
/// # Errors
///
/// A walk or IO failure, and [`JanusError::IndexUnstable`] for a manifest that
/// exists but does not parse.
pub fn compute(
    root: &Path,
    config: &Config,
    available: bool,
    degraded: Option<String>,
    cache: CacheStats,
) -> Result<Report, JanusError> {
    let dir = config::resolve_index_dir(root, &config.index_dir);
    let manifest = manifest(&dir.join(store::MANIFEST_FILE))?;
    // Freshness is the same one walk and one digest a read makes.
    let walk = vault::walk(root, config, WalkOptions::default())?;
    let digest = Signature::of(&walk.files).digest();
    // The metadata question needs the frontmatter, so the files this walk
    // already found are resolved. Not a second walk: the same file list is read
    // once, no generation is written, and no lock is taken (R1-14).
    let (documents, _) = resolve::resolve_all(&walk.files, config);
    let metadata = Metadata::of(&documents, config);
    Ok(Report {
        vault: root.display().to_string(),
        corpus_id: config.corpus_id.clone(),
        // M6 serves no write, whatever `[mcp] allow_write` says. Phase 5 refuses
        // a write tool and M7 adds the writer (§3.8).
        writes_enabled: false,
        embeddings: Embeddings {
            available,
            // The three vector fields of a manifest are one fact, so an
            // `embedder` names the rows the generation holds.
            model: manifest
                .as_ref()
                .and_then(|manifest| manifest.embedder.as_ref())
                .map(|id| id.name.clone()),
            degraded,
        },
        index: match &manifest {
            Some(manifest) => Index::of(manifest, &digest),
            None => Index::absent(),
        },
        metadata: MetadataBlock {
            dated: metadata.dated,
            undated: metadata.undated,
            share_undated: metadata.share_undated(),
        },
        cache: Cache {
            hits: cache.hits,
            misses: cache.misses,
        },
        workspace: None,
        corpora: None,
    })
}

/// The manifest, or `None` when no build has run.
///
/// A manifest that exists but does not parse is a broken index, not an absent
/// one, so it names the file instead of pretending the build never ran.
fn manifest(path: &Path) -> Result<Option<Manifest>, JanusError> {
    match fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| JanusError::IndexUnstable {
                    path: path.to_path_buf(),
                })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(JanusError::Io(error)),
    }
}

/// Writes the human form of one status answer.
///
/// The value is the object the protocol carries, so the two forms cannot
/// disagree. A closed pipe is not a read failure, so the caller may ignore it.
pub fn render(value: &Value, out: &mut dyn Write) -> io::Result<()> {
    let (index, embeddings) = (&value["index"], &value["embeddings"]);
    writeln!(out, "vault       {}", text(&value["vault"]))?;
    writeln!(out, "corpus      {}", text(&value["corpus_id"]))?;
    let writes = if value["writes_enabled"].as_bool() == Some(true) {
        "enabled"
    } else {
        "disabled"
    };
    writeln!(out, "writes      {writes}")?;
    writeln!(out, "embeddings  {}", embedding_line(embeddings))?;
    writeln!(out, "index       {}", index_line(index))?;
    let (hits, misses) = (&value["cache"]["hits"], &value["cache"]["misses"]);
    writeln!(out, "cache       {hits} hits, {misses} misses")?;
    // Only a workspace answer has corpora to list (case 3.3).
    if let Some(found) = value["workspace"].as_str() {
        writeln!(out, "workspace   {found}")?;
        for row in value["corpora"].as_array().into_iter().flatten() {
            writeln!(
                out,
                "  {}  {}  read-only: {}  index: {}",
                text(&row["id"]),
                text(&row["role"]),
                if row["read_only"].as_bool() == Some(true) {
                    "yes"
                } else {
                    "no"
                },
                workspace::index_state(
                    row["index_present"].as_bool() == Some(true),
                    row["index_stale"].as_bool(),
                ),
            )?;
        }
    }
    Ok(())
}

/// One optional string from an answer, or a dash.
fn text(value: &Value) -> String {
    value.as_str().unwrap_or("-").to_owned()
}

/// The one line that says whether this process can embed, and with what.
fn embedding_line(embeddings: &Value) -> String {
    if embeddings["available"].as_bool() == Some(true) {
        return format!("available: {}", text(&embeddings["model"]));
    }
    format!("unavailable: {}", text(&embeddings["degraded"]))
}

/// The one line that says whether a generation exists, and whether it is fresh.
fn index_line(index: &Value) -> String {
    if index["present"].as_bool() != Some(true) {
        return "absent; run janus index".to_owned();
    }
    let state = if index["stale"].as_bool() == Some(true) {
        "stale"
    } else {
        "fresh"
    };
    format!(
        "{state}: {} documents, {} chunks, format {}, sha {}",
        index["docs"],
        index["chunks"],
        index["format_version"],
        text(&index["index_sha"]),
    )
}
