//! Incremental embedding end to end: the receipt counters, row reuse, row 3's
//! carry-forward, `--dry-run`, and `--rebuild` (§3.4, §3.6, V-11…V-26).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::{fixture_vault, janus};
use serde_json::Value;

fn receipt(root: &Path, args: &[&str]) -> Value {
    let mut all = vec!["index"];
    all.extend_from_slice(args);
    common::json_ok(root, &all)
}

fn vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"incremental\"\n\n[embedding]\nmodel = \"hash-64\"\n",
    )
    .expect("write config");
    write(
        dir.path(),
        "a.md",
        "---\ntitle: Alpha\ndate: 2025-01-01\ntags: [one]\n---\n\n# Alpha\n\nalpha cache body\n",
    );
    write(
        dir.path(),
        "b.md",
        "---\ndate: 2025-01-01\n---\n\n# Beta\n\nbeta store body\n",
    );
    dir
}

fn write(root: &Path, rel_path: &str, text: &str) {
    let path = root.join(rel_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create document directory");
    }
    fs::write(path, text).expect("write document");
}

fn read(root: &Path, rel_path: &str) -> String {
    fs::read_to_string(root.join(rel_path)).expect("read document")
}

fn vectors(root: &Path) -> PathBuf {
    root.join(".janus/vectors.f32")
}

fn manifest(root: &Path) -> Value {
    serde_json::from_slice(&fs::read(root.join(".janus/manifest.json")).expect("read manifest"))
        .expect("manifest is JSON")
}

fn dir_snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(dir).expect("the index directory exists") {
        let entry = entry.expect("index directory entry");
        files.insert(
            entry.file_name().to_string_lossy().into_owned(),
            fs::read(entry.path()).expect("read an index directory file"),
        );
    }
    files
}

#[test]
fn a_second_index_embeds_nothing_and_writes_byte_identical_vectors() {
    let vault = fixture_vault();
    let first = receipt(vault.path(), &["--json"]);
    let first_vectors = fs::read(vectors(vault.path())).expect("read vectors");
    let first_sha = manifest(vault.path())["vectors_sha"].clone();

    let second = receipt(vault.path(), &["--json"]);

    assert_eq!(second["embedded"], 0, "{second}");
    assert_eq!(second["reused_vectors"], second["chunks"], "{second}");
    assert_eq!(second["retokenized"], 0, "{second}");
    assert_eq!(second["dim"], 64, "{second}");
    assert_eq!(
        second["vectors_bytes"].as_u64(),
        Some(first_vectors.len() as u64),
        "{second}"
    );
    assert_eq!(
        fs::read(vectors(vault.path())).expect("read vectors"),
        first_vectors
    );
    assert_eq!(manifest(vault.path())["vectors_sha"], first_sha);
    assert_eq!(first["embedded"], first["chunks"], "{first}");
}

#[test]
fn a_metadata_only_edit_embeds_and_retokenizes_nothing() {
    let vault = vault();
    let first = receipt(vault.path(), &["--json"]);
    let before = fs::read(vectors(vault.path())).expect("read vectors");
    let before_sha = manifest(vault.path())["vectors_sha"].clone();

    let text = read(vault.path(), "a.md");
    let changed = text.replace("tags: [one]", "tags: [one, two]");
    assert_ne!(
        text, changed,
        "the fixture document must carry the tag list"
    );
    write(vault.path(), "a.md", &changed);

    let second = receipt(vault.path(), &["--json"]);

    assert_eq!(second["embedded"], 0, "{second}");
    assert_eq!(second["retokenized"], 0, "{second}");
    assert_eq!(second["reused_vectors"], first["chunks"], "{second}");
    assert_eq!(
        fs::read(vectors(vault.path())).expect("read vectors"),
        before,
        "the payload is untouched because the body is untouched"
    );
    assert_eq!(manifest(vault.path())["vectors_sha"], before_sha);
}

#[test]
fn a_rename_costs_nothing_only_when_the_title_is_declared() {
    let vault = vault();
    let first = receipt(vault.path(), &["--json"]);
    let chunks = first["chunks"].as_u64().expect("chunks");
    assert_eq!(chunks, 2, "one short chunk per document");

    fs::rename(vault.path().join("a.md"), vault.path().join("aa.md"))
        .expect("rename the declared-title document");
    let declared = receipt(vault.path(), &["--json"]);
    assert_eq!(declared["embedded"], 0, "{declared}");
    assert_eq!(declared["retokenized"], 0, "{declared}");
    assert_eq!(declared["reused_vectors"], chunks, "{declared}");

    // `b.md` declares no title, so the derived title changes with the name and
    // its embedded text changes with it; Alpha alone is still reused.
    fs::rename(vault.path().join("b.md"), vault.path().join("bb.md"))
        .expect("rename the derived-title document");
    let derived = receipt(vault.path(), &["--json"]);
    assert_eq!(derived["embedded"], 1, "{derived}");
    assert_eq!(derived["retokenized"], 1, "{derived}");
    assert_eq!(derived["reused_vectors"], chunks - 1, "{derived}");
}

#[test]
fn no_embeddings_carries_an_unchanged_payload_and_drops_a_changed_one() {
    let vault = vault();
    let first = receipt(vault.path(), &["--json"]);
    let before = fs::read(vectors(vault.path())).expect("read vectors");
    let before_sha = manifest(vault.path())["vectors_sha"].clone();

    let output = janus()
        .arg("--vault")
        .arg(vault.path())
        .args(["index", "--no-embeddings", "--json"])
        .output()
        .expect("run janus index --no-embeddings");
    assert!(
        output.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let carried: Value =
        serde_json::from_slice(&output.stdout).expect("the receipt is JSON on stdout");
    assert_eq!(carried["embedded"], 0, "{carried}");
    assert_eq!(carried["reused_vectors"], first["chunks"], "{carried}");
    assert_eq!(
        fs::read(vectors(vault.path())).expect("read vectors"),
        before,
        "nothing changed, so the rows survive the degraded build"
    );
    let carried_manifest = manifest(vault.path());
    assert_eq!(carried_manifest["embedder"]["name"], "hash-64");
    assert_eq!(carried_manifest["vectors_sha"], before_sha);

    // The next featured run sees rows it can reuse, not a row-5 rebuild.
    let featured = receipt(vault.path(), &["--json"]);
    assert_eq!(featured["embedded"], 0, "{featured}");
    assert_eq!(featured["reused_vectors"], featured["chunks"], "{featured}");

    write(
        vault.path(),
        "a.md",
        &format!("{}\nchanged body\n", read(vault.path(), "a.md")),
    );
    let output = janus()
        .arg("--vault")
        .arg(vault.path())
        .args(["index", "--no-embeddings", "--json"])
        .output()
        .expect("run janus index --no-embeddings");
    assert!(
        output.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !vectors(vault.path()).exists(),
        "one changed document means no row can be carried"
    );
    let dropped = manifest(vault.path());
    assert!(dropped["embedder"].is_null(), "{dropped}");
    assert!(dropped["vectors_sha"].is_null(), "{dropped}");
    assert!(dropped["vectors_len"].is_null(), "{dropped}");
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("vectors dropped"))
        .collect();
    assert_eq!(lines.len(), 1, "expected exactly one drop line: {stderr:?}");
    assert!(
        lines[0].contains("1 document(s) changed"),
        "the line names the changed count: {stderr:?}"
    );
}

#[test]
fn a_dry_run_reports_the_embeds_and_writes_nothing() {
    let vault = vault();
    let first = receipt(vault.path(), &["--json"]);
    let chunks = first["chunks"].as_u64().expect("chunks");
    write(
        vault.path(),
        "a.md",
        &format!("{}\nchanged body\n", read(vault.path(), "a.md")),
    );
    let before = dir_snapshot(&vault.path().join(".janus"));

    let dry = receipt(vault.path(), &["--dry-run", "--json"]);

    assert_eq!(dry["dry_run"], true, "{dry}");
    assert_eq!(dry["embedded"], 1, "{dry}");
    assert_eq!(dry["reused_vectors"], chunks - 1, "{dry}");
    assert_eq!(dry["dim"], 64, "{dry}");
    assert!(dry.get("index_sha").is_none(), "{dry}");
    assert_eq!(
        dir_snapshot(&vault.path().join(".janus")),
        before,
        "a dry run writes nothing"
    );

    let real = receipt(vault.path(), &["--json"]);
    assert_eq!(real["embedded"], dry["embedded"], "{real}");
    assert_eq!(real["reused_vectors"], dry["reused_vectors"], "{real}");
    assert_ne!(
        fs::read(vectors(vault.path())).expect("read vectors"),
        before["vectors.f32"],
        "the real run writes the row the dry run predicted"
    );
}

#[test]
fn rebuild_embeds_every_chunk_and_matches_a_clean_build() {
    let vault = vault();
    let first = receipt(vault.path(), &["--json"]);
    let clean = fs::read(vectors(vault.path())).expect("read vectors");

    let rebuilt = receipt(vault.path(), &["--rebuild", "--json"]);

    assert_eq!(rebuilt["embedded"], rebuilt["chunks"], "{rebuilt}");
    assert_eq!(rebuilt["reused_vectors"], 0, "{rebuilt}");
    assert_eq!(rebuilt["retokenized"], rebuilt["chunks"], "{rebuilt}");
    assert_eq!(
        fs::read(vectors(vault.path())).expect("read vectors"),
        clean
    );
    assert_eq!(rebuilt["dim"], 64, "{rebuilt}");
    assert_eq!(first["embedded"], first["chunks"], "{first}");
}
