//! The commit: plan bytes, checked under the corpus lock, then replaced on disk
//! (§7.3 steps 2 and 7).
//!
//! [`commit`] does the four things the pure side cannot: it resolves the path it
//! may write, compares the hash the caller saw with the bytes on disk, takes the
//! corpus lock, compares again under that lock, and replaces the file. The first
//! comparison is a fast path; the second is the one the lock buys (I6, M7-10).
//!
//! # Order
//!
//! ```text
//! resolve the link ── outside ──▶ OutsideCorpus
//! read + hash ── mismatch ──▶ Conflict          (the fast path, no lock)
//!   │ match
//!   ├─ after_pre_lock seam                      (test only, M7-33)
//! lock ── busy ─────────────▶ Busy
//!   ├─ read + hash ── mismatch ──▶ Conflict     (the authoritative check)
//!   ├─ rename the new bytes in                  (super::atomic)
//!   └─ sync the directory ── failure ──▶ Unsynced
//! drop the lock
//! ```
//!
//! # What this module does not do
//!
//! It maps no error to a code or an exit code (M7-4), it does not decide whether
//! writes are enabled (M7-4), and it never refreshes the index (M7-17). It
//! takes one lock, the same `.janus/lock` the index builder takes, so one corpus
//! has one writer at a time (M7-9).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

use crate::config::{self, Config};
use crate::error::JanusError;
use crate::id::Blake3Hash;
use crate::index::store::{self, Lock};

use super::atomic::{self, Mode};
use super::patch::Plan;

/// What a successful commit changed, as the receipt reports it (§2.3).
///
/// `new_hash` is the hash of the bytes written, so the caller can present it as
/// the next `expected_hash` (M7-18).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    /// blake3 of the bytes the plan was made from.
    pub old_hash: String,
    /// blake3 of the bytes written.
    pub new_hash: String,
    /// Length of the file before the commit, in bytes.
    pub bytes_before: usize,
    /// Length of the file after the commit, in bytes.
    pub bytes_after: usize,
}

/// A failure to commit.
///
/// Every variant but [`CommitError::Unsynced`] is a refusal: the file keeps its
/// bytes and no temp file is left. `Unsynced` is the one answer that says the
/// bytes **were** written, so a caller cannot report "not written" when the
/// document already changed.
#[derive(Debug, Error)]
pub enum CommitError {
    /// The bytes on disk are not the bytes the plan was made from.
    ///
    /// Both hashes are reported, so the caller can re-read and retry (§2.4).
    #[error("the file changed since it was read: expected {expected}, found {current}")]
    Conflict {
        /// The hash the caller presented.
        expected: String,
        /// The hash of the bytes on disk now.
        current: String,
    },
    /// Another writer held the corpus lock for longer than the timeout.
    ///
    /// The path is the lock file, never the document: the document is fine, the
    /// corpus is busy.
    #[error("the corpus lock {path} is busy")]
    Busy {
        /// The lock file that could not be acquired in time.
        path: PathBuf,
    },
    /// The target already exists, so a create writes nothing (M7-27).
    #[error("{path} already exists")]
    Exists {
        /// The path the caller named.
        path: PathBuf,
    },
    /// The path resolves outside the corpus, so no write may touch it.
    ///
    /// An in-corpus symlink to another corpus is the reachable case: writing
    /// through it would change a file this corpus does not own (case 2.4c).
    #[error("{path} resolves outside the corpus")]
    OutsideCorpus {
        /// The path the caller gave.
        path: PathBuf,
    },
    /// The write landed and the directory sync did not.
    ///
    /// The bytes are on disk and visible to every reader. Only their survival
    /// across a crash is in doubt, so this is not a refusal.
    #[error("the bytes in {path} are written; syncing its directory failed: {source}")]
    Unsynced {
        /// The directory whose entry could not be synced.
        path: PathBuf,
        /// The underlying IO failure.
        #[source]
        source: io::Error,
    },
    /// A file or directory operation failed.
    #[error("{path}: {source}")]
    Io {
        /// The path the failure happened to.
        path: PathBuf,
        /// The underlying IO failure.
        #[source]
        source: io::Error,
    },
}

/// The two points a test needs to reach, and production does not (§2.4a, M7-33).
///
/// A seam is a parameter, like [`crate::clock::Clock`]: nothing reads ambient
/// state, and a caller that wants no seam passes [`NoHooks`]. Both methods name
/// the path the caller gave, not the file a link resolves to.
pub trait Hooks {
    /// Runs after the pre-lock check passed and before the lock is taken.
    ///
    /// Case 2.4a changes the file here, which is the only way to reach the
    /// authoritative check without a second process or a race.
    fn after_pre_lock(&self, path: &Path);

    /// Runs after the first rename and before a second one.
    ///
    /// **Not called in this phase.** A commit writes one file, so there is no
    /// second rename; the seam exists for the two-file transaction (M7-6), and
    /// case 2.4b proves it stays uncalled here.
    fn before_second_rename(&self, path: &Path);
}

/// The production [`Hooks`]: two methods that do nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHooks;

impl Hooks for NoHooks {
    fn after_pre_lock(&self, _path: &Path) {}

    fn before_second_rename(&self, _path: &Path) {}
}

/// Everything a commit needs, passed explicitly.
///
/// The lock directory is not a field. It is resolved inside [`commit`] from
/// `root` and `config` with [`config::resolve_index_dir`], the same function the
/// index builder uses, so two callers cannot lock two different files for one
/// corpus (M7-45).
pub struct CommitOptions<'a> {
    /// The document to replace, as the caller named it.
    pub path: PathBuf,
    /// The corpus root: the boundary a path must resolve inside.
    pub root: PathBuf,
    /// The corpus config, for the index directory that holds the lock.
    pub config: &'a Config,
    /// How long to wait for the lock before answering [`CommitError::Busy`].
    pub timeout: Duration,
    /// The test seams. Production passes [`NoHooks`].
    pub hooks: &'a dyn Hooks,
}

/// Replaces the document with the plan's bytes, under the corpus lock, or
/// refuses.
///
/// The caller passes the hash it saw; this function compares it twice. Nothing
/// here reads `expected_hash` from a request, writes a receipt, or touches the
/// index (M7-4, M7-17). The target must be an existing file: a commit proves the
/// bytes it replaces, and creating one is M7-5's `Mode::New`.
///
/// # Errors
///
/// [`CommitError::Conflict`] when the bytes on disk are not the plan's,
/// [`CommitError::OutsideCorpus`] when the path resolves outside the root,
/// [`CommitError::Busy`] when the lock is held past `timeout`,
/// [`CommitError::Unsynced`] when the write landed and the directory sync did
/// not, and [`CommitError::Io`] naming the path for anything else.
pub fn commit(plan: &Plan, options: &CommitOptions<'_>) -> Result<Committed, CommitError> {
    commit_bytes(
        &plan.old_hash,
        plan.text.as_bytes(),
        plan.bytes_before,
        options,
    )
}

/// Replaces one file with bytes whose old hash the caller already knows.
///
/// [`commit`] and the whole-file overwrite of M7-5 share this sequence, so the
/// two names cannot check a different number of times or lock a different file.
pub(super) fn commit_bytes(
    expected_old: &str,
    bytes: &[u8],
    bytes_before: usize,
    options: &CommitOptions<'_>,
) -> Result<Committed, CommitError> {
    // A link inside the corpus is a document under its own path, so the bytes
    // live at its target. Replacing the link itself would leave the target stale
    // and the corpus with two documents (case 2.4c).
    let target = resolve(&options.path, &options.root)?;

    // The fast path: the common conflict never pays for a lock (M7-10).
    verify(&target, expected_old)?;
    options.hooks.after_pre_lock(&options.path);

    let _lock = take_lock(&options.root, options.config, options.timeout)?;

    // The authoritative check. Without it, two writers that both pass the fast
    // path would both write, and the second would erase the first (I6).
    verify(&target, expected_old)?;

    atomic::replace(&target, bytes, Mode::Preserve)?;

    Ok(Committed {
        old_hash: expected_old.to_owned(),
        new_hash: Blake3Hash::of_bytes(bytes).to_hex(),
        bytes_before,
        bytes_after: bytes.len(),
    })
}

/// The file to write: `path` with every link resolved, inside `root`.
fn resolve(path: &Path, root: &Path) -> Result<PathBuf, CommitError> {
    let target = dunce::canonicalize(path).map_err(|source| CommitError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if target.starts_with(&root) {
        Ok(target)
    } else {
        Err(CommitError::OutsideCorpus {
            path: path.to_path_buf(),
        })
    }
}

/// Takes the corpus lock, or says why not.
pub(super) fn take_lock(
    root: &Path,
    config: &Config,
    timeout: Duration,
) -> Result<Lock, CommitError> {
    let dir = config::resolve_index_dir(root, &config.index_dir);
    Lock::acquire(&dir, timeout).map_err(|error| match error {
        JanusError::LockTimeout { path } => CommitError::Busy { path },
        JanusError::Write { path, source } | JanusError::CreateDir { path, source } => {
            CommitError::Io { path, source }
        }
        // The lock read fails in no other way this writer can name, so the
        // message keeps the cause and the path keeps the directory.
        other => CommitError::Io {
            path: dir.join(store::LOCK_FILE),
            source: io::Error::other(other.to_string()),
        },
    })
}

/// Fails unless the file's bytes hash to `expected`.
fn verify(path: &Path, expected: &str) -> Result<(), CommitError> {
    let bytes = fs::read(path).map_err(|source| CommitError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let current = Blake3Hash::of_bytes(&bytes).to_hex();
    if current == expected {
        Ok(())
    } else {
        Err(CommitError::Conflict {
            expected: expected.to_owned(),
            current,
        })
    }
}
