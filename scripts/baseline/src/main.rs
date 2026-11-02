//! D20's decision gate: out-of-the-box `tantivy` BM25 against out-of-the-box
//! janus BM25, on the M3 eval set (§3.8, V-18).
//!
//! D20 says: "At M4, compare nDCG@10 against `tantivy` on the eval set; swap only
//! if the hand-rolled version is >10% worse." That is a **one-off measurement**,
//! so it lives here — a standalone crate with an empty `[workspace]`, outside the
//! repository's build. Nothing in `cargo test`, `cargo clippy --all-targets` or
//! the Windows cross-check ever compiles `tantivy`.
//!
//! # Why this is a crate and not a shell script
//!
//! The comparison is only worth anything if both systems see the same corpus and
//! the same **chunk texts**. `janus chunks --text` renders a preview capped at
//! 120 characters (`src/chunk/dump.rs`), so parsing its output would hand
//! `tantivy` a 120-character prefix while janus scores the whole chunk — and the
//! difference would then be blamed on BM25. This crate depends on `janus-mem` by
//! path and calls [`chunk_body`] and [`janus_mem::chunk::Chunk::embed_text`] directly, so the
//! chunker is not re-implemented and the texts are byte-identical.
//!
//! # What is deliberately out of the box
//!
//! `tantivy`'s own default tokenizer and default BM25, with no stemmer, no field
//! boosts and no phrase queries; janus's shipped lexical path at α = 0, with the
//! same `FixedClock(2025-04-01)` the eval harness uses and the same document-level
//! rule (a document's score is its best chunk, M3 §3.1). Differences in
//! tokenization are part of the result, not a bug to normalise away.
//!
//! # Same corpus, including the generated inputs
//!
//! The eval harness copies the fixture vault and adds two documents that are
//! deliberately not committed (a ~2 MB one and a non-UTF-8 one). This crate does
//! the same when it is given no vault path, so the corpus is the one
//! `eval_quality` measures: `big/2025-03-06-large.md` exists for the query that
//! expects it, and the corpus statistics both BM25 implementations see are the
//! same. `tests/common/mod.rs` is a test module and cannot be imported here, so
//! the two generated inputs are reproduced below, verbatim.
//!
//! Run it with `scripts/eval.sh --baseline`, or directly:
//!
//! ```text
//! cargo run --release --manifest-path scripts/baseline/Cargo.toml [VAULT]
//! ```

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::NaiveDate;
use janus_mem::chunk::chunk_body;
use janus_mem::clock::FixedClock;
use janus_mem::config::Config;
use janus_mem::search::{self, Options};
use janus_mem::vault::{self, WalkOptions, resolve};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{STORED, STRING, Schema, TEXT, TantivyDocument, Value};
use tantivy::{Index, doc};

/// `nDCG@10`: the depth both systems are graded at (M3's definition, unchanged).
const DEPTH: usize = 10;

/// One eval query: a graded expectation, most relevant first.
#[derive(serde::Deserialize)]
struct Query {
    query: String,
    intent: String,
    expect: Vec<String>,
}

/// The generated large document, relative to the corpus root (`tests/common`).
const LARGE_REL: &str = "big/2025-03-06-large.md";

/// The generated non-UTF-8 document, relative to the corpus root.
const NON_UTF8_REL: &str = "notes/non-utf8.md";

/// Bytes that look like a frontmatter block but are not valid UTF-8.
const NON_UTF8_BYTES: &[u8] = b"---\ntitle: Invalid bytes\n\xff\xfe\x00\n---\n# Invalid UTF-8\n";

/// One chunk as both systems index it: the owning document's `rel_path` and the
/// exact text `Chunk::embed_text` produces.
struct ChunkText {
    doc: String,
    text: String,
}

/// Discounted gain at `rank` (1-based), for a linear grade.
///
/// The same arithmetic as `tests/eval_quality.rs`; the unit test below pins it
/// against hand-computed values so the comparison is not trusting itself.
fn discount(gain: f64, rank: usize) -> f64 {
    gain / ((rank as f64) + 1.0).log2()
}

/// `nDCG@10` of one ranked document list against one graded expectation.
///
/// `None` when the ideal DCG is zero (an empty expectation), so an ungraded
/// query cannot silently count as a perfect one.
fn ndcg_at_10(ranked: &[String], expect: &[String]) -> Option<f64> {
    let grades: Vec<f64> = (0..expect.len())
        .map(|index| (expect.len() - index) as f64)
        .collect();
    let dcg: f64 = ranked
        .iter()
        .take(DEPTH)
        .enumerate()
        .filter_map(|(rank, path)| {
            expect
                .iter()
                .position(|expected| expected == path)
                .map(|position| discount(grades[position], rank + 1))
        })
        .sum();
    let mut ideal = grades.clone();
    ideal.sort_by(|a, b| b.total_cmp(a));
    let idcg: f64 = ideal
        .iter()
        .take(DEPTH)
        .enumerate()
        .map(|(rank, gain)| discount(*gain, rank + 1))
        .sum();
    (idcg > 0.0).then(|| dcg / idcg)
}

/// The corpus as both systems see it: every chunk of every document, in
/// snapshot order.
fn corpus(root: &Path) -> Result<(Config, Vec<ChunkText>), Box<dyn Error>> {
    let config = Config::load(root)?;
    let walk = vault::walk(root, &config, WalkOptions::default())?;
    let (documents, _skips) = resolve::resolve_all(&walk.files, &config);

    let mut chunks = Vec::new();
    for document in &documents {
        let (found, _notes) = chunk_body(document.id, &document.body, &config.chunking);
        for chunk in &found {
            chunks.push(ChunkText {
                doc: document.rel_path.clone(),
                text: chunk.embed_text(&document.body, &document.title),
            });
        }
    }
    Ok((config, chunks))
}

/// The queries, read from the same JSON Lines file the eval harness reads.
fn queries(path: &Path) -> Result<Vec<Query>, Box<dyn Error>> {
    let text = fs::read_to_string(path)?;
    let mut parsed = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        parsed.push(serde_json::from_str(line)?);
    }
    Ok(parsed)
}

/// janus's own ranking, through the library at α = 0 — the shipped lexical path.
fn janus_ranked(
    root: &Path,
    config: &Config,
    clock: &FixedClock,
    query: &Query,
) -> Result<Vec<String>, Box<dyn Error>> {
    let options = Options {
        query: query.query.clone(),
        intent: Some(query.intent.clone()),
        scopes: Vec::new(),
        scope_only: false,
        limit: DEPTH,
        types: Vec::new(),
        tags: Vec::new(),
        decay_half_life: None,
        include_superseded: false,
        no_rebuild: false,
        // Lexical only: D20 compares BM25 implementations, and α > 0 would make
        // the hand-rolled side a hybrid that tantivy has no counterpart for.
        no_embeddings: true,
        embedding_reason: None,
        json: false,
    };
    let outcome = search::query(root, config, clock, None, &options)?;
    Ok(outcome
        .hits
        .iter()
        .map(|hit| {
            outcome.rows[outcome.selected[hit.doc_index]]
                .rel_path
                .clone()
        })
        .collect())
}

/// The `tantivy` side: one document per chunk, default tokenizer, default BM25.
struct Tantivy {
    searcher: tantivy::Searcher,
    parser: QueryParser,
    doc_field: tantivy::schema::Field,
    chunks: usize,
}

impl Tantivy {
    fn build(chunks: &[ChunkText]) -> Result<Self, Box<dyn Error>> {
        let mut builder = Schema::builder();
        let doc_field = builder.add_text_field("doc", STRING | STORED);
        let text_field = builder.add_text_field("text", TEXT);
        let index = Index::create_in_ram(builder.build());

        // 50 MB is tantivy's documented minimum for a writer's heap arena.
        let mut writer = index.writer(50_000_000)?;
        for chunk in chunks {
            writer.add_document(
                doc!(doc_field => chunk.doc.clone(), text_field => chunk.text.clone()),
            )?;
        }
        writer.commit()?;

        Ok(Self {
            searcher: index.reader()?.searcher(),
            parser: QueryParser::for_index(&index, vec![text_field]),
            doc_field,
            chunks: chunks.len(),
        })
    }

    /// Ranks documents, not chunks: a document's score is its best chunk
    /// (M3 §3.1), so every match is collected and grouped by path.
    fn ranked(&self, text: &str) -> Result<Vec<String>, Box<dyn Error>> {
        // tantivy's query grammar reads `'` as the start of a quoted literal, so
        // `it's fine say hello not a comment` is a syntax error. Its default
        // tokenizer splits on non-alphanumerics anyway, so treating the
        // apostrophe as a separator is what the tokenizer would have done; the
        // alternative (a `\'` escape table) would be a second grammar.
        let query = self.parser.parse_query(&text.replace('\'', " "))?;
        let found = self.searcher.search(
            &query,
            &TopDocs::with_limit(self.chunks.max(1)).order_by_score(),
        )?;

        let mut best: BTreeMap<String, f32> = BTreeMap::new();
        for (score, address) in found {
            let stored: TantivyDocument = self.searcher.doc(address)?;
            let Some(path) = stored
                .get_first(self.doc_field)
                .and_then(|value| value.as_str())
            else {
                continue;
            };
            let entry = best.entry(path.to_owned()).or_insert(score);
            if score > *entry {
                *entry = score;
            }
        }

        let mut ranked: Vec<(String, f32)> = best.into_iter().collect();
        // Score desc, then path asc: two documents that tie on a `f32` score must
        // not be ordered by hash-map iteration.
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(ranked
            .into_iter()
            .take(DEPTH)
            .map(|(path, _)| path)
            .collect())
    }
}

/// A scratch copy of the fixture vault with the two generated inputs written in,
/// exactly as `tests/common::fixture_vault` builds it.
fn prepared_fixture(fixture: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let root = std::env::temp_dir().join(format!("janus-baseline-{}", std::process::id()));
    copy_tree(fixture, &root)?;

    let large = root.join(LARGE_REL);
    if let Some(parent) = large.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&large, large_document())?;
    fs::write(root.join(NON_UTF8_REL), NON_UTF8_BYTES)?;
    Ok(root)
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// A deterministic document just over 2 MB (`tests/common/mod.rs`).
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

fn main() -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (root, scratch) = match std::env::args().nth(1) {
        Some(path) => (PathBuf::from(path), None),
        None => {
            let root = prepared_fixture(&manifest.join("../../tests/fixtures/vault"))?;
            (root.clone(), Some(root))
        }
    };
    let query_file = manifest.join("../../tests/eval/queries.jsonl");

    let (config, chunks) = corpus(&root)?;
    let queries = queries(&query_file)?;
    let clock = FixedClock(NaiveDate::from_ymd_opt(2025, 4, 1).expect("valid eval date"));
    let tantivy = Tantivy::build(&chunks)?;

    let documents = chunks
        .iter()
        .map(|chunk| chunk.doc.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    println!(
        "corpus {} ({} documents, {} chunks)",
        root.display(),
        documents,
        chunks.len()
    );
    println!(
        "queries {} ({} queries); tantivy {}",
        query_file.display(),
        queries.len(),
        tantivy::version_string()
    );

    let (mut janus_sum, mut tantivy_sum, mut graded) = (0.0_f64, 0.0_f64, 0_usize);
    for query in &queries {
        let ranked = janus_ranked(&root, &config, &clock, query)?;
        let baseline = tantivy.ranked(&query.query)?;
        if let Some(value) = ndcg_at_10(&ranked, &query.expect) {
            janus_sum += value;
            graded += 1;
        }
        if let Some(value) = ndcg_at_10(&baseline, &query.expect) {
            tantivy_sum += value;
        }
        if std::env::var_os("JANUS_BASELINE_VERBOSE").is_some() {
            println!(
                "  {:?}: janus {:?} tantivy {:?}",
                query.query, ranked, baseline
            );
        }
    }

    let janus = janus_sum / graded.max(1) as f64;
    let baseline = tantivy_sum / graded.max(1) as f64;
    println!("janus    nDCG@10 {janus:.4} ({graded} queries)");
    println!("tantivy  nDCG@10 {baseline:.4} ({graded} queries)");
    if baseline > 0.0 {
        println!("ratio    janus/tantivy {:.4}", janus / baseline);
    }
    println!(
        "D20: swap only if janus is >10% worse, i.e. ratio < 0.90 -> {}",
        if baseline > 0.0 && janus < 0.9 * baseline {
            "swap (candidate)"
        } else {
            "keep"
        }
    );
    println!("elapsed {} ms", started.elapsed().as_millis());

    if let Some(root) = scratch {
        let _ = fs::remove_dir_all(root);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two tiny result lists with hand-computed `nDCG@10`.
    ///
    /// DEPTH is 10 and the lists are shorter, so no truncation happens and the
    /// arithmetic is visible: `log2(2) = 1`, `log2(3) = 1.5849625`.
    #[test]
    fn ndcg_matches_hand_computed_values() {
        let expect = vec!["a".to_owned(), "b".to_owned()];

        // Perfect order: grades 2, 1 -> DCG = 2/1 + 1/1.5849625 = 2.6309298,
        // and IDCG is the same number.
        let perfect = vec!["a".to_owned(), "b".to_owned()];
        let value = ndcg_at_10(&perfect, &expect).expect("a graded query");
        assert!((value - 1.0).abs() < 1e-9, "perfect order: {value}");

        // Swapped: DCG = 1/1 + 2/1.5849625 = 2.2618595, IDCG = 2.6309298,
        // so 0.8597.
        let swapped = vec!["b".to_owned(), "a".to_owned()];
        let value = ndcg_at_10(&swapped, &expect).expect("a graded query");
        assert!((value - 0.8597).abs() < 1e-4, "swapped order: {value}");

        // Half the expectation missing: DCG = 2/1 = 2, so 2/2.6309298 = 0.7602.
        let partial = vec!["a".to_owned()];
        let value = ndcg_at_10(&partial, &expect).expect("a graded query");
        assert!((value - 0.7602).abs() < 1e-4, "missing document: {value}");

        // Nothing relevant found at all, and an empty expectation is ungraded.
        assert_eq!(ndcg_at_10(&[], &expect), Some(0.0));
        assert_eq!(ndcg_at_10(&perfect, &[]), None);
    }
}
