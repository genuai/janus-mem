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

    /// A file could not be written.
    #[error("failed to write {path}")]
    Write {
        /// The file that could not be written.
        path: PathBuf,
        /// The underlying IO failure.
        #[source]
        source: io::Error,
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
            Self::ConfigExists { .. } | Self::VaultNotDirectory { .. } => 4,

            Self::VaultResolve { .. }
            | Self::ConfigMissing { .. }
            | Self::ConfigRead { .. }
            | Self::ConfigParse { .. }
            | Self::CreateDir { .. }
            | Self::Write { .. }
            | Self::Io(_) => 1,
        }
    }
}
