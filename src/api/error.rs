//! The stable failure codes and the exit code each one carries (M6 §2.2).
//!
//! A code is public API: an agent branches on it, so a code string never
//! changes. The exit code beside it is the CLI's alone. A failure inside a
//! server is a response, not an exit (§9.1).
//!
//! The `match` in [`ApiCode::of`] has one arm per [`JanusError`] variant and no
//! `_` arm. A new variant is then a compile error here, and not a silent `1`:
//! `src/error.rs` already makes the same demand of `exit_code`.

use crate::error::JanusError;

/// One failure code, with the process exit code that pairs with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiCode {
    /// The stable string an agent branches on (§9.5).
    pub code: &'static str,
    /// The CLI exit code, copied from [`JanusError::exit_code`].
    pub exit: u8,
}

impl ApiCode {
    /// The policy refusal: a write tool asked of a surface that serves no write.
    ///
    /// No error variant reaches this row, because a refusal is a decision an
    /// adapter makes before any work runs (§3.8). It is a constructor so the
    /// adapters of later phases ask here, and not in their own files. M7 keeps
    /// the row and rewrites its callers.
    #[must_use]
    pub const fn refused() -> Self {
        Self {
            code: "refused",
            exit: 4,
        }
    }

    /// The code for one error. Several variants may share one code: an agent
    /// treats "no such document" and "no such topic" the same way, so they are
    /// one answer, not two (§2.2).
    #[must_use]
    pub fn of(err: &JanusError) -> Self {
        match err {
            JanusError::Usage { .. } => Self {
                code: "usage",
                exit: 2,
            },
            JanusError::ConfigExists { .. } => Self {
                code: "config_exists",
                exit: 4,
            },
            JanusError::VaultNotDirectory { .. } => Self {
                code: "vault_not_directory",
                exit: 4,
            },
            JanusError::PathEscape { .. } => Self {
                code: "path_escape",
                exit: 4,
            },
            JanusError::WorkspaceInvalid { .. } => Self {
                code: "workspace_invalid",
                exit: 2,
            },
            JanusError::ConfigMissing { .. } => Self {
                code: "config_missing",
                exit: 1,
            },
            JanusError::ConfigRead { .. } => Self {
                code: "config_read",
                exit: 1,
            },
            JanusError::ConfigParse { .. } => Self {
                code: "config_parse",
                exit: 1,
            },
            JanusError::VaultResolve { .. } => Self {
                code: "vault_resolve",
                exit: 1,
            },
            JanusError::VaultNotFound { .. } => Self {
                code: "vault_not_found",
                exit: 1,
            },
            JanusError::PathResolve { .. } => Self {
                code: "path_resolve",
                exit: 1,
            },
            JanusError::DocumentNotInCorpus { .. }
            | JanusError::DocumentChanged { .. }
            | JanusError::TopicNotFound { .. } => Self {
                code: "not_found",
                exit: 1,
            },
            JanusError::IndexMissing { .. } => Self {
                code: "index_missing",
                exit: 1,
            },
            JanusError::IndexUnstable { .. } => Self {
                code: "index_unstable",
                exit: 1,
            },
            JanusError::IndexNeedsFullBuild { .. } => Self {
                code: "index_needs_full_build",
                exit: 1,
            },
            JanusError::LockTimeout { .. } => Self {
                code: "busy",
                exit: 1,
            },
            JanusError::ChangedDuringRead { .. } => Self {
                code: "unstable_corpus",
                exit: 1,
            },
            JanusError::Write { .. } => Self {
                code: "write",
                exit: 1,
            },
            JanusError::CreateDir { .. } => Self {
                code: "create_dir",
                exit: 1,
            },
            JanusError::Io(_) => Self {
                code: "io",
                exit: 1,
            },
        }
    }
}
