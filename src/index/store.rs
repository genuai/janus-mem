//! Snapshot persistence, the manifest, and load-time validation (§3.5, §8.3).
//!
//! Loading validates every id, range and offset before use (S-18). Writing is a
//! publication: `.janus/lock` for the whole write (S-7), a unique temp file per
//! writer, `index.bin` renamed before `manifest.json` (S-17).

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use fs4::TryLockError;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tempfile::NamedTempFile;

use crate::config::{self, Config};
use crate::error::JanusError;
use crate::id::DocId;
use crate::tokenize::{Stemmer, Tokenizer, TokenizerFingerprint};
use crate::vault::{self, WalkOptions};

use super::build::{self, BuildPlan, BuildStats};
use super::embed::{Embedder, EmbedderId};
use super::refresh;
use super::vectors::{VECTORS_FILE, Vectors};
use super::{ChunkMeta, ChunkingFingerprint, DocMeta, FORMAT_VERSION, LexicalIndex, Snapshot};

macro_rules! corrupt {
    ($($arg:tt)*) => {
        return Err(Corrupt::new(format!($($arg)*)))
    };
}

/// The index snapshot inside the index directory (§2).
pub const INDEX_FILE: &str = "index.bin";

/// The generation manifest, written last (§2, S-17).
pub const MANIFEST_FILE: &str = "manifest.json";

/// The exclusive advisory writer lock (§8.3, S-7).
pub const LOCK_FILE: &str = "lock";

/// How long a writer waits for [`LOCK_FILE`] before giving up (§3.5, S-7).
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// Prefix of every publication temp file, swept under the lock only (§3.5).
const TEMP_PREFIX: &str = ".tmp";

/// The reader's bounded retry schedule (§3.5 step 4): three retries after the
/// first attempt, because a publication takes milliseconds, not seconds.
const RETRY_BACKOFF: [Duration; 3] = [
    Duration::from_millis(10),
    Duration::from_millis(50),
    Duration::from_millis(200),
];

/// How often a waiting writer re-tests the lock. Short enough that the 10 ms
/// timeout in tests is honoured, long enough not to spin.
const LOCK_POLL: Duration = Duration::from_millis(5);

/// The human-readable index manifest (`.janus/manifest.json`, §2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// The snapshot layout version.
    pub format_version: u16,
    /// The frontmatter schema this build understands.
    pub schema_version: u16,
    /// The declared corpus identity (§6.11).
    pub corpus_id: String,
    /// The canonical root; diagnostic only.
    pub built_root: String,
    /// RFC 3339 UTC build time; diagnostic only.
    pub built_at: String,
    /// Number of documents.
    pub docs: usize,
    /// Number of chunk occurrences.
    pub chunks: usize,
    /// Number of terms.
    pub terms: usize,
    /// Number of postings.
    pub postings: usize,
    /// Mean chunk length in tokens.
    pub avgdl: f64,
    /// blake3 of `index.bin`, the generation id (S-17).
    pub index_sha: String,
    /// Byte length of `index.bin`.
    pub index_len: u64,
    /// The tokenizer that produced the postings.
    pub tokenizer: TokenizerFingerprint,
    /// The chunker parameters the index was built with.
    pub chunker: ChunkingFingerprint,
    /// The embedder that produced `vectors.f32`; `None` when there are no vectors.
    ///
    /// The three vector fields below are one fact (§3.1): they are all `Some`
    /// together or all `None` together, and a manifest holding two of three is
    /// corrupt. The shape lives here, never in the payload file.
    pub embedder: Option<EmbedderId>,
    /// blake3 of the bytes in `vectors.f32`.
    pub vectors_sha: Option<String>,
    /// Byte length of `vectors.f32`, checked against `chunks × dim × 4`.
    pub vectors_len: Option<u64>,
    /// blake3 of the corpus signature this generation was built from (M4D §3.1).
    ///
    /// `None` on a manifest written before M4D, or by a binary that predates
    /// it. A read then falls back to the mtime rule. The field is additive: the
    /// manifest has no `deny_unknown_fields`, so an older binary ignores it, and
    /// `format_version` does not move (§8.3).
    #[serde(default)]
    pub corpus_sig: Option<String>,
}

/// The hashes and lengths of the payload files one publication wrote (§3.1).
///
/// `Manifest::new` takes this rather than loose fields so the three vector facts
/// cannot be built half-populated: one [`VectorArtifact`] is one writer's whole
/// claim about `vectors.f32`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payloads {
    /// blake3 of `index.bin`.
    pub index_sha: String,
    /// Byte length of `index.bin`.
    pub index_len: u64,
    /// The vectors payload, when this build wrote one.
    pub vectors: Option<VectorArtifact>,
}

/// What the manifest records about `vectors.f32` (§3.1): which embedder, which
/// bytes, and how many.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorArtifact {
    /// The embedder that produced the rows.
    pub id: EmbedderId,
    /// blake3 of the bytes actually written.
    pub sha: String,
    /// Byte length of those bytes.
    pub len: u64,
}

/// One build's rows together with the identity that produced them (§3.1).
///
/// [`Vectors`] deliberately carries no identity — it validates the shape it is
/// given — so a writer must hold the id and the rows at once: rows without an
/// id cannot be described in the manifest, and an id without rows describes
/// nothing. Construction refuses a width mismatch, so the dim the manifest
/// records is always the dim the payload has.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorSet {
    id: EmbedderId,
    rows: Vectors,
}

impl VectorSet {
    /// Pairs validated rows with the embedder that produced them.
    ///
    /// # Errors
    ///
    /// [`Corrupt`] when `rows.dim()` is not `id.dim`: the manifest would then
    /// describe a shape the payload does not have.
    pub fn new(id: EmbedderId, rows: Vectors) -> Result<Self, Corrupt> {
        if rows.dim() != id.dim as usize {
            corrupt!(
                "embedder `{}` has dim {} but the rows have dim {}",
                id.name,
                id.dim,
                rows.dim()
            );
        }
        Ok(Self { id, rows })
    }

    /// The embedder that produced the rows.
    #[must_use]
    pub fn id(&self) -> &EmbedderId {
        &self.id
    }

    /// The validated rows, one per snapshot chunk.
    #[must_use]
    pub fn rows(&self) -> &Vectors {
        &self.rows
    }

    /// The payload bytes and the manifest fields that describe them.
    ///
    /// The caller hashes exactly these bytes into `vectors_sha` (§3.1), so the
    /// manifest never names a file it did not write.
    #[must_use]
    pub fn artifact(&self) -> (Vec<u8>, VectorArtifact) {
        let bytes = self.rows.encode();
        let artifact = VectorArtifact {
            id: self.id.clone(),
            sha: blake3::hash(&bytes).to_hex().to_string(),
            len: bytes.len() as u64,
        };
        (bytes, artifact)
    }
}

impl Manifest {
    /// Assembles the manifest for an already-encoded snapshot (I-3).
    ///
    /// `corpus_sig` is the digest of the walk that produced the snapshot
    /// (M4D §3.1). A writer always has one, so the field is a parameter rather
    /// than a value patched in afterwards: there is no moment when a manifest
    /// exists without it.
    #[must_use]
    pub fn new(
        root: &Path,
        snapshot: &Snapshot,
        chunker: ChunkingFingerprint,
        payloads: Payloads,
        built_at: &str,
        corpus_sig: Option<&str>,
    ) -> Self {
        let (embedder, vectors_sha, vectors_len) = match payloads.vectors {
            Some(artifact) => (Some(artifact.id), Some(artifact.sha), Some(artifact.len)),
            None => (None, None, None),
        };
        Self {
            format_version: snapshot.format_version,
            schema_version: 1,
            corpus_id: snapshot.corpus_id.clone(),
            built_root: built_root(root),
            built_at: built_at.to_owned(),
            docs: snapshot.docs.len(),
            chunks: snapshot.chunks.len(),
            terms: snapshot.terms.len(),
            postings: snapshot.postings.len(),
            avgdl: snapshot.avgdl,
            index_sha: payloads.index_sha,
            index_len: payloads.index_len,
            tokenizer: snapshot.tokenizer,
            chunker,
            embedder,
            vectors_sha,
            vectors_len,
            corpus_sig: corpus_sig.map(str::to_owned),
        }
    }
}

/// A snapshot that failed validation, decoding, or IO. A rebuild condition (S-18).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Corrupt {
    /// Human-readable cause, naming the offending field or path.
    pub reason: String,
}

impl Corrupt {
    /// A build or validation failure that names the file or value that failed.
    ///
    /// `pub(crate)` because `build::plan` reports an embedder that broke its
    /// contract through this one type rather than a second vocabulary.
    pub(crate) fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// The bridge from the payload format's typed errors (§3.1, P1b).
///
/// `vectors::Corrupt` names the offending value in a `thiserror` message; this
/// impl carries that text into the store's single corruption vocabulary rather
/// than duplicating the enum here or losing the detail.
impl From<super::vectors::Corrupt> for Corrupt {
    fn from(corrupt: super::vectors::Corrupt) -> Self {
        Self::new(corrupt.to_string())
    }
}

impl Snapshot {
    /// Builds a snapshot around an in-memory index and freshly rebuilt metadata.
    #[must_use]
    pub fn from_index(
        index: &LexicalIndex,
        corpus_id: String,
        tokenizer: TokenizerFingerprint,
        docs: Vec<DocMeta>,
        chunks: Vec<ChunkMeta>,
    ) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            corpus_id,
            tokenizer,
            strings: index.pool().clone(),
            docs,
            chunks,
            terms: index.terms().to_vec(),
            postings: index.postings().to_vec(),
            avgdl: index.avgdl(),
        }
    }

    /// Rebuilds the index after validating every offset it uses.
    ///
    /// # Errors
    ///
    /// [`Corrupt`] when any id, range, index or `avgdl` is invalid.
    pub fn to_index(&self) -> Result<LexicalIndex, Corrupt> {
        self.validate()?;
        let lengths = self.chunks.iter().map(|chunk| chunk.tokens).collect();
        Ok(LexicalIndex::from_parts(
            self.strings.clone(),
            self.terms.clone(),
            self.postings.clone(),
            lengths,
            self.avgdl,
        ))
    }

    /// Encodes the snapshot after validating it.
    ///
    /// # Errors
    ///
    /// [`Corrupt`] when the snapshot is invalid or bincode cannot encode it.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Corrupt> {
        self.validate()?;
        bincode::serialize(self)
            .map_err(|error| Corrupt::new(format!("cannot encode index.bin: {error}")))
    }

    /// Decodes and validates a snapshot. A `format_version` mismatch is a rebuild.
    ///
    /// # Errors
    ///
    /// [`Corrupt`] when bincode rejects the bytes or validation fails.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Corrupt> {
        let snapshot: Self = bincode::deserialize(bytes)
            .map_err(|error| Corrupt::new(format!("cannot decode index.bin: {error}")))?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Checks every value read from disk before it is used (§3.5, S-18).
    ///
    /// # Errors
    ///
    /// [`Corrupt`] naming the first violation found.
    pub fn validate(&self) -> Result<(), Corrupt> {
        if self.format_version != FORMAT_VERSION {
            corrupt!("format_version {} != {FORMAT_VERSION}", self.format_version);
        }
        if !self.avgdl.is_finite() || self.avgdl < 0.0 {
            corrupt!("avgdl {} is invalid", self.avgdl);
        }
        let strings = self.strings.len();
        if self.terms.len() > strings {
            corrupt!(
                "term count {} exceeds string pool {strings}",
                self.terms.len()
            );
        }
        let mut previous = 0;
        for (index, term) in self.terms.iter().enumerate() {
            let (start, end) = (term.postings.0 as usize, term.postings.1 as usize);
            if start > end || end > self.postings.len() {
                corrupt!(
                    "term {index} postings {start}..{end} outside 0..{}",
                    self.postings.len()
                );
            }
            if start < previous {
                corrupt!("term {index} start {start} before {previous}");
            }
            if term.df as usize != end - start {
                corrupt!("term {index} df {} != {}", term.df, end - start);
            }
            previous = end;
        }
        let mut previous = 0;
        for (index, doc) in self.docs.iter().enumerate() {
            for (field, id) in [
                ("rel_path", doc.rel_path),
                ("title", doc.title),
                ("type_name", doc.type_name),
                ("scope", doc.scope),
            ] {
                if id.as_usize() >= strings {
                    corrupt!("doc {index} {field} StringId {} >= {strings}", id.as_u32());
                }
            }
            for id in doc.shared_with.iter().chain(&doc.tags) {
                if id.as_usize() >= strings {
                    corrupt!("doc {index} tag StringId {} >= {strings}", id.as_u32());
                }
            }
            let (start, end) = (doc.chunk_range.0 as usize, doc.chunk_range.1 as usize);
            if start > end || end > self.chunks.len() {
                corrupt!(
                    "doc {index} chunk_range {start}..{end} outside 0..{}",
                    self.chunks.len()
                );
            }
            if start < previous {
                corrupt!("doc {index} chunk_range start {start} before {previous}");
            }
            previous = end;
        }
        for (index, chunk) in self.chunks.iter().enumerate() {
            if chunk.doc as usize >= self.docs.len() {
                corrupt!("chunk {index} doc {} >= {}", chunk.doc, self.docs.len());
            }
            for id in &chunk.heading_path {
                if id.as_usize() >= strings {
                    corrupt!(
                        "chunk {index} heading StringId {} >= {strings}",
                        id.as_u32()
                    );
                }
            }
        }
        for (index, posting) in self.postings.iter().enumerate() {
            if posting.chunk as usize >= self.chunks.len() {
                corrupt!(
                    "posting {index} chunk {} >= {}",
                    posting.chunk,
                    self.chunks.len()
                );
            }
        }
        Ok(())
    }
}

/// The manifest's `built_root`: canonicalized with `dunce`, `/`-separated (§8.7).
#[must_use]
pub fn built_root(root: &Path) -> String {
    let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    root.to_string_lossy().replace('\\', "/")
}

// ───────────────────────────────── writer ──────────────────────────────────

/// The exclusive advisory writer lock, [`LOCK_FILE`] (S-7). Dropping it releases
/// it; it is a plain file, so it can be held across a whole publish.
#[derive(Debug)]
pub struct Lock {
    file: File,
}

impl Lock {
    /// Takes the lock, re-testing until `timeout` elapses.
    ///
    /// # Errors
    ///
    /// [`JanusError::LockTimeout`], or [`JanusError::Write`] on the lock file.
    pub fn acquire(dir: &Path, timeout: Duration) -> Result<Self, JanusError> {
        let path = dir.join(LOCK_FILE);
        let file = open_lock_file(dir)?;

        let deadline = Instant::now() + timeout;
        loop {
            // UFCS on purpose: `File::try_lock` is inherent on newer compilers,
            // so `file.try_lock()` would resolve to std's, not fs4's, method.
            match fs4::FileExt::try_lock(&file) {
                Ok(()) => return Ok(Self { file }),
                Err(TryLockError::WouldBlock) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(JanusError::LockTimeout { path });
                    }
                    thread::sleep(LOCK_POLL.min(deadline - now));
                }
                Err(TryLockError::Error(source)) => {
                    return Err(JanusError::Write { path, source });
                }
            }
        }
    }

    /// Tries the lock once and never waits (§3.4, D-11).
    ///
    /// A read uses this: the refresh is a convenience, so a busy writer means
    /// "skip the refresh", never a timeout and never a failed read.
    ///
    /// # Errors
    ///
    /// [`JanusError::Write`] on the lock file, or [`JanusError::CreateDir`] when
    /// the index directory cannot be made.
    pub fn try_acquire(dir: &Path) -> Result<Option<Self>, JanusError> {
        let path = dir.join(LOCK_FILE);
        let file = open_lock_file(dir)?;
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => Ok(Some(Self { file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(source)) => Err(JanusError::Write { path, source }),
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Closing the handle releases it too; unlocking first is explicit.
        let _ = fs4::FileExt::unlock(&self.file);
    }
}

/// Opens (and creates) `.janus/lock` for both lock variants.
fn open_lock_file(dir: &Path) -> Result<File, JanusError> {
    fs::create_dir_all(dir).map_err(|source| JanusError::CreateDir {
        path: dir.to_path_buf(),
        source,
    })?;
    let path = dir.join(LOCK_FILE);
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| JanusError::Write {
            path: path.clone(),
            source,
        })
}

/// The stages of a publication (S-17, V-3), the seam behind
/// [`publish_with_hook`].
///
/// The three vectors stages fire only when the build wrote a payload. A
/// degraded publish has three index and three manifest stages, then
/// [`Stage::VectorsDropped`] **last**, once the commit record no longer names
/// the file it removes.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    VectorsWritten,
    VectorsRenamed,
    VectorsDirSynced,
    IndexWritten,
    IndexRenamed,
    IndexDirSynced,
    ManifestWritten,
    ManifestRenamed,
    ManifestDirSynced,
    VectorsDropped,
}

/// One generation to write; the manifest is derived from these fields (S-17).
#[derive(Debug, Clone, Copy)]
pub struct Publication<'a> {
    /// The index directory (normally `.janus`).
    pub dir: &'a Path,
    /// The corpus root recorded in the manifest.
    pub root: &'a Path,
    /// The snapshot to publish.
    pub snapshot: &'a Snapshot,
    /// The chunker fingerprint the manifest records.
    pub chunker: ChunkingFingerprint,
    /// The rows to write to `vectors.f32`, or `None` for a lexical generation.
    pub vectors: Option<&'a VectorSet>,
    /// Documents whose rows a previous payload could not carry; the drop line
    /// names this count. `None` keeps the generic degraded message.
    pub vectors_dropped: Option<usize>,
    /// Injected build time; no library path invents one (I-3).
    pub built_at: &'a str,
    /// The digest of the walk this plan was built from (§3.1). `None` leaves the
    /// manifest without one, which makes the next read fall back to the mtime
    /// rule.
    pub corpus_sig: Option<&'a str>,
}

/// Publishes one generation under the writer lock, manifest last (S-7, S-17).
/// An interrupted publish is recovered by a rebuild, never by a restore (§3.5).
///
/// # Errors
///
/// [`JanusError::LockTimeout`], and any stage's IO or encoding failure.
pub fn publish(publication: &Publication<'_>) -> Result<Manifest, JanusError> {
    publish_with_hook(publication, &mut |_| {})
}

/// [`publish`] with a callback at each [`Stage`]: the §3.5 test seam.
/// Errors are exactly [`publish`]'s.
#[doc(hidden)]
pub fn publish_with_hook(
    publication: &Publication<'_>,
    hook: &mut dyn FnMut(Stage),
) -> Result<Manifest, JanusError> {
    let lock = Lock::acquire(publication.dir, LOCK_TIMEOUT)?;
    publish_locked(publication, hook, &lock)
}

/// The write, stage by stage. `_lock` is a witness that the caller holds it.
///
/// Order is vectors, index, manifest (§3.1, V-3). The manifest is last because
/// it is the commit record; the largest payload goes first so the window in
/// which the commit record lags its payload is as short as possible. That order
/// is not atomic and does not pretend to be: recovery is a rebuild.
///
/// A **degraded** publish is the exception in one place: the removal of an
/// existing `vectors.f32` happens *after* the manifest rename, not with the
/// other payload work. Anywhere earlier would leave the old manifest naming a
/// file that is already gone, so a reader that wants vectors would meet
/// `is missing` and rebuild a state it should never have seen — and a failure
/// after the drop would have destroyed a good payload for nothing.
fn publish_locked(
    publication: &Publication<'_>,
    hook: &mut dyn FnMut(Stage),
    _lock: &Lock,
) -> Result<Manifest, JanusError> {
    let dir = publication.dir;
    // Only writers create temp files, so a leftover one is a crashed writer's.
    sweep_temps(dir)?;

    // Encode the snapshot before touching either payload: an invalid snapshot
    // must fail the publish with the previous generation untouched.
    let index_bytes = publication.snapshot.to_bytes()?;

    let vectors = match publication.vectors {
        Some(set) => {
            let (bytes, artifact) = set.artifact();
            let path = dir.join(VECTORS_FILE);
            let temp = write_temp(dir, &bytes)?;
            hook(Stage::VectorsWritten);
            persist(temp, &path)?;
            hook(Stage::VectorsRenamed);
            sync_dir(dir)?;
            hook(Stage::VectorsDirSynced);
            Some(artifact)
        }
        // The degraded case removes any existing payload *after* the manifest
        // rename, below; deleting it here would strand the old manifest.
        None => None,
    };

    let index_path = dir.join(INDEX_FILE);
    let temp = write_temp(dir, &index_bytes)?;
    hook(Stage::IndexWritten);
    persist(temp, &index_path)?;
    hook(Stage::IndexRenamed);
    sync_dir(dir)?;
    hook(Stage::IndexDirSynced);

    let manifest = Manifest::new(
        publication.root,
        publication.snapshot,
        publication.chunker,
        Payloads {
            index_sha: blake3::hash(&index_bytes).to_hex().to_string(),
            index_len: index_bytes.len() as u64,
            vectors,
        },
        publication.built_at,
        publication.corpus_sig,
    );
    let manifest_path = dir.join(MANIFEST_FILE);
    let json = serde_json::to_vec_pretty(&manifest).map_err(|error| {
        let reason = format!("cannot encode {}: {error}", manifest_path.display());
        JanusError::Io(io::Error::new(io::ErrorKind::InvalidData, reason))
    })?;
    let temp = write_temp(dir, &json)?;
    hook(Stage::ManifestWritten);
    persist(temp, &manifest_path)?;
    hook(Stage::ManifestRenamed);
    sync_dir(dir)?;
    hook(Stage::ManifestDirSynced);

    // Now the new commit record says `embedder: None`, so no reader opens the
    // file. An orphan left by a crash between the two renames is unreachable
    // and is overwritten by the next vectors-bearing publish.
    if publication.vectors.is_none() && drop_vectors(dir, publication.vectors_dropped)? {
        hook(Stage::VectorsDropped);
    }
    Ok(manifest)
}

/// Removes a previous `vectors.f32` when this build cannot carry its rows (§3.6).
///
/// The caller has already re-evaluated §3.4's predicate under the previous
/// manifest's embedder id; `documents` is how many failed it. A build that can
/// carry every row publishes `Some(vectors)` and never reaches here. On drop the
/// manifest has just been renamed with all three vector fields `None`, so no
/// reader can open the file this removes.
///
/// Returns whether a payload was actually removed, which is what the
/// [`Stage::VectorsDropped`] seam reports.
fn drop_vectors(dir: &Path, documents: Option<usize>) -> Result<bool, JanusError> {
    let path = dir.join(VECTORS_FILE);
    match fs::remove_file(&path) {
        Ok(()) => {
            sync_dir(dir)?;
            match documents {
                Some(changed) => tracing::warn!(
                    "vectors dropped: {changed} document(s) changed and this build cannot embed"
                ),
                None => tracing::warn!(
                    "vectors dropped: this build has no embedder, so {VECTORS_FILE} cannot be rebuilt"
                ),
            }
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error(&path, source)),
    }
}

/// Writes `bytes` to a uniquely named temp file in `dir` and syncs it.
fn write_temp(dir: &Path, bytes: &[u8]) -> Result<NamedTempFile, JanusError> {
    let mut temp = NamedTempFile::new_in(dir).map_err(|source| write_error(dir, source))?;
    temp.write_all(bytes)
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|source| write_error(temp.path(), source))?;
    Ok(temp)
}

/// Atomically replaces `target` with the temp file.
fn persist(temp: NamedTempFile, target: &Path) -> Result<(), JanusError> {
    temp.persist(target)
        .map(|_file| ())
        .map_err(|error| write_error(target, error.error))
}

/// Removes leftover `.tmp*` files from crashed writers.
fn sweep_temps(dir: &Path) -> Result<(), JanusError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_error(dir, source)),
    };
    for entry in entries {
        let entry = entry.map_err(|source| io_error(dir, source))?;
        if !entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_file()) {
            let path = entry.path();
            fs::remove_file(&path).map_err(|source| io_error(&path, source))?;
        }
    }
    Ok(())
}

/// Syncs a directory so both renames are durable (§3.5, §8.7).
fn sync_dir(dir: &Path) -> Result<(), JanusError> {
    match File::open(dir).and_then(|file| file.sync_all()) {
        Ok(()) => Ok(()),
        // Windows cannot open a directory as a file; the renames stay atomic.
        #[cfg(unix)]
        Err(source) => Err(write_error(dir, source)),
        #[cfg(not(unix))]
        Err(_) => Ok(()),
    }
}

fn write_error(path: &Path, source: io::Error) -> JanusError {
    JanusError::Write {
        path: path.to_path_buf(),
        source,
    }
}

/// An IO failure whose message still names the path that failed (AGENTS.md).
fn io_error(path: &Path, source: io::Error) -> JanusError {
    let reason = format!("{}: {source}", path.display());
    JanusError::Io(io::Error::new(source.kind(), reason))
}

impl From<Corrupt> for JanusError {
    fn from(corrupt: Corrupt) -> Self {
        Self::Io(io::Error::new(io::ErrorKind::InvalidData, corrupt.reason))
    }
}

// ───────────────────────────────── reader ──────────────────────────────────

/// What a manifest must agree with before its bytes are trusted (§3.5, S-8).
struct Expected {
    format_version: u16,
    tokenizer: TokenizerFingerprint,
    chunker: ChunkingFingerprint,
    corpus_id: String,
    built_root: String,
    /// The id this process would embed with, or `None` when it cannot embed.
    embedder: Option<EmbedderId>,
}

impl Expected {
    fn new(root: &Path, config: &Config, embedder: Option<&dyn Embedder>) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            tokenizer: Tokenizer::new().fingerprint(),
            chunker: ChunkingFingerprint {
                max_chars: config.chunking.max_chars,
                overlap: config.chunking.overlap,
            },
            corpus_id: config.corpus_id.clone(),
            built_root: built_root(root),
            embedder: embedder.map(|embedder| embedder.id().clone()),
        }
    }

    /// Why this manifest does or does not describe us, and what a read may do.
    ///
    /// The five fields that change the identity of every chunk or every row are
    /// compared **first**. A moved vault whose generation has no vectors must
    /// report `Refresh`, not `Refuse`: the reverse order would send a read to a
    /// cold build that re-embeds the whole corpus (M4D §3.3, D-3a).
    ///
    /// The embedder comparison is V-21's **ordering**, not an equality: a
    /// generation that has vectors is acceptable to a process that cannot embed
    /// (it holds strictly more information than that process reads), while a
    /// generation without vectors is stale for a process that can embed, so
    /// vectors appear without a manual `--rebuild`. Plain equality would let a
    /// feature-off binary and a featured one delete each other's vectors forever.
    fn check(&self, manifest: &Manifest) -> Check {
        let cold = [
            (
                MismatchField::FormatVersion,
                manifest.format_version.to_string(),
                self.format_version.to_string(),
            ),
            (
                MismatchField::Tokenizer,
                tokenizer_name(&manifest.tokenizer),
                tokenizer_name(&self.tokenizer),
            ),
            (
                MismatchField::Chunker,
                chunker_name(manifest.chunker),
                chunker_name(self.chunker),
            ),
            (
                MismatchField::CorpusId,
                manifest.corpus_id.clone(),
                self.corpus_id.clone(),
            ),
        ];
        for (field, found, wanted) in cold {
            if found != wanted {
                return Check::Refuse(Mismatch {
                    field,
                    found,
                    wanted,
                });
            }
        }

        match (&manifest.embedder, &self.embedder) {
            // Row 6: strictly more information than this process reads.
            (Some(_), None) => {}
            // Row 5: the rows are missing, so a build must add them.
            (None, Some(want)) => {
                return Check::Refuse(Mismatch {
                    field: MismatchField::Embedder,
                    found: NO_VECTORS.to_owned(),
                    wanted: want.name.clone(),
                });
            }
            (Some(have), Some(want)) if have != want => {
                return Check::Refuse(Mismatch {
                    field: MismatchField::Embedder,
                    found: embedder_name(have),
                    wanted: embedder_name(want),
                });
            }
            _ => {}
        }

        // Diagnostic only: every `DocId` is unchanged, so the generation still
        // loads and a refresh rewrites the manifest (D-3a, SPEC §8.3).
        if manifest.built_root != self.built_root {
            return Check::Refresh(Mismatch {
                field: MismatchField::BuiltRoot,
                found: manifest.built_root.clone(),
                wanted: self.built_root.clone(),
            });
        }
        Check::Ok
    }
}

/// The manifest field that did not agree with this process (M4D §2, §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MismatchField {
    /// The snapshot layout changed.
    FormatVersion,
    /// The tokenizer changed, so every posting changes.
    Tokenizer,
    /// The chunker changed, so every chunk boundary changes.
    Chunker,
    /// The corpus identity changed, so every `DocId` changes.
    CorpusId,
    /// The root recorded in the manifest is not this root. Diagnostic only.
    BuiltRoot,
    /// The embedder changed, or the rows it needs are absent.
    Embedder,
}

impl MismatchField {
    /// The field name the manifest and the old messages use.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::FormatVersion => "format_version",
            Self::Tokenizer => "tokenizer",
            Self::Chunker => "chunker",
            Self::CorpusId => "corpus_id",
            Self::BuiltRoot => "built_root",
            Self::Embedder => "embedder",
        }
    }

    /// True when only a cold build can cure this field (M4D D-3).
    #[must_use]
    pub fn needs_full_build(self) -> bool {
        self != Self::BuiltRoot
    }
}

/// The `found` value of the embedder row that reports absent rows.
///
/// The two embedder rows read differently, and M3's wording is part of the
/// contract, so one sentinel keeps both texts (M4D §3.3).
const NO_VECTORS: &str = "no vectors";

/// One disagreement between a manifest and this process (M4D §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    /// Which field disagreed.
    pub field: MismatchField,
    /// What the manifest says.
    pub found: String,
    /// What this process wants.
    pub wanted: String,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.field, self.found.as_str()) {
            (MismatchField::Embedder, NO_VECTORS) => write!(
                f,
                "index has no vectors and this build embeds with `{}`; rebuilding to add them",
                self.wanted
            ),
            (MismatchField::Embedder, found) => {
                write!(f, "embedder `{found}` != `{}`", self.wanted)
            }
            (field, found) => write!(f, "{} {found} != {}", field.name(), self.wanted),
        }
    }
}

/// The three outcomes of validation (M4D §2, §3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// The manifest describes this process and this corpus.
    Ok,
    /// The generation is usable, but the manifest must be rewritten.
    Refresh(Mismatch),
    /// Only a cold build can cure this. A read refuses.
    Refuse(Mismatch),
}

impl Check {
    /// D-3: true when a read must refuse instead of building.
    #[must_use]
    pub fn needs_full_build(&self) -> bool {
        match self {
            Self::Refuse(mismatch) => mismatch.field.needs_full_build(),
            Self::Ok | Self::Refresh(_) => false,
        }
    }

    /// The mismatch that carried this outcome, if any.
    #[must_use]
    pub fn mismatch(&self) -> Option<&Mismatch> {
        match self {
            Self::Ok => None,
            Self::Refresh(mismatch) | Self::Refuse(mismatch) => Some(mismatch),
        }
    }
}

/// The reason a manifest's three vector fields disagree, or `None`.
///
/// This is a corrupt manifest, not a mismatch with this process, so it stays on
/// [`Attempt::Stale`]: a rebuild repairs it (M4D §3.3 row 4).
fn vector_fields_reason(manifest: &Manifest) -> Option<String> {
    let vectors = (
        manifest.embedder.is_some(),
        manifest.vectors_sha.is_some(),
        manifest.vectors_len.is_some(),
    );
    if vectors == (true, true, true) || vectors == (false, false, false) {
        return None;
    }
    Some(format!(
        "manifest vector fields disagree: embedder {}, vectors_sha {}, vectors_len {}",
        present(vectors.0),
        present(vectors.1),
        present(vectors.2)
    ))
}

fn present(yes: bool) -> &'static str {
    if yes { "present" } else { "absent" }
}

fn embedder_name(id: &EmbedderId) -> String {
    format!("{}/{}/{}", id.name, id.dim, id.dtype)
}

fn tokenizer_name(fingerprint: &TokenizerFingerprint) -> String {
    let stemmer = match fingerprint.stemmer {
        Stemmer::None => "none",
    };
    format!("{}/{stemmer}", fingerprint.algorithm)
}

fn chunker_name(chunker: ChunkingFingerprint) -> String {
    format!("{}@{}", chunker.max_chars, chunker.overlap)
}

/// A validated pair: the bytes hashed to the manifest that names them.
struct Generation {
    manifest: Manifest,
    snapshot: Snapshot,
    /// The rows, present only when the read asked for them (V-4).
    vectors: Option<Vectors>,
    /// A disagreement the generation survives, such as a moved root. The
    /// caller repairs the manifest and answers from these bytes (M4D §3.3).
    refresh: Option<Mismatch>,
}

/// The result of one validated-read attempt (§3.5 steps 1–4).
enum Attempt {
    /// The manifest and the bytes it names agree. This is a generation.
    Ready(Box<Generation>),
    /// The pair did not agree; a publisher may be between two renames. `path`
    /// names the file that failed, so a refusal can name it too.
    Retry { path: PathBuf, reason: String },
    /// Nothing retrying can fix: absent, unreadable, or the manifest's vector
    /// fields disagree.
    Stale(String),
    /// Nothing retrying can fix either, and only a cold build can cure it.
    /// `reason` is the mismatch, in the words M3 used (M4D §3.3, D-3).
    NeedsFullBuild(String),
}

/// Reads one file, distinguishing absence from a real IO failure.
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, JanusError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error(path, source)),
    }
}

/// One lock-free read, validation included.
fn read_generation(
    dir: &Path,
    expected: &Expected,
    want_vectors: bool,
) -> Result<Attempt, JanusError> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let Some(manifest_bytes) = read_optional(&manifest_path)? else {
        return Ok(Attempt::Stale(format!(
            "{} is missing",
            manifest_path.display()
        )));
    };
    let manifest: Manifest = match serde_json::from_slice(&manifest_bytes) {
        Ok(manifest) => manifest,
        Err(error) => {
            let reason = format!("{} is unreadable: {error}", manifest_path.display());
            return Ok(Attempt::Stale(reason));
        }
    };
    // A manifest holding two of the three vector facts is corrupt, not a
    // mismatch with this process, so a rebuild repairs it.
    if let Some(reason) = vector_fields_reason(&manifest) {
        return Ok(Attempt::Stale(reason));
    }
    let refresh = match expected.check(&manifest) {
        Check::Ok => None,
        Check::Refresh(mismatch) => Some(mismatch),
        Check::Refuse(mismatch) => return Ok(Attempt::NeedsFullBuild(mismatch.to_string())),
    };

    let index_path = dir.join(INDEX_FILE);
    let Some(bytes) = read_optional(&index_path)? else {
        return Ok(Attempt::Stale(format!(
            "{} is missing",
            index_path.display()
        )));
    };
    if blake3::hash(&bytes).to_hex().to_string() != manifest.index_sha {
        return Ok(retry(&index_path, "does not match manifest.index_sha"));
    }
    let snapshot = match Snapshot::from_bytes(&bytes) {
        Ok(snapshot) => snapshot,
        Err(corrupt) => return Ok(retry(&index_path, &corrupt.reason)),
    };

    // §3.1: the payload is read, hashed and decoded against the manifest's
    // shape, or not opened at all. A lexical-only read cannot observe a corrupt
    // vectors file and must not pay for learning something it will not use (V-4).
    let vectors = if want_vectors {
        match read_vectors(dir, &manifest, snapshot.chunks.len())? {
            Ok(vectors) => vectors,
            Err((path, reason)) => return Ok(retry(&path, &reason)),
        }
    } else {
        None
    };

    Ok(Attempt::Ready(Box::new(Generation {
        manifest,
        snapshot,
        vectors,
        refresh,
    })))
}

/// A retry cause that names the file whose bytes did not validate.
fn retry(path: &Path, reason: &str) -> Attempt {
    Attempt::Retry {
        path: path.to_path_buf(),
        reason: format!("{} {reason}", path.display()),
    }
}

/// Reads `vectors.f32` and validates it against the manifest (§3.1).
///
/// `Ok(None)` means the generation has no vectors. `Err((path, reason))` is a
/// mismatch, which the caller treats like any other: retry, then rebuild.
fn read_vectors(
    dir: &Path,
    manifest: &Manifest,
    chunks: usize,
) -> Result<Result<Option<Vectors>, (PathBuf, String)>, JanusError> {
    // `Expected::check` guarantees all three fields agree, so a manifest with
    // `vectors_sha` always has the id that supplies `dim`.
    let Some(id) = manifest.embedder.as_ref() else {
        return Ok(Ok(None));
    };
    let path = dir.join(VECTORS_FILE);
    let Some(bytes) = read_optional(&path)? else {
        return Ok(Err((path, "is missing".to_owned())));
    };
    let sha = blake3::hash(&bytes).to_hex().to_string();
    if manifest.vectors_sha.as_ref() != Some(&sha) {
        return Ok(Err((
            path,
            "does not match manifest.vectors_sha".to_owned(),
        )));
    }
    if manifest.vectors_len != Some(bytes.len() as u64) {
        return Ok(Err((
            path,
            format!(
                "is {} bytes but manifest.vectors_len is {}",
                bytes.len(),
                manifest
                    .vectors_len
                    .map_or_else(|| "absent".to_owned(), |len| len.to_string())
            ),
        )));
    }
    match Vectors::decode(&bytes, id.dim, chunks) {
        Ok(vectors) => Ok(Ok(Some(vectors))),
        Err(corrupt) => Ok(Err((path, corrupt.to_string()))),
    }
}

/// What a read may do when no generation validates (S-8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Build from the vault and publish it under the writer lock.
    Rebuild,
    /// Refuse with [`JanusError::IndexUnstable`] and write nothing.
    Refuse,
}

/// The inputs a load needs that are not on disk.
#[derive(Clone, Copy)]
pub struct LoadOptions<'a> {
    /// The corpus root: the manifest's `built_root` and the rebuild's input.
    pub root: &'a Path,
    /// The resolved config: corpus id, chunking, and the index location.
    pub config: &'a Config,
    /// Injected build time for a manifest this call writes (I-3).
    pub built_at: &'a str,
    /// How long a recovering reader waits for the writer lock (S-7).
    pub lock_timeout: Duration,
    /// Whether a read may rebuild (S-8).
    pub policy: Policy,
    /// Whether to read and validate `vectors.f32` (V-4).
    ///
    /// False for a lexical-only read: the file is not opened, hashed or
    /// validated, because a read that will not use a row must not pay for it.
    pub want_vectors: bool,
    /// The embedder resolved once at the edge, or `None` when this build cannot
    /// embed (V-25). It decides V-21's ordering and, on a rebuild, what the
    /// published generation contains.
    pub embedder: Option<&'a dyn Embedder>,
}

impl<'a> LoadOptions<'a> {
    /// The defaults: [`LOCK_TIMEOUT`], [`Policy::Rebuild`], and a lexical-only
    /// read with no embedder.
    #[must_use]
    pub fn new(root: &'a Path, config: &'a Config, built_at: &'a str) -> Self {
        Self {
            root,
            config,
            built_at,
            lock_timeout: LOCK_TIMEOUT,
            policy: Policy::Rebuild,
            want_vectors: false,
            embedder: None,
        }
    }
}

impl fmt::Debug for LoadOptions<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadOptions")
            .field("root", &self.root)
            .field("config", &self.config)
            .field("built_at", &self.built_at)
            .field("lock_timeout", &self.lock_timeout)
            .field("policy", &self.policy)
            .field("want_vectors", &self.want_vectors)
            .field("embedder", &self.embedder.map(|embedder| embedder.id()))
            .finish()
    }
}

/// A validated generation, plus what the read that produced it did (§3.5).
#[derive(Debug)]
pub struct Load {
    /// The snapshot whose bytes hashed to [`Load::manifest`]'s `index_sha`.
    pub snapshot: Snapshot,
    /// The manifest those bytes were validated against.
    pub manifest: Manifest,
    /// The validated rows, when the caller wanted them and the generation has
    /// them (§3.1). `None` for every lexical-only read.
    pub vectors: Option<Vectors>,
    /// True when this call built and published the generation.
    pub rebuilt: bool,
    /// What a build reused; all zero when the generation was adopted.
    pub stats: BuildStats,
    /// Why a generation on disk was rejected (S-8); `None` when one validated.
    pub stale: Option<String>,
    /// A disagreement this generation survives, so the caller must rewrite the
    /// manifest and may still answer (M4D §3.3 row 6, D-3a). `None` otherwise.
    pub refresh_request: Option<Mismatch>,
}

impl Load {
    /// The outcome when an existing generation validated.
    fn adopted(generation: Generation, stale: Option<String>) -> Self {
        Self {
            snapshot: generation.snapshot,
            manifest: generation.manifest,
            vectors: generation.vectors,
            rebuilt: false,
            stats: BuildStats::default(),
            stale,
            refresh_request: generation.refresh,
        }
    }
}

/// Reads a validated generation, rebuilding once under the writer lock if none
/// validates and [`Policy`] allows it (§3.5). A healthy read takes no lock;
/// recovery does, and may wait for an active publisher. Errors:
/// [`JanusError::IndexUnstable`] under [`Policy::Refuse`], naming the file that
/// failed (`.janus/index.bin`, `.janus/vectors.f32` or the manifest),
/// [`JanusError::IndexNeedsFullBuild`] when only a cold build can cure the
/// manifest and a rebuild is allowed (M4D D-3),
/// [`JanusError::LockTimeout`] if recovery cannot get the lock, and IO failures.
pub fn load(options: &LoadOptions<'_>) -> Result<Load, JanusError> {
    let dir = config::resolve_index_dir(options.root, &options.config.index_dir);
    let expected = Expected::new(options.root, options.config, options.embedder);

    let mut ready = None;
    let mut last = None;
    // A refusal names the file that failed. A retry says which payload it was;
    // a stale manifest keeps M3's answer, `index.bin`, because the generation it
    // describes is what could not be used.
    let mut failed = dir.join(INDEX_FILE);
    let mut retries = 0;
    loop {
        match read_generation(&dir, &expected, options.want_vectors)? {
            Attempt::Ready(generation) => {
                ready = Some(generation);
                break;
            }
            // A cold mismatch is not a retry: nothing a publisher does cures it.
            Attempt::NeedsFullBuild(reason) => {
                if options.policy == Policy::Refuse {
                    return Err(JanusError::IndexUnstable { path: failed });
                }
                return Err(JanusError::IndexNeedsFullBuild { reason });
            }
            Attempt::Stale(reason) => {
                last = Some(reason);
                break;
            }
            Attempt::Retry { path, reason } => {
                failed = path;
                last = Some(reason);
            }
        }
        let Some(backoff) = RETRY_BACKOFF.get(retries) else {
            break;
        };
        retries += 1;
        thread::sleep(*backoff);
    }

    if let Some(generation) = ready {
        return Ok(Load::adopted(*generation, last));
    }
    if options.policy == Policy::Refuse {
        return Err(JanusError::IndexUnstable { path: failed });
    }

    let lock = Lock::acquire(&dir, options.lock_timeout)?;
    // Double-checked freshness (S-7): another process may have published while
    // we waited. Adopting it is what makes two auto-builds cost one build.
    if let Attempt::Ready(generation) = read_generation(&dir, &expected, options.want_vectors)? {
        return Ok(Load::adopted(*generation, last));
    }

    let walk = vault::walk(options.root, options.config, WalkOptions::default())?;
    let outcome = build_until_stable(
        &dir,
        options.root,
        options.config,
        options.embedder,
        options.built_at,
        walk,
        &BTreeSet::new(),
        false,
        &lock,
    )?;
    // A read that did not want vectors does not receive them, even though the
    // rebuild it triggered may have written them.
    let vectors = if options.want_vectors {
        outcome.plan.vectors.as_ref().map(|set| set.rows().clone())
    } else {
        None
    };
    let stats = outcome.plan.stats;
    Ok(Load {
        snapshot: outcome.plan.snapshot,
        manifest: outcome.manifest,
        vectors,
        rebuilt: true,
        stats,
        stale: last,
        refresh_request: None,
    })
}

/// How many passes one build may take before it reports an incomplete refresh
/// (§3.2). A corpus under a continuous writer cannot converge, and one line is
/// better than a promise the build cannot keep.
pub const MAX_PASSES: u32 = 3;

/// What the write loop produced, described by the pass that finished.
#[derive(Debug)]
pub struct BuildOutcome {
    /// The last pass's plan: its snapshot and stats are what was published.
    pub plan: BuildPlan,
    /// The manifest the last pass published.
    pub manifest: Manifest,
    /// How many passes ran. One on a quiet corpus, more when it changed.
    pub passes: u32,
}

/// Plans, publishes, re-walks, and repeats while the corpus keeps changing
/// (§3.2, D-13, D-14).
///
/// The digest in the manifest describes the walk that produced the plan, so an
/// edit made during the build would otherwise be invisible: the published digest
/// would say the corpus is fresh while the file is missing from the index. The
/// loop closes that window with no stored clock and no second payload version.
///
/// The caller holds `lock` and owns `first_walk`; this function publishes under
/// that one lock for every pass. It prints the incomplete line itself, because
/// both writers reach this one function.
///
/// # Errors
///
/// Any walk, resolve, plan, encode or publication failure. A failed pass
/// publishes nothing, so the previous generation stays readable.
// Every argument is one input the loop reads: the writers' two neighbours, the
// walk, its digest inputs, and the lock the caller already holds. Grouping them
// into a struct would make one call site read `request.request`.
#[allow(clippy::too_many_arguments)]
pub fn build_until_stable(
    dir: &Path,
    root: &Path,
    config: &Config,
    embedder: Option<&dyn Embedder>,
    built_at: &str,
    first_walk: vault::WalkResult,
    forced: &BTreeSet<DocId>,
    rebuild: bool,
    lock: &Lock,
) -> Result<BuildOutcome, JanusError> {
    let mut walk = first_walk;
    let mut passes = 0;
    loop {
        passes += 1;
        let signature = refresh::Signature::of(&walk.files);
        let plan = refresh::plan_from_walk(config, &walk, dir, embedder, forced, rebuild)?;
        let digest = signature.digest();
        let publication = Publication {
            dir,
            root,
            snapshot: &plan.snapshot,
            chunker: plan.chunker,
            vectors: plan.vectors.as_ref(),
            vectors_dropped: plan.vectors_dropped,
            built_at,
            corpus_sig: Some(&digest),
        };
        let manifest = publish_locked(&publication, &mut |_| {}, lock)?;

        let next = vault::walk(root, config, WalkOptions::default())?;
        let stable = refresh::Signature::of(&next.files).digest() == digest;
        if stable || passes >= MAX_PASSES {
            if !stable {
                eprintln!("refresh: incomplete, corpus changed during the build");
            }
            return Ok(BuildOutcome {
                plan,
                manifest,
                passes,
            });
        }
        walk = next;
    }
}

/// The previous snapshot and its validated rows, if both are usable (§3.4).
///
/// A corrupt snapshot is no reuse source. The rows are paired with the manifest
/// only when that manifest names the snapshot's exact bytes: a crash can leave a
/// new `index.bin` beside an old manifest, and pairing those rows with this
/// snapshot would misalign every row. Any decode failure means `None`, which
/// makes the build re-embed everything.
pub(crate) fn previous_generation(dir: &Path) -> Option<build::Previous> {
    let index_bytes = fs::read(dir.join(INDEX_FILE)).ok()?;
    let snapshot = Snapshot::from_bytes(&index_bytes).ok()?;
    let manifest_bytes = fs::read(dir.join(MANIFEST_FILE)).ok()?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes).ok()?;
    if manifest.index_sha != blake3::hash(&index_bytes).to_hex().to_string() {
        return Some(build::Previous::tokens(snapshot));
    }
    let vectors = match (
        manifest.embedder.as_ref(),
        manifest.vectors_sha.as_ref(),
        manifest.vectors_len,
    ) {
        (Some(id), Some(sha), Some(len)) => {
            let Ok(bytes) = fs::read(dir.join(VECTORS_FILE)) else {
                return Some(build::Previous::tokens(snapshot));
            };
            if bytes.len() as u64 != len || blake3::hash(&bytes).to_hex().to_string() != *sha {
                None
            } else {
                Vectors::decode(&bytes, id.dim, snapshot.chunks.len())
                    .ok()
                    .map(|rows| (id.clone(), rows))
            }
        }
        _ => None,
    };
    Some(match vectors {
        Some((id, rows)) => build::Previous::with_vectors(snapshot, id, rows),
        None => build::Previous::tokens(snapshot),
    })
}

/// The persisted fingerprint names only known algorithms and stemmers (I-1).
impl Serialize for TokenizerFingerprint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        (self.algorithm, "none").serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TokenizerFingerprint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (algorithm, stemmer) = <(String, String)>::deserialize(deserializer)?;
        if algorithm != "v1" || stemmer != "none" {
            return Err(D::Error::custom(format!(
                "unknown tokenizer fingerprint {algorithm}/{stemmer}"
            )));
        }
        Ok(Self {
            algorithm: Tokenizer::new().algorithm(),
            stemmer: Stemmer::None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{BLAKE3_LEN, Blake3Hash, ChunkId, DOC_ID_LEN, DocId};
    use crate::index::{ChunkingFingerprint, StatusCode, StringId};
    use crate::tokenize::Tokenizer;
    use proptest::prelude::*;

    fn sample_snapshot() -> Snapshot {
        let index = LexicalIndex::build(&["alpha beta", "beta"]);
        let doc = DocMeta {
            id: DocId::from_bytes([1; DOC_ID_LEN]),
            rel_path: StringId(0),
            title: StringId(1),
            type_name: StringId(0),
            status: StatusCode::Active,
            scope: StringId(0),
            shared_with: Vec::new(),
            tags: Vec::new(),
            date: None,
            date_source: crate::document::DateSource::None,
            effective_from: None,
            timeless: false,
            content_hash: Blake3Hash::of_bytes(b"body"),
            body_hash: Blake3Hash::of_bytes(b"body"),
            lex_key: [0; 32],
            chunk_range: (0, 2),
        };
        let chunks = vec![
            ChunkMeta {
                id: ChunkId::from_bytes([1; BLAKE3_LEN]),
                doc: 0,
                span: (0, 5),
                heading_path: Vec::new(),
                tokens: 2,
            },
            ChunkMeta {
                id: ChunkId::from_bytes([2; BLAKE3_LEN]),
                doc: 0,
                span: (6, 10),
                heading_path: Vec::new(),
                tokens: 1,
            },
        ];
        Snapshot::from_index(
            &index,
            "main".to_owned(),
            Tokenizer::new().fingerprint(),
            vec![doc],
            chunks,
        )
    }

    fn reject(snapshot: &Snapshot, needle: &str) {
        let error = snapshot.validate().expect_err("snapshot must be rejected");
        assert!(
            error.reason.contains(needle),
            "reason {:?} did not name {needle:?}",
            error.reason
        );
    }

    #[test]
    fn snapshot_bytes_round_trip_and_keep_scores() {
        let snapshot = sample_snapshot();
        let before = snapshot.to_index().expect("sample is valid");
        let bytes = snapshot.to_bytes().expect("encode");
        let decoded = Snapshot::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, snapshot);

        let query = ["alpha", "beta"];
        assert_eq!(
            before.score_query(&query),
            decoded.to_index().expect("valid").score_query(&query)
        );
    }

    #[test]
    fn bad_string_id_is_rejected() {
        let mut snapshot = sample_snapshot();
        snapshot.docs[0].title = StringId(99);
        reject(&snapshot, "StringId 99");
    }

    #[test]
    fn inverted_postings_range_is_rejected() {
        let mut snapshot = sample_snapshot();
        snapshot.terms[0].postings = (1, 0);
        reject(&snapshot, "postings 1..0");
    }

    #[test]
    fn out_of_order_chunk_range_is_rejected() {
        let mut snapshot = sample_snapshot();
        let mut second = snapshot.docs[0].clone();
        second.id = DocId::from_bytes([2; DOC_ID_LEN]);
        second.chunk_range = (0, 1);
        snapshot.docs.push(second);
        reject(&snapshot, "chunk_range start 0 before 2");
    }

    #[test]
    fn non_finite_avgdl_is_rejected() {
        let mut snapshot = sample_snapshot();
        snapshot.avgdl = f64::NAN;
        reject(&snapshot, "avgdl NaN");
    }

    #[test]
    fn wrong_format_version_is_rejected() {
        let mut snapshot = sample_snapshot();
        snapshot.format_version = FORMAT_VERSION + 1;
        reject(&snapshot, "format_version");
    }

    #[test]
    fn manifest_is_human_readable_and_a_generation_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let snapshot = sample_snapshot();
        let index_dir = dir.path().join(".janus");
        let publication = Publication {
            dir: &index_dir,
            root: dir.path(),
            snapshot: &snapshot,
            chunker: ChunkingFingerprint {
                max_chars: 1800,
                overlap: 0.15,
            },
            vectors: None,
            vectors_dropped: None,
            built_at: "2025-04-01T00:00:00Z",
            corpus_sig: None,
        };
        let manifest = publish(&publication).expect("publish");

        let json: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(index_dir.join("manifest.json")).expect("read manifest"),
        )
        .expect("manifest is JSON");
        assert_eq!(json["format_version"], FORMAT_VERSION);
        assert_eq!(json["docs"], 1);
        assert_eq!(json["index_sha"], manifest.index_sha);

        let read = fs::read(index_dir.join("index.bin")).expect("read index");
        assert_eq!(Snapshot::from_bytes(&read).expect("decode"), snapshot);
    }

    /// An `Expected` that agrees with `manifest`, except for the root it names.
    fn expected_for(manifest: &Manifest, built_root: &str) -> Expected {
        Expected {
            format_version: FORMAT_VERSION,
            tokenizer: Tokenizer::new().fingerprint(),
            chunker: manifest.chunker,
            corpus_id: manifest.corpus_id.clone(),
            built_root: built_root.to_owned(),
            embedder: None,
        }
    }

    /// One published manifest: no vectors, a known root, and `sample_snapshot`.
    fn published_manifest(dir: &Path) -> Manifest {
        let index_dir = dir.join(".janus");
        let snapshot = sample_snapshot();
        let publication = Publication {
            dir: &index_dir,
            root: dir,
            snapshot: &snapshot,
            chunker: ChunkingFingerprint {
                max_chars: 1800,
                overlap: 0.15,
            },
            vectors: None,
            vectors_dropped: None,
            built_at: "2025-04-01T00:00:00Z",
            corpus_sig: None,
        };
        publish(&publication).expect("publish")
    }

    // M4D P2 item 1 and D-3: all five identity-changing fields refuse, and only
    // `built_root` refreshes. The message text is M3's, unchanged.
    #[test]
    fn check_refuses_each_cold_field_and_refreshes_only_a_moved_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = published_manifest(dir.path());
        let expected = expected_for(&manifest, &manifest.built_root);
        assert_eq!(expected.check(&manifest), Check::Ok);

        let mut cases: Vec<(Manifest, MismatchField, &str)> = Vec::new();

        let mut patched = manifest.clone();
        patched.format_version = FORMAT_VERSION + 1;
        cases.push((
            patched,
            MismatchField::FormatVersion,
            "format_version 4 != 3",
        ));

        let mut patched = manifest.clone();
        patched.tokenizer = TokenizerFingerprint {
            algorithm: "v2",
            stemmer: Stemmer::None,
        };
        cases.push((
            patched,
            MismatchField::Tokenizer,
            "tokenizer v2/none != v1/none",
        ));

        let mut patched = manifest.clone();
        patched.chunker = ChunkingFingerprint {
            max_chars: 1700,
            overlap: 0.15,
        };
        cases.push((
            patched,
            MismatchField::Chunker,
            "chunker 1700@0.15 != 1800@0.15",
        ));

        let mut patched = manifest.clone();
        patched.corpus_id = "other".to_owned();
        cases.push((patched, MismatchField::CorpusId, "corpus_id other != main"));

        // A generation without vectors, read by a process that can embed. This
        // is the row that would re-embed every chunk, so it must refuse.
        let embedding = Expected {
            embedder: Some(EmbedderId {
                name: "hash-8".to_owned(),
                dim: 8,
                dtype: "f32".to_owned(),
                normalisation: "l2".to_owned(),
            }),
            ..expected_for(&manifest, &manifest.built_root)
        };
        cases.push((
            manifest.clone(),
            MismatchField::Embedder,
            "index has no vectors and this build embeds with `hash-8`; rebuilding to add them",
        ));

        for (patched, field, text) in &cases {
            let check = if *field == MismatchField::Embedder {
                embedding.check(patched)
            } else {
                expected.check(patched)
            };
            assert!(check.needs_full_build(), "{field:?} must refuse");
            let got = check.mismatch().expect("a mismatch");
            assert_eq!(got.field, *field);
            assert_eq!(&got.to_string(), text, "{field:?} keeps M3's wording");
        }

        // The root is diagnostic: the generation loads and the manifest is
        // rewritten. `built_root` is compared **last**, so a moved vault with no
        // vectors refuses instead of re-embedding every chunk (D-3a).
        let moved = expected_for(&manifest, "/elsewhere");
        let check = moved.check(&manifest);
        assert!(!check.needs_full_build());
        assert_eq!(
            check.mismatch().map(|mismatch| mismatch.field),
            Some(MismatchField::BuiltRoot)
        );
        let both = embedding.check(&Manifest {
            built_root: "/elsewhere".to_owned(),
            ..manifest.clone()
        });
        assert_eq!(
            both.mismatch().map(|mismatch| mismatch.field),
            Some(MismatchField::Embedder),
            "a cold field must win over the root, or a read embeds everything"
        );
    }

    #[test]
    fn a_vector_set_refuses_rows_whose_width_disagrees_with_the_embedder() {
        let id = EmbedderId {
            name: "hash-4".to_owned(),
            dim: 4,
            dtype: "f32".to_owned(),
            normalisation: "l2".to_owned(),
        };
        let rows = Vectors::new(2, vec![1.0, 0.0]).expect("one unit row of width 2");
        let error = VectorSet::new(id, rows).expect_err("the widths disagree");

        assert!(error.reason.contains("hash-4"), "{}", error.reason);
        assert!(error.reason.contains("dim"), "{}", error.reason);
    }

    proptest! {
        #[test]
        fn arbitrary_term_content_survives_the_round_trip(
            chunks in prop::collection::vec(
                prop::collection::vec("[a-z]{1,8}", 1..5),
                1..6,
            )
        ) {
            let texts: Vec<String> = chunks
                .iter()
                .map(|words| words.join(" "))
                .collect();
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            let index = LexicalIndex::build(&refs);
            let docs = vec![DocMeta {
                id: DocId::from_bytes([1; DOC_ID_LEN]),
                rel_path: StringId(0),
                title: StringId(0),
                type_name: StringId(0),
                status: StatusCode::Active,
                scope: StringId(0),
                shared_with: Vec::new(),
                tags: Vec::new(),
                date: None,
                date_source: crate::document::DateSource::None,
                effective_from: None,
                timeless: false,
                content_hash: Blake3Hash::of_bytes(b"body"),
                body_hash: Blake3Hash::of_bytes(b"body"),
                lex_key: [0; 32],
                chunk_range: (0, index.len() as u32),
            }];
            let chunks_meta = (0..index.len())
                .map(|chunk| ChunkMeta {
                    id: ChunkId::from_bytes([chunk as u8; BLAKE3_LEN]),
                    doc: 0,
                    span: (0, 1),
                    heading_path: Vec::new(),
                    tokens: index.lengths()[chunk],
                })
                .collect();
            let snapshot = Snapshot::from_index(
                &index,
                "main".to_owned(),
                Tokenizer::new().fingerprint(),
                docs,
                chunks_meta,
            );
            let bytes = snapshot.to_bytes().expect("encode valid snapshot");
            let decoded = Snapshot::from_bytes(&bytes).expect("decode valid snapshot");
            prop_assert_eq!(decoded, snapshot);
        }
    }
}
