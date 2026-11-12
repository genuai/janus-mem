//! The single error type and its exit-code mapping (SPEC §9.1, §10).
//!
//! One enum serves the whole library, which keeps `?` usable everywhere and
//! makes the exit-code mapping exhaustive: adding a variant without deciding
//! its code is a compile error rather than a silent `1`.

use std::io;
use std::path::PathBuf;

/// Everything that can go wrong, with the failing path attached.
///
/// Variants carry `path` fields on purpose. An error that says "permission
/// denied" without naming the file forces the user to guess, and forces an
/// agent to retry blindly.
#[derive(Debug, thiserror::Error)]
pub enum JanusError {
    /// A config already exists where `janus init` was asked to create one.
    /// Initialisation never overwrites, so this is a refusal, not a failure.
    #[error("vault already initialized at {path}")]
    ConfigExists {
        /// The existing config's full path.
        path: PathBuf,
    },

    /// The resolved vault has no `janus.toml`. Config is required: without it
    /// the engine cannot know the vault's frontmatter aliases or type weights,
    /// so guessing would silently mis-read the whole corpus (D47, §6.11).
    #[error("no corpus config at {path}")]
    ConfigMissing {
        /// Where the missing config was expected.
        path: PathBuf,
    },

    /// The config exists but could not be read (permissions, invalid UTF-8).
    #[error("failed to read config {path}")]
    ConfigRead {
        /// The config's full path.
        path: PathBuf,
        /// The underlying IO failure.
        #[source]
        source: io::Error,
    },

    /// The config is not valid TOML, or a field has the wrong type.
    ///
    /// Unknown keys are *not* an error: real vaults carry forward-compatible
    /// settings, and refusing to start on an unrecognised key would make
    /// downgrades impossible.
    #[error("invalid config {path}")]
    ConfigParse {
        /// The config's full path.
        path: PathBuf,
        /// The parser's complaint, including a line and column.
        #[source]
        source: toml::de::Error,
    },

    /// The workspace file cannot be used: it is absent, unreadable, not valid
    /// TOML, missing a required key, holding a duplicate `id`, or naming more
    /// than one primary corpus (§6.11, §2.1 of the M6b plan). The message names
    /// the file and the entry.
    ///
    /// Refused rather than patched: "primary" is what an unqualified query
    /// means, so guessing which entry was meant would answer the wrong question.
    #[error("invalid workspace {path}: {message}")]
    WorkspaceInvalid {
        /// The workspace file's full path.
        path: PathBuf,
        /// What is wrong, naming the entry when one is to blame.
        message: String,
    },

    /// The path could not be turned into an absolute path (missing, permission denied).
    /// Carries the path so the user knows *which* directory failed.
    #[error("cannot resolve vault directory {path}")]
    VaultResolve {
        /// The path as the user supplied it.
        path: PathBuf,
        /// The underlying IO failure.
        #[source]
        source: io::Error,
    },

    /// No corpus root could be found: no `--vault`, and no ancestor of the
    /// working directory contains `janus.toml` or `.janus/` (§6.1).
    #[error("no corpus found from {from}: pass --vault or run `janus init`")]
    VaultNotFound {
        /// The directory the search started from.
        from: PathBuf,
    },

    /// A path could not be canonicalized, so confinement cannot be decided.
    #[error("cannot resolve path {path}")]
    PathResolve {
        /// The path that failed to resolve.
        path: PathBuf,
        /// The underlying IO failure.
        #[source]
        source: io::Error,
    },

    /// A path resolved outside the corpus root, or into `.git/` / `.janus/`.
    /// Refused rather than skipped: a caller asking for such a path is either
    /// confused or hostile, and both deserve a clear answer (§8.5, §8.6).
    #[error("refusing path outside the corpus: {path}")]
    PathEscape {
        /// The offending path, as supplied.
        path: PathBuf,
    },

    /// A file kept changing while it was being read, so no consistent snapshot
    /// could be taken (§6.9).
    #[error("file changed while being read: {path}")]
    ChangedDuringRead {
        /// The file that would not hold still.
        path: PathBuf,
    },

    /// The path exists but is a file, a socket, etc.
    #[error("vault path is not a directory: {path}")]
    VaultNotDirectory {
        /// The offending path.
        path: PathBuf,
    },

    /// A directory could not be created.
    #[error("failed to create directory {path}")]
    CreateDir {
        /// The directory that could not be created.
        path: PathBuf,
        /// The underlying IO failure.
        #[source]
        source: io::Error,
    },

    /// A `PATH` argument resolved inside the corpus but matched no admitted
    /// document: it is excluded by config, hidden, not Markdown, or could not be
    /// read. Not a refusal — the path is legal and the vault is untouched — so
    /// it is a runtime error that names the path, which is the only way a caller
    /// can tell "excluded" from "typo".
    #[error("no document at {path} in this corpus (excluded, hidden, not Markdown, or unreadable)")]
    DocumentNotInCorpus {
        /// The path as the user supplied it.
        path: PathBuf,
    },

    /// A document read found the file, but the generation does not describe
    /// these bytes: the body moved after the build, or no chunk of it answers
    /// to the id. A span over older bytes is wrong, so a rebuild is the cure.
    #[error("document {path}: the index does not describe these bytes; run janus index")]
    DocumentChanged {
        /// Vault-relative path of the document that moved.
        path: PathBuf,
    },

    /// A `--topic` query returned no `current` result, so there is no document
    /// whose chain could be rendered. Not an error in the corpus: the query is
    /// the subject, and the message repeats it verbatim.
    #[error("lineage --topic {topic:?} matched no current document")]
    TopicNotFound {
        /// The query text as the user supplied it.
        topic: String,
    },

    /// A file could not be written.
    #[error("failed to write {path}")]
    Write {
        /// The file that could not be written.
        path: PathBuf,
        /// The underlying IO failure.
        #[source]
        source: io::Error,
    },

    /// `.janus/lock` was not free within the timeout (§3.5, S-7). Another
    /// writer holds it, and this command changed nothing.
    #[error("lock timeout: {path} is held by another writer")]
    LockTimeout {
        /// The lock file that could not be acquired in time.
        path: PathBuf,
    },

    /// No generation validated and no rebuild was allowed (§3.5, S-17). Recovery
    /// is a rebuild, never a restore: the index rename destroys the old bytes.
    #[error("index is unstable: {path}")]
    IndexUnstable {
        /// The index file whose bytes did not match a manifest.
        path: PathBuf,
    },

    /// The vault has never been indexed. Refusing a read is not corruption: an
    /// agent told "corrupt" would try to repair a file that does not exist
    /// (S-8, phase 6).
    #[error("no index yet in {dir}; run `janus index`")]
    IndexMissing {
        /// The resolved index directory to name in the message.
        dir: PathBuf,
    },

    /// The generation was built for another format, tokenizer, chunker, corpus
    /// or embedder, so only a cold build can cure it (M4D §3.3, D-3). A read may
    /// not rebuild these: it would re-embed the whole corpus unasked. The
    /// message names the command that can, and repeats the mismatch that
    /// refused the read.
    #[error("index needs a full build: run janus index ({reason})")]
    IndexNeedsFullBuild {
        /// The manifest field that disagreed, and its two values.
        reason: String,
    },

    /// The surface was started without `--allow-write`, so it serves no write
    /// (M7-15, M7-36). The flag is the only switch; a config key cannot grant
    /// a write.
    #[error("{surface} serves no write: start it with --allow-write")]
    WriteDisabled {
        /// The surface that refused, e.g. `janus mcp`.
        surface: String,
    },

    /// The corpus refuses every write: its `read_only` is true, which an
    /// archived corpus only defaults to (M7-16, M7-28). The field exists only in
    /// a workspace, so this refusal cannot fire in single-corpus mode
    /// (case 2.7b).
    #[error("corpus {corpus} is read-only, so no write may reach it")]
    ReadOnlyCorpus {
        /// The name the caller used, followed by the declared `corpus_id` when
        /// the two differ. A workspace's name for a corpus is the one a caller
        /// types, so a message that named only the declared id would send them
        /// looking for a corpus their file does not hold.
        corpus: String,
    },

    /// The bytes on disk are not the bytes the caller saw, so the write would
    /// erase another writer's change (§9.4, M7-12). Both hashes are reported, so
    /// the caller can re-read and retry. Exit 3.
    #[error("the file changed since it was read: expected {expected}, found {current} ({path})")]
    Conflict {
        /// The document the write named.
        path: PathBuf,
        /// The hash the caller presented.
        expected: String,
        /// The hash of the bytes on disk now.
        current: String,
    },

    /// A write to an existing file carried no etag, so it never happened
    /// (case 2.1, M7-12). `preview` is the one line the failure offers in
    /// `hint`: the keys that would change (M7-13). Exit 3.
    #[error("no expected_hash for {path}; read the document and pass the content_hash it returned")]
    PreconditionRequired {
        /// The document the write named.
        path: PathBuf,
        /// The one-line preview the failure carries in `hint`.
        preview: String,
    },

    /// A patch key's value has a shape the writer cannot splice, so the write is
    /// refused and the file keeps its bytes (§2.4).
    #[error("{path}: `{key}` holds a value the writer cannot splice: {reason}")]
    UnsupportedPatch {
        /// The document the write named.
        path: PathBuf,
        /// The key whose value is refused.
        key: String,
        /// Why it is refused, from [`crate::write::reason`].
        reason: &'static str,
    },

    /// A patch key is outside `[A-Za-z0-9][A-Za-z0-9._-]*` (§2.1). A key is
    /// written as a line, so a key with a newline, `: `, `#` or a quote could
    /// add or split a key.
    #[error("{path}: `{key}` is not a key: use letters, digits, dot, underscore or dash")]
    KeyShape {
        /// The document the write named.
        path: PathBuf,
        /// The offending key, as the request wrote it.
        key: String,
    },

    /// The document has no frontmatter block and the request did not ask for
    /// one, so there is nothing to patch.
    #[error("{path} has no frontmatter block to patch")]
    NoFrontmatter {
        /// The document the write named.
        path: PathBuf,
    },

    /// A create named a path that already holds a file (§2.4, M7-27). A retry
    /// with the same request cannot succeed, so this is a refusal.
    #[error("{path} already exists; use a mode that replaces it")]
    PathExists {
        /// The path the create named.
        path: PathBuf,
    },

    /// A command-line combination that cannot mean anything: understood, but
    /// rejected before any work (§9.1 code 2).
    #[error("{message}")]
    Usage {
        /// The explanation, naming the flags involved.
        message: String,
    },

    /// Any IO failure without a more specific variant.
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl JanusError {
    /// The process exit code for this error (§9.1).
    ///
    /// `0` success, `1` runtime failure, `2` usage error (raised by clap before
    /// we get here), `3` conflict/stale hash, `4` refused.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            // Refusals: the command was understood and deliberately not carried
            // out. Distinguishing these from runtime failures lets a script
            // retry a transient error without retrying a policy decision.
            Self::ConfigExists { .. }
            | Self::VaultNotDirectory { .. }
            | Self::PathEscape { .. }
            | Self::WriteDisabled { .. }
            | Self::ReadOnlyCorpus { .. }
            | Self::UnsupportedPatch { .. }
            | Self::KeyShape { .. }
            | Self::NoFrontmatter { .. }
            | Self::PathExists { .. } => 4,

            // A write that lost a race, and a write with no etag: both are
            // cured by re-reading and retrying (M7-12).
            Self::Conflict { .. } | Self::PreconditionRequired { .. } => 3,

            Self::VaultResolve { .. }
            | Self::VaultNotFound { .. }
            | Self::PathResolve { .. }
            | Self::DocumentNotInCorpus { .. }
            | Self::DocumentChanged { .. }
            | Self::TopicNotFound { .. }
            | Self::ChangedDuringRead { .. }
            | Self::ConfigMissing { .. }
            | Self::ConfigRead { .. }
            | Self::ConfigParse { .. }
            | Self::CreateDir { .. }
            | Self::Write { .. }
            | Self::LockTimeout { .. }
            | Self::IndexUnstable { .. }
            | Self::IndexNeedsFullBuild { .. }
            | Self::IndexMissing { .. }
            | Self::Io(_) => 1,

            // Understood and rejected before doing anything: the caller can fix
            // the invocation, and a retry without the change is pointless.
            Self::Usage { .. } | Self::WorkspaceInvalid { .. } => 2,
        }
    }
}
