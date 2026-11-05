//! `janus suggest-links`: semantic candidate pairs that no declared link joins
//! (M5 §3.12).
//!
//! A candidate is a pair whose embedding directions are close and that no edge
//! joins in either direction, effective or inert. An inert cross-scope edge is
//! stored like any other (M5-8), so it excludes its pair here too: the
//! declaration is the user's, and a suggestion must not contradict it.
//!
//! The report is a convenience, never a fact: nothing here writes a link,
//! applies a suggestion or asks for confirmation (D7).
//!
//! A read, like `moves`: it loads the validated generation under
//! [`Policy::Refuse`], so it takes no lock, builds nothing, refreshes nothing
//! and writes nothing. Without vectors it prints one `degraded` line and an
//! empty list, and it exits 0.
//!
//! **Representative vector.** A document is one vector, the mean of its chunk
//! rows, renormalised. The plan named "the best chunk of each pair", which is
//! `O(n²·m²·d)`: on a 2 119-document vault with 27 234 chunks that is about
//! `7×10⁸` dot products. Mean pooling makes the pair loop `O(n²·d)`, which is
//! what §3.12's cost sentence assumes (M5-49).

use std::collections::BTreeSet;
use std::io::{self, Write as _};
use std::path::Path;

use serde::Serialize;

use crate::clock::Clock;
use crate::config::Config;
use crate::error::JanusError;
use crate::id::DocId;
use crate::index::Snapshot;
use crate::index::embed::Embedder;
use crate::index::store::{self, LoadOptions, Policy};
use crate::index::vectors::{self, Vectors};
use crate::scan::API_VERSION;

/// The default cosine at or above which a pair is reported.
pub const DEFAULT_THRESHOLD: f32 = 0.85;

/// The one `degraded` line the command prints when it cannot score a pair.
pub const NO_EMBEDDER: &str = "no embedder; no semantic candidates";

/// How `janus suggest-links` should be rendered.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Report pairs whose cosine is at or above this value.
    pub threshold: f32,
    /// Emit one JSON object on stdout instead of the table.
    pub json: bool,
}

/// Parses a `--threshold` value, which must be a number in `0.0..=1.0`.
///
/// # Errors
///
/// [`JanusError::Usage`] when the text is not a number or lies outside the
/// range. The message names the value the user typed, so `--threshold 1.5` says
/// so.
pub fn parse_threshold(raw: &str) -> Result<f32, JanusError> {
    let trimmed = raw.trim();
    let value = trimmed.parse::<f32>().map_err(|_| {
        usage(format!(
            "unparseable --threshold value {trimmed:?}; expected a number from 0 to 1"
        ))
    })?;
    if !(0.0..=1.0).contains(&value) {
        return Err(usage(format!(
            "--threshold value {trimmed:?} is outside 0 to 1"
        )));
    }
    Ok(value)
}

/// The usage error, built in one place so every message has one shape.
fn usage(message: String) -> JanusError {
    JanusError::Usage { message }
}

/// Runs `janus suggest-links`.
///
/// Results go to stdout, the `degraded` diagnostic to stderr, so
/// `janus suggest-links --json | jq` always parses (§8.2).
///
/// # Errors
///
/// Propagates config and IO failures. A missing generation, a missing embedder
/// and a missing payload are all [`NO_EMBEDDER`], never an error: the command
/// reports what it can and stops.
pub fn run(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    embedder: Option<&dyn Embedder>,
    options: Options,
) -> Result<(), JanusError> {
    let (candidates, degraded) = if embedder.is_some() {
        read(root, config, clock, options.threshold)
    } else {
        (Vec::new(), vec![NO_EMBEDDER.to_owned()])
    };

    let mut out = io::stdout().lock();
    if options.json {
        let report = Report {
            api: API_VERSION,
            vault: root.display().to_string(),
            threshold: options.threshold,
            degraded,
            candidates,
        };
        let json = serde_json::to_string_pretty(&report)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        // A closed pipe is not a failure (`janus suggest-links --json | head`).
        let _ = writeln!(out, "{json}");
    } else {
        write_human(&mut out, options.threshold, &degraded, &candidates);
    }
    Ok(())
}

/// The `--json` report: one stable object, like every other command's.
#[derive(Debug, Serialize)]
struct Report {
    /// Contract version; see [`API_VERSION`].
    api: u32,
    /// Corpus root, absolute.
    vault: String,
    /// The cosine the caller asked for, echoed so the rows can be read alone.
    threshold: f32,
    /// Why this run scored nothing; empty when it scored.
    degraded: Vec<String>,
    /// Candidate pairs, sorted by `(cosine desc, path_a asc, path_b asc)`.
    candidates: Vec<Candidate>,
}

/// Loads the generation and its rows, then scores. A failure is `degraded`.
fn read(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    threshold: f32,
) -> (Vec<Candidate>, Vec<String>) {
    // The embedder is deliberately **not** handed to the load: no build can run
    // under [`Policy::Refuse`], and a generation whose rows came from another
    // model is still internally consistent, so its cosines are still meaningful.
    let options = LoadOptions {
        policy: Policy::Refuse,
        want_vectors: true,
        ..LoadOptions::new(root, config, clock.today())
    };
    let load = match store::load(&options) {
        Ok(load) => load,
        // A refusal is not a failure of this command: it has no pairs to score.
        Err(err) => return (Vec::new(), vec![format!("{err}; {NO_EMBEDDER}")]),
    };
    let Some(rows) = load.vectors.as_ref() else {
        return (Vec::new(), vec![NO_EMBEDDER.to_owned()]);
    };
    (candidates(&load.snapshot, rows, threshold), Vec::new())
}

/// One document's representative vector and the path it is reported under.
struct Representative {
    /// The document id.
    id: DocId,
    /// Vault-relative path, `/`-separated.
    path: String,
    /// The mean of the document's chunk rows, renormalised.
    vector: Vec<f32>,
}

/// One unlinked pair at or above the threshold.
#[derive(Debug, Serialize)]
pub struct Candidate {
    /// Cosine of the two representative vectors, in `-1.0..=1.0`.
    pub cosine: f32,
    /// The lexicographically smaller path of the pair.
    pub path_a: String,
    /// The id behind `path_a`, in hex.
    pub id_a: String,
    /// The other path of the pair.
    pub path_b: String,
    /// The id behind `path_b`, in hex.
    pub id_b: String,
}

/// Every document's representative vector, in snapshot order.
///
/// A document with no chunk row, or whose chunk rows cancel to the zero vector,
/// has no direction and is left out: it can be in no candidate pair.
fn representatives(snapshot: &Snapshot, rows: &Vectors) -> Vec<Representative> {
    let width = rows.dim();
    let mut out = Vec::with_capacity(snapshot.docs.len());
    for doc in &snapshot.docs {
        let (start, end) = doc.chunk_range;
        let mut sum = vec![0.0f32; width];
        let mut count = 0usize;
        for ordinal in start..end {
            let Some(row) = rows.row(ordinal as usize) else {
                continue;
            };
            if row.len() != width {
                continue;
            }
            for (slot, value) in sum.iter_mut().zip(row) {
                *slot += *value;
            }
            count += 1;
        }
        if count == 0 {
            continue;
        }
        let divisor = count as f32;
        for value in &mut sum {
            *value /= divisor;
        }
        if vectors::normalize(&mut sum) == 0.0 {
            continue;
        }
        out.push(Representative {
            id: doc.id,
            path: snapshot
                .strings
                .get(doc.rel_path)
                .unwrap_or_default()
                .to_owned(),
            vector: sum,
        });
    }
    out
}

/// Every id pair an edge joins, in both directions.
fn linked(snapshot: &Snapshot) -> BTreeSet<(DocId, DocId)> {
    let mut pairs = BTreeSet::new();
    for edge in &snapshot.edges {
        pairs.insert((edge.from, edge.to));
        pairs.insert((edge.to, edge.from));
    }
    pairs
}

/// Every unlinked pair at or above `threshold`, in report order.
///
/// The order is `(cosine desc, path_a asc, path_b asc)`, with `path_a < path_b`
/// in byte order, so two runs on one generation print identical bytes.
#[must_use]
pub fn candidates(snapshot: &Snapshot, rows: &Vectors, threshold: f32) -> Vec<Candidate> {
    let linked = linked(snapshot);
    let reps = representatives(snapshot, rows);
    let mut out = Vec::new();
    for (index, left) in reps.iter().enumerate() {
        for right in &reps[index + 1..] {
            if linked.contains(&(left.id, right.id)) {
                continue;
            }
            // Both rows are unit length, so the dot product is the cosine. The
            // clamp hides the `1 + 1e-7` an `f32` sum can produce.
            let cosine = vectors::dot(&left.vector, &right.vector).clamp(-1.0, 1.0);
            if cosine < threshold {
                continue;
            }
            let (a, b) = if left.path <= right.path {
                (left, right)
            } else {
                (right, left)
            };
            out.push(Candidate {
                cosine,
                path_a: a.path.clone(),
                id_a: a.id.to_hex(),
                path_b: b.path.clone(),
                id_b: b.id.to_hex(),
            });
        }
    }
    out.sort_by(|left, right| {
        right
            .cosine
            .total_cmp(&left.cosine)
            .then_with(|| left.path_a.cmp(&right.path_a))
            .then_with(|| left.path_b.cmp(&right.path_b))
    });
    out
}

// ───────────────────────────────── human ───────────────────────────────────

/// Prints one line per candidate, then one `degraded` line per reason.
fn write_human(
    out: &mut impl io::Write,
    threshold: f32,
    degraded: &[String],
    candidates: &[Candidate],
) {
    for row in candidates {
        let _ = writeln!(
            out,
            "{:.4}  {}  {}  {}  {}",
            row.cosine, row.path_a, row.id_a, row.path_b, row.id_b
        );
    }
    for line in degraded {
        eprintln!("degraded: {line}");
    }
    if candidates.is_empty() && degraded.is_empty() {
        let _ = writeln!(out, "no unlinked pair at or above {threshold}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{DateSource, Status};
    use crate::id::Blake3Hash;
    use crate::index::{ChunkMeta, DocMeta, Edge, EdgeKind, LexicalIndex, StringId};
    use crate::tokenize::Tokenizer;

    /// One document whose metadata no score reads.
    ///
    /// `unused` is a real pool handle with empty text: the score reads the id,
    /// the path and the chunk range, and nothing else.
    fn meta(id: u8, rel_path: StringId, unused: StringId, chunk_range: (u32, u32)) -> DocMeta {
        DocMeta {
            id: DocId::from_bytes([id; crate::id::DOC_ID_LEN]),
            rel_path,
            title: unused,
            type_name: unused,
            status: Status::Active,
            scope: unused,
            shared_with: Vec::new(),
            tags: Vec::new(),
            date: None,
            date_source: DateSource::None,
            effective_from: None,
            effective_to: None,
            timeless: false,
            content_hash: Blake3Hash::from_bytes([id; crate::id::BLAKE3_LEN]),
            body_hash: Blake3Hash::from_bytes([id; crate::id::BLAKE3_LEN]),
            lex_key: [0; 32],
            chunk_range,
            hint: None,
        }
    }

    /// A snapshot holding `paths`, one after another, over `chunks` rows.
    ///
    /// Each document owns exactly one chunk row except where a test splits a
    /// range itself, which is what makes mean pooling visible.
    fn snapshot(paths: &[&str]) -> Snapshot {
        let index = LexicalIndex::build(&[]);
        let mut snapshot = Snapshot::from_index(
            &index,
            "suggest".to_owned(),
            Tokenizer::new().fingerprint(),
            Vec::new(),
            Vec::new(),
        );
        let unused = snapshot.strings.intern("");
        for (ordinal, path) in paths.iter().enumerate() {
            let rel_path = snapshot.strings.intern(path);
            let ordinal = u32::try_from(ordinal).expect("small fixture");
            snapshot.docs.push(meta(
                ordinal as u8 + 1,
                rel_path,
                unused,
                (ordinal, ordinal + 1),
            ));
            snapshot.chunks.push(ChunkMeta {
                id: crate::id::ChunkId::from_bytes([ordinal as u8 + 1; crate::id::BLAKE3_LEN]),
                doc: ordinal,
                span: (0, 1),
                heading_path: Vec::new(),
                tokens: 1,
            });
        }
        snapshot
    }

    /// A payload of unit rows over `dim` columns.
    fn rows(dim: u32, values: &[&[f32]]) -> Vectors {
        Vectors::new(
            dim,
            values.iter().flat_map(|row| row.iter().copied()).collect(),
        )
        .expect("the fixture rows are unit length")
    }

    #[test]
    fn a_document_vector_is_the_mean_of_its_chunk_rows() {
        // A has two chunks, one on each axis, so its mean direction is the
        // diagonal. B has one chunk on that same diagonal. The cosine is
        // `1.0` only because A is mean-pooled; "the best chunk of the pair"
        // would compare B with one axis and report `1/√2 = 0.7071`.
        let mut snapshot = snapshot(&["notes/a.md", "notes/b.md"]);
        snapshot.docs[0].chunk_range = (0, 2);
        snapshot.docs[1].chunk_range = (2, 3);
        snapshot.chunks.push(ChunkMeta {
            id: crate::id::ChunkId::from_bytes([3; crate::id::BLAKE3_LEN]),
            doc: 0,
            span: (0, 1),
            heading_path: Vec::new(),
            tokens: 1,
        });
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let rows = rows(2, &[&[1.0, 0.0], &[0.0, 1.0], &[half, half]]);

        let found = candidates(&snapshot, &rows, 0.85);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            (found[0].cosine - 1.0).abs() < 1e-6,
            "the diagonal is B's own direction: {}",
            found[0].cosine
        );
        assert_eq!(found[0].path_a, "notes/a.md");
        assert_eq!(found[0].path_b, "notes/b.md");
    }

    #[test]
    fn an_edge_excludes_its_pair_in_both_directions() {
        let mut snapshot = snapshot(&["notes/a.md", "notes/b.md", "notes/c.md"]);
        // One row per document, all identical, so every pair would qualify.
        let rows = rows(1, &[&[1.0], &[1.0], &[1.0]]);
        assert_eq!(candidates(&snapshot, &rows, 0.85).len(), 3);

        // `supersedes` on `b` names `a`: the edge is `a → b`.
        snapshot.edges.push(Edge {
            from: snapshot.docs[0].id,
            to: snapshot.docs[1].id,
            kind: EdgeKind::Supersedes,
        });
        let found = candidates(&snapshot, &rows, 0.85);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(
            found
                .iter()
                .all(|row| row.path_a != "notes/a.md" || row.path_b != "notes/b.md"),
            "the linked pair is gone: {found:?}"
        );
    }

    #[test]
    fn rows_sort_by_cosine_then_path() {
        // `a` is the diagonal, `b` and `c` are the axes: a-b and a-c are both
        // `0.7071`, b-c is `0`. Path order decides the two equal rows.
        let snapshot = snapshot(&["notes/b.md", "notes/a.md", "notes/c.md"]);
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let rows = rows(2, &[&[1.0, 0.0], &[half, half], &[0.0, 1.0]]);

        let found = candidates(&snapshot, &rows, 0.5);
        let paths: Vec<(&str, &str)> = found
            .iter()
            .map(|row| (row.path_a.as_str(), row.path_b.as_str()))
            .collect();
        assert_eq!(
            paths,
            vec![("notes/a.md", "notes/b.md"), ("notes/a.md", "notes/c.md")],
            "cosine desc, then path_a asc"
        );
    }

    #[test]
    fn a_threshold_above_every_cosine_reports_nothing() {
        let snapshot = snapshot(&["notes/a.md", "notes/b.md"]);
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let rows = rows(2, &[&[1.0, 0.0], &[half, half]]);
        // The cosine is 1/√2 = 0.7071, below both limits.
        assert!(candidates(&snapshot, &rows, 0.71).is_empty());
        assert_eq!(candidates(&snapshot, &rows, 0.70).len(), 1);
    }

    #[test]
    fn a_threshold_outside_the_range_names_the_value() {
        let err = parse_threshold("1.5").expect_err("1.5 is outside 0 to 1");
        assert!(
            err.to_string().contains("\"1.5\""),
            "the message names the value: {err}"
        );
        assert_eq!(err.exit_code(), 2);

        let err = parse_threshold("-0.1").expect_err("-0.1 is outside 0 to 1");
        assert!(err.to_string().contains("\"-0.1\""), "{err}");

        // NaN parses as a number and is not in the range either.
        let err = parse_threshold("NaN").expect_err("NaN is not a threshold");
        assert!(err.to_string().contains("\"NaN\""), "{err}");
    }

    #[test]
    fn a_threshold_inside_the_range_is_kept() {
        assert!((parse_threshold("0.85").expect("0.85") - 0.85).abs() < 1e-6);
        assert_eq!(parse_threshold("0").expect("0"), 0.0);
        assert_eq!(parse_threshold("1").expect("1"), 1.0);
    }
}
