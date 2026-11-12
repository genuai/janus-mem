//! The durable replace: one file's bytes, swapped by a rename (§7.3 step 7).
//!
//! [`replace`] writes a complete temp file, syncs it, renames it over the
//! target, then syncs the target's directory. A reader sees the old bytes or the
//! new bytes, never a mixture, and a crash leaves one of the two.
//!
//! The temp file lives in the target's **own directory**, because a rename
//! across directories is a copy. Its name starts with `.tmp`, so a file left
//! behind is recognisable: **nothing sweeps a document directory, and a running
//! process removes its temp file on every error path only** (M7-47). A process
//! killed between the create and the rename leaves one file, and M8's `doctor`
//! is where it gets reported.
//!
//! # What this module does not do
//!
//! It reads no document, compares no hash and takes no lock. Those belong to
//! [`super::commit`], which calls here once the bytes are known to be current.
//! The errors are [`CommitError`] because a failed replace is a failed commit.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use super::commit::CommitError;

/// How [`replace`] treats the target's mode and owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Keep the target's mode and owner. The target must exist.
    ///
    /// A rename installs a new inode, so without this the file would take the
    /// temp file's mode and the writer's ownership, and a group-shared vault
    /// would lose the group's access.
    Preserve,
    /// A new document: `0o666` masked by the process umask on Unix, and the
    /// writer's own ownership.
    ///
    /// The mode is set **at creation**, so the umask applies exactly as it does
    /// for any other new file. M7-5's create path passes this.
    New,
}

/// Attempts at a unique temp name before giving up.
const TEMP_ATTEMPTS: u32 = 16;

/// Creates `path` with `bytes`, refusing a path that exists.
///
/// A rename replaces its target, so it must never create one: it would replace a
/// file the caller did not name. This function opens the target itself with
/// `create_new`, which is atomic and fails with `AlreadyExists` before a byte is
/// written (M7-27, case 5.2).
///
/// # Errors
///
/// [`CommitError::Exists`] when the path is already there, and
/// [`CommitError::Io`] naming the path for any other failure.
fn create_new(path: &Path, bytes: &[u8]) -> Result<(), CommitError> {
    let dir = parent(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // The umask applies exactly as it does for any other new file.
        options.mode(0o666);
    }
    let mut file = options.open(path).map_err(|source| {
        if source.kind() == io::ErrorKind::AlreadyExists {
            CommitError::Exists {
                path: path.to_path_buf(),
            }
        } else {
            io_failure(path, source)
        }
    })?;
    if let Err(source) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        // The file is new, so removing it restores the state the caller saw.
        drop(file);
        let _ = fs::remove_file(path);
        return Err(io_failure(path, source));
    }
    drop(file);
    sync_parent_dir(dir)
}

/// Writes `path` with `bytes`, durably, under the [`Mode`] policy.
///
/// Under [`Mode::Preserve`] the sequence is: a temp file in the same directory,
/// the target's owner and mode applied to it, `fsync` the file, `rename` over
/// the target, `fsync` the directory. A symlink at `path` is replaced, not
/// followed: [`super::commit`] resolves a link before it reaches here.
///
/// Under [`Mode::New`] the target is opened with `create_new`, so the umask
/// decides its mode and an existing path is refused instead of replaced. A
/// create is the one call that could destroy a file the caller did not name, so
/// it never reaches the rename.
///
/// # Errors
///
/// [`CommitError::Exists`] under [`Mode::New`] when the path is already there.
/// [`CommitError::Io`] naming the path that failed, with the target unchanged:
/// a failure before the rename leaves the old bytes and no temp file. The one
/// exception is [`CommitError::Unsynced`], which reports a rename that happened
/// and a directory sync that did not.
pub fn replace(path: &Path, bytes: &[u8], mode: Mode) -> Result<(), CommitError> {
    if mode == Mode::New {
        return create_new(path, bytes);
    }
    let dir = parent(path)?;
    let existing = fs::metadata(path).map_err(|source| io_failure(path, source))?;
    // The exact mode is set a moment later, so the creation mode only has to be
    // safe if that step never runs.
    let creation = 0o600;

    let (mut temp, mut file) = create_temp(dir, creation)?;
    copy_owner(&existing, &file);
    file.set_permissions(existing.permissions())
        .map_err(|source| io_failure(temp.path(), source))?;
    // Both go before the sync, so the fsync makes the bytes and their metadata
    // durable together.
    file.write_all(bytes)
        .map_err(|source| io_failure(temp.path(), source))?;
    file.sync_all()
        .map_err(|source| io_failure(temp.path(), source))?;
    drop(file);

    fs::rename(temp.path(), path).map_err(|source| io_failure(path, source))?;
    // Past the rename the temp path is gone, so the guard has nothing to remove.
    temp.renamed = true;
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
/// [`CommitError::Unsynced`] naming the directory. On Windows, never.
pub fn sync_parent_dir(dir: &Path) -> Result<(), CommitError> {
    match File::open(dir).and_then(|file| file.sync_all()) {
        Ok(()) => Ok(()),
        #[cfg(unix)]
        Err(source) => Err(CommitError::Unsynced {
            path: dir.to_path_buf(),
            source,
        }),
        #[cfg(not(unix))]
        Err(_) => Ok(()),
    }
}

/// The temp file and its cleanup guard.
///
/// A process killed between the create and the rename leaves the file behind,
/// and this guard cannot run. That is M7-47, and it is reported, not swept.
struct TempFile {
    path: PathBuf,
    renamed: bool,
}

impl TempFile {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.renamed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Creates a uniquely named temp file in `dir`, with `creation` as its mode.
///
/// The name carries the process id and an attempt counter. `create_new` never
/// truncates, so a file left by a dead process costs one attempt, not the
/// caller's bytes.
fn create_temp(dir: &Path, creation: u32) -> Result<(TempFile, File), CommitError> {
    let pid = std::process::id();
    let mut attempt = 0;
    loop {
        let path = dir.join(format!(".tmp{pid}-{attempt}"));
        match open_new(&path, creation) {
            Ok(file) => {
                return Ok((
                    TempFile {
                        path,
                        renamed: false,
                    },
                    file,
                ));
            }
            Err(source)
                if source.kind() == io::ErrorKind::AlreadyExists && attempt + 1 < TEMP_ATTEMPTS =>
            {
                attempt += 1;
            }
            Err(source) => return Err(io_failure(&path, source)),
        }
    }
}

/// Opens `path` for writing, creating it and refusing to reuse one.
fn open_new(path: &Path, creation: u32) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(creation);
    }
    #[cfg(not(unix))]
    let _ = creation;
    options.open(path)
}

/// Copies the target's owner onto the temp file (SPEC §7.3 step 7).
///
/// Best effort. A writer that does not own the document cannot hand it to
/// another user (`EPERM`), and a filesystem without ownership has no owner
/// (`EINVAL`). Neither is a failed write: the bytes are what the caller asked
/// for, and a mode it cannot widen is not worth losing them over.
#[cfg(unix)]
fn copy_owner(meta: &fs::Metadata, file: &File) {
    use std::os::fd::AsFd as _;
    use std::os::unix::fs::MetadataExt as _;
    let _ = std::os::unix::fs::fchown(file.as_fd(), Some(meta.uid()), Some(meta.gid()));
}

/// A platform without Unix ownership has nothing to copy.
#[cfg(not(unix))]
fn copy_owner(_meta: &fs::Metadata, _file: &File) {}

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
