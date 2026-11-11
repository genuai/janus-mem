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
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::NaiveDate;

use crate::config::{Config, Refresh};
use crate::error::JanusError;
use crate::id::DocId;
use crate::index::Snapshot;
use crate::index::build::{self, BuildPlan, EmbedSink};
use crate::index::embed::Embedder;
use crate::index::store::{self, Load, LoadOptions, Lock, Policy};
use crate::vault::{DiscoveredFile, WalkResult, resolve};

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
    pub corpus: bool,
    /// True when the manifest was built under another config (M7-1).
    pub config: bool,
    /// True when either changed, so the generation no longer describes this
    /// read.
    pub stale: bool,
}

impl Witness {
    /// Compares a walk with the manifest that describes the index.
    ///
    /// A manifest with `corpus_sig` is compared by digest. A manifest without
    /// one predates M4D, so the mtime rule decides. `index_mtime` is
    /// `mtime(index.bin)`; `None` means it could not be read, and a read that
    /// cannot prove a change reports a fresh corpus.
    ///
    /// `config_hash` is this process's [`Config::config_hash`], or `None` for a
    /// report that judges the corpus only. A mismatch is a refresh trigger, the
    /// same class as a changed file, never a cold rebuild (M7-32). A manifest
    /// without a hash predates M7-1, so it cannot prove it matches and is stale
    /// exactly once.
    #[must_use]
    pub fn of(
        files: &[DiscoveredFile],
        manifest: &store::Manifest,
        index_mtime: Option<SystemTime>,
        config_hash: Option<&str>,
    ) -> Self {
        let signature = Signature::of(files);
        let corpus = match manifest.corpus_sig.as_deref() {
            Some(stored) => signature.digest() != stored,
            None => Self::from_signature(&signature, manifest.docs, index_mtime).stale,
        };
        let config = config_hash.is_some_and(|hash| manifest.config_hash.as_deref() != Some(hash));
        Self {
            corpus,
            config,
            stale: corpus || config,
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
        let corpus = if signature.is_empty() {
            indexed_docs > 0
        } else {
            match (index_mtime, signature.max_mtime()) {
                (Some(index_mtime), Some(newest)) => newest > index_mtime,
                _ => false,
            }
        };
        Self {
            corpus,
            config: false,
            stale: corpus,
        }
    }
}

/// Resolves a walk and plans the next snapshot; writes nothing (§3.2).
///
/// Both writers call this: `janus index` with the walk it already made, and a
/// read that has to rebuild with the walk it made after the lock. `rebuild`
/// ignores the previous generation, so every chunk is tokenized again. `sink`
/// reports the embedding work this plan does (§3.6).
///
/// # Errors
///
/// Propagates a read failure that is not a skip, and any [`build::plan_forced`]
/// failure such as an embedder that breaks its contract.
#[allow(clippy::too_many_arguments)]
pub fn plan_from_walk(
    config: &Config,
    walk: &WalkResult,
    dir: &Path,
    embedder: Option<&dyn Embedder>,
    basis: Option<&Snapshot>,
    today: NaiveDate,
    forced: &BTreeSet<DocId>,
    rebuild: bool,
    sink: EmbedSink<'_>,
) -> Result<BuildPlan, JanusError> {
    let (mut documents, _skips) = resolve::resolve_all(&walk.files, config);
    // `rebuild` drops the reuse source, never the identity basis (M5-2): a
    // rebuild re-tokenizes, and re-identifying would churn every id.
    let previous = if rebuild {
        None
    } else {
        store::previous_generation(dir)
    };
    Ok(build::plan_forced(
        config,
        &mut documents,
        previous.as_ref(),
        basis,
        today,
        forced,
        embedder,
        sink,
    )?)
}

// ──────────────────────────── refresh on read ─────────────────────────────

/// What a read decided about the index, in the words of §3.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The generation describes this corpus. Nothing is printed and nothing is
    /// written.
    Fresh,
    /// A corpus refresh published a generation.
    CorpusChanged,
    /// The corpus and the config both changed, so one refresh republished the
    /// generation (M7-1).
    CorpusAndConfigChanged,
    /// The config changed, so a refresh republished the generation (M7-1).
    ConfigChanged,
    /// The root moved, so the manifest was rewritten and the generation read.
    ManifestRepaired,
    /// The index was absent, so a read built it.
    NoIndexYet,
    /// No pair validated, so a read rebuilt it.
    IndexRepaired,
    /// Another writer holds the lock, so the read answered from disk.
    SkippedBusy,
    /// `[index] refresh = "manual"`.
    SkippedManual,
    /// The caller passed `--no-rebuild`.
    SkippedNoRebuild,
    /// The config file on disk disagrees with this process and with the
    /// manifest, so this process is the stale one and must not publish
    /// (M7-41).
    SkippedConfigElsewhere,
}

impl Decision {
    /// The one line this decision prints. At most one per read (§3.5, D-12).
    #[must_use]
    pub fn line(self) -> Option<&'static str> {
        match self {
            Self::Fresh => None,
            Self::CorpusChanged => Some("refresh: corpus changed"),
            Self::CorpusAndConfigChanged => Some("refresh: corpus and config changed"),
            Self::ConfigChanged => Some("refresh: config changed"),
            Self::ManifestRepaired => Some("refresh: manifest repaired"),
            Self::NoIndexYet => Some("refresh: no index yet"),
            Self::IndexRepaired => Some("refresh: index repaired"),
            Self::SkippedBusy => Some("refresh: skipped, index busy"),
            Self::SkippedManual => Some("refresh: skipped, refresh = manual"),
            Self::SkippedNoRebuild => Some("refresh: skipped, --no-rebuild"),
            Self::SkippedConfigElsewhere => Some("refresh: skipped, config changed elsewhere"),
        }
    }
}

/// What a read got: the generation to answer from, the line to print, and
/// whether that generation still describes an older corpus.
pub struct ReadOutcome {
    /// The generation this read answers from.
    pub load: Load,
    /// Why a build ran, or why one did not (§3.3, §3.5).
    pub decision: Decision,
    /// True when the answered generation is older than the corpus. Only a
    /// declined refresh leaves this true.
    pub stale: bool,
}

/// Loads a validated generation, then refreshes it when the corpus changed
/// (§3.4 steps 1–7, D-10, D-11).
///
/// The order is the contract: the generation loads first, without a lock, so a
/// read that can answer never waits for a writer. Only then is the walk
/// compared with the manifest's digest. A busy lock is a skip, not an error.
///
/// `walk` is the walk the caller already made for the witness. It feeds
/// [`store::build_until_stable`] when something must be built, so a stale read
/// pays one walk for the witness and one more after the publish (§3.1).
///
/// # Errors
///
/// Exactly [`store::load`]'s, plus any walk, plan or publication failure.
pub fn on_read(
    dir: &Path,
    options: &LoadOptions<'_>,
    walk: WalkResult,
    refresh: Refresh,
) -> Result<ReadOutcome, JanusError> {
    let digest = Signature::of(&walk.files).digest();
    // Read before the load, so a rebuild can be told from a repair (§3.3 rows 3
    // and 4). The load may create the directory; this check does not.
    let first_build = !dir.join(store::MANIFEST_FILE).exists();
    let load = store::load(options)?;

    // Rows 3 and 4: a build already ran under the lock, so these are its bytes.
    if load.rebuilt {
        let decision = if first_build {
            Decision::NoIndexYet
        } else {
            Decision::IndexRepaired
        };
        return Ok(ReadOutcome {
            load,
            decision,
            stale: false,
        });
    }

    // Row 6: the generation is usable, but the manifest names another root.
    if load.refresh_request.is_some() {
        let declined = decline(options, refresh, false);
        if let Some(decision) = declined {
            return Ok(ReadOutcome {
                load,
                decision,
                stale: false,
            });
        }
        let Some(lock) = Lock::try_acquire(dir)? else {
            return Ok(ReadOutcome {
                load,
                decision: Decision::SkippedBusy,
                stale: false,
            });
        };
        // The repair republishes the current plan, so the new manifest carries
        // this root and this digest (§3.4 step 4).
        let load = rebuild(dir, options, walk, &lock)?;
        return Ok(ReadOutcome {
            load,
            decision: Decision::ManifestRepaired,
            stale: false,
        });
    }

    // Step 2: the witness. One walk, one digest, one config hash, one answer.
    let config_hash = options.config.config_hash();
    let witness = Witness::of(
        &walk.files,
        &load.manifest,
        index_mtime(dir),
        Some(&config_hash),
    );
    if !witness.stale {
        return Ok(ReadOutcome {
            load,
            decision: Decision::Fresh,
            stale: false,
        });
    }

    // M7-41: a process keeps the config it started with, so a mismatch can be
    // the file's fault or this process's. Publishing under a config that
    // disagrees with the file makes two processes overwrite each other forever,
    // so the file decides. Nothing here runs on a fresh read: one config parse
    // per mismatch, not per query.
    if witness.config {
        match config_hash_on_disk(options.root) {
            // The file names the config the manifest records, so the index
            // describes the corpus as it is now. Only a corpus change can be
            // left, and this process cannot publish under its own config.
            Some(on_disk) if load.manifest.config_hash.as_deref() == Some(on_disk.as_str()) => {
                let decision = if witness.corpus {
                    Decision::SkippedConfigElsewhere
                } else {
                    Decision::Fresh
                };
                return Ok(ReadOutcome {
                    load,
                    decision,
                    stale: witness.corpus,
                });
            }
            // This process's config is the file's, so a publish describes the
            // corpus and no other process will undo it.
            Some(on_disk) if on_disk == config_hash => {}
            // Neither the manifest's nor this process's, or the file cannot be
            // read: this process cannot prove its config is the corpus's, so it
            // publishes nothing. The answer still comes from the validated
            // generation, which may not match the file, so it is stale.
            _ => {
                return Ok(ReadOutcome {
                    load,
                    decision: Decision::SkippedConfigElsewhere,
                    stale: true,
                });
            }
        }
    }

    if let Some(decision) = decline(options, refresh, true) {
        return Ok(ReadOutcome {
            load,
            decision,
            stale: true,
        });
    }

    // Steps 5 and 6: one non-blocking attempt. A read never waits and never
    // fails on a lock, because another writer will publish what this one wants.
    let Some(lock) = Lock::try_acquire(dir)? else {
        return Ok(ReadOutcome {
            load,
            decision: Decision::SkippedBusy,
            stale: true,
        });
    };
    // Step 7: adopt only when the writer's manifest names the corpus this read
    // walked. A writer whose plan predates the edit also validates, and
    // adopting it would lose the edit (§3.4).
    if let Some(fresh) = adopt_under_lock(options, &digest, &config_hash)? {
        return Ok(ReadOutcome {
            load: fresh,
            decision: Decision::Fresh,
            stale: false,
        });
    }
    let load = rebuild(dir, options, walk, &lock)?;
    // Name both causes when both are true: a reader that edited a document and
    // the config must not be told only one of them moved.
    Ok(ReadOutcome {
        load,
        decision: match (witness.corpus, witness.config) {
            (true, true) => Decision::CorpusAndConfigChanged,
            (false, true) => Decision::ConfigChanged,
            _ => Decision::CorpusChanged,
        },
        stale: false,
    })
}

/// The config hash the corpus file states now, or `None` when it cannot be
/// read or does not parse.
///
/// `None` is "cannot prove", never "unchanged": a caller that cannot read the
/// file must not publish a manifest that claims to describe it (M7-41).
fn config_hash_on_disk(root: &Path) -> Option<String> {
    Config::load(root).ok().map(|config| config.config_hash())
}

/// Why this read may not write, or `None` when it may (§3.3, rows 2 and 6).
fn decline(options: &LoadOptions<'_>, refresh: Refresh, corpus_changed: bool) -> Option<Decision> {
    if options.policy == Policy::Refuse {
        return Some(Decision::SkippedNoRebuild);
    }
    // `manual` disables the corpus-changed row only. A moved root is a repair,
    // not a refresh of a changed corpus.
    if corpus_changed && refresh == Refresh::Manual {
        return Some(Decision::SkippedManual);
    }
    None
}

/// Re-reads the manifest under the lock and adopts it only when it describes
/// the corpus and the config this read walked (§3.4 step 7).
///
/// A failed read here is not an error: the caller builds instead, and that build
/// reports the real failure if the disk is broken.
fn adopt_under_lock(
    options: &LoadOptions<'_>,
    digest: &str,
    config_hash: &str,
) -> Result<Option<Load>, JanusError> {
    // `Refuse` guarantees this never tries to take the lock we already hold.
    let locked = LoadOptions {
        policy: Policy::Refuse,
        ..*options
    };
    let Ok(load) = store::load(&locked) else {
        return Ok(None);
    };
    if load.refresh_request.is_some() {
        return Ok(None);
    }
    Ok(adoptable(
        load.manifest.corpus_sig.as_deref(),
        digest,
        load.manifest.config_hash.as_deref(),
        config_hash,
    )
    .then_some(load))
}

/// True when a writer's manifest names the corpus this read walked, and the
/// config it resolved (§3.4 step 7).
///
/// A manifest without a digest predates M4D and one without a config hash
/// predates M7-1, so neither can be compared and the caller builds.
fn adoptable(stored: Option<&str>, digest: &str, config: Option<&str>, hash: &str) -> bool {
    stored == Some(digest) && config == Some(hash)
}

/// Builds a generation under a lock the caller holds, as a [`Load`].
fn rebuild(
    dir: &Path,
    options: &LoadOptions<'_>,
    walk: WalkResult,
    lock: &Lock,
) -> Result<Load, JanusError> {
    let basis = store::previous_snapshot(dir);
    let outcome = store::build_until_stable(
        dir,
        options.root,
        options.config,
        options.embedder,
        options.today,
        walk,
        basis.as_ref(),
        &BTreeSet::new(),
        false,
        options.sink,
        lock,
    )?;
    // A read that did not want vectors does not receive them, even though the
    // build it triggered may have written them (V-4).
    let vectors = if options.want_vectors {
        outcome.plan.vectors.as_ref().map(|set| set.rows().clone())
    } else {
        None
    };
    Ok(Load {
        snapshot: outcome.plan.snapshot,
        manifest: outcome.manifest,
        vectors,
        rebuilt: true,
        stats: outcome.plan.stats,
        stale: None,
        refresh_request: None,
    })
}

/// `mtime(index.bin)`, which only the pre-M4D fallback reads (§3.1).
fn index_mtime(dir: &Path) -> Option<SystemTime> {
    fs::metadata(dir.join(store::INDEX_FILE))
        .and_then(|metadata| metadata.modified())
        .ok()
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
    fn adoption_needs_the_writers_digest_to_equal_the_one_this_read_computed() {
        assert!(adoptable(Some("abc"), "abc", Some("h1"), "h1"));
        assert!(!adoptable(Some("abc"), "abcd", Some("h1"), "h1"));
        assert!(
            !adoptable(None, "abc", Some("h1"), "h1"),
            "a pre-M4D manifest cannot be compared"
        );
        // A manifest built under another config resolves other metadata, so
        // adopting it would serve the type the edit removed (M7-1).
        assert!(!adoptable(Some("abc"), "abc", Some("h2"), "h1"));
        assert!(
            !adoptable(Some("abc"), "abc", None, "h1"),
            "a pre-M7-1 manifest cannot be compared either"
        );
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
