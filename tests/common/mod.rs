//! Shared helpers for tests that scan the committed fixture vault.
//!
//! Two ugly inputs are **generated**, not committed: a ~2 MB document (a
//! committed one costs every clone forever) and a non-UTF-8 `.md` (an
//! unreviewable binary diff). Both are written into the temporary copy, so the
//! golden exercises them without either living in git (T-2 in §12).
//!
//! The fixture is copied to a temp directory because `scan --json` carries the
//! absolute root, config path, every document path and its `mtime`. Scanning the
//! checkout would make the golden depend on where the repo lives.
//!
//! `notes/undated.md` deliberately has no frontmatter or filename date, so its
//! `date` is the checkout time. The redactor replaces it with `<mtime-date>`;
//! its declared id keeps the golden's identity stable.

// Each integration-test crate compiles this module separately, so helpers used
// by only one of them would otherwise be dead code in the others.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::{Value, json};

/// The committed fixture vault, relative to the crate root.
pub const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vault");

/// Fixture document generated at ~2 MB instead of being committed.
pub const LARGE_REL: &str = "big/2025-03-06-large.md";

/// Fixture document whose bytes are not valid UTF-8.
pub const NON_UTF8_REL: &str = "notes/non-utf8.md";

/// A `janus` command ready to configure.
pub fn janus() -> Command {
    Command::cargo_bin("janus").expect("the janus binary must build for tests")
}

/// Copies the committed fixture into a fresh temporary directory and adds the
/// generated ugly inputs.
///
/// The returned `TempDir` is the vault root.
pub fn fixture_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir for the fixture copy");
    copy_tree(Path::new(FIXTURE), dir.path()).expect("copy the fixture vault");
    write_generated(dir.path());
    dir
}

/// Runs `janus --vault <root> scan --json` and returns the parsed report.
pub fn scan_json(root: &Path, extra: &[&str]) -> Value {
    let output = janus()
        .arg("--vault")
        .arg(root)
        .arg("scan")
        .arg("--json")
        .args(extra)
        .output()
        .expect("run janus scan --json");

    assert!(
        output.status.success(),
        "scan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// Replaces every field that legitimately differs between machines and clones.
///
/// The set is deliberately narrow: only `taken_ms`, the absolute paths, and the
/// `mtime`-derived date. Redacting whole documents would stop the golden from
/// noticing a real change (J-7).
pub fn redact(report: &mut Value) {
    report["taken_ms"] = json!(0);
    report["vault"] = json!("<vault>");
    report["config"] = json!("<vault>/janus.toml");

    let Some(documents) = report["docs"].as_array_mut() else {
        return;
    };

    for document in documents {
        let rel_path = document["rel_path"]
            .as_str()
            .unwrap_or("<unknown>")
            .to_owned();
        document["path"] = json!(format!("<vault>/{rel_path}"));
        document["mtime"] = json!("<mtime>");
        if document["date_source"] == "mtime" {
            document["date"] = json!("<mtime-date>");
        }
    }
}

/// Recursively copies `from` to `to`, byte-for-byte.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target: PathBuf = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Writes the two generated fixture documents.
fn write_generated(root: &Path) {
    let large = root.join(LARGE_REL);
    fs::create_dir_all(large.parent().expect("large document has a parent"))
        .expect("create the generated directory");
    fs::write(&large, large_document()).expect("write the large fixture document");

    let non_utf8 = root.join(NON_UTF8_REL);
    fs::write(non_utf8, NON_UTF8_BYTES).expect("write the non-UTF-8 fixture document");
}

/// A deterministic document just over 2 MB.
fn large_document() -> String {
    const PARAGRAPH: &str = "This paragraph is repeated until the document is large enough to \
        exercise the read path without being committed to the repository.\n\n";

    let mut text = String::from(
        "---\ntitle: Large document\ndate: 2025-03-06\ntags: [large]\n---\n\n\
         # Large document\n\n",
    );
    while text.len() < 2_000_000 {
        text.push_str(PARAGRAPH);
    }
    text
}

/// Bytes that look like a frontmatter block but are not valid UTF-8.
const NON_UTF8_BYTES: &[u8] = b"---\ntitle: Invalid bytes\n\xff\xfe\x00\n---\n# Invalid UTF-8\n";
