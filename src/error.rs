use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum JanusError {
    #[error("vault already initialized at {path}")]
    ConfigExists { path: PathBuf },

    #[error("failed to write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl JanusError {
    pub fn exit_code(&self) -> u8 {
        match self {
            JanusError::ConfigExists { .. } => 4,
            JanusError::Write { .. } | JanusError::Io(_) => 1,
        }
    }
}
