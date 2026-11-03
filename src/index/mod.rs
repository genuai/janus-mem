//! The in-memory lexical index (M3 phase 2, SPEC §7.2).
//!
//! The index is a flat inverted index: terms are interned once, every term owns
//! a contiguous range of [`Posting`]s, and a chunk is identified by its position
//! in the order given to [`LexicalIndex::build`]. The shape is deliberately the
//! shape phase 4 persists in `.janus/index.bin`, so persistence becomes a
//! derive over these fields rather than a second layout.
//!
//! Term ids are assigned in first-seen order while chunks are scanned in index
//! order, and postings are emitted grouped by term id and ascending by chunk.
//! That makes the whole value deterministic without relying on hash iteration:
//! [`StringPool`] uses a `BTreeMap` for lookup, and nothing iterates it to build
//! output.
//!
//! This module contains no I/O. The phase-4 snapshot in [`store`] persists
//! exactly these values, and [`build`] rebuilds them incrementally. The M4
//! embedding seam lives in [`embed`], and the separate flat payload format it
//! writes lives in [`vectors`].
//!
//! [`LexicalIndex`]: crate::index::LexicalIndex
//! [`LexicalIndex::build`]: crate::index::LexicalIndex::build

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::id::{Blake3Hash, ChunkId, DocId};

pub mod build;
pub mod cmd;
pub mod embed;
mod lexical;
pub mod store;
pub mod vectors;

pub use lexical::{LexicalIndex, squash};

/// The on-disk snapshot layout; a mismatch is a rebuild, never an error (S-6, §8.3).
///
/// Version 2 (M4 V-1) adds `DocMeta::body_hash` to the snapshot and the
/// `vectors.f32` payload with its three manifest fields. Version 3 (M4B B-7)
/// adds `DocMeta::date_source`. An older index is refused by `Expected::check`
/// and rebuilt with one warning, never misread.
pub const FORMAT_VERSION: u16 = 3;

/// The compact persisted status (§3.2); the resolver's `Status` is reused.
pub use crate::document::Status as StatusCode;

/// The chunker parameters that participate in the reuse key (§3.4, S-9).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChunkingFingerprint {
    /// The configured soft chunk ceiling in characters.
    pub max_chars: usize,
    /// The configured overlap fraction.
    pub overlap: f64,
}

/// A rebuildable snapshot of the lexical index (`.janus/index.bin`, §2).
///
/// Term text lives in `strings` at the term's index in `terms`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The layout version, checked before any offset is trusted.
    pub format_version: u16,
    /// The declared corpus identity (§6.11). Never path-derived.
    pub corpus_id: String,
    /// The tokenizer that produced the postings.
    pub tokenizer: crate::tokenize::TokenizerFingerprint,
    /// Interned vocabulary and metadata strings.
    pub strings: StringPool,
    /// One row per document, in snapshot order.
    pub docs: Vec<DocMeta>,
    /// One row per chunk occurrence, in snapshot order.
    pub chunks: Vec<ChunkMeta>,
    /// Per-term document frequency and posting range.
    pub terms: Vec<TermMeta>,
    /// Flat postings, grouped by term id and ascending by chunk.
    pub postings: Vec<Posting>,
    /// Mean chunk length in tokens.
    pub avgdl: f64,
}

/// Persisted document metadata. Metadata is always rebuilt, never reused (§3.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocMeta {
    /// Stable document identity (§6.4).
    pub id: DocId,
    /// Vault-relative path, `/`-separated.
    pub rel_path: StringId,
    /// Effective title, including the frontmatter or filename fallback.
    pub title: StringId,
    /// Canonical type name.
    pub type_name: StringId,
    /// Declared status.
    pub status: StatusCode,
    /// Normalised owning scope.
    pub scope: StringId,
    /// Retrieval tags from `shared_with`.
    pub shared_with: Vec<StringId>,
    /// Frontmatter tags, preserved for `--tags` after reload.
    pub tags: Vec<StringId>,
    /// Authored or name-derived date.
    pub date: Option<NaiveDate>,
    /// Where [`DocMeta::date`] came from, so a result can explain itself (§3.5).
    pub date_source: crate::document::DateSource,
    /// Valid-time start (§6.10).
    pub effective_from: Option<NaiveDate>,
    /// Whether the type (or the document) is exempt from decay.
    pub timeless: bool,
    /// Hash of the full file bytes.
    pub content_hash: Blake3Hash,
    /// blake3 of `Document.body` **raw**, never a normalised body (V-26).
    ///
    /// The reuse witness both caches key on: a frontmatter edit that leaves the
    /// body alone must not re-embed or re-tokenize, and normalising the body
    /// here would fold line endings and reopen the hole V-23 closes.
    /// `content_hash` stays as the whole-file fact M5's move detection wants.
    pub body_hash: Blake3Hash,
    /// Identity of the indexed input (§3.4, S-9).
    pub lex_key: [u8; 32],
    /// Half-open range of this document's chunk occurrences.
    pub chunk_range: (u32, u32),
}

/// Persisted chunk occurrence metadata (§2, §3.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkMeta {
    /// Content identity. Never a postings key; two documents may share it.
    pub id: ChunkId,
    /// Index into [`Snapshot::docs`].
    pub doc: u32,
    /// Byte span into the document body.
    pub span: (u32, u32),
    /// Ancestor headings, outermost first.
    pub heading_path: Vec<StringId>,
    /// Token count, BM25's `dl`.
    pub tokens: u32,
}

/// A stable, compact identity for a string in a [`StringPool`].
///
/// Interned ids are assigned in insertion order, so they are deterministic for
/// a deterministic build. The `u32` form is the one the snapshot stores; it
/// caps a pool at [`u32::MAX`] distinct strings, far beyond any real corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StringId(u32);

impl StringId {
    /// The id as the `u32` written to the snapshot.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// The id as an index into a `Vec` that mirrors the pool's insertion order.
    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// The minimal string interner for phase 2 (the plan's §2 `StringPool`).
///
/// It supports exactly three operations — intern, lookup, and length — because
/// that is all an in-memory build and query need. Serialization, reserved ids,
/// and reload belong to phase 4.
///
/// The pool keeps each string twice: once in insertion order for
/// [`StringPool::get`], once as a `BTreeMap` key for [`StringPool::lookup`].
/// That is deliberate for a cache-sized vocabulary; a single-allocation design
/// would need unsafe aliasing or a second index and is not worth it here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StringPool {
    /// Strings in id order; `strings[id]` is the text of `StringId(id)`.
    strings: Vec<String>,
    /// Reverse lookup. A `BTreeMap`, not a `HashMap`, so no code path can make
    /// the build depend on hash iteration order.
    ids: BTreeMap<String, StringId>,
}

impl Serialize for StringPool {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.strings.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for StringPool {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let strings = Vec::<String>::deserialize(deserializer)?;
        Ok(Self::from_strings(strings))
    }
}

impl StringPool {
    /// Creates an empty pool.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the id of `text`, inserting it at the end if it is new.
    ///
    /// Ids are dense and start at zero; a pool with `len()` strings has ids
    /// `0..len()`. At the (unreachable) [`u32::MAX`] ceiling the string is not
    /// inserted, so the function stays panic-free instead of wrapping ids.
    pub fn intern(&mut self, text: &str) -> StringId {
        if let Some(id) = self.ids.get(text) {
            return *id;
        }

        let next = self.strings.len();
        let Ok(raw) = u32::try_from(next) else {
            // Unreachable for any corpus that fits in memory: four billion
            // distinct terms. Returning the ceiling keeps the caller total.
            return StringId(u32::MAX);
        };

        let id = StringId(raw);
        self.strings.push(text.to_owned());
        self.ids.insert(text.to_owned(), id);
        id
    }

    /// Looks up an existing id without inserting.
    #[must_use]
    pub fn lookup(&self, text: &str) -> Option<StringId> {
        self.ids.get(text).copied()
    }

    /// Returns the text stored at `id`, or `None` if the id is out of range.
    #[must_use]
    pub fn get(&self, id: StringId) -> Option<&str> {
        self.strings.get(id.as_usize()).map(String::as_str)
    }

    /// Number of interned strings, which is also the number of valid ids.
    #[must_use]
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// True when nothing has been interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }

    /// Rebuilds the derived reverse map from the serialized string list.
    fn from_strings(strings: Vec<String>) -> Self {
        let mut ids = BTreeMap::new();
        for (index, text) in strings.iter().enumerate() {
            if let Ok(raw) = u32::try_from(index) {
                ids.insert(text.clone(), StringId(raw));
            }
        }
        Self { strings, ids }
    }
}

/// One term occurrence count in one chunk.
///
/// `chunk` is a position in the order passed to [`LexicalIndex::build`], not a
/// [`ChunkId`]: phase 4 keys postings on the indexed input,
/// never on content identity (§3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Posting {
    /// Zero-based position of the chunk in the built index.
    pub chunk: u32,
    /// How many times the term occurs in that chunk.
    pub tf: u32,
}

/// Where one term's postings live and how many chunks contain it.
///
/// `terms` is indexed by term id, and `postings` is a half-open range into the
/// index's flat posting list. Ranges are ascending and disjoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermMeta {
    /// Number of chunks containing the term (document frequency).
    pub df: u32,
    /// Half-open range into the flat posting list.
    pub postings: (u32, u32),
}
