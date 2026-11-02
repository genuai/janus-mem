//! Snapshot persistence, the manifest, and load-time validation (§3.5, §8.3).
//!
//! Loading validates every id, range and offset before use (S-18). Writing is a
//! publication: `.janus/lock` for the whole write (S-7), a unique temp file per
//! writer, `index.bin` renamed before `manifest.json` (S-17).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use fs4::TryLockError;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tempfile::NamedTempFile;

use crate::config::{self, Config};
use crate::error::JanusError;
use crate::tokenize::{Stemmer, Tokenizer, TokenizerFingerprint};
use crate::vault::{self, WalkOptions, identity, resolve};

use super::build::{self, BuildPlan, BuildStats};
use super::embed::EmbedderId;
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
    #[must_use]
    pub fn new(
        root: &Path,
        snapshot: &Snapshot,
        chunker: ChunkingFingerprint,
        payloads: Payloads,
        built_at: &str,
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
        fs::create_dir_all(dir).map_err(|source| JanusError::CreateDir {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = dir.join(LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| JanusError::Write {
                path: path.clone(),
                source,
            })?;

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
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Closing the handle releases it too; unlocking first is explicit.
        let _ = fs4::FileExt::unlock(&self.file);
    }
}

/// The nine stages of a publication (S-17, V-3), the seam behind
/// [`publish_with_hook`].
///
/// The three vectors stages fire only when the build wrote a payload; a
/// degraded publish has three index and three manifest stages and drops any
/// existing `vectors.f32` instead.
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
    /// Injected build time; no library path invents one (I-3).
    pub built_at: &'a str,
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
        None => {
            drop_vectors(dir)?;
            None
        }
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
    Ok(manifest)
}

/// Removes a previous `vectors.f32` when this build cannot write one (§3.6).
///
/// P1b has no reuse predicate, so a degraded build cannot carry a row forward
/// and must drop the file rather than leave rows the next manifest does not
/// describe. P4 replaces this branch with the carry-forward rule; until then the
/// removal is unconditional and announced on exactly one line.
fn drop_vectors(dir: &Path) -> Result<(), JanusError> {
    let path = dir.join(VECTORS_FILE);
    match fs::remove_file(&path) {
        Ok(()) => {
            sync_dir(dir)?;
            tracing::warn!(
                "vectors dropped: this build has no embedder, so {VECTORS_FILE} cannot be rebuilt"
            );
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
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
}

impl Expected {
    fn new(root: &Path, config: &Config) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            tokenizer: Tokenizer::new().fingerprint(),
            chunker: ChunkingFingerprint {
                max_chars: config.chunking.max_chars,
                overlap: config.chunking.overlap,
            },
            corpus_id: config.corpus_id.clone(),
            built_root: built_root(root),
        }
    }

    /// The reason this manifest is stale, or `Ok` when it describes us.
    fn check(&self, manifest: &Manifest) -> Result<(), String> {
        let found = [
            ("format_version", manifest.format_version.to_string()),
            ("tokenizer", tokenizer_name(&manifest.tokenizer)),
            ("chunker", chunker_name(manifest.chunker)),
            ("corpus_id", manifest.corpus_id.clone()),
            ("built_root", manifest.built_root.clone()),
        ];
        let wanted = [
            self.format_version.to_string(),
            tokenizer_name(&self.tokenizer),
            chunker_name(self.chunker),
            self.corpus_id.clone(),
            self.built_root.clone(),
        ];
        match found.iter().zip(&wanted).position(|((_, a), b)| a != b) {
            None => Ok(()),
            Some(index) => {
                let (field, actual) = &found[index];
                Err(format!("{field} {actual} != {}", wanted[index]))
            }
        }
    }
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
}

/// The result of one validated-read attempt (§3.5 steps 1–4).
enum Attempt {
    /// The manifest and the bytes it names agree. This is a generation.
    Ready(Box<Generation>),
    /// The pair did not agree; a publisher may be between the two renames.
    Retry(String),
    /// Nothing retrying can fix: absent, unreadable, or built for another world.
    Stale(String),
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
fn read_generation(dir: &Path, expected: &Expected) -> Result<Attempt, JanusError> {
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
    if let Err(reason) = expected.check(&manifest) {
        return Ok(Attempt::Stale(reason));
    }

    let index_path = dir.join(INDEX_FILE);
    let Some(bytes) = read_optional(&index_path)? else {
        return Ok(Attempt::Stale(format!(
            "{} is missing",
            index_path.display()
        )));
    };
    if blake3::hash(&bytes).to_hex().to_string() != manifest.index_sha {
        let reason = format!("{} does not match manifest.index_sha", index_path.display());
        return Ok(Attempt::Retry(reason));
    }
    match Snapshot::from_bytes(&bytes) {
        Ok(snapshot) => Ok(Attempt::Ready(Box::new(Generation { manifest, snapshot }))),
        Err(corrupt) => {
            let reason = format!("{} {}", index_path.display(), corrupt.reason);
            Ok(Attempt::Retry(reason))
        }
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
#[derive(Debug, Clone, Copy)]
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
}

impl<'a> LoadOptions<'a> {
    /// The defaults: [`LOCK_TIMEOUT`] and [`Policy::Rebuild`].
    #[must_use]
    pub fn new(root: &'a Path, config: &'a Config, built_at: &'a str) -> Self {
        Self {
            root,
            config,
            built_at,
            lock_timeout: LOCK_TIMEOUT,
            policy: Policy::Rebuild,
        }
    }
}

/// A validated generation, plus what the read that produced it did (§3.5).
#[derive(Debug)]
pub struct Load {
    /// The snapshot whose bytes hashed to [`Load::manifest`]'s `index_sha`.
    pub snapshot: Snapshot,
    /// The manifest those bytes were validated against.
    pub manifest: Manifest,
    /// True when this call built and published the generation.
    pub rebuilt: bool,
    /// What a build reused; all zero when the generation was adopted.
    pub stats: BuildStats,
    /// Why a generation on disk was rejected (S-8); `None` when one validated.
    pub stale: Option<String>,
}

impl Load {
    /// The outcome when an existing generation validated.
    fn adopted(generation: Generation, stale: Option<String>) -> Self {
        Self {
            snapshot: generation.snapshot,
            manifest: generation.manifest,
            rebuilt: false,
            stats: BuildStats::default(),
            stale,
        }
    }
}

/// Reads a validated generation, rebuilding once under the writer lock if none
/// validates and [`Policy`] allows it (§3.5). A healthy read takes no lock;
/// recovery does, and may wait for an active publisher. Errors:
/// [`JanusError::IndexUnstable`] under [`Policy::Refuse`],
/// [`JanusError::LockTimeout`] if recovery cannot get the lock, and IO failures.
pub fn load(options: &LoadOptions<'_>) -> Result<Load, JanusError> {
    let dir = config::resolve_index_dir(options.root, &options.config.index_dir);
    let expected = Expected::new(options.root, options.config);

    let mut ready = None;
    let mut last = String::new();
    let mut retries = 0;
    loop {
        match read_generation(&dir, &expected)? {
            Attempt::Ready(generation) => {
                ready = Some(generation);
                break;
            }
            Attempt::Stale(reason) => {
                last = reason;
                break;
            }
            Attempt::Retry(reason) => last = reason,
        }
        let Some(backoff) = RETRY_BACKOFF.get(retries) else {
            break;
        };
        retries += 1;
        thread::sleep(*backoff);
    }

    if let Some(generation) = ready {
        return Ok(Load::adopted(
            *generation,
            (!last.is_empty()).then_some(last),
        ));
    }
    if options.policy == Policy::Refuse {
        return Err(JanusError::IndexUnstable {
            path: dir.join(INDEX_FILE),
        });
    }

    let lock = Lock::acquire(&dir, options.lock_timeout)?;
    // Double-checked freshness (S-7): another process may have published while
    // we waited. Adopting it is what makes two auto-builds cost one build.
    if let Attempt::Ready(generation) = read_generation(&dir, &expected)? {
        return Ok(Load::adopted(
            *generation,
            (!last.is_empty()).then_some(last),
        ));
    }

    let plan = rebuild_plan(options.root, options.config, &dir)?;
    let publication = Publication {
        dir: &dir,
        root: options.root,
        snapshot: &plan.snapshot,
        chunker: plan.chunker,
        vectors: plan.vectors.as_ref(),
        built_at: options.built_at,
    };
    let manifest = publish_locked(&publication, &mut |_| {}, &lock)?;
    Ok(Load {
        snapshot: plan.snapshot,
        manifest,
        rebuilt: true,
        stats: plan.stats,
        stale: (!last.is_empty()).then_some(last),
    })
}

/// Computes the next snapshot from the vault, reusing tokens where §3.4 allows.
fn rebuild_plan(root: &Path, config: &Config, dir: &Path) -> Result<BuildPlan, JanusError> {
    let walk = vault::walk(root, config, WalkOptions::default())?;
    let (mut documents, _skips) = resolve::resolve_all(&walk.files, config);
    identity::derive_identity(&mut documents, config);
    let previous = previous_snapshot(dir);
    Ok(build::plan(config, &documents, previous.as_ref(), None)?)
}

/// The old snapshot if its bytes still decode: reuse needs tokens, not a manifest.
fn previous_snapshot(dir: &Path) -> Option<Snapshot> {
    let bytes = fs::read(dir.join(INDEX_FILE)).ok()?;
    Snapshot::from_bytes(&bytes).ok()
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
            built_at: "2025-04-01T00:00:00Z",
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
