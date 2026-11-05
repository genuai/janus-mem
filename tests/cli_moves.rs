//! `janus moves`: confirmed path changes and continuation hints (M5 §3.11).
//!
//! Every vault is built in a temp directory, and every confidence is derived by
//! hand from the four components in the test comment. The wall clock is never
//! read: `--since` takes an explicit date, and the index stamps its own.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;

/// A `janus` command ready to configure.
fn janus() -> Command {
    Command::cargo_bin("janus").expect("the janus binary must build for tests")
}

/// Writes a config and one Markdown file per entry into a temp vault.
fn vault(docs: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"moves\"\n\n[chunking]\nmax_chars = 1800\noverlap = 0.0\n",
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

/// Moves one document: remove the old path, write the new one.
fn move_doc(root: &Path, from: &str, to: &str, text: &str) {
    fs::remove_file(root.join(from)).expect("remove old path");
    write(root, to, text);
}

/// Runs `janus --vault <root> <args>` and returns its output.
fn run(root: &Path, args: &[&str]) -> std::process::Output {
    janus()
        .arg("--vault")
        .arg(root)
        .args(args)
        .output()
        .expect("run janus")
}

/// Runs `janus index --no-embeddings` and returns the output.
fn index(root: &Path, extra: &[&str]) -> std::process::Output {
    let mut args = vec!["index", "--no-embeddings"];
    args.extend_from_slice(extra);
    let output = run(root, &args);
    assert!(
        output.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// Runs `janus moves --json` and parses the report.
fn moves(root: &Path, extra: &[&str]) -> Value {
    let mut args = vec!["moves", "--json"];
    args.extend_from_slice(extra);
    let output = run(root, &args);
    assert!(
        output.status.success(),
        "moves failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// A body with `count` sections, all sharing one prefix so their chunk ids match
/// across two generations.
fn body(prefix: &str, count: usize) -> String {
    (1..=count)
        .map(|index| format!("## {prefix} {index}\n\n{prefix} body {index}.\n\n"))
        .collect()
}

/// A document with a title, a date, one tag and a body.
fn doc(title: &str, date: &str, tag: &str, body: &str) -> String {
    format!("---\ntitle: {title}\ndate: {date}\ntags: [{tag}]\n---\n{body}")
}

/// The confirmed rows of a report.
fn confirmed(report: &Value) -> &Vec<Value> {
    report["confirmed"].as_array().expect("confirmed")
}

/// The hint rows of a report.
fn hints(report: &Value) -> &Vec<Value> {
    report["hints"].as_array().expect("hints")
}

/// Asserts a JSON number is close enough to a hand-derived value.
fn near(value: &Value, expected: f64, label: &str) {
    let actual = value.as_f64().unwrap_or_else(|| panic!("{label}: {value}"));
    assert!(
        (actual - expected).abs() < 1e-6,
        "{label}: expected {expected}, got {actual}"
    );
}

/// Every file under `root`, path and bytes, sorted by path.
fn files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in fs::read_dir(dir).expect("read_dir") {
            let entry = entry.expect("entry");
            let path = entry.path();
            if entry.file_type().expect("file type").is_dir() {
                walk(&path, out);
            } else {
                out.push((path.clone(), fs::read(&path).expect("read file")));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort_by(|left, right| left.0.cmp(&right.0));
    out
}

// ───────────────────────────── acceptance cases ────────────────────────────

#[test]
fn case_11_1_a_move_with_title_and_date_intact_is_confirmed_with_no_hint() {
    // Case 11.1. The body changes, so content hash (rung 2) fails; title+date
    // (rung 3) matches the one vanished id, so the move is confirmed. No
    // candidate remains, so no hint exists to score.
    let vault = vault(&[(
        "notes/plan.md",
        &doc("Plan", "2024-01-01", "old", &body("shared", 4)),
    )]);
    let root = vault.path();
    index(root, &[]);

    let text = doc(
        "Plan",
        "2024-01-01",
        "old",
        &format!("{}## Extra\n\nnew words\n\n", body("shared", 4)),
    );
    move_doc(root, "notes/plan.md", "notes/plan-v2.md", &text);
    index(root, &[]);

    let report = moves(root, &[]);
    assert_eq!(confirmed(&report).len(), 1, "{report}");
    let row = &confirmed(&report)[0];
    assert_eq!(row["from"], "notes/plan.md", "{row}");
    assert_eq!(row["to"], "notes/plan-v2.md", "{row}");
    assert_eq!(row["reason"], "title_date", "{row}");
    assert!(
        hints(&report).is_empty(),
        "a kept id is not a guess: {report}"
    );
}

#[test]
fn case_11_2_eight_of_ten_chunks_and_equal_stems_score_fifty_five() {
    // Case 11.2. Old 8 chunks, new 10; all 8 old ids reappear.
    //   chunk_jaccard  = 8/10 = 0.8   -> 0.50 * 0.8 = 0.40
    //   title_match    = 0           -> 0.20 * 0 = 0.00 (titles differ)
    //   basename_match = 1           -> 0.15 * 1 = 0.15 (both stems "plan")
    //   tag_jaccard    = 0           -> 0.15 * 0 = 0.00 (tags disjoint)
    //   confidence                    = 0.55, at or above the 0.50 floor.
    let old = doc("Old plan", "2024-01-01", "old", &body("shared", 8));
    let vault = vault(&[("notes/2024-01-01-plan.md", &old)]);
    let root = vault.path();
    index(root, &[]);

    let new = doc("New plan", "2025-02-02", "new", &body("shared", 10));
    move_doc(
        root,
        "notes/2024-01-01-plan.md",
        "notes/2025-02-02-plan.md",
        &new,
    );
    index(root, &[]);

    let report = moves(root, &[]);
    assert!(
        confirmed(&report).is_empty(),
        "the id did not survive: {report}"
    );
    assert_eq!(hints(&report).len(), 1, "{report}");
    let row = &hints(&report)[0];
    assert_eq!(row["from"], "notes/2024-01-01-plan.md", "{row}");
    assert_eq!(row["to"], "notes/2025-02-02-plan.md", "{row}");
    near(&row["chunk_jaccard"], 0.8, "chunk_jaccard");
    near(&row["title_match"], 0.0, "title_match");
    near(&row["basename_match"], 1.0, "basename_match");
    near(&row["tag_jaccard"], 0.0, "tag_jaccard");
    near(&row["confidence"], 0.55, "confidence");
}

#[test]
fn case_11_3_four_of_four_chunks_and_equal_titles_score_seventy() {
    // Case 11.3. Both documents have 4 chunks, all 4 shared. A second vanished
    // document carries the same title, so rung 5 (unique title) is ambiguous and
    // cannot carry the id; the hint is the only link to the intended one.
    //   chunk_jaccard  = 4/4 = 1.0   -> 0.50 * 1.0 = 0.50
    //   title_match    = 1           -> 0.20 * 1 = 0.20 (titles equal)
    //   basename_match = 0           -> 0.15 * 0 = 0.00 (stems differ)
    //   tag_jaccard    = 0           -> 0.15 * 0 = 0.00 (tags disjoint)
    //   confidence                    = 0.70.
    // The decoy scores 0 + 0.20 + 0 + 0 = 0.20, so it is not the best candidate.
    let old = doc("Cache Plan", "2024-01-01", "old", &body("shared", 4));
    let decoy = doc("Cache Plan", "2023-01-01", "decoy", &body("other", 1));
    let vault = vault(&[
        ("notes/2024-01-01-old.md", &old),
        ("notes/2023-01-01-decoy.md", &decoy),
    ]);
    let root = vault.path();
    index(root, &[]);

    let new = doc("Cache Plan", "2025-02-02", "new", &body("shared", 4));
    move_doc(
        root,
        "notes/2024-01-01-old.md",
        "notes/2025-02-02-new.md",
        &new,
    );
    fs::remove_file(root.join("notes/2023-01-01-decoy.md")).expect("remove decoy");
    index(root, &[]);

    let report = moves(root, &[]);
    assert_eq!(hints(&report).len(), 1, "{report}");
    let row = &hints(&report)[0];
    near(&row["chunk_jaccard"], 1.0, "chunk_jaccard");
    near(&row["title_match"], 1.0, "title_match");
    near(&row["basename_match"], 0.0, "basename_match");
    near(&row["tag_jaccard"], 0.0, "tag_jaccard");
    near(&row["confidence"], 0.70, "confidence");
}

#[test]
fn case_11_4_two_shared_chunks_of_ten_score_below_the_floor() {
    // Case 11.4. Old 10 chunks, new 2; both new ids are old ids.
    //   chunk_jaccard  = 2/10 = 0.2  -> 0.50 * 0.2 = 0.10
    //   title_match    = 0           -> 0.20 * 0 = 0.00 (titles differ)
    //   basename_match = 1           -> 0.15 * 1 = 0.15 (both stems "plan")
    //   tag_jaccard    = 0           -> 0.15 * 0 = 0.00 (tags disjoint)
    //   confidence                    = 0.25, below the 0.50 floor: no hint.
    let old = doc("Old plan", "2024-01-01", "old", &body("shared", 10));
    let vault = vault(&[("notes/2024-01-01-plan.md", &old)]);
    let root = vault.path();
    index(root, &[]);

    let new = doc("New plan", "2025-02-02", "new", &body("shared", 2));
    move_doc(
        root,
        "notes/2024-01-01-plan.md",
        "notes/2025-02-02-plan.md",
        &new,
    );
    index(root, &[]);

    let report = moves(root, &[]);
    assert!(
        hints(&report).is_empty(),
        "0.25 is below the floor: {report}"
    );
}

#[test]
fn case_11_5_a_since_before_every_first_seen_keeps_every_row() {
    // Case 11.5. One confirmed move and one hinted move, both first seen today.
    // `2000-01-01` precedes every `first_seen`, so both lists survive. The unit
    // test in `src/moves.rs` covers `first_seen` and `last_seen` disagreeing.
    let (vault, root) = two_moves();
    let report = moves(&root, &["--since", "2000-01-01"]);
    assert_eq!(confirmed(&report).len(), 1, "{report}");
    assert_eq!(hints(&report).len(), 1, "{report}");
    drop(vault);
}

#[test]
fn case_11_6_a_since_after_every_first_seen_keeps_no_row() {
    // Case 11.6. `2999-12-31` follows every `first_seen`, so both lists are empty.
    let (vault, root) = two_moves();
    let report = moves(&root, &["--since", "2999-12-31"]);
    assert!(confirmed(&report).is_empty(), "{report}");
    assert!(hints(&report).is_empty(), "{report}");
    drop(vault);
}

#[test]
fn case_11_7_two_runs_are_byte_identical_and_write_nothing() {
    // Case 11.7. The report is a pure function of the published generation, and
    // a run touches neither a vault file nor a `.janus/` file.
    let (vault, root) = two_moves();
    let before = files(&root);

    let first = run(&root, &["moves", "--json"]);
    let second = run(&root, &["moves", "--json"]);
    assert!(first.status.success(), "{first:?}");
    assert_eq!(first.stdout, second.stdout, "two runs, one report");
    assert_eq!(files(&root), before, "a read writes nothing");
    drop(vault);
}

#[test]
fn case_11_8_files_without_an_id_still_get_a_hint() {
    // Case 11.8. Neither file declares an `id`, so the hint is the only signal
    // that the new document continues the vanished one.
    //
    //   4/4 chunks = 1.0 -> 0.50; titles differ -> 0; stems equal (both "plan")
    //   -> 0.15; tags disjoint -> 0; confidence = 0.65.
    let old = doc("Old continuation", "2024-01-01", "old", &body("shared", 4));
    let vault = vault(&[("notes/2024-01-01-plan.md", &old)]);
    let root = vault.path();
    index(root, &[]);

    let new = doc("New continuation", "2025-02-02", "new", &body("shared", 4));
    move_doc(
        root,
        "notes/2024-01-01-plan.md",
        "notes/2025-02-02-plan.md",
        &new,
    );
    index(root, &[]);

    let path = "notes/2025-02-02-plan.md";
    let text = fs::read_to_string(root.join(path)).expect("read");
    assert!(!text.contains("id:"), "no file declares an id: {path}");
    let report = moves(root, &[]);
    assert_eq!(hints(&report).len(), 1, "{report}");
    near(&hints(&report)[0]["confidence"], 0.65, "confidence");
}

// ─────────────────────────── the rebuild guard ─────────────────────────────

#[test]
fn a_rebuild_after_a_move_computes_the_hint_from_the_basis() {
    // §3.3. `--rebuild` passes a basis and no reuse source, so a hint computed
    // from `Previous` would be absent. The basis still holds the 8 old chunks:
    //   8/10 -> 0.40; titles differ -> 0; stems equal -> 0.15; tags -> 0;
    //   confidence = 0.55.
    let old = doc("Old plan", "2024-01-01", "old", &body("shared", 8));
    let vault = vault(&[("notes/2024-01-01-plan.md", &old)]);
    let root = vault.path();
    index(root, &[]);

    let new = doc("New plan", "2025-02-02", "new", &body("shared", 10));
    move_doc(
        root,
        "notes/2024-01-01-plan.md",
        "notes/2025-02-02-plan.md",
        &new,
    );
    index(root, &["--rebuild"]);

    let report = moves(root, &[]);
    assert_eq!(hints(&report).len(), 1, "{report}");
    near(&hints(&report)[0]["chunk_jaccard"], 0.8, "chunk_jaccard");
    near(&hints(&report)[0]["confidence"], 0.55, "confidence");
}

#[test]
fn a_hint_survives_a_rebuild_with_the_same_confidence() {
    // §10. `--rebuild` has no reuse source, so a hint computed from `Previous`
    // would vanish. The basis still holds the tombstone and the recorded hint,
    // so the same pair and the same 0.55 survive.
    let old = doc("Old plan", "2024-01-01", "old", &body("shared", 8));
    let vault = vault(&[("notes/2024-01-01-plan.md", &old)]);
    let root = vault.path();
    index(root, &[]);

    let new = doc("New plan", "2025-02-02", "new", &body("shared", 10));
    move_doc(
        root,
        "notes/2024-01-01-plan.md",
        "notes/2025-02-02-plan.md",
        &new,
    );
    index(root, &[]);
    let before = moves(root, &[]);
    let row = &hints(&before)[0];

    index(root, &["--rebuild"]);
    let after = moves(root, &[]);
    assert_eq!(hints(&after).len(), 1, "{after}");
    let rebuilt = &hints(&after)[0];
    assert_eq!(rebuilt["id"], row["id"], "{rebuilt}");
    assert_eq!(rebuilt["previous_id"], row["previous_id"], "{rebuilt}");
    near(&rebuilt["confidence"], 0.55, "confidence after rebuild");
    drop(vault);
}

/// A vault with one confirmed move and one hinted move, both built once.
fn two_moves() -> (tempfile::TempDir, PathBuf) {
    let kept = doc("Kept", "2024-01-01", "old", &body("kept", 4));
    let changed = doc("Old plan", "2024-01-01", "old", &body("shared", 8));
    let vault = vault(&[
        ("notes/kept.md", &kept),
        ("notes/2024-01-01-plan.md", &changed),
    ]);
    let root = vault.path().to_path_buf();
    index(&root, &[]);

    let kept_new = doc(
        "Kept",
        "2024-01-01",
        "old",
        &format!("{}## Extra\n\nnew words\n\n", body("kept", 4)),
    );
    move_doc(&root, "notes/kept.md", "notes/kept-v2.md", &kept_new);
    let changed_new = doc("New plan", "2025-02-02", "new", &body("shared", 10));
    move_doc(
        &root,
        "notes/2024-01-01-plan.md",
        "notes/2025-02-02-plan.md",
        &changed_new,
    );
    index(&root, &[]);
    (vault, root)
}
