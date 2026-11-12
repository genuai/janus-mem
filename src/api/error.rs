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
            // The two codes of exit 3: a write that lost a race, and a write
            // with no etag. Both mean "re-read, then retry" (M7-12).
            JanusError::Conflict { .. } => Self {
                code: "conflict",
                exit: 3,
            },
            JanusError::PreconditionRequired { .. } => Self {
                code: "precondition_required",
                exit: 3,
            },
            // Refusals: a retry without a change of request cannot succeed.
            JanusError::UnsupportedPatch { .. } => Self {
                code: "unsupported_patch",
                exit: 4,
            },
            JanusError::KeyShape { .. } => Self {
                code: "key_shape",
                exit: 4,
            },
            JanusError::NoFrontmatter { .. } => Self {
                code: "no_frontmatter",
                exit: 4,
            },
            JanusError::ReadOnlyCorpus { .. } => Self {
                code: "read_only_corpus",
                exit: 4,
            },
            JanusError::WriteDisabled { .. } => Self {
                code: "write_disabled",
                exit: 4,
            },
            JanusError::PathExists { .. } => Self {
                code: "path_exists",
                exit: 4,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The eight codes M7-4 adds, with the exit code each one carries (§2.4).
    ///
    /// A server answers and does not exit, so this table is where an exit code
    /// is asserted for every code. `tests/cli_set.rs` proves the same numbers
    /// through a process.
    #[test]
    fn case_4_3_the_eight_new_codes_name_their_exit_code() {
        let path = PathBuf::from("notes/one.md");
        let cases: [(JanusError, &str, u8); 8] = [
            (
                JanusError::Conflict {
                    path: path.clone(),
                    expected: "aa".to_owned(),
                    current: "bb".to_owned(),
                },
                "conflict",
                3,
            ),
            (
                JanusError::PreconditionRequired {
                    path: path.clone(),
                    preview: "would change: status".to_owned(),
                },
                "precondition_required",
                3,
            ),
            (
                JanusError::UnsupportedPatch {
                    path: path.clone(),
                    key: "tags".to_owned(),
                    reason: "block sequence",
                },
                "unsupported_patch",
                4,
            ),
            (
                JanusError::KeyShape {
                    path: path.clone(),
                    key: "a b".to_owned(),
                },
                "key_shape",
                4,
            ),
            (
                JanusError::NoFrontmatter { path: path.clone() },
                "no_frontmatter",
                4,
            ),
            (
                JanusError::ReadOnlyCorpus {
                    corpus: "context".to_owned(),
                },
                "read_only_corpus",
                4,
            ),
            (
                JanusError::WriteDisabled {
                    surface: "janus mcp".to_owned(),
                },
                "write_disabled",
                4,
            ),
            (JanusError::PathExists { path }, "path_exists", 4),
        ];
        for (error, code, exit) in cases {
            assert_eq!(ApiCode::of(&error).code, code);
            assert_eq!(ApiCode::of(&error).exit, exit, "{code}");
            // The table and the CLI's own mapping cannot drift apart.
            assert_eq!(error.exit_code(), exit, "{code}");
        }
    }

    /// Exit 3 is used for the first time, by two codes, and no existing code
    /// moved (§8 of the M7 plan).
    #[test]
    fn case_4_3_exit_three_is_new_and_nothing_else_moved() {
        let exits = |error: JanusError| ApiCode::of(&error).exit;
        assert_eq!(
            exits(JanusError::Usage {
                message: String::new()
            }),
            2
        );
        assert_eq!(
            exits(JanusError::DocumentNotInCorpus {
                path: PathBuf::from("notes/one.md")
            }),
            1
        );
        assert_eq!(
            exits(JanusError::PathEscape {
                path: PathBuf::from("../one.md")
            }),
            4
        );
        assert_eq!(
            exits(JanusError::LockTimeout {
                path: PathBuf::from(".janus/lock")
            }),
            1
        );
        assert_eq!(
            exits(JanusError::Write {
                path: PathBuf::from("notes/one.md"),
                source: std::io::Error::other("x"),
            }),
            1
        );
        assert_eq!(ApiCode::refused().exit, 4);
    }
}
