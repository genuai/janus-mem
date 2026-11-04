//! The corpus signature, the witness, and the plan both writers share (M4D
//! §3.1, §3.2).
//!
//! A **signature** (the sorted `(rel_path, size, mtime)` of every admitted file)
//! is what a read compares with the manifest. The manifest stores only its
//! blake3 digest, so the comparison costs one hash of about 85 KB, not a diff.
//! The digest sees an add, a remove, a rename and an edit. It cannot see a
//! content change that keeps path, size and mtime.
//!
//! The **witness** is that comparison: one boolean per read, never persisted.
//! An index built before M4D has no digest, so it falls back to
//! `max(document mtime) > mtime(index.bin)`. The fallback cannot see a deletion
//! or a rename; one `janus index` writes the digest and lifts the limit.
//!
//! This module reads no file for the signature. [`plan_from_walk`] does read the
//! corpus, because resolving a document reads its bytes; the caller passes the
//! walk so one walk feeds both the plan and the digest.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::error::JanusError;
use crate::id::DocId;
use crate::index::build::{self, BuildPlan};
use crate::index::embed::Embedder;
use crate::index::store;
use crate::vault::{DiscoveredFile, WalkResult, identity, resolve};

/// Tag of an mtime at or after the Unix epoch (1970-01-01T00:00:00Z).
const AFTER_EPOCH: u8 = 0;

/// Tag of an mtime before the Unix epoch.
///
/// A distinct tag is what keeps a pre-epoch time from encoding as the same bytes
/// as a post-epoch time. Without it, `-1.5 s` and `1.5 s` would hash alike.
const BEFORE_EPOCH: u8 = 1;

/// The sorted `(rel_path, size, mtime)` of the files a walk admitted.
///
/// The order is by `rel_path`, so two walks of an unchanged corpus encode to the
/// same bytes. A walk never yields the same path twice, so the order is total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    entries: Vec<Entry>,
}

/// One admitted file's stat facts, in signature order.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    rel_path: String,
    size: u64,
    mtime: SystemTime,
}

impl Signature {
    /// Sorts the walked files and keeps the facts the digest encodes.
    #[must_use]
    pub fn of(files: &[DiscoveredFile]) -> Self {
        let mut entries: Vec<Entry> = files
            .iter()
            .map(|file| Entry {
                rel_path: file.rel_path.clone(),
                size: file.size,
                mtime: file.mtime,
            })
            .collect();
        entries.sort_by(|left, right| left.rel_path.cmp(&right.rel_path));
        Self { entries }
    }

    /// The blake3 hex digest the manifest stores and a read recomputes.
    ///
    /// Each entry encodes as its length-prefixed path, its size, and its mtime
    /// as `(tag, secs, nanos)`. Every field is little-endian and fixed width, and
    /// the path carries its own length, so two different corpora cannot encode
    /// to the same bytes by shifting a boundary.
    #[must_use]
    pub fn digest(&self) -> String {
        let mut bytes = Vec::with_capacity(self.entries.len() * 48);
        for entry in &self.entries {
            push_field(&mut bytes, entry.rel_path.as_bytes());
            bytes.extend_from_slice(&entry.size.to_le_bytes());
            let (tag, secs, nanos) = encode_mtime(entry.mtime);
            bytes.push(tag);
            bytes.extend_from_slice(&secs.to_le_bytes());
            bytes.extend_from_slice(&nanos.to_le_bytes());
        }
        blake3::hash(&bytes).to_hex().to_string()
    }

    /// How many files the signature covers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the walk admitted no file at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The newest mtime in the corpus, or `None` for an empty corpus.
    ///
    /// This is the pre-M4D fallback's input, not a fact the digest uses.
    #[must_use]
    pub fn max_mtime(&self) -> Option<SystemTime> {
        self.entries.iter().map(|entry| entry.mtime).max()
    }
}

/// Encodes an mtime as `(tag, seconds, nanoseconds)` since the Unix epoch.
///
/// A time before the epoch is encoded by its distance from it and a distinct
/// tag, so the encoding never panics and never hides a change. `as_secs` alone
/// would lose the sub-second part and so weaken the witness inside one second.
fn encode_mtime(mtime: SystemTime) -> (u8, u64, u32) {
    match mtime.duration_since(UNIX_EPOCH) {
        Ok(after) => (AFTER_EPOCH, after.as_secs(), after.subsec_nanos()),
        Err(before) => {
            let distance = before.duration();
            (BEFORE_EPOCH, distance.as_secs(), distance.subsec_nanos())
        }
    }
}

/// Appends a little-endian `u64` length and then the bytes.
fn push_field(bytes: &mut Vec<u8>, field: &[u8]) {
    bytes.extend_from_slice(&(field.len() as u64).to_le_bytes());
    bytes.extend_from_slice(field);
}

/// What a read learned about the corpus it just walked (§3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Witness {
    /// True when the corpus changed after the index was built.
    pub stale: bool,
}

impl Witness {
    /// Compares a walk with the manifest that describes the index.
    ///
    /// A manifest with `corpus_sig` is compared by digest. A manifest without
    /// one predates M4D, so the mtime rule decides. `index_mtime` is
    /// `mtime(index.bin)`; `None` means it could not be read, and a read that
    /// cannot prove a change reports a fresh corpus.
    #[must_use]
    pub fn of(
        files: &[DiscoveredFile],
        manifest: &store::Manifest,
        index_mtime: Option<SystemTime>,
    ) -> Self {
        let signature = Signature::of(files);
        match manifest.corpus_sig.as_deref() {
            Some(stored) => Self {
                stale: signature.digest() != stored,
            },
            None => Self::from_signature(&signature, manifest.docs, index_mtime),
        }
    }

    /// The pre-M4D rule, for a manifest that stores no digest.
    ///
    /// Two extra rules apply (§3.1): an empty corpus over an index that names
    /// documents is stale, and the comparison is strict `>`, so an edit inside
    /// the publish's own timestamp tick is missed.
    #[must_use]
    pub fn fallback(
        files: &[DiscoveredFile],
        indexed_docs: usize,
        index_mtime: Option<SystemTime>,
    ) -> Self {
        Self::from_signature(&Signature::of(files), indexed_docs, index_mtime)
    }

    fn from_signature(
        signature: &Signature,
        indexed_docs: usize,
        index_mtime: Option<SystemTime>,
    ) -> Self {
        if signature.is_empty() {
            return Self {
                stale: indexed_docs > 0,
            };
        }
        let (Some(index_mtime), Some(newest)) = (index_mtime, signature.max_mtime()) else {
            return Self { stale: false };
        };
        Self {
            stale: newest > index_mtime,
        }
    }
}

/// Resolves a walk and plans the next snapshot; writes nothing (§3.2).
///
/// Both writers call this: `janus index` with the walk it already made, and a
/// read that has to rebuild with the walk it made after the lock. `rebuild`
/// ignores the previous generation, so every chunk is tokenized again.
///
/// # Errors
///
/// Propagates a read failure that is not a skip, and any [`build::plan_forced`]
/// failure such as an embedder that breaks its contract.
pub fn plan_from_walk(
    config: &Config,
    walk: &WalkResult,
    dir: &Path,
    embedder: Option<&dyn Embedder>,
    forced: &BTreeSet<DocId>,
    rebuild: bool,
) -> Result<BuildPlan, JanusError> {
    let (mut documents, _skips) = resolve::resolve_all(&walk.files, config);
    identity::derive_identity(&mut documents, config);
    let previous = if rebuild {
        None
    } else {
        store::previous_generation(dir)
    };
    Ok(build::plan_forced(
        config,
        &documents,
        previous.as_ref(),
        forced,
        embedder,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(rel_path: &str, size: u64, secs: u64, nanos: u32) -> DiscoveredFile {
        DiscoveredFile {
            path: Path::new(rel_path).to_path_buf(),
            rel_path: rel_path.to_owned(),
            size,
            mtime: UNIX_EPOCH + std::time::Duration::new(secs, nanos),
        }
    }

    #[test]
    fn a_pre_epoch_time_and_a_post_epoch_time_of_equal_magnitude_differ() {
        let before = UNIX_EPOCH - std::time::Duration::new(90, 0);
        let after = UNIX_EPOCH + std::time::Duration::new(90, 0);

        assert_eq!(encode_mtime(before), (BEFORE_EPOCH, 90, 0));
        assert_eq!(encode_mtime(after), (AFTER_EPOCH, 90, 0));
        assert_ne!(encode_mtime(before), encode_mtime(after));
    }

    #[test]
    fn the_digest_keeps_the_sub_second_part_of_an_mtime() {
        let one = Signature::of(&[file("a.md", 7, 1_700_000_000, 0)]);
        let two = Signature::of(&[file("a.md", 7, 1_700_000_000, 1)]);

        assert_ne!(one.digest(), two.digest());
    }

    #[test]
    fn the_digest_does_not_depend_on_walk_order() {
        let first = Signature::of(&[file("a.md", 1, 10, 0), file("b.md", 2, 20, 0)]);
        let second = Signature::of(&[file("b.md", 2, 20, 0), file("a.md", 1, 10, 0)]);

        assert_eq!(first.digest(), second.digest());
        assert_eq!(first.len(), 2);
    }

    #[test]
    fn a_path_that_could_shift_a_boundary_changes_the_digest() {
        // "ab" + size 1 must not encode like "a" + size (b << 8 | 1).
        let long = Signature::of(&[file("ab", 1, 0, 0)]);
        let short = Signature::of(&[file("a", 1, 0, 0), file("b", 1, 0, 0)]);

        assert_ne!(long.digest(), short.digest());
    }

    #[test]
    fn the_empty_corpus_fallback_is_stale_only_when_the_index_names_documents() {
        assert!(Witness::fallback(&[], 3, None).stale);
        assert!(!Witness::fallback(&[], 0, None).stale);
    }

    #[test]
    fn the_fallback_is_strict_about_equal_times() {
        let index_mtime = UNIX_EPOCH + std::time::Duration::new(100, 0);
        let same = [file("a.md", 1, 100, 0)];
        let newer = [file("a.md", 1, 101, 0)];

        assert!(!Witness::fallback(&same, 1, Some(index_mtime)).stale);
        assert!(Witness::fallback(&newer, 1, Some(index_mtime)).stale);
    }
}
