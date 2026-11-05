//! `janus doctor` end to end: the scan-half cases of M5 §3.10.
//!
//! Every vault is a temp copy. The committed fixtures are never written to, and
//! no test reads the wall clock.

mod common;

use std::fs;
use std::path::Path;

use common::{run, run_ok};
use serde_json::Value;

/// Runs `doctor --json` and returns the process output and parsed report.
fn doctor(root: &Path) -> (std::process::Output, Value) {
    let output = run(root, &["doctor", "--json"]);
    let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    (output, report)
}

/// Every check's code, in report order.
fn codes(report: &Value) -> Vec<String> {
    report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .map(|check| check["code"].as_str().expect("code").to_owned())
        .collect()
}

/// Writes a config and one Markdown file per entry into a temp vault.
fn vault(docs: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"doctor\"\n",
    )
    .expect("write config");
    for (rel_path, text) in docs {
        write(dir.path(), rel_path, text);
    }
    dir
}

/// Writes one document, creating its directories.
fn write(root: &Path, rel_path: &str, text: &str) {
    let path = root.join(rel_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("directory");
    }
    fs::write(path, text).expect("write document");
}

/// Builds a lexical index, which is the generation the later graph half reads.
fn index(root: &Path) {
    run_ok(root, &["index", "--no-embeddings"]);
}

#[test]
fn case_10_5_one_unknown_type_exits_zero() {
    // `speculative` is not in the registry, so the document keeps its name and
    // gets one `unknown_type` warning. A warning never changes the exit code.
    // The index is built first, so `index_absent` is not a second warning.
    let vault = vault(&[(
        "notes/a.md",
        "---\ntitle: A\ntype: speculative\ndate: 2021-01-01\n---\n# A\n",
    )]);
    let root = vault.path();
    index(root);

    let (output, report) = doctor(root);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["summary"]["errors"], 0, "{report}");
    assert_eq!(report["summary"]["warnings"], 1, "{report}");
    assert_eq!(codes(&report), ["unknown_type"]);
    assert_eq!(report["checks"][0]["path"], "notes/a.md", "{report}");
}

#[test]
fn case_10_6_duplicate_identity_is_an_error_that_names_both_paths() {
    // Two documents with the same title and date carry the same rung-3 key. The
    // note is promoted to an error, and the one row names both paths. The index
    // is built first to prove the check survives a published generation (M5-44).
    let vault = vault(&[
        (
            "notes/a.md",
            "---\ntitle: Same\ndate: 2021-01-01\n---\n# A\n",
        ),
        (
            "notes/b.md",
            "---\ntitle: Same\ndate: 2021-01-01\n---\n# B\n",
        ),
    ]);
    let root = vault.path();
    index(root);

    let (output, report) = doctor(root);

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(report["ok"], false, "{report}");
    assert_eq!(report["summary"]["errors"], 1, "{report}");
    assert_eq!(codes(&report), ["duplicate_identity"], "{report}");
    let check = &report["checks"][0];
    assert_eq!(check["severity"], "error", "{check}");
    let message = check["message"].as_str().expect("message");
    assert!(message.contains("notes/a.md"), "{message}");
    assert!(message.contains("notes/b.md"), "{message}");
}
