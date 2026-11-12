//! The whole-file write: one frontmatter block from the request, then the body
//! (SPEC §9.3, M7-5).
//!
//! [`plan`] is pure. It renders each value through [`super::value::render_value`],
//! so a key is one `key: value` line and a value that needs quoting gets it. The
//! body is appended byte for byte: no newline is added, removed or translated.
//!
//! # Create and overwrite
//!
//! A create writes a path that does not exist. [`create`] passes [`Mode::New`],
//! and the atomic layer creates the target with `create_new`, never a rename:
//! a rename would replace a file the caller did not name (M7-27). An overwrite
//! replaces bytes the caller proved it had seen, through the same checked commit
//! the patch uses.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::id::Blake3Hash;
use crate::vault;

use super::PatchError;
use super::atomic::{self, Mode};
use super::commit::{self, CommitError, CommitOptions, Committed};
use super::scan::{self, is_patch_key};
use super::value::render_value;

/// The whole file a request asks for, and the facts a receipt reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// The bytes to write, as text.
    pub text: String,
    /// Every key the request wrote, sorted.
    pub changed_keys: Vec<String>,
    /// Keys the old file did not have, sorted. Every key for a create.
    pub appended_keys: Vec<String>,
    /// blake3 of the old bytes; `None` for a create.
    pub old_hash: Option<String>,
    /// Length of the old file; zero for a create.
    pub bytes_before: usize,
    /// True when the old file had no block, so this write inserts one.
    pub created_block: bool,
}

impl Document {
    /// blake3 of the bytes this document would write.
    #[must_use]
    pub fn new_hash(&self) -> String {
        Blake3Hash::of_bytes(self.text.as_bytes()).to_hex()
    }
}

/// Builds the whole file, or refuses a key that cannot be a line.
///
/// `existing` is the old bytes, when the file exists. It decides
/// `appended_keys` and the old hash: a whole-file write replaces everything
/// else. Bytes that are not UTF-8 have no readable keys, so every key counts as
/// appended and the hash still describes the bytes the caller saw.
///
/// # Errors
///
/// [`PatchError::KeyShape`] when a key is outside
/// `[A-Za-z0-9][A-Za-z0-9._-]*`. A key is written as a line, so a newline or a
/// colon inside one would add or split a key.
pub fn plan(
    frontmatter: &BTreeMap<String, Option<String>>,
    body: &str,
    existing: Option<&[u8]>,
) -> Result<Document, PatchError> {
    for key in frontmatter.keys() {
        if !is_patch_key(key) {
            return Err(PatchError::KeyShape { key: key.clone() });
        }
    }

    let mut text = String::with_capacity(body.len() + 32);
    text.push_str(scan::FENCE);
    text.push('\n');
    for (key, value) in frontmatter {
        text.push_str(key);
        text.push_str(": ");
        text.push_str(&render_value(value.as_deref()));
        text.push('\n');
    }
    text.push_str(scan::FENCE);
    text.push('\n');
    text.push_str(body);

    let old_scan = existing
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .and_then(|text| scan::scan(text).ok());
    let created_block = old_scan.as_ref().is_none_or(|scan| scan.span.is_empty());
    let present: Vec<String> = old_scan
        .map(|scan| scan.keys.into_iter().map(|span| span.key).collect())
        .unwrap_or_default();
    let changed_keys: Vec<String> = frontmatter.keys().cloned().collect();
    let appended_keys: Vec<String> = changed_keys
        .iter()
        .filter(|key| !present.iter().any(|name| name == *key))
        .cloned()
        .collect();

    Ok(Document {
        text,
        changed_keys,
        appended_keys,
        old_hash: existing.map(|bytes| Blake3Hash::of_bytes(bytes).to_hex()),
        bytes_before: existing.map_or(0, <[u8]>::len),
        created_block,
    })
}

/// The file a create would write, resolved inside the corpus.
///
/// A create names a path that does not exist, so the file cannot be
/// canonicalized. The parent directory is, and that is the boundary a new file
/// must not leave. A parent outside the corpus, or inside `.git` or `.janus`,
/// is refused before any IO on the target.
///
/// # Errors
///
/// [`CommitError::OutsideCorpus`] when the parent resolves outside the root, and
/// [`CommitError::Io`] when the parent does not exist.
pub fn new_target(root: &Path, path: &Path) -> Result<PathBuf, CommitError> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let name = candidate.file_name().ok_or_else(|| {
        io_failure(
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "the path has no file name"),
        )
    })?;
    let parent = candidate
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .ok_or_else(|| {
            io_failure(
                path,
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "the path has no parent directory",
                ),
            )
        })?;
    let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    match vault::confine(&root, parent) {
        Ok(dir) => Ok(dir.join(name)),
        Err(crate::error::JanusError::PathEscape { .. }) => Err(CommitError::OutsideCorpus {
            path: path.to_path_buf(),
        }),
        Err(error) => Err(io_failure(
            path,
            io::Error::new(io::ErrorKind::NotFound, error.to_string()),
        )),
    }
}

/// Creates `target`, or answers that it exists.
///
/// The corpus lock is held from before the existence check to after the bytes
/// are synced, so two creates cannot both think the path is free (M7-9). The
/// check itself is `create_new`, which no race can turn into an overwrite.
///
/// # Errors
///
/// [`CommitError::Exists`] when the path is already there. Every other failure
/// is the one [`atomic::replace`] names.
pub fn create(
    target: &Path,
    document: &Document,
    options: &CommitOptions<'_>,
) -> Result<(), CommitError> {
    let _lock = commit::take_lock(&options.root, options.config, options.timeout)?;
    atomic::replace(target, document.text.as_bytes(), Mode::New)
}

/// Replaces the whole file under the corpus lock, or refuses a stale hash.
///
/// # Errors
///
/// The failures of a checked commit: a conflict, a busy corpus, and the IO
/// failures a replace can have.
pub fn overwrite(
    document: &Document,
    options: &CommitOptions<'_>,
) -> Result<Committed, CommitError> {
    let old = document.old_hash.as_deref().unwrap_or_default();
    commit::commit_bytes(
        old,
        document.text.as_bytes(),
        document.bytes_before,
        options,
    )
}

/// An IO failure that names the path it happened to.
fn io_failure(path: &Path, source: io::Error) -> CommitError {
    CommitError::Io {
        path: path.to_path_buf(),
        source,
    }
}
