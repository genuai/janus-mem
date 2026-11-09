//! M6b phase 5 cases 5.1–5.5: a nested corpus root is pruned from its parent
//! (§3.6 of the M6b plan).
//!
//! Every case runs the binary against a copy of `tests/fixtures/workspace-nested`,
//! whose archived root lies inside the primary's root and holds `notes/shared.md`
//! with different bytes, so a result, a read or a document count proves which copy
//! answered. No case sleeps, reads the clock, or writes to the fixture.

use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::Command;
use janus_mem::config::Config;
use serde_json::{Value, json};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/workspace-nested"
);

/// The primary corpus's root, relative to the fixture.
const PRIMARY: &str = "primary";

/// The nested path, as the primary's root sees it.
const NESTED: &str = "archive/notes/shared.md";

/// The path both roots hold, each in its own corpus.
const SHARED: &str = "notes/shared.md";

/// The body the primary corpus holds at [`SHARED`].
const PRIMARY_BODY: &str =
    "\n# Cache plan\n\nThe primary corpus keeps one cache in front of the database.\n";

/// The body the archived corpus holds at [`SHARED`].
const ARCHIVED_BODY: &str =
    "\n# Cache plan\n\nThe archived corpus describes the retired cache fleet.\n";

/// A word only the archived copy holds.
const ARCHIVED_ONLY: &str = "retired";

/// A private copy of the committed fixture, without any index.
fn copied() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir for the fixture copy");
    copy_tree(Path::new(FIXTURE), dir.path());
    dir
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).expect("read the fixture") {
        let entry = entry.expect("a directory entry");
        let target = to.join(entry.file_name());
        if !entry.file_type().expect("a file type").is_dir() {
            fs::copy(entry.path(), &target).expect("copy the file");
        } else if entry.file_name() != ".janus" {
            fs::create_dir_all(&target).expect("create the directory");
            copy_tree(&entry.path(), &target);
        }
    }
}

/// The workspace file inside one fixture copy.
fn workspace(dir: &Path) -> String {
    dir.join("workspace.toml").display().to_string()
}

/// A `janus` command for one fixture copy, with no workspace in the ambient
/// configuration: every case names its workspace with the flag or not at all.
fn janus_in(dir: &Path) -> Command {
    let mut command = Command::cargo_bin("janus").expect("the janus binary must build for tests");
    command
        .current_dir(dir)
        .env("RUST_LOG", "off")
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env_remove("JANUS_WORKSPACE")
        .env_remove("JANUS_VAULT")
        .env_remove("APPDATA")
        .env_remove("HOME");
    command
}

fn run(dir: &Path, args: &[&str]) -> Output {
    janus_in(dir).args(args).output().expect("run janus")
}

/// Runs one command that must succeed, and returns its stdout and stderr.
fn run_ok(dir: &Path, args: &[&str]) -> (String, String) {
    let output = run(dir, args);
    assert!(
        output.status.success(),
        "janus {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    )
}

/// Runs one command that must succeed and parses its JSON report.
fn report(dir: &Path, args: &[&str]) -> Value {
    let mut all = args.to_vec();
    all.push("--json");
    let (text, _) = run_ok(dir, &all);
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("stdout is not JSON ({err}):\n{text}"))
}

/// One command of the fixture workspace, named with `--workspace`.
fn workspace_report(dir: &Path, args: &[&str]) -> Value {
    let named = workspace(dir);
    let mut all = args.to_vec();
    all.extend(["--workspace", &named]);
    report(dir, &all)
}

/// One query of the workspace, and the corpus when the case names one.
fn search(dir: &Path, query: &str, corpus: Option<&str>) -> Value {
    let mut args = vec!["search", query, "--no-embeddings"];
    args.extend(corpus.into_iter().flat_map(|id| ["--corpus", id]));
    workspace_report(dir, &args)
}

/// One read of the workspace, and the corpus when the case names one.
fn read(dir: &Path, path: &str, corpus: Option<&str>) -> Value {
    let mut args = vec!["read", path];
    args.extend(corpus.into_iter().flat_map(|id| ["--corpus", id]));
    workspace_report(dir, &args)
}

/// The `(corpus, rel_path)` of every result, in order.
fn rows(report: &Value) -> Vec<(String, String)> {
    let rows = report["results"].as_array().expect("results");
    rows.iter()
        .map(|row| {
            let path = row["rel_path"].as_str().expect("rel_path").to_owned();
            (row["corpus"].as_str().unwrap_or("").to_owned(), path)
        })
        .collect()
}

// ─────────────────────────────── the cases ─────────────────────────────────

#[test]
fn case_5_1_a_default_query_reads_nothing_from_the_nested_tree() {
    let dir = copied();

    // The word only the archived copy holds: a parent that walked the tree would
    // return it, with `corpus: main` and the parent's weight of 1.0.
    let report = search(dir.path(), ARCHIVED_ONLY, None);
    assert_eq!(report["corpora"], json!(["main"]));
    assert!(rows(&report).is_empty(), "{:?}", rows(&report));

    // The same word, asked of the corpus that owns the tree: one row, weighted.
    let archived = search(dir.path(), ARCHIVED_ONLY, Some("context"));
    assert_eq!(rows(&archived), [("context".to_owned(), SHARED.to_owned())]);
    assert_eq!(archived["results"][0]["scores"]["corpus"], json!(0.8));

    // Which copy answered, stated as bytes: the two bodies differ.
    assert_eq!(read(dir.path(), SHARED, None)["body"], json!(PRIMARY_BODY));
    assert_eq!(
        read(dir.path(), SHARED, Some("context"))["body"],
        json!(ARCHIVED_BODY)
    );
}

#[test]
fn case_5_2_the_prune_is_reported_once_and_marked_in_the_listing() {
    let dir = copied();
    let named = workspace(dir.path());

    // One warning on load, naming the parent, the nested corpus and the count.
    let (_, stderr) = run_ok(
        dir.path(),
        &["search", ARCHIVED_ONLY, "--workspace", &named],
    );
    let warning = stderr
        .lines()
        .find(|line| line.starts_with("warning: ") && line.contains("is inside corpus"))
        .expect("the prune warning");
    for fragment in ["\"context\"", "\"main\"", "1 document pruned"] {
        assert!(warning.contains(fragment), "{fragment}: {warning}");
    }

    // The listing marks the parent with the child it no longer walks.
    let report = workspace_report(dir.path(), &["corpora"]);
    let listed = report["corpora"].as_array().expect("a corpora array");
    assert_eq!(listed[0]["id"], json!("main"));
    assert_eq!(listed[0]["pruned"], json!(["context"]));
    // A corpus that prunes nothing gains no field.
    assert!(listed[1].get("pruned").is_none());

    let (table, _) = run_ok(dir.path(), &["corpora", "--workspace", &named]);
    let main = table
        .lines()
        .find(|line| line.starts_with("main  primary"))
        .expect("the parent's row");
    assert!(main.contains("prunes: context"), "{main}");
}

#[test]
fn case_5_3_a_nested_path_asked_of_the_parent_names_the_corpus_that_owns_it() {
    let dir = copied();
    let named = workspace(dir.path());
    let output = run(
        dir.path(),
        &["read", "--workspace", &named, "--json", NESTED],
    );
    assert_eq!(output.status.code(), Some(2), "a usage failure, not a miss");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("is in corpus \"context\"") && stderr.contains("--corpus context"),
        "the answer names the owner and what to do: {stderr}"
    );
    assert!(!stderr.contains("no document at"), "{stderr}");
    // The envelope carries the same failure, and no document text.
    let answer: Value = serde_json::from_slice(&output.stdout).expect("the failure envelope");
    assert_eq!(
        (&answer["ok"], &answer["code"]),
        (&json!(false), &json!("usage"))
    );
    assert!(answer["body"].is_null(), "a refused read prints no text");

    // The corpus that owns the tree answers at the path it knows.
    let archived = read(dir.path(), SHARED, Some("context"));
    assert_eq!(archived["body"], json!(ARCHIVED_BODY));
    assert_eq!(archived["corpus"], json!("context"));
}

#[test]
fn case_5_4_a_stale_index_is_pruned_by_the_rebuild_and_by_janus_index() {
    let dir = copied();
    // The build comes before the declaration, which is the order the defect came
    // in: the index already holds the primary's copy of the nested tree.
    let declared = dir.path().join("workspace.toml");
    let parked = dir.path().join("workspace.toml.parked");
    fs::rename(&declared, &parked).expect("park the workspace file");
    let manifest = dir.path().join(PRIMARY).join(".janus/manifest.json");
    let docs = |path: &Path| -> Value {
        serde_json::from_slice::<Value>(&fs::read(path).expect("the manifest"))
            .expect("JSON")["docs"]
            .clone()
    };

    run_ok(
        dir.path(),
        &["--vault", PRIMARY, "index", "--no-embeddings"],
    );
    assert_eq!(docs(&manifest), json!(2), "the build indexed both copies");

    fs::rename(&parked, &declared).expect("declare the workspace");
    let report = search(dir.path(), ARCHIVED_ONLY, None);
    assert!(rows(&report).is_empty(), "{:?}", rows(&report));

    // The read rebuilt the parent, so its index is fresh and one document short,
    // and the archive is still reachable through its own id.
    assert_eq!(docs(&manifest), json!(1));
    let listing = workspace_report(dir.path(), &["corpora"]);
    assert_eq!(listing["corpora"][0]["index_stale"], json!(false));
    let archived = search(dir.path(), ARCHIVED_ONLY, Some("context"));
    assert_eq!(rows(&archived), [("context".to_owned(), SHARED.to_owned())]);

    // `janus index --workspace` applies the same prune a read does (M6b-30), so
    // the workspace arrives as a flag and the receipt names the pruned count.
    fs::remove_dir_all(dir.path().join(PRIMARY).join(".janus")).expect("drop the index");
    let (stdout, stderr) = run_ok(
        dir.path(),
        &[
            "--vault",
            PRIMARY,
            "index",
            "--no-embeddings",
            "--workspace",
            &workspace(dir.path()),
        ],
    );
    assert!(stderr.contains("is inside corpus"), "{stderr}");
    assert!(stdout.contains("1 document(s)"), "{stdout}");
    assert_eq!(docs(&manifest), json!(1));
}

#[test]
fn case_5_5_the_prune_is_derived_from_the_workspace_and_writes_nothing() {
    let dir = copied();
    let path = dir.path().join(PRIMARY).join("janus.toml");
    let before = fs::read(&path).expect("the corpus config");
    let declared = Config::load(&dir.path().join(PRIMARY)).expect("the corpus config parses");
    assert!(
        !declared.is_excluded("archive") && !declared.is_excluded(NESTED),
        "the fixture excludes nothing, so only the workspace can prune the tree"
    );

    // With no workspace, one corpus walks its whole tree, as it always has: the
    // prune is a fact about a workspace, not about a corpus (§6.11).
    let report = report(dir.path(), &["--vault", PRIMARY, "search", ARCHIVED_ONLY]);
    assert_eq!(rows(&report), [(String::new(), NESTED.to_owned())]);

    // And no command wrote the prune into the corpus's own file.
    assert_eq!(fs::read(&path).expect("the corpus config"), before);
}
