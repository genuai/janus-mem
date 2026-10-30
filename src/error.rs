use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum JanusError {
    #[error("vault already initialized at {path}")]
    ConfigExists { path: PathBuf },

    /// The path could not be turned into an absolute path (missing, permission denied).
    /// Carries the path so the user knows *which* directory failed.
    #[error("cannot resolve vault directory {path}")]
    VaultResolve {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// The path exists but is a file, a socket, etc.
    #[error("vault path is not a directory: {path}")]
    VaultNotDirectory { path: PathBuf },

    #[error("failed to create directory {path}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error(transparent)]
    Io(#[from] io::Error),
}

impl JanusError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::ConfigExists { .. } | Self::VaultNotDirectory { .. } => 4,
            Self::VaultResolve { .. }
            | Self::CreateDir { .. }
            | Self::Write { .. }
            | Self::Io(_) => 1,
        }
    }
}
