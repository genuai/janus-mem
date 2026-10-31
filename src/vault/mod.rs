//! Corpus root resolution, directory walking, and path confinement
//! (SPEC §6.1, §8.5, §8.6, §8.7).
//!
//! Phase 2 of M1 is deliberately split from parsing: this module only answers
//! *"which files are in the corpus, and which were skipped and why?"*. It reads
//! no file contents. Content — frontmatter isolation, metadata resolution,
//! identity — belongs to [`crate::document`] and arrives in phase 3, which
//! consumes [`DiscoveredFile`]s and calls [`read_consistent`] per file.
//!
//! That split matters for memory: a walk of a 20k-document vault must not hold
//! 300 MB of text in flight (§13). Dirents are cheap; bytes are not.
//!
//! # What the walk guarantees
//!
//! * Only `.md` / `.markdown` files are admitted, case-insensitively (§6.1, D27).
//! * `.janus/`, `.git/`, `target/`, `node_modules/`, `.obsidian/` and every
//!   dot-file are skipped, and `janus.toml`'s `[index] exclude` globs win over
//!   everything else.
//! * Directory symlinks are never followed, so the walk cannot cycle or escape.
//! * File symlinks are resolved once and included only if their target is inside
//!   the corpus (§8.5).
//! * Every skipped entry is reported with the rule that skipped it, so "why
//!   isn't this file searchable?" is answerable (§6.1).
//! * Output order is deterministic: sorted by vault-relative path (§10).

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use ignore::{DirEntry, WalkBuilder};

use crate::config::{self, Config};
use crate::error::JanusError;
use crate::id::Blake3Hash;

/// File extensions the corpus indexes. Nothing else is ever opened (§6.1, D27).
pub const INDEXED_EXTENSIONS: [&str; 2] = ["md", "markdown"];

/// How many times a file may change under us before the read is abandoned
/// (§6.9). Three attempts covers a `git checkout` landing mid-read without
/// turning a pathological writer into a hang.
const READ_ATTEMPTS: usize = 3;

// ─────────────────────────────── discovery ─────────────────────────────────

/// A file the walk admitted, before its bytes are read.
///
/// Carrying `size` and `mtime` from the walk saves a second `stat` per file, and
/// gives the incremental indexer (M4) its cheap change signal.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    /// Absolute path on disk. Equals the symlink's own path when the entry is a
    /// symlink whose target is inside the corpus.
    pub path: PathBuf,
    /// Vault-relative path, always `/`-separated, never empty.
    pub rel_path: String,
    /// Size in bytes, as of the walk.
    pub size: u64,
    /// Modification time, as of the walk.
    pub mtime: SystemTime,
}

/// Why the walk refused an entry.
///
/// One variant per rule so the scan summary can attribute a skip to exactly one
/// cause instead of reporting an opaque total (§6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `.janus/`, `.git/`, `target/`, `node_modules/`, `.obsidian/` — pruned
    /// before descent, and not re-includable by config.
    AlwaysExcluded,
    /// A dot-prefixed file or directory.
    Hidden,
    /// Matched an `[index] exclude` glob.
    ConfigExclude,
    /// Not `.md` / `.markdown`.
    NotMarkdown,
    /// A directory symlink. Never followed: it can cycle, and it would index the
    /// same content under two paths.
    SymlinkedDirectory,
    /// A symlink whose target resolves outside the corpus (§8.5).
    SymlinkEscape,
    /// The path is not valid UTF-8, so it cannot be a stable `rel_path`.
    NonUtf8Path,
}

impl SkipReason {
    /// The stable machine-readable code used in reports.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::AlwaysExcluded => "always_excluded",
            Self::Hidden => "hidden",
            Self::ConfigExclude => "config_exclude",
            Self::NotMarkdown => "not_markdown",
            Self::SymlinkedDirectory => "symlinked_directory",
            Self::SymlinkEscape => "symlink_escape",
            Self::NonUtf8Path => "non_utf8_path",
        }
    }
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One entry the walk refused, with the rule that refused it.
#[derive(Debug, Clone)]
pub struct SkippedEntry {
    /// The entry's absolute path.
    pub path: PathBuf,
    /// Vault-relative path, or a lossy rendering when the path is not UTF-8.
    pub rel_path: String,
    /// Whether the skipped entry was a directory. A skipped directory stands for
    /// everything beneath it, which the walk never enumerated.
    pub is_dir: bool,
    /// The rule that skipped it.
    pub reason: SkipReason,
}

/// A directory or file the walker could not read at all.
///
/// Never fatal: an unreadable subdirectory is reported and the scan continues,
/// because one permission problem in an archive folder should not hide the rest
/// of the corpus.
///
/// There is no separate `path` field because `ignore::Error` exposes no path
/// accessor — only `is_io`/`is_partial`. Its `Display` already embeds the path,
/// so unwrapping it here would only risk disagreeing with it.
#[derive(Debug, Clone)]
pub struct WalkError {
    /// The walker's message, including the path it failed on.
    pub message: String,
}

/// Everything a walk found.
#[derive(Debug, Default)]
pub struct WalkResult {
    /// Admitted files, sorted by `rel_path`.
    pub files: Vec<DiscoveredFile>,
    /// Refused entries, sorted by `rel_path`.
    pub skipped: Vec<SkippedEntry>,
    /// Unreadable paths, sorted by `path`.
    pub errors: Vec<WalkError>,
    /// How many markdown files would have been admitted if `.gitignore` did not
    /// exist. `None` when attribution was disabled.
    ///
    /// The `ignore` crate filters gitignored paths internally and never reports
    /// them, so the only way to count them is a second, cheaper walk with
    /// gitignore turned off. That costs one extra dirent traversal and zero file
    /// reads — worth it, because "why isn't this file searchable?" is otherwise
    /// unanswerable for the most common cause (§6.1).
    pub ignored_by_git: Option<usize>,
}

impl WalkResult {
    /// How many entries were skipped for a given reason.
    #[must_use]
    pub fn count(&self, reason: SkipReason) -> usize {
        self.skipped.iter().filter(|s| s.reason == reason).count()
    }

    /// Total entries skipped, for the one-line summary.
    #[must_use]
    pub fn skipped_total(&self) -> usize {
        self.skipped.len()
    }
}

/// Knobs for the walk.
///
/// Exists because two `ignore` defaults are environment-dependent: the global
/// gitignore (`~/.config/git/ignore`) and parent-directory ignore files. Real
/// runs should honour them; tests must not, or a developer's dotfiles would
/// change the outcome. This is the seam that keeps `cargo test` hermetic
/// without touching process-wide environment variables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkOptions {
    /// Honour `core.excludesFile` / `~/.config/git/ignore`.
    pub respect_global_gitignore: bool,
    /// Honour `.gitignore` files in directories above the corpus root.
    pub read_parent_ignores: bool,
    /// Apply git ignore rules even when the corpus is not a git repository.
    ///
    /// `false` is what a vault owner expects from "respects `.gitignore`": an
    /// exported or synced corpus often has a `.gitignore` and no `.git`.
    pub require_git: bool,
    /// Do the extra traversal needed to count gitignored files.
    pub attribute_gitignore: bool,
}

impl Default for WalkOptions {
    /// Production behaviour: honour everything the user configured, including
    /// their global gitignore, and attribute gitignored files.
    fn default() -> Self {
        Self {
            respect_global_gitignore: true,
            read_parent_ignores: true,
            require_git: false,
            attribute_gitignore: true,
        }
    }
}

impl WalkOptions {
    /// Environment-independent behaviour, for tests and reproducible scans.
    #[must_use]
    pub const fn hermetic() -> Self {
        Self {
            respect_global_gitignore: false,
            read_parent_ignores: false,
            require_git: false,
            attribute_gitignore: true,
        }
    }
}

// ───────────────────────────── root resolution ─────────────────────────────

/// Resolves the corpus root for a command that *reads* a corpus (§6.1).
///
/// Order: the explicit path (`--vault`, or `JANUS_VAULT` via clap), then the
/// nearest ancestor of the working directory containing `janus.toml` or
/// `.janus/`, then an error.
///
/// Ancestor discovery uses `.janus/` as a marker as well as `janus.toml` because
/// M0 created both, and a corpus whose config was deleted should still be
/// findable so that the resulting error can name the config it wants.
///
/// # Errors
///
/// [`JanusError::VaultResolve`] if the explicit path cannot be canonicalized,
/// [`JanusError::VaultNotDirectory`] if it is not a directory, or
/// [`JanusError::VaultNotFound`] when discovery fails.
pub fn resolve_corpus(explicit: Option<PathBuf>) -> Result<PathBuf, JanusError> {
    if let Some(path) = explicit {
        return config::resolve_vault(Some(path));
    }

    // Canonicalize the working directory once so `ancestors()` yields real
    // paths; a symlinked cwd would otherwise produce paths that fail
    // confinement's `starts_with` check later.
    let cwd = std::env::current_dir()?;
    let cwd = dunce::canonicalize(&cwd).map_err(|source| JanusError::VaultResolve {
        path: cwd.clone(),
        source,
    })?;

    for candidate in cwd.ancestors() {
        let has_config = candidate.join(config::CONFIG_FILE).is_file();
        let has_index = candidate.join(config::JANUS_DIR).is_dir();
        if has_config || has_index {
            return Ok(candidate.to_path_buf());
        }
    }

    Err(JanusError::VaultNotFound { from: cwd })
}

// ────────────────────────────── confinement ────────────────────────────────

/// Resolves `candidate` and proves it stays inside `root` (§8.5, §8.6 rule 1).
///
/// `root` must already be canonical (as [`resolve_corpus`] returns it): the check
/// compares components, so comparing a canonical path against a symlinked root
/// would reject everything.
///
/// This is the single choke point every future read and write path goes through
/// (M6's `read_document`, all of M7's writers), so it is worth reading twice:
///
/// * `starts_with` compares **path components**, not string prefixes, which is
///   what makes `/vault-evil/x` correctly fail against `/vault`.
/// * Non-existent paths are rejected (`canonicalize` fails) — deliberate, since
///   confinement without a real resolution is a guess, and M7 creates new files
///   by writing a temp file and renaming into a directory it has already
///   confined.
///
/// # Errors
///
/// [`JanusError::PathResolve`] when the path cannot be resolved,
/// [`JanusError::PathEscape`] when it resolves outside the root or into a
/// forbidden directory.
pub fn confine(root: &Path, candidate: &Path) -> Result<PathBuf, JanusError> {
    let resolved = dunce::canonicalize(candidate).map_err(|source| JanusError::PathResolve {
        path: candidate.to_path_buf(),
        source,
    })?;

    if !resolved.starts_with(root) {
        return Err(JanusError::PathEscape {
            path: candidate.to_path_buf(),
        });
    }

    let enters_forbidden = resolved.strip_prefix(root).is_ok_and(|relative| {
        relative.components().any(
            |component| matches!(component, Component::Normal(name) if config::is_forbidden_dir(name)),
        )
    });
    if enters_forbidden {
        return Err(JanusError::PathEscape {
            path: candidate.to_path_buf(),
        });
    }

    Ok(resolved)
}

// ──────────────────────────────── walking ─────────────────────────────────

/// Walks a corpus and reports what it admitted, refused, and could not read.
///
/// Reads no file contents. Admitted files are sorted by `rel_path`, so the same
/// tree always produces the same report (§10).
///
/// # Errors
///
/// Only when the root itself is unusable. Problems below the root are reported
/// in [`WalkResult::errors`] — one unreadable directory must not abort a scan.
pub fn walk(root: &Path, config: &Config, options: WalkOptions) -> Result<WalkResult, JanusError> {
    // Canonicalize once, here, rather than trusting the caller. Confinement
    // compares path components, so it silently rejects everything when one side
    // is canonical and the other is not — which on macOS is the difference
    // between `/var/folders/...` and `/private/var/folders/...`. Doing it at the
    // single entry point means every caller gets it right by construction.
    let root = dunce::canonicalize(root).map_err(|source| JanusError::VaultResolve {
        path: root.to_path_buf(),
        source,
    })?;

    if !root.is_dir() {
        return Err(JanusError::VaultNotDirectory { path: root });
    }

    let root = root.as_path();
    let (mut files, mut skipped, mut errors) = run_walk(root, config, options, true);

    let ignored_by_git = if options.attribute_gitignore {
        // Second pass with git ignore rules off. Everything the first pass
        // admitted must reappear here, so the difference is exactly what
        // `.gitignore` removed.
        let (with_ignores_off, _, _) = run_walk(root, config, options, false);
        let admitted: HashSet<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
        Some(
            with_ignores_off
                .iter()
                .filter(|file| !admitted.contains(file.rel_path.as_str()))
                .count(),
        )
    } else {
        None
    };

    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    skipped.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    errors.sort_by(|a, b| a.message.cmp(&b.message));

    Ok(WalkResult {
        files,
        skipped,
        errors,
        ignored_by_git,
    })
}

/// One traversal, optionally honouring git ignore rules.
fn run_walk(
    root: &Path,
    config: &Config,
    options: WalkOptions,
    use_git_ignores: bool,
) -> (Vec<DiscoveredFile>, Vec<SkippedEntry>, Vec<WalkError>) {
    // The filter predicate must be `'static + Send + Sync`, so the config and
    // the collected skips are shared behind `Arc`. Cloning a `Config` once per
    // walk is a few hundred bytes; cloning it per entry would not be.
    let config = Arc::new(config.clone());
    let root_owned = root.to_path_buf();
    let skipped: Arc<Mutex<Vec<SkippedEntry>>> = Arc::new(Mutex::new(Vec::new()));

    let mut builder = WalkBuilder::new(root);
    builder
        // We attribute hidden entries ourselves rather than letting the walker
        // silently drop them: "skipped because hidden" is a real answer to a
        // real question.
        .hidden(false)
        // Never chase directory symlinks: that is how a walk escapes the corpus
        // or loops forever (§8.5).
        .follow_links(false)
        .standard_filters(false)
        .ignore(use_git_ignores)
        .git_ignore(use_git_ignores)
        .git_exclude(use_git_ignores)
        .git_global(use_git_ignores && options.respect_global_gitignore)
        .parents(options.read_parent_ignores)
        .require_git(options.require_git);

    {
        let config = Arc::clone(&config);
        let root = root_owned.clone();
        let skipped = Arc::clone(&skipped);

        builder.filter_entry(move |entry| {
            // The root itself is never skipped, even if it is named `.vault`.
            if entry.depth() == 0 {
                return true;
            }

            let name = entry.file_name();
            let file_type = entry.file_type();
            let is_symlink = file_type.is_some_and(|kind| kind.is_symlink());
            // With `follow_links(false)` the walker never descends a directory
            // symlink, but it still yields it — and its own `file_type` says
            // "symlink", not "dir". One `stat` per symlink (rare) recovers what
            // the target actually is, so the classifier can treat a linked
            // directory as a directory.
            let is_dir = file_type.is_some_and(|kind| kind.is_dir())
                || (is_symlink
                    && std::fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir()));
            let rel_path = rel_path_of(&root, entry.path());

            let reason = match &rel_path {
                Some(rel_path) => classify(&config, rel_path, name, is_dir, is_symlink),
                // A path we cannot render as UTF-8 can never be a `rel_path`,
                // so it can never be a document. Skip it here rather than
                // carrying an unrepresentable path into parsing.
                None => Some(SkipReason::NonUtf8Path),
            };

            match reason {
                None => true,
                Some(reason) => {
                    let rendered = rel_path.unwrap_or_else(|| path_to_display(&root, entry.path()));
                    // `filter_entry` cannot fail, and a poisoned lock would mean
                    // another thread panicked inside the filter. Losing one skip
                    // record is far better than panicking, so ignore the error.
                    if let Ok(mut guard) = skipped.lock() {
                        guard.push(SkippedEntry {
                            path: entry.path().to_path_buf(),
                            rel_path: rendered,
                            is_dir,
                            reason,
                        });
                    }
                    // Returning false prunes a directory and skips a file.
                    false
                }
            }
        });
    }

    let mut files = Vec::new();
    let mut errors = Vec::new();

    for entry in builder.build() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(WalkError {
                    message: error.to_string(),
                });
                continue;
            }
        };

        if entry.depth() == 0 {
            continue;
        }
        // A symlink to a file reports `is_symlink()`, never `is_file()`, so it
        // must be admitted here and proved inside the corpus by `admit_file`.
        let admissible = entry
            .file_type()
            .is_some_and(|kind| kind.is_file() || kind.is_symlink());
        if !admissible {
            continue;
        }

        let Some(rel_path) = rel_path_of(root, entry.path()) else {
            // Unreachable in practice: the filter already skipped non-UTF-8
            // paths. Keeping the branch means no `unwrap` and no panic if the
            // two ever disagree.
            continue;
        };

        let (path, metadata) = match admit_file(root, &entry) {
            Ok(admitted) => admitted,
            Err(skip) => {
                if let Ok(mut guard) = skipped.lock() {
                    guard.push(SkippedEntry {
                        path: entry.path().to_path_buf(),
                        rel_path,
                        is_dir: false,
                        reason: skip,
                    });
                }
                continue;
            }
        };

        files.push(DiscoveredFile {
            path,
            rel_path,
            size: metadata.len(),
            mtime: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        });
    }

    let skipped = skipped
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    (files, skipped, errors)
}

/// Checks a file entry that survived the filter: symlinks must resolve inside
/// the corpus (§8.5).
///
/// Returns the path to record and the target's metadata, or the reason to skip.
fn admit_file(root: &Path, entry: &DirEntry) -> Result<(PathBuf, std::fs::Metadata), SkipReason> {
    let path = entry.path();

    if entry.path_is_symlink() {
        // A file symlink is admitted only if it points inside the corpus. The
        // link's own path is what identifies the document; the target is only
        // provenance.
        match confine(root, path) {
            Ok(resolved) => {
                let metadata =
                    std::fs::metadata(&resolved).map_err(|_| SkipReason::SymlinkEscape)?;
                if !metadata.is_file() {
                    return Err(SkipReason::SymlinkedDirectory);
                }
                return Ok((path.to_path_buf(), metadata));
            }
            Err(_) => return Err(SkipReason::SymlinkEscape),
        }
    }

    let metadata = entry.metadata().map_err(|_| SkipReason::NonUtf8Path)?;
    Ok((path.to_path_buf(), metadata))
}

/// The rule ladder, in order. First match wins.
///
/// Pure and independent of the walker, so every rule can be tested directly.
fn classify(
    config: &Config,
    rel_path: &str,
    name: &OsStr,
    is_dir: bool,
    is_symlink: bool,
) -> Option<SkipReason> {
    if is_dir && config::is_always_excluded_dir(name) {
        return Some(SkipReason::AlwaysExcluded);
    }
    if is_hidden(name) {
        return Some(SkipReason::Hidden);
    }
    if config.is_excluded(rel_path) {
        return Some(SkipReason::ConfigExclude);
    }
    if is_symlink {
        // A linked directory is refused here (the walker would not descend it
        // anyway, and indexing the same tree twice is never intended). A linked
        // file is *not* decided here: its target must be resolved and proved to
        // stay inside the corpus, which is `admit_file`'s job.
        return is_dir.then_some(SkipReason::SymlinkedDirectory);
    }
    if !is_dir && !is_markdown(name) {
        return Some(SkipReason::NotMarkdown);
    }
    None
}

/// Whether a name is dot-prefixed.
///
/// Uses the OS encoding so a non-UTF-8 name is still classified without
/// allocating or lossily converting it.
fn is_hidden(name: &OsStr) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.')
}

/// Whether a filename has an indexed extension, case-insensitively.
///
/// Case-insensitive because macOS and Windows filesystems are: `README.MD` is
/// the same file as `readme.md` to the OS, so treating them differently would be
/// a cross-platform bug (§8.7).
fn is_markdown(name: &OsStr) -> bool {
    Path::new(name)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            INDEXED_EXTENSIONS
                .iter()
                .any(|wanted| extension.eq_ignore_ascii_case(wanted))
        })
}

/// Builds the vault-relative, `/`-separated path for reporting and identity.
///
/// Returns `None` when the path leaves the root or contains a non-UTF-8
/// component — both mean "this cannot be a document".
#[must_use]
pub fn rel_path_of(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut out = String::new();

    for component in relative.components() {
        let Component::Normal(part) = component else {
            return None;
        };
        let part = part.to_str()?;
        if part.is_empty() {
            return None;
        }
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(part);
    }

    if out.is_empty() { None } else { Some(out) }
}

/// A lossy, display-only rendering for paths that cannot become `rel_path`s.
fn path_to_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

// ────────────────────────────── file reading ───────────────────────────────

/// A file's bytes plus the facts observed alongside them.
#[derive(Debug)]
pub struct FileContent {
    /// The exact bytes read. Parsing slices these; nothing re-reads the file.
    pub bytes: Vec<u8>,
    /// Hash of [`FileContent::bytes`], which doubles as the write etag (D18).
    pub content_hash: Blake3Hash,
    /// Size of the bytes actually read.
    pub size: u64,
    /// Modification time observed before and after the read agreed.
    pub mtime: SystemTime,
}

/// Reads a file, retrying if it changes underneath the read (§6.9).
///
/// A vault is assumed to be a git repository, so a `git checkout` can rewrite
/// hundreds of files mid-scan. Trusting `mtime` alone is not enough — the index
/// must hash bytes it actually read, or a half-written file would be stored as
/// content. Size is checked before and after, and used as the primary signal
/// because it is cheap and exact for the common truncate-then-write case.
///
/// Known limit: a rewrite within one `mtime` granularity that preserves length
/// is undetectable here. Such a change still surfaces later as a hash mismatch
/// against the index, which triggers a re-index rather than serving stale text.
///
/// # Errors
///
/// [`JanusError::Io`] for an unreadable file,
/// [`JanusError::ChangedDuringRead`] when the retry budget is exhausted.
pub fn read_consistent(path: &Path) -> Result<FileContent, JanusError> {
    for _ in 0..READ_ATTEMPTS {
        let before = std::fs::metadata(path)?;
        let bytes = std::fs::read(path)?;
        let after = std::fs::metadata(path)?;

        let read_len = bytes.len() as u64;
        // Both `modified()` results are compared as `Option`s: a filesystem that
        // cannot report mtime (or a platform where it errors) still gets the
        // length check rather than a spurious failure.
        let unchanged = before.len() == read_len
            && after.len() == read_len
            && before.modified().ok() == after.modified().ok();

        if unchanged {
            return Ok(FileContent {
                content_hash: Blake3Hash::of_bytes(&bytes),
                size: read_len,
                mtime: after.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                bytes,
            });
        }
    }

    Err(JanusError::ChangedDuringRead {
        path: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Builds a config with the default rules plus optional exclude globs.
    fn config_with_excludes(patterns: &[&str]) -> Config {
        let excludes = patterns
            .iter()
            .map(|pattern| format!("\"{pattern}\""))
            .collect::<Vec<_>>()
            .join(", ");
        Config::from_toml(
            &format!("[index]\nexclude = [{excludes}]\n"),
            Path::new("<test>"),
        )
        .expect("test config must parse")
    }

    fn walk_hermetic(root: &Path, config: &Config) -> WalkResult {
        walk(root, config, WalkOptions::hermetic()).expect("walk must succeed")
    }

    fn rel_paths(result: &WalkResult) -> Vec<&str> {
        result.files.iter().map(|f| f.rel_path.as_str()).collect()
    }

    // ───────────────────────── path helpers ─────────────────────────

    #[test]
    fn rel_path_is_slash_separated_and_relative() {
        assert_eq!(
            rel_path_of(Path::new("/vault"), Path::new("/vault/a/b.md")).as_deref(),
            Some("a/b.md")
        );
        assert_eq!(
            rel_path_of(Path::new("/vault"), Path::new("/vault/top.md")).as_deref(),
            Some("top.md")
        );
    }

    #[test]
    fn rel_path_rejects_paths_outside_the_root_and_the_root_itself() {
        assert_eq!(
            rel_path_of(Path::new("/vault"), Path::new("/elsewhere/a.md")),
            None
        );
        assert_eq!(rel_path_of(Path::new("/vault"), Path::new("/vault")), None);
    }

    #[test]
    fn rel_path_handles_unicode_and_spaces() {
        assert_eq!(
            rel_path_of(Path::new("/vault"), Path::new("/vault/日本語/mein plan.md")).as_deref(),
            Some("日本語/mein plan.md")
        );
    }

    // ─────────────────────────── classification ───────────────────────────

    #[test]
    fn classify_prefers_the_first_matching_rule() {
        let config = config_with_excludes(&["notes/skip.md"]);

        assert_eq!(
            classify(&config, ".git/config", OsStr::new(".git"), true, false),
            Some(SkipReason::AlwaysExcluded)
        );
        assert_eq!(
            classify(&config, ".secret/x.md", OsStr::new(".secret"), true, false),
            Some(SkipReason::Hidden)
        );
        assert_eq!(
            classify(
                &config,
                "notes/skip.md",
                OsStr::new("skip.md"),
                false,
                false
            ),
            Some(SkipReason::ConfigExclude)
        );
        assert_eq!(
            classify(
                &config,
                "notes/keep.md",
                OsStr::new("keep.md"),
                false,
                false
            ),
            None
        );
    }

    #[test]
    fn markdown_detection_is_case_insensitive() {
        for name in ["a.md", "a.MD", "a.Markdown", "a.markdown"] {
            assert!(is_markdown(OsStr::new(name)), "{name} should be markdown");
        }
        for name in ["a.txt", "a.md.bak", "a", "md"] {
            assert!(!is_markdown(OsStr::new(name)), "{name} should not be");
        }
    }

    #[test]
    fn hidden_detection_only_looks_at_the_leading_dot() {
        assert!(is_hidden(OsStr::new(".git")));
        assert!(is_hidden(OsStr::new(".md")));
        assert!(!is_hidden(OsStr::new("a.md")));
        assert!(!is_hidden(OsStr::new("a.b.md")));
    }

    // ──────────────────────────── walking ────────────────────────────

    #[test]
    fn walk_admits_only_markdown_outside_excluded_trees() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("notes")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::create_dir_all(root.join(".private")).unwrap();
        fs::create_dir_all(root.join("archive/private")).unwrap();
        fs::create_dir_all(root.join("archive/public")).unwrap();

        fs::write(root.join("notes/keep.md"), "# keep").unwrap();
        fs::write(root.join("notes/ALSO.MD"), "# upper").unwrap();
        fs::write(root.join("notes/readme.txt"), "no").unwrap();
        fs::write(root.join(".git/config"), "no").unwrap();
        fs::write(root.join("target/build.md"), "no").unwrap();
        fs::write(root.join(".private/secret.md"), "no").unwrap();
        fs::write(root.join("archive/private/secret.md"), "no").unwrap();
        fs::write(root.join("archive/public/ok.md"), "# ok").unwrap();

        let config = config_with_excludes(&["archive/private/**"]);
        let result = walk_hermetic(root, &config);

        assert_eq!(
            rel_paths(&result),
            ["archive/public/ok.md", "notes/ALSO.MD", "notes/keep.md"]
        );
    }

    #[test]
    fn every_skip_is_attributed_to_one_rule() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::create_dir_all(root.join(".obsidian")).unwrap();
        fs::create_dir_all(root.join("private")).unwrap();

        fs::write(root.join("ok.md"), "# ok").unwrap();
        fs::write(root.join("notes.txt"), "no").unwrap();
        fs::write(root.join("target/out.md"), "no").unwrap();
        fs::write(root.join(".obsidian/ws.md"), "no").unwrap();
        fs::write(root.join(".hidden.md"), "no").unwrap();
        fs::write(root.join("private/x.md"), "no").unwrap();

        let config = config_with_excludes(&["private/**"]);
        let result = walk_hermetic(root, &config);

        assert_eq!(rel_paths(&result), ["ok.md"]);
        assert_eq!(result.count(SkipReason::AlwaysExcluded), 2);
        assert_eq!(result.count(SkipReason::Hidden), 1);
        assert_eq!(result.count(SkipReason::ConfigExclude), 1);
        assert_eq!(result.count(SkipReason::NotMarkdown), 1);
        assert_eq!(result.skipped_total(), 5);
    }

    #[test]
    fn walk_output_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for name in ["c.md", "a.md", "b.md"] {
            fs::write(root.join(name), "# doc").unwrap();
        }
        let config = config_with_excludes(&[]);

        let first_result = walk_hermetic(root, &config);
        let second_result = walk_hermetic(root, &config);
        let first = rel_paths(&first_result);
        let second = rel_paths(&second_result);

        assert_eq!(
            first,
            ["a.md", "b.md", "c.md"],
            "must be sorted by rel_path"
        );
        assert_eq!(first, second);
    }

    #[test]
    fn gitignored_files_are_excluded_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join(".gitignore"), "draft.md\nscratch/\n").unwrap();
        fs::write(root.join("draft.md"), "# draft").unwrap();
        fs::write(root.join("keep.md"), "# keep").unwrap();
        fs::create_dir_all(root.join("scratch")).unwrap();
        fs::write(root.join("scratch/a.md"), "# a").unwrap();

        let config = config_with_excludes(&[]);
        let result = walk_hermetic(root, &config);

        assert_eq!(rel_paths(&result), ["keep.md"]);
        assert_eq!(
            result.ignored_by_git,
            Some(2),
            "both the ignored file and the ignored directory's file must be counted"
        );
    }

    #[test]
    fn gitignore_is_honoured_without_a_git_repository() {
        // `require_git` defaults to true in `ignore`; a corpus that is an
        // exported copy has a `.gitignore` and no `.git` (§6.1).
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(!root.join(".git").exists());
        fs::write(root.join(".gitignore"), "skip.md\n").unwrap();
        fs::write(root.join("skip.md"), "# skip").unwrap();
        fs::write(root.join("keep.md"), "# keep").unwrap();

        let config = config_with_excludes(&[]);
        assert_eq!(rel_paths(&walk_hermetic(root, &config)), ["keep.md"]);
    }

    #[test]
    fn attribution_can_be_switched_off() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.md"), "# a").unwrap();

        let config = config_with_excludes(&[]);
        let options = WalkOptions {
            attribute_gitignore: false,
            ..WalkOptions::hermetic()
        };
        let result = walk(root, &config, options).unwrap();

        assert_eq!(result.ignored_by_git, None);
    }

    #[test]
    fn an_empty_corpus_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_with_excludes(&[]);
        let result = walk_hermetic(dir.path(), &config);

        assert!(result.files.is_empty());
        assert!(result.skipped.is_empty());
        assert!(result.errors.is_empty());
    }

    #[test]
    fn walking_a_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.md");
        fs::write(&file, "# a").unwrap();

        let config = config_with_excludes(&[]);
        let error = walk(&file, &config, WalkOptions::hermetic()).unwrap_err();
        assert!(matches!(error, JanusError::VaultNotDirectory { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn directory_symlinks_are_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("hidden-away.md"), "# not mine").unwrap();
        fs::write(root.join("real.md"), "# real").unwrap();

        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        let config = config_with_excludes(&[]);
        let result = walk_hermetic(&root, &config);

        assert_eq!(rel_paths(&result), ["real.md"]);
        assert!(
            result
                .skipped
                .iter()
                .any(|s| s.reason == SkipReason::SymlinkEscape
                    || s.reason == SkipReason::SymlinkedDirectory),
            "the escaping symlink must be reported: {:?}",
            result.skipped
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_symlinks_inside_the_corpus_are_admitted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("real.md"), "# real").unwrap();
        std::os::unix::fs::symlink(root.join("real.md"), root.join("alias.md")).unwrap();

        let config = config_with_excludes(&[]);
        let result = walk_hermetic(root, &config);

        assert_eq!(rel_paths(&result), ["alias.md", "real.md"]);
    }

    #[cfg(unix)]
    #[test]
    fn file_symlinks_escaping_the_corpus_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        fs::create_dir_all(&root).unwrap();
        let outside = dir.path().join("outside.md");
        fs::write(&outside, "# not mine").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("leak.md")).unwrap();

        let config = config_with_excludes(&[]);
        let result = walk_hermetic(&root, &config);

        assert!(result.files.is_empty());
        assert_eq!(result.count(SkipReason::SymlinkEscape), 1);
    }

    // ─────────────────────────── confinement ───────────────────────────

    #[test]
    fn confine_accepts_paths_inside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        fs::create_dir_all(root.join("a")).unwrap();
        let file = root.join("a/b.md");
        fs::write(&file, "# b").unwrap();

        assert_eq!(
            confine(&root, &file).unwrap(),
            dunce::canonicalize(&file).unwrap()
        );
    }

    #[test]
    fn confine_rejects_escaping_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let outside = root.parent().unwrap().join("outside.md");
        fs::write(&outside, "# no").unwrap();

        let error = confine(&root, &outside).unwrap_err();
        assert!(matches!(error, JanusError::PathEscape { .. }));
        assert_eq!(
            error.exit_code(),
            4,
            "a refusal must not look like a runtime error"
        );
    }

    #[test]
    fn confine_rejects_a_sibling_with_the_same_prefix() {
        // `/vault-evil` must not pass a check against `/vault`: `starts_with`
        // compares components, not string prefixes.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        let sibling = dir.path().join("vault-evil");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&sibling).unwrap();
        let file = sibling.join("x.md");
        fs::write(&file, "# x").unwrap();

        let error = confine(&root, &file).unwrap_err();
        assert!(matches!(error, JanusError::PathEscape { .. }));
    }

    #[test]
    fn confine_refuses_paths_into_the_sidecar_or_git() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        for forbidden in [".janus", ".git"] {
            fs::create_dir_all(root.join(forbidden)).unwrap();
            let file = root.join(forbidden).join("x.md");
            fs::write(&file, "# x").unwrap();

            let error = confine(&root, &file).unwrap_err();
            assert!(
                matches!(error, JanusError::PathEscape { .. }),
                "{forbidden} must be refused, got: {error}"
            );
        }
    }

    #[test]
    fn confine_rejects_missing_paths_instead_of_guessing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();

        let error = confine(&root, &root.join("not-yet.md")).unwrap_err();
        assert!(matches!(error, JanusError::PathResolve { .. }));
        assert_eq!(error.exit_code(), 1);
    }

    // ──────────────────────────── read_consistent ────────────────────────────

    #[test]
    fn read_consistent_returns_bytes_and_their_hash() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.md");
        fs::write(&file, "# hello\n").unwrap();

        let content = read_consistent(&file).unwrap();

        assert_eq!(content.bytes, b"# hello\n");
        assert_eq!(content.size, 8);
        assert_eq!(content.content_hash, Blake3Hash::of_bytes(b"# hello\n"));
    }

    #[test]
    fn read_consistent_reports_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let error = read_consistent(&dir.path().join("nope.md")).unwrap_err();
        assert!(matches!(error, JanusError::Io(_)));
    }

    // ────────────────────────── root resolution ──────────────────────────

    #[test]
    fn resolve_corpus_prefers_the_explicit_path() {
        let dir = tempfile::tempdir().unwrap();
        let explicit = dir.path().join("explicit");
        fs::create_dir_all(&explicit).unwrap();

        let resolved = resolve_corpus(Some(explicit.clone())).unwrap();
        assert_eq!(resolved, dunce::canonicalize(&explicit).unwrap());
    }

    #[test]
    fn resolve_corpus_rejects_an_explicit_path_that_is_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.md");
        fs::write(&file, "# a").unwrap();

        let error = resolve_corpus(Some(file)).unwrap_err();
        assert!(matches!(error, JanusError::VaultNotDirectory { .. }));
    }
}
