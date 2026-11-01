//! `insta` golden over `scan --json` on the committed fixture vault (§10).
//!
//! The fixture is copied to a temporary directory first and every
//! machine-dependent field is redacted, so the snapshot is stable across clones,
//! checkouts and CI runners. See `tests/common/mod.rs` for the redaction set and
//! why two fixture inputs are generated instead of committed.
//!
//! # Hermeticity caveat
//!
//! `janus scan` uses `WalkOptions::default()`, which honours the user's global
//! gitignore and parent-directory ignore files — the right behaviour for a real
//! corpus (§6.1). CI runners have neither, so the golden is stable there. A
//! developer with an exotic `~/.config/git/ignore` (or an ignore file in a parent
//! of the system temp directory) may see a diff in `ignored_by_git` or
//! `skipped`. That is a signal about the environment, not a flaky test; a
//! `--hermetic` scan flag is M5's to add, not phase 5's.

mod common;

use common::{fixture_vault, redact, scan_json};

#[test]
fn scan_json_matches_the_golden() {
    let vault = fixture_vault();
    let mut report = scan_json(vault.path(), &[]);
    redact(&mut report);

    let json = serde_json::to_string_pretty(&report).expect("the report serializes");
    insta::assert_snapshot!("scan", json);
}
