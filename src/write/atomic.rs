//! The durable replace: one file's bytes, swapped by a rename (§7.3 step 7).
//!
//! [`replace`] writes a complete temp file, syncs it, renames it over the
//! target, then syncs the target's directory. A reader sees the old bytes or the
//! new bytes, never a mixture, and a crash leaves one of the two.
//!
//! The temp file lives in the target's **own directory**, because a rename
//! across directories is a copy. `tempfile` also deletes it on every error path,
//! so a refused write leaves the directory as it found it (cases 3.1, 3.2).
//!
//! # What this module does not do
//!
//! It reads no document, compares no hash and takes no lock. Those belong to
//! [`super::commit`], which calls here once the bytes are known to be current.
//! The errors are [`CommitError`] because a failed replace is a failed commit.

use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::Path;

use tempfile::NamedTempFile;

use super::commit::CommitError;

/// Replaces `path` with `bytes`, durably, keeping the file's mode.
///
/// The sequence is: a temp file in the same directory, the target's mode applied
/// to it, `fsync` the file, `rename` over the target, `fsync` the directory. A
/// symlink at `path` is replaced, not followed.
///
/// # Errors
///
/// [`CommitError::Io`] naming the path that failed. The target keeps its old
/// bytes unless the rename itself succeeded: a failure after the rename means
/// the new bytes are in place and only their durability is in doubt.
pub fn replace(path: &Path, bytes: &[u8]) -> Result<(), CommitError> {
    let dir = parent(path)?;
    // A target that does not exist yet has no mode to keep (M7-5 creates one).
    let mode = fs::metadata(path).ok().map(|meta| meta.permissions());

    let mut temp = NamedTempFile::new_in(dir).map_err(|source| io_failure(dir, source))?;
    temp.write_all(bytes)
        .map_err(|source| io_failure(temp.path(), source))?;
    // The mode goes on before the sync, so the fsync also makes it durable.
    if let Some(permissions) = mode {
        temp.as_file()
            .set_permissions(permissions)
            .map_err(|source| io_failure(temp.path(), source))?;
    }
    temp.as_file()
        .sync_all()
        .map_err(|source| io_failure(temp.path(), source))?;

    temp.persist(path)
        .map_err(|error| io_failure(path, error.error))?;
    sync_parent_dir(dir)
}

/// Syncs a directory after a rename, so the rename survives a crash.
///
/// This is the one place a document's directory is synced. Windows cannot open a
/// directory as a file, so there the call is a no-op and the rename alone makes
/// the change visible; the bytes are already synced.
///
/// # Errors
///
/// [`CommitError::Io`] naming the directory. On Windows, never.
pub fn sync_parent_dir(dir: &Path) -> Result<(), CommitError> {
    match File::open(dir).and_then(|file| file.sync_all()) {
        Ok(()) => Ok(()),
        #[cfg(unix)]
        Err(source) => Err(io_failure(dir, source)),
        #[cfg(not(unix))]
        Err(_) => Ok(()),
    }
}

/// The directory a temp file must live in for the rename to be a rename.
fn parent(path: &Path) -> Result<&Path, CommitError> {
    path.parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .ok_or_else(|| {
            io_failure(
                path,
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "the path has no parent directory",
                ),
            )
        })
}

/// An IO failure that names the path it happened to.
fn io_failure(path: &Path, source: io::Error) -> CommitError {
    CommitError::Io {
        path: path.to_path_buf(),
        source,
    }
}
