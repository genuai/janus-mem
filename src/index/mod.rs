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
//! This module contains no I/O, no serde, and no manifest. [`LexicalIndex`] is
//! the only consumer today; phase 4 adds serialization to [`StringPool`] and the
//! layout types below.
//!
//! [`LexicalIndex`]: crate::index::LexicalIndex
//! [`LexicalIndex::build`]: crate::index::LexicalIndex::build

use std::collections::BTreeMap;

mod lexical;

pub use lexical::{LexicalIndex, squash};

/// A stable, compact identity for a string in a [`StringPool`].
///
/// Interned ids are assigned in insertion order, so they are deterministic for
/// a deterministic build. The `u32` form is the one the snapshot stores; it
/// caps a pool at [`u32::MAX`] distinct strings, far beyond any real corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
#[derive(Debug, Clone, Default)]
pub struct StringPool {
    /// Strings in id order; `strings[id]` is the text of `StringId(id)`.
    strings: Vec<String>,
    /// Reverse lookup. A `BTreeMap`, not a `HashMap`, so no code path can make
    /// the build depend on hash iteration order.
    ids: BTreeMap<String, StringId>,
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
}

/// One term occurrence count in one chunk.
///
/// `chunk` is a position in the order passed to [`LexicalIndex::build`], not a
/// [`ChunkId`](crate::id::ChunkId): phase 4 keys postings on the indexed input,
/// never on content identity (§3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermMeta {
    /// Number of chunks containing the term (document frequency).
    pub df: u32,
    /// Half-open range into the flat posting list.
    pub postings: (u32, u32),
}
