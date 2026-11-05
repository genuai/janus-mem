//! `janus suggest-links` end to end: cases 12.1–12.6 of M5 §3.12.
//!
//! Every vault is a temp vault built with the `hash-64` test double, so a run
//! needs no model file and no wall clock. No committed fixture is written to.
//!
//! The positive case uses a **synthetic** pair rather than the committed
//! `similar/` pair: the committed pair's cosine depends on bucket collisions
//! across about eighty tokens, so a hand-derived number cannot be stated for it.
//! The synthetic pair shares eleven of twelve signed buckets, and this file
//! derives the expected cosine from the published `hash-64` embedder in
//! `expected_cosine`, not from `suggest`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use janus_mem::index::embed::{Embedder as _, HashEmbedder};
use serde_json::Value;

/// The two word sets of the positive pair.
///
/// The title adds `pair` and `topic` to both documents. Ten body words are
/// shared and one differs, so the two rows share 12 of their 13 tokens. Every
/// token lands in its own bucket, so the dot product is 12 and each row's norm
/// is `√13`: the cosine is `12/13 = 0.9231` (4 dp), which is above the default
/// `0.85` and below `0.99`.
const SHARED: &str = "alpha beta gamma delta epsilon zeta eta theta iota kappa";

/// A `janus` command ready to configure.
fn janus() -> Command {
    Command::cargo_bin("janus").expect("the janus binary must build for tests")
}

/// Writes a config and one Markdown file per entry into a temp vault.
fn vault(docs: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"suggest\"\n\n[embedding]\nmodel = \"hash-64\"\n",
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

/// A document with a title, optional extra frontmatter and a body.
fn doc(title: &str, extra: &str, body: &str) -> String {
    format!("---\ntitle: {title}\n{extra}---\n{body}\n")
}

/// Runs `janus --vault <root> <args>` with tracing silenced.
fn run(root: &Path, args: &[&str]) -> std::process::Output {
    janus()
        .env("RUST_LOG", "off")
        .arg("--vault")
        .arg(root)
        .args(args)
        .output()
        .expect("run janus")
}

/// Builds the index with the `hash-64` double, so `vectors.f32` exists.
fn index(root: &Path) {
    let output = run(root, &["index"]);
    assert!(
        output.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Runs `janus suggest-links --json` and parses the report, asserting success.
fn suggest(root: &Path, extra: &[&str]) -> Value {
    let mut args = vec!["suggest-links", "--json"];
    args.extend_from_slice(extra);
    let output = run(root, &args);
    assert!(
        output.status.success(),
        "suggest-links failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// The candidate rows of a report.
fn candidates(report: &Value) -> &Vec<Value> {
    report["candidates"].as_array().expect("candidates")
}

/// The `(path_a, path_b)` of every candidate, in report order.
fn pairs(report: &Value) -> Vec<(String, String)> {
    candidates(report)
        .iter()
        .map(|row| {
            (
                row["path_a"].as_str().expect("path_a").to_owned(),
                row["path_b"].as_str().expect("path_b").to_owned(),
            )
        })
        .collect()
}

/// Asserts a JSON number is close enough to a hand-derived value.
fn near(value: &Value, expected: f64, label: &str) {
    let actual = value.as_f64().unwrap_or_else(|| panic!("{label}: {value}"));
    assert!(
        (actual - expected).abs() < 1e-6,
        "{label}: expected {expected}, got {actual}"
    );
}

/// The cosine of two one-chunk documents, computed from the `hash-64` double.
///
/// This is the published embedder, not `suggest`: the case states `12/13` and
/// this function proves the stated number is the one the double produces.
fn expected_cosine(title: &str, body_a: &str, body_b: &str) -> f64 {
    let embedder = HashEmbedder::new(64).expect("64 buckets");
    let texts = [
        format!("{title}\n\n{body_a}"),
        format!("{title}\n\n{body_b}"),
    ];
    let rows = embedder
        .embed(&[texts[0].as_str(), texts[1].as_str()])
        .expect("the hash double never fails");
    let norm = |row: &[f32]| -> f64 {
        row.iter()
            .map(|value| f64::from(*value) * f64::from(*value))
            .sum::<f64>()
            .sqrt()
    };
    let dot = rows[0]
        .iter()
        .zip(&rows[1])
        .map(|(left, right)| f64::from(*left) * f64::from(*right))
        .sum::<f64>();
    dot / (norm(&rows[0]) * norm(&rows[1]))
}

/// Every file under `root`, including `.janus/`, mapped to its bytes.
fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).expect("read dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = fs::read(&path).expect("read file");
                out.insert(
                    path.strip_prefix(root).expect("relative").to_path_buf(),
                    bytes,
                );
            }
        }
    }
    out
}

/// Case 12.1: two near-identical documents and no link give one candidate.
#[test]
fn case_12_1_a_near_identical_unlinked_pair_is_one_candidate() {
    let body_a = format!("{SHARED} leftonly");
    let body_b = format!("{SHARED} rightword");
    let vault = vault(&[
        ("notes/pair-a.md", &doc("Pair topic", "", &body_a)),
        ("notes/pair-b.md", &doc("Pair topic", "", &body_b)),
    ]);
    index(vault.path());

    let report = suggest(vault.path(), &[]);
    assert_eq!(report["api"], 1, "{report}");
    assert_eq!(
        report["degraded"].as_array().expect("degraded").len(),
        0,
        "a scored run is not degraded: {report}"
    );
    near(&report["threshold"], 0.85, "default threshold");

    // The two texts share 12 of 13 tokens and every token has its own bucket,
    // so the dot product is 12 and both norms are √13: 12/13 = 0.923077.
    let expected = expected_cosine("Pair topic", &body_a, &body_b);
    assert!(
        (expected - 12.0 / 13.0).abs() < 1e-9,
        "the stated 12/13 must be the double's own value, got {expected}"
    );
    assert_eq!(
        pairs(&report),
        vec![("notes/pair-a.md".to_owned(), "notes/pair-b.md".to_owned())],
        "{report}"
    );
    near(&candidates(&report)[0]["cosine"], 12.0 / 13.0, "cosine");
    assert_eq!(
        candidates(&report)[0]["path_a"],
        "notes/pair-a.md",
        "path_a sorts first: {report}"
    );
    assert_ne!(candidates(&report)[0]["id_a"], Value::Null);
    assert_ne!(candidates(&report)[0]["id_b"], Value::Null);
}

/// Case 12.2: a pair an edge joins is never reported, in either direction and
/// across disjoint scopes.
#[test]
fn case_12_2_a_linked_pair_is_never_reported() {
    // Ten mutually distinct words per group, so only the intended pairs are
    // near-identical. Group one is linked by `supersedes`; group two is linked
    // by `superseded_by` across disjoint scopes, so the edge is stored and inert
    // (M5-8). Group three is the positive control: identical, unlinked.
    let one = "amber birch cedar dogwood elm fir gum hazel ironwood juniper";
    let two = "kelp larch maple nettle oak pine quince rowan spruce thyme";
    let three = "umber vetch walnut yarrow zinnia aster borage clover dill fennel";
    let vault = vault(&[
        (
            "notes/left.md",
            &doc("Group one", "", &format!("{one} shared")),
        ),
        (
            "notes/right.md",
            &doc(
                "Group one",
                "supersedes: notes/left.md\n",
                &format!("{one} shared"),
            ),
        ),
        (
            "notes/first.md",
            &doc("Group two", "scope: org:acme\n", &format!("{two} shared")),
        ),
        (
            "notes/second.md",
            &doc(
                "Group two",
                "scope: org:beta\nsuperseded_by: notes/first.md\n",
                &format!("{two} shared"),
            ),
        ),
        (
            "notes/third.md",
            &doc("Group three", "", &format!("{three} shared")),
        ),
        (
            "notes/fourth.md",
            &doc("Group three", "", &format!("{three} shared")),
        ),
    ]);
    index(vault.path());

    // The inert edge is a fact of this vault, so a pair that is merely missing
    // from the report cannot be an unresolved link: `doctor` names the edge.
    let doctor = run(vault.path(), &["doctor", "--json"]);
    let report: Value = serde_json::from_slice(&doctor.stdout).expect("doctor JSON");
    let inert = report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .filter(|check| check["code"] == "cross_scope_supersession")
        .count();
    assert_eq!(inert, 1, "exactly one inert edge: {report}");

    let report = suggest(vault.path(), &[]);
    assert_eq!(
        pairs(&report),
        vec![("notes/fourth.md".to_owned(), "notes/third.md".to_owned())],
        "only the unlinked pair is reported: {report}"
    );
}

/// Case 12.3: a threshold above the pair's cosine gives an empty list.
#[test]
fn case_12_3_a_high_threshold_gives_an_empty_list() {
    let body_a = format!("{SHARED} leftonly");
    let body_b = format!("{SHARED} rightword");
    let vault = vault(&[
        ("notes/pair-a.md", &doc("Pair topic", "", &body_a)),
        ("notes/pair-b.md", &doc("Pair topic", "", &body_b)),
    ]);
    index(vault.path());

    let report = suggest(vault.path(), &["--threshold", "0.99"]);
    assert!(candidates(&report).is_empty(), "{report}");
    near(&report["threshold"], 0.99, "echoed threshold");

    // The same vault at the default threshold does have the pair, so the case
    // fails if the threshold is ignored.
    assert_eq!(pairs(&suggest(vault.path(), &[])).len(), 1);
}

/// Case 12.4: a threshold outside `0..=1` is a usage error naming the value.
#[test]
fn case_12_4_a_threshold_outside_the_range_exits_two() {
    let vault = vault(&[("notes/only.md", &doc("Only", "", "text"))]);
    index(vault.path());

    // `=` form, because clap reads a bare `-0.5` as a flag.
    for value in ["1.5", "-0.5", "NaN"] {
        let given = format!("--threshold={value}");
        let output = run(vault.path(), &["suggest-links", &given]);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{value} must be a usage error: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(value),
            "the message names {value}: {stderr}"
        );
    }
}

/// Case 12.5: without embeddings the command degrades and exits 0.
#[test]
fn case_12_5_without_embeddings_the_report_degrades() {
    let body_a = format!("{SHARED} leftonly");
    let body_b = format!("{SHARED} rightword");
    let vault = vault(&[
        ("notes/pair-a.md", &doc("Pair topic", "", &body_a)),
        ("notes/pair-b.md", &doc("Pair topic", "", &body_b)),
    ]);
    index(vault.path());

    let report = suggest(vault.path(), &["--no-embeddings"]);
    assert!(candidates(&report).is_empty(), "{report}");
    assert_eq!(
        report["degraded"],
        serde_json::json!(["no embedder; no semantic candidates"]),
        "{report}"
    );

    let output = run(vault.path(), &["suggest-links", "--no-embeddings"]);
    assert!(output.status.success(), "degraded is not an error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.lines().count(),
        1,
        "one degraded line, nothing else: {stderr}"
    );
    assert!(
        stderr.starts_with("degraded: no embedder"),
        "the line says what is missing: {stderr}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).is_empty(),
        "no rows: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// Case 12.6: two runs are byte-identical and write no file.
#[test]
fn case_12_6_two_runs_are_byte_identical_and_write_nothing() {
    let body_a = format!("{SHARED} leftonly");
    let body_b = format!("{SHARED} rightword");
    let vault = vault(&[
        ("notes/pair-a.md", &doc("Pair topic", "", &body_a)),
        ("notes/pair-b.md", &doc("Pair topic", "", &body_b)),
    ]);
    index(vault.path());

    let before = files(vault.path());
    let first = run(vault.path(), &["suggest-links", "--json"]);
    let second = run(vault.path(), &["suggest-links", "--json"]);
    let after = files(vault.path());

    assert!(first.status.success() && second.status.success());
    assert_eq!(
        first.stdout, second.stdout,
        "two runs must print the same bytes"
    );
    assert_eq!(before, after, "a read changes no vault and no .janus file");
}
