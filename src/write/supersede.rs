//! The two-file transaction: retire one document and create its successor
//! (SPEC §7.4, M7-6).
//!
//! This is the only write that changes two documents, so it is the only one
//! where a failure can leave the vault inconsistent. The order is fixed:
//!
//! ```text
//! verify the old bytes ── mismatch ──▶ Conflict   (no lock)
//!   ├─ after_pre_lock seam                        (test only)
//! lock
//!   ├─ verify the old bytes again ── mismatch ──▶ Conflict
//!   ├─ refuse an existing new path ──▶ Exists
//!   ├─ replace the old document                   (atomic::Mode::Preserve)
//!   ├─ before_second_rename seam                  (test only)
//!   ├─ create the new document ── failure ──▶ restore the old, then RolledBack
//!   └─ both files hold their new bytes
//! drop the lock
//! ```
//!
//! The restore uses [`Plan::original`], the bytes the plan was made from. The
//! staged new file cannot restore the old one, and the old bytes exist nowhere
//! else after the first rename (M7-31). The old mode is restored with the bytes.
//!
//! If the restore itself fails, [`CommitError::Inconsistent`] names the file
//! that is wrong. It never claims a success.

use std::fs;
use std::io;
use std::path::Path;

use crate::id::Blake3Hash;

use super::atomic::{self, Mode};
use super::commit::{self, CommitError, CommitOptions, Committed};
use super::patch::Plan;

/// Replaces `old` with its plan and creates `new`, or puts `old` back.
///
/// `new` is the resolved path a create may touch; the caller has already
/// checked that it is absent. This function checks again under the lock, so a
/// file that appears while the lock is held is refused before a byte is
/// replaced. The new file is created with `create_new`, never renamed over a
/// path (M7-35).
///
/// # Errors
///
/// The first file's conflict and IO failures, [`CommitError::Exists`] when the
/// new path is taken, and the two transaction answers: [`CommitError::RolledBack`]
/// when the first file is back and [`CommitError::Inconsistent`] when it is not.
pub fn commit_pair(
    old: &Plan,
    new: &Path,
    new_bytes: &[u8],
    options: &CommitOptions<'_>,
) -> Result<Committed, CommitError> {
    let target = commit::resolve(&options.path, &options.root)?;
    commit::verify(&target, &old.old_hash)?;
    options.hooks.after_pre_lock(&options.path);

    let _lock = commit::take_lock(&options.root, options.config, options.timeout)?;
    commit::verify(&target, &old.old_hash)?;
    if fs::symlink_metadata(new).is_ok() {
        return Err(CommitError::Exists {
            path: new.to_path_buf(),
        });
    }
    let original_mode = fs::metadata(&target)
        .map_err(|source| io_failure(&target, source))?
        .permissions();
    // An unsynced first rename still holds the new bytes on disk, so the call
    // goes on and reports the unsynced state only after both files are written.
    let first_unsynced = match atomic::replace(&target, old.text.as_bytes(), Mode::Preserve) {
        Ok(()) => None,
        Err(failure @ CommitError::Unsynced { .. }) => Some(failure),
        Err(failure) => return Err(failure),
    };

    options.hooks.before_second_rename(new);
    let second = atomic::replace(new, new_bytes, Mode::New);
    match second {
        Ok(()) => match first_unsynced {
            Some(failure) => Err(failure),
            None => Ok(Committed {
                old_hash: old.old_hash.clone(),
                new_hash: Blake3Hash::of_bytes(new_bytes).to_hex(),
                bytes_before: old.bytes_before,
                bytes_after: old.text.len(),
            }),
        },
        // The create wrote the file and only its directory entry is unsynced.
        // Rolling the old document back would leave the new one behind, so the
        // transaction reports the unsynced state and keeps both new files.
        Err(failure @ CommitError::Unsynced { .. }) => Err(failure),
        Err(cause) => {
            let restored = restore(&target, &old.original, original_mode);
            match restored {
                Ok(()) => Err(CommitError::RolledBack {
                    failed: new.to_path_buf(),
                    restored: target,
                    cause: Box::new(cause),
                }),
                Err(restore) => Err(CommitError::Inconsistent {
                    failed: new.to_path_buf(),
                    old: target,
                    cause: Box::new(cause),
                    restore: Box::new(restore),
                }),
            }
        }
    }
}

/// Puts the original bytes and mode back, or says why not.
fn restore(path: &Path, original: &[u8], mode: fs::Permissions) -> Result<(), CommitError> {
    atomic::replace(path, original, Mode::Preserve)?;
    fs::set_permissions(path, mode).map_err(|source| io_failure(path, source))
}

/// An IO failure that names the path it happened to.
fn io_failure(path: &Path, source: io::Error) -> CommitError {
    CommitError::Io {
        path: path.to_path_buf(),
        source,
    }
}
