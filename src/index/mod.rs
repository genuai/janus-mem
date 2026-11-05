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
pub mod refresh;
pub mod store;
pub mod vectors;

pub use lexical::{LexicalIndex, squash};

/// The on-disk snapshot layout; a mismatch is a rebuild, never an error (S-6, §8.3).
///
/// Version 2 (M4 V-1) adds `DocMeta::body_hash` to the snapshot and the
/// `vectors.f32` payload with its three manifest fields. Version 3 (M4B B-7)
/// adds `DocMeta::date_source`. Version 4 (M5 P1) adds `Snapshot::path_history`,
/// `Snapshot::identity` and `Snapshot::tombstones`. Version 5 (M5 P2) adds
/// `Snapshot::edges`, `Snapshot::dangling_links` and `DocMeta::effective_to`.
/// Version 6 (M5 P6) adds `DocMeta::hint`. An older index is refused by
/// `Expected::check` and rebuilt with one warning, never misread.
pub const FORMAT_VERSION: u16 = 6;

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
    /// Every path each `DocId` has occupied, including the paths of retired
    /// ids (M5 §2.1). The alias map is derived from this, never stored.
    pub path_history: BTreeMap<DocId, Vec<PathEntry>>,
    /// Why each `DocId` has its value, and the match keys it answers to.
    pub identity: BTreeMap<DocId, IdentityRecord>,
    /// The ids this generation saw last and no longer sees, sorted by id.
    pub tombstones: Vec<TombstoneMeta>,
    /// Every declared link this generation resolved, sorted by `(from, to)`
    /// (M5 §2.2). The graph at query time is derived from these rows.
    pub edges: Vec<Edge>,
    /// Every declared link that resolved to nothing, sorted and de-duplicated.
    pub dangling_links: Vec<DanglingLink>,
}

/// One path a document has occupied (M5 §2.1). `path_history` is keyed by
/// `DocId`, so a path survives a move the document no longer occupies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathEntry {
    /// Vault-relative, `/`-separated (§8.7).
    pub rel_path: String,
    /// The calendar date of the build that first saw this path. Injected, never
    /// the wall clock (§7.2).
    pub first_seen: NaiveDate,
    /// The calendar date of the build that last saw this path.
    pub last_seen: NaiveDate,
    /// True on the last entry of a `DocId` that no longer appears in the scan. A
    /// path the document merely left is not a tombstone: the document is still
    /// live, and its new path is the last entry.
    pub tombstone: bool,
}

/// One identity key of one document. Every reachable rung is stored, because a
/// second move is matched against a rung that no longer produces the id (case
/// 1.14). `ContentHash` is a **match** key and never a derivation key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MatchKey {
    /// Rung 1: a declared `id`, used verbatim.
    DeclaredId(String),
    /// Rung 2: the whole-file hash, which proves a byte-identical move.
    ContentHash(Blake3Hash),
    /// Rung 3: `title + date`, namespaced by corpus and scope at encoding time.
    TitleDate {
        /// The effective title.
        title: String,
        /// The usable date.
        date: NaiveDate,
    },
    /// Rung 4: `title + doc_version`, only when no usable date exists.
    TitleVersion {
        /// The effective title.
        title: String,
        /// The declared `doc_version`.
        version: String,
    },
    /// Rung 5: the title alone, when it is unique.
    Title(String),
}

/// Why a `DocId` has its value, and every match key it answers to (M5 §2.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityRecord {
    /// The rung that produced the id (§6.4).
    pub tier: crate::document::IdentityTier,
    /// One key per match rung, in ladder order. Refreshed on every build for a
    /// live document and kept unchanged for a retired one, so it can be matched
    /// again if the document returns. Rung 6 stores no key: the path is the key.
    pub keys: Vec<MatchKey>,
}

/// The only edge kind in v1 (§8.9, D35).
///
/// A second kind is additive and needs a new variant, never a new field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EdgeKind {
    /// `to` replaces `from` (§6.7).
    Supersedes,
}

/// A declared link, resolved to two document ids (M5 §2.2).
///
/// `from` and `to` are relational, not chronological: `from` is the document
/// the declaration replaces, and the dates need not agree with the direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Edge {
    /// The document the declaration replaces.
    pub from: DocId,
    /// The document that replaces it.
    pub to: DocId,
    /// Reserved for additive kinds (§8.9).
    pub kind: EdgeKind,
}

/// A declared link that resolved to nothing (M5 §2.2).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DanglingLink {
    /// The document that declared the link.
    pub from: DocId,
    /// The text the frontmatter carried, for `doctor` to print.
    ///
    /// Text, not `StringId`: the value may name nothing in the corpus, so there
    /// is no pool handle to give it (M5-24, M5-29).
    pub target: String,
    /// Why the target did not resolve.
    pub reason: DanglingReason,
}

/// Why a declared link did not resolve (M5 §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DanglingReason {
    /// No current path, historical path or declared id matched.
    NotFound,
    /// The target names another corpus. Cross-corpus links are M6 (§6.11).
    OtherCorpus,
}

/// The last known facts about a document that no longer exists (M5 §2.1).
///
/// The row lets a retired successor close its predecessor's window (§3.6) and
/// gives `lineage` and `ids` a title. Its path comes from `path_history`:
/// a path is never stored twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TombstoneMeta {
    /// The id the document had.
    pub id: DocId,
    /// The title at that moment.
    pub title: String,
    /// The date at that moment.
    pub date: Option<NaiveDate>,
    /// The valid-time start at that moment (`effective_from ?? date`).
    pub valid_from: Option<NaiveDate>,
    /// The owning scope at that moment, so the scope rule still applies.
    pub scope: String,
    /// The retrieval tags at that moment, for the same reason.
    pub shared_with: Vec<String>,
}

/// A display-only guess that a new document continues a vanished one (M5 §3.11).
///
/// The four components are stored beside the confidence so `janus moves` can
/// show the arithmetic. A tombstoned candidate has no `DocMeta` row, so the
/// reader cannot recompute them from the generation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Hint {
    /// The vanished document this one probably continues.
    pub id: DocId,
    /// The weighted score. Never applied to anything.
    pub confidence: f32,
    /// `jaccard` of the two chunk-id sets, a weight of `0.50`.
    pub chunk_jaccard: f32,
    /// `1.0` when the normalised titles are equal, a weight of `0.20`.
    pub title_match: f32,
    /// `1.0` when the date-stripped file stems are equal, a weight of `0.15`.
    pub basename_match: f32,
    /// `jaccard` of `tags ∪ shared_with`, a weight of `0.15`.
    pub tag_jaccard: f32,
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
    /// Valid-time end, exclusive (§6.10). The window rule reads it in P3.
    pub effective_to: Option<NaiveDate>,
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
    /// The best display-only continuation guess for this build, if any (M5 §3.11).
    pub hint: Option<Hint>,
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
