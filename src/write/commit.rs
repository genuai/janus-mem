//! The commit: plan bytes, checked under the corpus lock, then replaced on disk
//! (§7.3 steps 2 and 7).
//!
//! [`commit`] does the four things the pure side cannot: it compares the hash
//! the caller saw with the bytes on disk, takes the corpus lock, compares again
//! under that lock, and replaces the file. The first comparison is a fast path;
//! the second is the one the lock buys (I6, M7-10).
//!
//! # Order
//!
//! ```text
//! read + hash ── mismatch ──▶ Conflict          (the fast path, no lock)
//!   │ match
//!   ├─ after_pre_lock seam                      (test only, M7-33)
//! lock ── busy ─────────────▶ Busy
//!   ├─ read + hash ── mismatch ──▶ Conflict     (the authoritative check)
//!   ├─ rename the new bytes in                  (super::atomic)
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

use crate::error::JanusError;
use crate::id::Blake3Hash;
use crate::index::store::{self, Lock};

use super::atomic;
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

/// A failure to commit. Every variant is documented, and none is a partial
/// write: a refusal leaves the file byte-identical.
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
/// state, and a caller that wants no seam passes [`NoHooks`].
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
    /// case 2.4a's hook asserts that it stays uncalled here.
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
/// `index_dir` is the directory that holds `.janus/lock`. It is a separate field
/// rather than `root.join(".janus")` because a corpus can move its index
/// directory (`config::resolve_index_dir`), and a writer that locked a different
/// directory would race the index builder instead of waiting for it (M7-9).
pub struct CommitOptions<'a> {
    /// The document to replace.
    pub path: PathBuf,
    /// The directory holding [`store::LOCK_FILE`], as the builder resolves it.
    pub index_dir: PathBuf,
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
/// index (M7-4, M7-17).
///
/// # Errors
///
/// [`CommitError::Conflict`] when the bytes on disk are not the plan's,
/// [`CommitError::Busy`] when the lock is held past `timeout`, and
/// [`CommitError::Io`] naming the path for anything else.
pub fn commit(plan: &Plan, options: &CommitOptions<'_>) -> Result<Committed, CommitError> {
    // The fast path: the common conflict never pays for a lock (M7-10).
    verify(&options.path, &plan.old_hash)?;
    options.hooks.after_pre_lock(&options.path);

    let _lock = take_lock(options)?;

    // The authoritative check. Without it, two writers that both pass the fast
    // path would both write, and the second would erase the first (I6).
    verify(&options.path, &plan.old_hash)?;

    atomic::replace(&options.path, plan.text.as_bytes())?;

    Ok(Committed {
        old_hash: plan.old_hash.clone(),
        new_hash: plan.new_hash.clone(),
        bytes_before: plan.bytes_before,
        bytes_after: plan.bytes_after,
    })
}

/// Takes the corpus lock, or says why not.
fn take_lock(options: &CommitOptions<'_>) -> Result<Lock, CommitError> {
    Lock::acquire(&options.index_dir, options.timeout).map_err(|error| match error {
        JanusError::LockTimeout { path } => CommitError::Busy { path },
        JanusError::Write { path, source } | JanusError::CreateDir { path, source } => {
            CommitError::Io { path, source }
        }
        // The lock read fails in no other way this writer can name, so the
        // message keeps the cause and the path keeps the directory.
        other => CommitError::Io {
            path: options.index_dir.join(store::LOCK_FILE),
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
