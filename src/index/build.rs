//! Incremental snapshot build (§3.4, S-9).
//!
//! Reuse is per **document**, keyed on the indexed input rather than [`ChunkId`]
//! alone: a chunk id excludes the title, while the indexed text includes it.
//! Tokens only are reused; metadata, spans, headings and ordering are rebuilt.
//! Old term-major postings become per-occurrence `(term, tf)` lists in one pass.
//!
//! Term ids are lexicographic. A snapshot cannot recover the token order inside
//! an old chunk, so a text-based canonical order is what lets a reused build
//! reproduce a clean build byte-for-byte (I-4).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::NaiveDate;

use crate::chunk::{Chunk, chunk_body};
use crate::config::Config;
use crate::document::Document;
use crate::graph::jaccard;
use crate::id::{Blake3Hash, ChunkId, DocId};
use crate::link;
use crate::tokenize::{Stemmer, Tokenizer, TokenizerFingerprint, Tokens};
use crate::vault::identity;
use crate::vault::resolve::strip_leading_date;

use super::embed::{Embedder, EmbedderId};
use super::store::{Corrupt, Manifest, Payloads, VectorSet};
use super::vectors::{Vectors, normalize};
use super::{
    ChunkMeta, ChunkingFingerprint, DocMeta, Hint, LexicalIndex, Posting, Snapshot, StringId,
    StringPool, TermMeta,
};

/// One callback, the seam the progress printer and the tests use (§3.6).
///
/// It fires once per chunk that goes through [`Embedder::embed`]. `done` is
/// cumulative over this build and `total` is how many texts this build embeds,
/// so a caller can compute a step without knowing the batch size.
pub type EmbedSink<'a> = &'a dyn Fn(u64, u64);

/// A sink that reports nothing: a dry run and a quiet caller pass this.
pub fn no_progress(_done: u64, _total: u64) {}

/// What one build did, for the phase-6 receipt and the §3.4 tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuildStats {
    /// Chunks that actually went through the tokenizer.
    pub retokenized_chunks: usize,
    /// Documents whose tokens came from the previous snapshot.
    pub reused_documents: usize,
    /// Documents whose chunks were tokenized again.
    pub rebuilt_documents: usize,
    /// Chunks that actually went through `Embedder::embed` (or, on a dry run,
    /// the chunks a real run would embed).
    pub embedded_chunks: usize,
    /// Rows copied from the previous generation instead of being embedded.
    pub reused_vector_chunks: usize,
}

/// The previous generation's snapshot and its validated rows (§3.4).
///
/// The rows are kept with the id that produced them because a row without its
/// embedder cannot be compared, copied, or described. Both are supplied by the
/// caller that read the files; `build` reads no I/O of its own.
#[derive(Debug, Clone)]
pub struct Previous {
    /// The previous `index.bin`, validated by its decoder.
    pub snapshot: Snapshot,
    /// The previous `vectors.f32` and its manifest identity, when they decoded
    /// against that snapshot. `None` means “re-embed everything”.
    pub vectors: Option<(EmbedderId, Vectors)>,
}

impl Previous {
    /// A previous snapshot with no usable rows: token reuse only.
    #[must_use]
    pub fn tokens(snapshot: Snapshot) -> Self {
        Self {
            snapshot,
            vectors: None,
        }
    }

    /// A previous generation whose rows validated against its chunk count.
    #[must_use]
    pub fn with_vectors(snapshot: Snapshot, id: EmbedderId, rows: Vectors) -> Self {
        Self {
            snapshot,
            vectors: Some((id, rows)),
        }
    }
}

/// A snapshot plus its build statistics, before anything is written.
#[derive(Debug, Clone)]
pub struct BuildPlan {
    /// The snapshot a write would publish.
    pub snapshot: Snapshot,
    /// What the build reused and rebuilt.
    pub stats: BuildStats,
    /// The chunker fingerprint the manifest should record.
    pub chunker: ChunkingFingerprint,
    /// The rows to publish beside the snapshot, or `None` when no rows are
    /// available (§3.1). A dry run leaves this `None` while `vector_id` still
    /// names what a real run would publish.
    pub vectors: Option<VectorSet>,
    /// The embedder id the plan's rows would carry, even when a dry run did not
    /// build them.
    pub vector_id: Option<EmbedderId>,
    /// Documents whose rows could not be carried; present only when a previous
    /// payload exists and this build cannot embed (§3.6 row 3).
    pub vectors_dropped: Option<usize>,
}

impl BuildPlan {
    /// The byte length `vectors.f32` would have, derived from the chunk count
    /// and the row width. `None` when the plan publishes no rows.
    #[must_use]
    pub fn vectors_bytes(&self) -> Option<u64> {
        let id = self.vector_id.as_ref()?;
        let rows = u64::try_from(self.snapshot.chunks.len()).unwrap_or(u64::MAX);
        Some(rows.saturating_mul(u64::from(id.dim)).saturating_mul(4))
    }

    /// Builds the manifest without writing anything.
    ///
    /// `corpus_sig` must be the digest of the walk that produced this plan when
    /// the manifest is published: the field describes the files the plan was
    /// built from, not the end of the build (M4D §3.2). `config_hash` must be
    /// the hash of the config the plan was resolved under (M7-1). `None` is for
    /// a caller that only inspects the manifest.
    ///
    /// # Errors
    ///
    /// [`Corrupt`] when the snapshot cannot be encoded.
    pub fn manifest(
        &self,
        root: &Path,
        built_at: &str,
        corpus_sig: Option<&str>,
        config_hash: Option<&str>,
    ) -> Result<Manifest, Corrupt> {
        let bytes = self.snapshot.to_bytes()?;
        let vectors = self.vectors.as_ref().map(|set| set.artifact().1);
        Ok(Manifest::new(
            root,
            &self.snapshot,
            self.chunker,
            Payloads {
                index_sha: blake3::hash(&bytes).to_hex().to_string(),
                index_len: bytes.len() as u64,
                vectors,
            },
            built_at,
            corpus_sig,
            config_hash,
        ))
    }
}

/// Computes the next snapshot, reusing tokens and rows where §3.4 allows.
///
/// An invalid `previous` is treated as absent; computing a plan performs no I/O.
/// Reused rows are copied into the new snapshot's positions, so ordinal is the
/// mapping because the predicate requires the chunk-id sequence to match.
///
/// `basis` is the identity basis, which is a different question from `previous`:
/// `--rebuild` passes a basis and no reuse source, because a rebuild
/// re-tokenizes and never re-identifies (M5-2). `today` stamps the identity
/// tables and is injected (§7.2).
///
/// # Errors
///
/// [`Corrupt`] when the embedder breaks its contract (wrong row count, wrong
/// width, a non-finite value), or when a reused row cannot be addressed. The
/// build publishes nothing in that case: a substituted zero row would be
/// invisible quality loss (§3.2 V-7).
pub fn plan(
    config: &Config,
    docs: &mut [Document],
    previous: Option<&Previous>,
    basis: Option<&Snapshot>,
    today: NaiveDate,
    embedder: Option<&dyn Embedder>,
    sink: EmbedSink<'_>,
) -> Result<BuildPlan, Corrupt> {
    plan_forced(
        config,
        docs,
        previous,
        basis,
        today,
        &BTreeSet::new(),
        embedder,
        sink,
    )
}

/// [`plan`] with a set of documents whose tokens must be recomputed anyway.
///
/// `janus index PATHS` uses this for a targeted refresh: reuse still applies to
/// every other document, so the index stays complete and the save is real.
/// Identity is resolved here, once, from the corpus and the basis.
///
/// # Errors
///
/// Exactly [`plan`]'s.
#[allow(clippy::too_many_arguments)]
pub fn plan_forced(
    config: &Config,
    docs: &mut [Document],
    previous: Option<&Previous>,
    basis: Option<&Snapshot>,
    today: NaiveDate,
    force: &BTreeSet<DocId>,
    embedder: Option<&dyn Embedder>,
    sink: EmbedSink<'_>,
) -> Result<BuildPlan, Corrupt> {
    plan_inner(
        config, docs, previous, basis, today, force, embedder, true, sink,
    )
}

/// [`plan_forced`] without calling the embedder: the `--dry-run` shape.
///
/// The reuse decision, the counters and the id of the rows a real run would
/// publish are computed exactly as a write would compute them. `vectors` stays
/// `None` because no row was produced, and nothing is written.
///
/// # Errors
///
/// Exactly [`plan_forced`]'s.
#[allow(clippy::too_many_arguments)]
pub fn plan_forced_dry_run(
    config: &Config,
    docs: &mut [Document],
    previous: Option<&Previous>,
    basis: Option<&Snapshot>,
    today: NaiveDate,
    force: &BTreeSet<DocId>,
    embedder: Option<&dyn Embedder>,
    sink: EmbedSink<'_>,
) -> Result<BuildPlan, Corrupt> {
    plan_inner(
        config, docs, previous, basis, today, force, embedder, false, sink,
    )
}

#[allow(clippy::too_many_arguments)]
fn plan_inner(
    config: &Config,
    docs: &mut [Document],
    previous: Option<&Previous>,
    basis: Option<&Snapshot>,
    today: NaiveDate,
    force: &BTreeSet<DocId>,
    embedder: Option<&dyn Embedder>,
    embed: bool,
    sink: EmbedSink<'_>,
) -> Result<BuildPlan, Corrupt> {
    // Identity first: every row below names the id it resolves to, and the
    // tables describe the generation this plan would publish (§3.1 step 4).
    let resolution = identity::resolve(docs, basis, config, today);
    let tokenizer = Tokenizer::new();
    let tokenizer_fp = tokenizer.fingerprint();
    let chunker = ChunkingFingerprint {
        max_chars: config.chunking.max_chars,
        overlap: config.chunking.overlap,
    };

    let previous = previous.filter(|previous| previous.snapshot.validate().is_ok());
    let previous_snapshot = previous.map(|previous| &previous.snapshot);
    let previous_vectors = previous.and_then(|previous| previous.vectors.as_ref());
    // A row count that disagrees with the snapshot is a misalignment, and a
    // misaligned row is the worst failure this milestone can produce.
    let previous_valid_vectors = previous_vectors.filter(|(_, rows)| {
        previous_snapshot.is_some_and(|snapshot| rows.len() == snapshot.chunks.len())
    });
    let previous_rows = previous_valid_vectors.map(|(_, rows)| rows);
    let old_docs: BTreeMap<DocId, usize> = previous_snapshot
        .map(|snapshot| {
            snapshot
                .docs
                .iter()
                .enumerate()
                .map(|(index, doc)| (doc.id, index))
                .collect()
        })
        .unwrap_or_default();
    let old_forward = previous_snapshot.map(forward_index).unwrap_or_default();
    // With an embedder, its id is the target. Without one, row 3 substitutes
    // the previous manifest's id, so a title change is still visible.
    let target_id = embedder
        .map(|embedder| embedder.id())
        .or_else(|| previous_valid_vectors.map(|(id, _)| id));

    let mut ordered: Vec<&Document> = docs.iter().collect();
    ordered.sort_by(|a, b| a.rel_path.cmp(&b.rel_path).then_with(|| a.id.cmp(&b.id)));

    let mut stats = BuildStats::default();
    let mut tokens = Tokens::default();
    let mut drafts = Vec::with_capacity(ordered.len());
    // The embedded texts a real run would send, in snapshot order. Only chunks
    // that did not reuse a row enter this list.
    let mut embed_texts = Vec::new();
    // One source per published row, in snapshot order. Reused rows name their
    // old position; embedded rows take the next text from `embed_texts`.
    let mut row_sources = Vec::new();
    let mut vector_failures = 0usize;

    for doc in ordered {
        let body_hash = Blake3Hash::of_bytes(doc.body.as_bytes());
        let (chunks, _notes) = chunk_body(doc.id, &doc.body, &config.chunking);
        let key = lex_key(&tokenizer_fp, &chunker, &doc.title);
        let old_match = previous_snapshot.and_then(|snapshot| {
            old_docs
                .get(&doc.id)
                .and_then(|&index| snapshot.docs.get(index).map(|meta| (snapshot, meta)))
        });
        let reused = !force.contains(&doc.id)
            && old_match.is_some_and(|(snapshot, old_doc)| {
                reusable(
                    snapshot,
                    old_doc,
                    &old_forward,
                    doc,
                    &body_hash,
                    &chunks,
                    &tokenizer_fp,
                    &chunker,
                )
            });
        if reused {
            stats.reused_documents += 1;
        } else {
            stats.rebuilt_documents += 1;
        }

        let vector_reused = target_id.is_some_and(|target| {
            old_match.is_some_and(|(snapshot, old_doc)| {
                previous_valid_vectors.is_some_and(|(old_id, rows)| {
                    vectors_reusable(
                        snapshot, old_doc, rows, old_id, target, doc, &body_hash, &chunks,
                    )
                })
            })
        });
        if !vector_reused {
            vector_failures += 1;
        }

        let old_range = old_match.map(|(_, old_doc)| old_doc.chunk_range);
        let mut chunk_drafts = Vec::with_capacity(chunks.len());
        for (ordinal, chunk) in chunks.iter().enumerate() {
            let text = chunk.embed_text(&doc.body, &doc.title);
            let (terms, length) = if reused {
                let index = old_range
                    .and_then(|(start, _)| start.checked_add(u32::try_from(ordinal).ok()?));
                let pairs = index
                    .and_then(|index| old_forward.get(index as usize))
                    .map(|pairs| old_terms(previous_snapshot, pairs))
                    .unwrap_or_default();
                let length = pairs.values().copied().fold(0u32, u32::saturating_add);
                (pairs, length)
            } else {
                stats.retokenized_chunks += 1;
                tokenizer.tokenize_into(&text, &mut tokens);
                let mut terms: BTreeMap<String, u32> = BTreeMap::new();
                for term in tokens.iter() {
                    let count = terms.entry(term.to_owned()).or_insert(0);
                    *count = count.saturating_add(1);
                }
                let length = u32::try_from(tokens.len()).unwrap_or(u32::MAX);
                (terms, length)
            };
            if vector_reused {
                let start = old_range
                    .map(|(start, _)| usize::try_from(start).unwrap_or(usize::MAX))
                    .unwrap_or(usize::MAX);
                row_sources.push(RowSource::Reused(start.saturating_add(ordinal)));
                stats.reused_vector_chunks += 1;
            } else {
                row_sources.push(RowSource::Embedded);
                if embedder.is_some() {
                    embed_texts.push(text);
                }
            }
            chunk_drafts.push(ChunkDraft {
                id: chunk.id,
                span: chunk.span,
                heading_path: chunk.heading_path.clone(),
                terms,
                tokens: length,
            });
        }
        drafts.push(DocDraft {
            doc,
            lex_key: key,
            body_hash,
            chunks: chunk_drafts,
        });
    }

    let mut vocabulary = BTreeSet::new();
    for chunk in drafts.iter().flat_map(|draft| draft.chunks.iter()) {
        vocabulary.extend(chunk.terms.keys().cloned());
    }
    let ordered_terms: Vec<String> = vocabulary.into_iter().collect();
    let mut pool = StringPool::new();
    for term in &ordered_terms {
        pool.intern(term);
    }

    let mut buckets: Vec<Vec<Posting>> = vec![Vec::new(); ordered_terms.len()];
    for (index, chunk) in drafts
        .iter()
        .flat_map(|draft| draft.chunks.iter())
        .enumerate()
    {
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        for (term, tf) in &chunk.terms {
            if let Some(id) = pool.lookup(term)
                && let Some(bucket) = buckets.get_mut(id.as_usize())
            {
                bucket.push(Posting {
                    chunk: index,
                    tf: *tf,
                });
            }
        }
    }

    let mut postings = Vec::new();
    let mut terms = Vec::with_capacity(buckets.len());
    for bucket in &buckets {
        let Ok(df) = u32::try_from(bucket.len()) else {
            break;
        };
        let Ok(start) = u32::try_from(postings.len()) else {
            break;
        };
        let Some(end) = start.checked_add(df) else {
            break;
        };
        postings.extend_from_slice(bucket);
        terms.push(TermMeta {
            df,
            postings: (start, end),
        });
    }

    let lengths: Vec<u32> = drafts
        .iter()
        .flat_map(|draft| draft.chunks.iter())
        .map(|chunk| chunk.tokens)
        .collect();
    let avgdl = if lengths.is_empty() {
        0.0
    } else {
        lengths.iter().map(|&length| u64::from(length)).sum::<u64>() as f64 / lengths.len() as f64
    };

    let hints = continuation_hints(basis, &resolution, &drafts);
    let mut docs_meta = Vec::with_capacity(drafts.len());
    let mut chunks_meta = Vec::with_capacity(lengths.len());
    for (index, draft) in drafts.iter().enumerate() {
        let doc = draft.doc;
        let start = u32::try_from(chunks_meta.len()).unwrap_or(u32::MAX);
        let rel_path = pool.intern(&doc.rel_path);
        let title = pool.intern(&doc.title);
        let type_name = pool.intern(&doc.type_name);
        let scope = pool.intern(&doc.scope);
        let shared_with = doc.shared_with.iter().map(|tag| pool.intern(tag)).collect();
        let tags = doc
            .frontmatter
            .tags
            .iter()
            .map(|tag| pool.intern(tag))
            .collect();
        for chunk in &draft.chunks {
            let heading_path = chunk
                .heading_path
                .iter()
                .map(|heading| pool.intern(heading))
                .collect();
            chunks_meta.push(ChunkMeta {
                id: chunk.id,
                doc: u32::try_from(index).unwrap_or(u32::MAX),
                span: chunk.span,
                heading_path,
                tokens: chunk.tokens,
            });
        }
        let end = u32::try_from(chunks_meta.len()).unwrap_or(u32::MAX);
        docs_meta.push(DocMeta {
            id: doc.id,
            rel_path,
            title,
            type_name,
            status: doc.status,
            scope,
            shared_with,
            tags,
            date: doc.date,
            date_source: doc.date_source,
            effective_from: doc.effective_from,
            effective_to: doc.effective_to,
            timeless: doc.type_spec.timeless,
            content_hash: doc.content_hash,
            // Raw body bytes: a normalised body would fold line endings and
            // reopen the hole V-23 closes (V-26).
            body_hash: draft.body_hash,
            lex_key: draft.lex_key,
            chunk_range: (start, end),
            hint: hints.get(&doc.id).copied(),
        });
    }

    // Links resolve from this generation's own tables: no file is opened and no
    // path is confined, because every step is a lookup (§3.2). This runs before
    // the pool moves into the index, and needs nothing the pool holds.
    let declared: Vec<link::Declared<'_>> = drafts
        .iter()
        .map(|draft| link::Declared {
            id: draft.doc.id,
            rel_path: &draft.doc.rel_path,
            supersedes: &draft.doc.frontmatter.supersedes,
            superseded_by: &draft.doc.frontmatter.superseded_by,
        })
        .collect();
    let (edges, dangling_links) = link::resolve(
        &declared,
        &resolution.path_history,
        &resolution.identity,
        &config.corpus_id,
    );

    let index = LexicalIndex::from_parts(pool, terms, postings, lengths, avgdl);
    let mut snapshot = Snapshot::from_index(
        &index,
        config.corpus_id.clone(),
        tokenizer_fp,
        docs_meta,
        chunks_meta,
    );
    // Carrying the tables forward is what keeps a move visible across builds:
    // a snapshot built from this scan alone would erase the history on the next
    // refresh and re-derive every id (§3.1 step 4).
    snapshot.path_history = resolution.path_history;
    snapshot.identity = resolution.identity;
    snapshot.tombstones = resolution.tombstones;
    snapshot.edges = edges;
    snapshot.dangling_links = dangling_links;
    let total_rows = snapshot.chunks.len();
    let (vectors, vector_id, vectors_dropped) = match (embedder, target_id) {
        (Some(embedder), Some(id)) => {
            let embedded = if embed && !embed_texts.is_empty() {
                Some(embed_all(
                    embedder,
                    &embed_texts,
                    config.embedding.batch_size,
                    sink,
                )?)
            } else {
                None
            };
            stats.embedded_chunks = embed_texts.len();
            let rows = if embed {
                Some(assemble_vectors(
                    id.clone(),
                    &row_sources,
                    embedded.as_ref(),
                    previous_rows,
                    total_rows,
                )?)
            } else {
                None
            };
            (rows, Some(id.clone()), None)
        }
        (None, Some(id)) => {
            let all_reused = previous_valid_vectors.is_some()
                && vector_failures == 0
                && row_sources.len() == total_rows;
            if all_reused {
                stats.embedded_chunks = 0;
                stats.reused_vector_chunks = total_rows;
                let rows =
                    assemble_vectors(id.clone(), &row_sources, None, previous_rows, total_rows)?;
                (Some(rows), Some(id.clone()), None)
            } else {
                // A row that cannot be carried means the whole file goes: a
                // partial payload would put one document's row on another's
                // chunk. The count is the documents that failed the predicate.
                stats.reused_vector_chunks = 0;
                let dropped = previous_valid_vectors.is_some().then_some(vector_failures);
                (None, None, dropped)
            }
        }
        _ => (None, None, None),
    };
    Ok(BuildPlan {
        snapshot,
        stats,
        chunker,
        vectors,
        vector_id,
        vectors_dropped,
    })
}

/// One row's provenance while the new snapshot is being assembled.
#[derive(Debug, Clone, Copy)]
enum RowSource {
    /// Copy the row at this index from the previous generation.
    Reused(usize),
    /// Take the next row produced by this build's embedder.
    Embedded,
}

/// Copies reused rows into their new positions and interleaves fresh ones.
///
/// With the chunk-id sequence conjunct, ordinal is the mapping: a reused
/// document's chunk `n` is old position `start + n` and new position
/// `new_start + n`. The function re-validates the assembled array, so a bad
/// copy fails the build rather than reaching disk.
fn assemble_vectors(
    id: EmbedderId,
    sources: &[RowSource],
    embedded: Option<&VectorSet>,
    previous_rows: Option<&Vectors>,
    total_rows: usize,
) -> Result<VectorSet, Corrupt> {
    let dim = id.dim as usize;
    let mut flat = Vec::with_capacity(total_rows.saturating_mul(dim));
    let mut embedded_index = 0usize;
    for (row, source) in sources.iter().enumerate() {
        match source {
            RowSource::Reused(index) => {
                let old = previous_rows
                    .and_then(|rows| rows.row(*index))
                    .ok_or_else(|| {
                        Corrupt::new(format!(
                            "cannot reuse row {index} for new chunk {row}: the previous payload is absent or too short"
                        ))
                    })?;
                flat.extend_from_slice(old);
            }
            RowSource::Embedded => {
                let fresh = embedded
                    .and_then(|set| set.rows().row(embedded_index))
                    .ok_or_else(|| {
                        Corrupt::new(format!(
                            "new chunk {row} has no embedded row at index {embedded_index}"
                        ))
                    })?;
                flat.extend_from_slice(fresh);
                embedded_index = embedded_index.saturating_add(1);
            }
        }
    }
    if sources.len() != total_rows {
        return Err(Corrupt::new(format!(
            "assembled {} rows for {total_rows} chunks",
            sources.len()
        )));
    }
    if let Some(set) = embedded
        && embedded_index != set.rows().len()
    {
        return Err(Corrupt::new(format!(
            "used {embedded_index} embedded rows of {}",
            set.rows().len()
        )));
    }
    let rows = Vectors::new(id.dim, flat)?;
    VectorSet::new(id, rows)
}

/// Embeds every chunk text and assembles the validated payload (§3.2 V-7).
///
/// The rows are normalised here, not trusted from the embedder, and every
/// contract violation fails the build with a message naming the embedder and the
/// chunk index. Nothing is published when this returns an error.
fn embed_all(
    embedder: &dyn Embedder,
    texts: &[String],
    batch_size: usize,
    sink: EmbedSink<'_>,
) -> Result<VectorSet, Corrupt> {
    let id = embedder.id();
    let dim = id.dim as usize;
    let mut flat: Vec<f32> = Vec::with_capacity(texts.len().saturating_mul(dim));
    let step = batch_size.max(1);
    let total = u64::try_from(texts.len()).unwrap_or(u64::MAX);
    let mut done = 0u64;
    for (batch, slice) in texts.chunks(step).enumerate() {
        let refs: Vec<&str> = slice.iter().map(String::as_str).collect();
        let rows = embedder
            .embed(&refs)
            .map_err(|error| Corrupt::new(error.to_string()))?;
        if rows.len() != refs.len() {
            return Err(Corrupt::new(format!(
                "embedder `{}` returned {} rows for {} texts in batch {batch}",
                id.name,
                rows.len(),
                refs.len()
            )));
        }
        for (offset, mut row) in rows.into_iter().enumerate() {
            let chunk = batch.saturating_mul(step).saturating_add(offset);
            if row.len() != dim {
                return Err(Corrupt::new(format!(
                    "embedder `{}` chunk {chunk} has {} values, expected {dim}",
                    id.name,
                    row.len()
                )));
            }
            if let Some(index) = row.iter().position(|value| !value.is_finite()) {
                return Err(Corrupt::new(format!(
                    "embedder `{}` chunk {chunk} value {index} is not finite",
                    id.name
                )));
            }
            let _ = normalize(&mut row);
            flat.extend_from_slice(&row);
            done = done.saturating_add(1);
            // Once per chunk, after the row is accepted: a broken row fails the
            // build before it can be reported as progress.
            sink(done, total);
        }
    }

    let rows = Vectors::new(id.dim, flat)?;
    VectorSet::new(id.clone(), rows)
}

/// The four §3.11 weights. Named because every hint test derives its number from
/// them, and a silent edit here must be a visible edit in the formula.
const CHUNK_WEIGHT: f64 = 0.50;
const TITLE_WEIGHT: f64 = 0.20;
const BASENAME_WEIGHT: f64 = 0.15;
const TAG_WEIGHT: f64 = 0.15;
/// A score below this is not a hint.
const HINT_FLOOR: f32 = 0.50;

/// One vanished candidate's evidence, read from the basis generation alone.
struct HintEvidence {
    id: DocId,
    chunks: BTreeSet<ChunkId>,
    title: String,
    stem: String,
    tags: BTreeSet<String>,
}

/// Computes one hint per new document from the basis, never from `Previous`
/// (M5-15). `--rebuild` has no reuse source but still has a basis.
fn continuation_hints(
    basis: Option<&Snapshot>,
    resolution: &identity::Resolution,
    drafts: &[DocDraft<'_>],
) -> BTreeMap<DocId, Hint> {
    let Some(basis) = basis else {
        return BTreeMap::new();
    };
    let tombstoned: BTreeSet<DocId> = resolution.tombstones.iter().map(|meta| meta.id).collect();
    if tombstoned.is_empty() {
        return BTreeMap::new();
    }
    let evidence = hint_evidence(basis, &tombstoned);
    let mut hints = BTreeMap::new();
    for draft in drafts {
        // A document that kept an id is not a new document, so it gets no guess.
        if resolution.inherited.contains(&draft.doc.id) {
            continue;
        }
        let chunks: BTreeSet<ChunkId> = draft.chunks.iter().map(|chunk| chunk.id).collect();
        let title = normalise_title(&draft.doc.title);
        let stem = stem_key(&draft.doc.rel_path);
        let tags = document_tags(draft.doc);
        let mut best: Option<Hint> = None;
        for candidate in evidence.values() {
            let hint = score_hint(candidate, &chunks, &title, &stem, &tags);
            if hint.confidence < HINT_FLOOR {
                continue;
            }
            let replace = match best {
                None => true,
                Some(current) => {
                    hint.confidence > current.confidence
                        || (hint.confidence == current.confidence && hint.id < current.id)
                }
            };
            if replace {
                best = Some(hint);
            }
        }
        if let Some(hint) = best {
            hints.insert(draft.doc.id, hint);
        }
    }
    // A candidate that retired before this build has no `DocMeta` row to score
    // again. Its still-live continuation carries the answer the basis recorded,
    // so a rebuild does not erase a hint whose pair has not changed.
    for doc in &basis.docs {
        let Some(hint) = doc.hint else {
            continue;
        };
        if !tombstoned.contains(&hint.id) || evidence.contains_key(&hint.id) {
            continue;
        }
        if resolution.inherited.contains(&doc.id) {
            hints.entry(doc.id).or_insert(hint);
        }
    }
    hints
}

/// The tombstoned candidates that still have a basis `DocMeta` row and chunks.
fn hint_evidence(basis: &Snapshot, tombstoned: &BTreeSet<DocId>) -> BTreeMap<DocId, HintEvidence> {
    let mut chunk_sets: BTreeMap<DocId, BTreeSet<ChunkId>> = BTreeMap::new();
    for chunk in &basis.chunks {
        if let Some(doc) = basis.docs.get(chunk.doc as usize) {
            chunk_sets.entry(doc.id).or_default().insert(chunk.id);
        }
    }
    let mut evidence = BTreeMap::new();
    for doc in &basis.docs {
        if !tombstoned.contains(&doc.id) {
            continue;
        }
        let (Some(title), Some(rel_path)) = (
            basis.strings.get(doc.title),
            basis.strings.get(doc.rel_path),
        ) else {
            continue;
        };
        let mut tags: BTreeSet<String> = doc
            .shared_with
            .iter()
            .filter_map(|id| basis.strings.get(*id))
            .map(str::to_owned)
            .collect();
        tags.extend(
            doc.tags
                .iter()
                .filter_map(|id| basis.strings.get(*id))
                .map(str::to_owned),
        );
        evidence.insert(
            doc.id,
            HintEvidence {
                id: doc.id,
                chunks: chunk_sets.get(&doc.id).cloned().unwrap_or_default(),
                title: normalise_title(title),
                stem: stem_key(rel_path),
                tags,
            },
        );
    }
    evidence
}

/// The weighted score of one candidate, and the components that produced it.
fn score_hint(
    candidate: &HintEvidence,
    chunks: &BTreeSet<ChunkId>,
    title: &str,
    stem: &str,
    tags: &BTreeSet<String>,
) -> Hint {
    let chunk_jaccard = jaccard(chunks, &candidate.chunks);
    let title_match = f64::from(title == candidate.title);
    let basename_match = f64::from(stem == candidate.stem);
    let tag_jaccard = jaccard(tags, &candidate.tags);
    let confidence = CHUNK_WEIGHT * chunk_jaccard
        + TITLE_WEIGHT * title_match
        + BASENAME_WEIGHT * basename_match
        + TAG_WEIGHT * tag_jaccard;
    Hint {
        id: candidate.id,
        confidence: confidence as f32,
        chunk_jaccard: chunk_jaccard as f32,
        title_match: title_match as f32,
        basename_match: basename_match as f32,
        tag_jaccard: tag_jaccard as f32,
    }
}

/// Trims and collapses internal whitespace. Case is kept, as the identity key
/// keeps it.
fn normalise_title(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A file stem with any leading date token removed, so a rename that only moves
/// the date still counts as the same base name.
fn stem_key(rel_path: &str) -> String {
    let stem = Path::new(rel_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(rel_path);
    strip_leading_date(stem).to_owned()
}

/// `tags ∪ shared_with`, the set the tag component compares.
fn document_tags(document: &Document) -> BTreeSet<String> {
    document
        .frontmatter
        .tags
        .iter()
        .chain(document.shared_with.iter())
        .cloned()
        .collect()
}

/// One document being assembled, before term ids and the string pool exist.
struct DocDraft<'a> {
    doc: &'a Document,
    lex_key: [u8; 32],
    body_hash: Blake3Hash,
    chunks: Vec<ChunkDraft>,
}

/// One chunk's token counts, keyed by term text until the pool exists.
struct ChunkDraft {
    id: ChunkId,
    span: (u32, u32),
    heading_path: Vec<String>,
    terms: BTreeMap<String, u32>,
    tokens: u32,
}

/// The token half of §3.4. Any false result means “rebuild this document”.
// Every argument is one conjunct or one of the two fingerprints it compares;
// grouping them would hide which input the decision actually read.
#[allow(clippy::too_many_arguments)]
fn reusable(
    previous: &Snapshot,
    old_doc: &DocMeta,
    old_forward: &[Vec<(u32, u32)>],
    doc: &Document,
    body_hash: &Blake3Hash,
    chunks: &[Chunk],
    tokenizer: &TokenizerFingerprint,
    chunker: &ChunkingFingerprint,
) -> bool {
    if old_doc.lex_key != lex_key(tokenizer, chunker, &doc.title)
        || old_doc.body_hash != *body_hash
        || previous.tokenizer != *tokenizer
    {
        return false;
    }
    let (start, end) = (
        old_doc.chunk_range.0 as usize,
        old_doc.chunk_range.1 as usize,
    );
    let Some(old_chunks) = previous.chunks.get(start..end) else {
        return false;
    };
    old_chunks.len() == chunks.len()
        && old_forward.get(start..end).is_some()
        && chunks
            .iter()
            .all(|chunk| old_chunks.iter().any(|old| old.id == chunk.id))
}

/// The vector half of §3.4: raw text, embedder + title, then the chunk-id
/// **sequence**, and finally room in the old row range.
///
/// The sequence, not a set, is what lets rows be copied by ordinal. `body_hash`
/// already denies reuse after a section reorder, so this conjunct is defence in
/// depth rather than the only guard (V-24).
#[allow(clippy::too_many_arguments)]
fn vectors_reusable(
    previous: &Snapshot,
    old_doc: &DocMeta,
    rows: &Vectors,
    old_id: &EmbedderId,
    target_id: &EmbedderId,
    doc: &Document,
    body_hash: &Blake3Hash,
    chunks: &[Chunk],
) -> bool {
    if old_doc.body_hash != *body_hash || rows.len() != previous.chunks.len() {
        return false;
    }
    let Some(old_title) = previous.strings.get(old_doc.title) else {
        return false;
    };
    if vec_key(old_id, old_title) != vec_key(target_id, &doc.title) {
        return false;
    }
    let (start, end) = (
        old_doc.chunk_range.0 as usize,
        old_doc.chunk_range.1 as usize,
    );
    let Some(old_chunks) = previous.chunks.get(start..end) else {
        return false;
    };
    old_chunks.len() == chunks.len()
        && end <= rows.len()
        && old_chunks
            .iter()
            .map(|old| old.id)
            .eq(chunks.iter().map(|chunk| chunk.id))
}

/// Turns term-major postings into one `(term, tf)` list per old chunk.
fn forward_index(snapshot: &Snapshot) -> Vec<Vec<(u32, u32)>> {
    let mut forward = vec![Vec::new(); snapshot.chunks.len()];
    for (term_id, term) in snapshot.terms.iter().enumerate() {
        let Ok(term_id) = u32::try_from(term_id) else {
            break;
        };
        let (start, end) = (term.postings.0 as usize, term.postings.1 as usize);
        let Some(postings) = snapshot.postings.get(start..end) else {
            continue;
        };
        for posting in postings {
            if let Some(terms) = forward.get_mut(posting.chunk as usize) {
                terms.push((term_id, posting.tf));
            }
        }
    }
    forward
}

/// Reads one old chunk's term texts, deduplicating a corrupt repeated posting.
fn old_terms(previous: Option<&Snapshot>, pairs: &[(u32, u32)]) -> BTreeMap<String, u32> {
    let mut terms = BTreeMap::new();
    let Some(previous) = previous else {
        return terms;
    };
    for &(term_id, tf) in pairs {
        if let Some(term) = previous.strings.get(StringId(term_id)) {
            let count = terms.entry(term.to_owned()).or_insert(0u32);
            *count = count.saturating_add(tf);
        }
    }
    terms
}

/// The §3.4 formula, with a stable byte encoding for both fingerprints.
fn lex_key(
    tokenizer: &TokenizerFingerprint,
    chunker: &ChunkingFingerprint,
    title: &str,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(tokenizer.algorithm.as_bytes());
    hasher.update(&[0, stemmer_code(tokenizer.stemmer), 0]);
    hasher.update(&chunker.max_chars.to_le_bytes());
    hasher.update(&chunker.overlap.to_bits().to_le_bytes());
    hasher.update(&[0]);
    hasher.update(title.as_bytes());
    *hasher.finalize().as_bytes()
}

/// The §3.4 vector key: the embedder, then the title `ChunkId` excludes.
fn vec_key(id: &EmbedderId, title: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&id.to_bytes());
    hasher.update(&[0]);
    hasher.update(title.as_bytes());
    *hasher.finalize().as_bytes()
}

fn stemmer_code(stemmer: Stemmer) -> u8 {
    match stemmer {
        Stemmer::None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ChunkingConfig, Config, TypeSpec};
    use crate::document::{
        DateSource, Document, Frontmatter, FrontmatterSpan, IdentityTier, Status,
    };
    use crate::id::Blake3Hash;
    use crate::index::embed::{EmbedError, Embedder, EmbedderId, HashEmbedder};
    use chrono::NaiveDate;
    use proptest::prelude::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::SystemTime;

    fn config() -> Config {
        let mut config = Config::shipped_example().expect("shipped config");
        config.chunking = ChunkingConfig {
            max_chars: 1800,
            overlap: 0.0,
        };
        config
    }

    fn document(rel_path: &str, title: &str, body: &str) -> Document {
        Document {
            id: DocId::from_match_key(title.as_bytes()),
            id_source: IdentityTier::Title,
            path: PathBuf::from(rel_path),
            rel_path: rel_path.to_owned(),
            content_hash: Blake3Hash::of_bytes(body.as_bytes()),
            frontmatter: Frontmatter {
                title: Some(title.to_owned()),
                ..Frontmatter::default()
            },
            raw_frontmatter: None,
            body: body.to_owned(),
            span: FrontmatterSpan::default(),
            mtime: SystemTime::UNIX_EPOCH,
            size: body.len() as u64,
            title: title.to_owned(),
            type_name: "note".to_owned(),
            type_spec: TypeSpec {
                authority: 1.0,
                timeless: false,
                half_life_days: 180.0,
            },
            status: Status::Active,
            date: NaiveDate::from_ymd_opt(2025, 1, 1),
            date_source: DateSource::Frontmatter,
            effective_from: None,
            effective_to: None,
            scope: "global".to_owned(),
            shared_with: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn bytes(plan: &BuildPlan) -> Vec<u8> {
        plan.snapshot.to_bytes().expect("valid snapshot")
    }

    /// A pinned build date, so no identity table depends on the clock.
    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date")
    }

    /// A plan built with no embedder: the token-side cases do not embed.
    ///
    /// Planning resolves identity, which writes to the documents it is given, so
    /// the helper plans a copy and leaves the caller's set untouched.
    fn built(docs: &[Document], previous: Option<&Previous>) -> BuildPlan {
        plan(
            &config(),
            &mut docs.to_vec(),
            previous,
            None,
            today(),
            None,
            &no_progress,
        )
        .expect("a build without an embedder cannot fail")
    }

    /// A plan built from a basis with no reuse source, the `--rebuild` shape.
    fn rebuilt_from(docs: &[Document], basis: &Snapshot) -> BuildPlan {
        plan(
            &config(),
            &mut docs.to_vec(),
            None,
            Some(basis),
            today(),
            None,
            &no_progress,
        )
        .expect("a build without an embedder cannot fail")
    }

    /// A body with `count` sections that keep their chunk ids across a move.
    fn parts(prefix: &str, count: usize) -> String {
        (1..=count)
            .map(|index| format!("## {prefix} {index}\n\n{prefix} body {index}.\n\n"))
            .collect()
    }

    /// A document whose date and title are explicit, so identity can change.
    fn dated(rel_path: &str, title: &str, date: NaiveDate, body: &str) -> Document {
        let mut doc = document(rel_path, title, body);
        doc.date = Some(date);
        doc.date_source = DateSource::Frontmatter;
        doc.frontmatter.date = Some(date);
        doc
    }

    fn embedder(dim: u32) -> HashEmbedder {
        HashEmbedder::new(dim).expect("a non-zero dimension is valid")
    }

    /// A plan that embeds, so the vector half of every case is observable.
    fn vector_plan(
        docs: &[Document],
        previous: Option<&Previous>,
        embedder: &dyn crate::index::embed::Embedder,
    ) -> BuildPlan {
        plan(
            &config(),
            &mut docs.to_vec(),
            previous,
            None,
            today(),
            Some(embedder),
            &no_progress,
        )
        .expect("the hash double never fails")
    }

    /// The previous generation a follow-up build would read from disk.
    fn previous_of(plan: &BuildPlan) -> Previous {
        match plan.vectors.as_ref() {
            Some(set) => {
                Previous::with_vectors(plan.snapshot.clone(), set.id().clone(), set.rows().clone())
            }
            None => Previous::tokens(plan.snapshot.clone()),
        }
    }

    fn vector_bytes(plan: &BuildPlan) -> Vec<u8> {
        plan.vectors
            .as_ref()
            .expect("a plan with an embedder publishes rows")
            .rows()
            .encode()
    }

    /// A document whose title is derived from its path, not declared.
    fn derived_document(rel_path: &str, title: &str, body: &str) -> Document {
        let mut doc = document(rel_path, title, body);
        doc.frontmatter.title = None;
        doc.id = DocId::from_match_key(rel_path.as_bytes());
        doc.id_source = IdentityTier::RelPath;
        doc
    }

    /// Counts embed calls, so a dry run can prove it made none.
    struct CountingEmbedder {
        inner: HashEmbedder,
        calls: AtomicUsize,
    }

    impl CountingEmbedder {
        fn new(dim: u32) -> Self {
            Self {
                inner: embedder(dim),
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Embedder for CountingEmbedder {
        fn id(&self) -> &EmbedderId {
            self.inner.id()
        }

        fn embed(&self, batch: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.inner.embed(batch)
        }
    }

    /// Compares the encoded snapshots. `built_at` lives only in the manifest, so
    /// the snapshot itself has no timestamp to normalise.
    fn assert_same_as_clean(incremental: &BuildPlan, clean: &BuildPlan) {
        assert_eq!(
            bytes(incremental),
            bytes(clean),
            "incremental snapshot differs from a clean rebuild"
        );
    }

    #[test]
    fn a_frontmatter_title_edit_rebuilds_to_the_clean_snapshot() {
        let old_docs = [document("a.md", "Cache", "alpha cache body")];
        let old = built(&old_docs, None).snapshot;
        let new_docs = [document("a.md", "Cache v2", "alpha cache body")];
        let incremental = built(&new_docs, Some(&Previous::tokens(old)));
        let clean = built(&new_docs, None);

        assert!(incremental.stats.retokenized_chunks > 0);
        assert_same_as_clean(&incremental, &clean);
    }

    #[test]
    fn a_derived_title_rename_rebuilds_to_the_clean_snapshot() {
        let old_docs = [document("cache.md", "cache", "alpha cache body")];
        let old = built(&old_docs, None).snapshot;
        let new_docs = [document("renamed.md", "renamed", "alpha cache body")];
        let incremental = built(&new_docs, Some(&Previous::tokens(old)));
        let clean = built(&new_docs, None);

        assert_same_as_clean(&incremental, &clean);
    }

    #[test]
    fn identical_bodies_with_different_titles_keep_both_occurrences() {
        let docs = [
            document("a.md", "Alpha", "shared cache body"),
            document("b.md", "Beta", "shared cache body"),
        ];
        let clean = built(&docs, None);

        assert_eq!(clean.snapshot.docs.len(), 2);
        assert_eq!(clean.snapshot.chunks.len(), 2);
        assert_eq!(
            clean.snapshot.chunks[0].id, clean.snapshot.chunks[1].id,
            "ChunkId excludes the title, so both occurrences share it"
        );

        let cache = clean
            .snapshot
            .terms
            .iter()
            .enumerate()
            .find(|(index, _)| clean.snapshot.strings.get(StringId(*index as u32)) == Some("cache"))
            .map(|(_, term)| term)
            .expect("cache is in the index");
        assert_eq!(cache.df, 2);
        let postings =
            &clean.snapshot.postings[cache.postings.0 as usize..cache.postings.1 as usize];
        assert_eq!(
            postings
                .iter()
                .map(|posting| (posting.chunk, posting.tf))
                .collect::<Vec<_>>(),
            [(0, 1), (1, 1)]
        );
        // Each embed text is one title token plus three body tokens.
        assert!((clean.snapshot.avgdl - 4.0).abs() < 1e-12);
    }

    #[test]
    fn a_pure_move_reuses_every_chunk() {
        let old_docs = [document("a/cache.md", "Cache", "alpha cache body")];
        let old = built(&old_docs, None).snapshot;
        let moved_docs = [document("b/cache.md", "Cache", "alpha cache body")];
        let incremental = built(&moved_docs, Some(&Previous::tokens(old)));
        let clean = built(&moved_docs, None);

        assert_eq!(incremental.stats.retokenized_chunks, 0);
        assert_eq!(incremental.stats.reused_documents, 1);
        assert_eq!(incremental.stats.rebuilt_documents, 0);
        assert_same_as_clean(&incremental, &clean);
    }

    #[test]
    fn a_metadata_only_edit_reuses_tokens_and_vectors() {
        // The whole-file hash changes (a tag was added), the raw body does not.
        let hash = embedder(8);
        let mut old_doc = document("a.md", "Alpha", "alpha cache body");
        old_doc.content_hash = Blake3Hash::of_bytes(b"before the tag edit");
        let old = vector_plan(&[old_doc], None, &hash);
        let mut new_doc = document("a.md", "Alpha", "alpha cache body");
        new_doc.content_hash = Blake3Hash::of_bytes(b"after the tag edit");

        let incremental = vector_plan(&[new_doc], Some(&previous_of(&old)), &hash);
        assert_eq!(incremental.stats.retokenized_chunks, 0);
        assert_eq!(incremental.stats.reused_documents, 1);
        assert_eq!(incremental.stats.embedded_chunks, 0);
        assert_eq!(
            incremental.stats.reused_vector_chunks,
            old.snapshot.chunks.len()
        );
        assert_eq!(vector_bytes(&incremental), vector_bytes(&old));
    }

    #[test]
    fn a_body_edit_retokenizes_only_that_document() {
        let old_docs = [
            document("a.md", "Alpha", "alpha cache"),
            document("b.md", "Beta", "beta store"),
        ];
        let old = built(&old_docs, None).snapshot;
        let new_docs = [
            document("a.md", "Alpha", "alpha cache changed"),
            document("b.md", "Beta", "beta store"),
        ];
        let incremental = built(&new_docs, Some(&Previous::tokens(old)));
        let clean = built(&new_docs, None);

        assert_eq!(incremental.stats.retokenized_chunks, 1);
        assert_eq!(incremental.stats.reused_documents, 1);
        assert_eq!(incremental.stats.rebuilt_documents, 1);
        assert_same_as_clean(&incremental, &clean);
    }

    #[test]
    fn a_title_edit_reembeds_and_retokenizes_only_that_document() {
        let hash = embedder(8);
        let old_docs = [
            document("a.md", "Alpha", "alpha cache"),
            document("b.md", "Beta", "beta store"),
        ];
        let old = vector_plan(&old_docs, None, &hash);
        let new_docs = [
            document("a.md", "Alpha v2", "alpha cache"),
            document("b.md", "Beta", "beta store"),
        ];
        let incremental = vector_plan(&new_docs, Some(&previous_of(&old)), &hash);
        let clean = vector_plan(&new_docs, None, &hash);

        // One chunk per document at this size; only Alpha's chunk changed.
        assert_eq!(incremental.stats.embedded_chunks, 1);
        assert_eq!(incremental.stats.reused_vector_chunks, 1);
        assert_eq!(incremental.stats.retokenized_chunks, 1);
        assert_eq!(vector_bytes(&incremental), vector_bytes(&clean));
        assert_same_as_clean(&incremental, &clean);
    }

    #[test]
    fn a_body_edit_reembeds_only_that_document() {
        let hash = embedder(8);
        let old_docs = [
            document("a.md", "Alpha", "alpha cache"),
            document("b.md", "Beta", "beta store"),
        ];
        let old = vector_plan(&old_docs, None, &hash);
        let new_docs = [
            document("a.md", "Alpha", "alpha cache changed"),
            document("b.md", "Beta", "beta store"),
        ];
        let incremental = vector_plan(&new_docs, Some(&previous_of(&old)), &hash);
        let clean = vector_plan(&new_docs, None, &hash);

        assert_eq!(incremental.stats.embedded_chunks, 1);
        assert_eq!(incremental.stats.reused_vector_chunks, 1);
        assert_eq!(incremental.stats.retokenized_chunks, 1);
        assert_eq!(vector_bytes(&incremental), vector_bytes(&clean));
        assert_same_as_clean(&incremental, &clean);
    }

    #[test]
    fn a_declared_title_rename_publishes_identical_vectors() {
        let hash = embedder(8);
        let old = vector_plan(&[document("a.md", "Alpha", "alpha cache")], None, &hash);
        let new = vector_plan(
            &[document("renamed.md", "Alpha", "alpha cache")],
            Some(&previous_of(&old)),
            &hash,
        );
        let clean = vector_plan(
            &[document("renamed.md", "Alpha", "alpha cache")],
            None,
            &hash,
        );

        assert_eq!(new.stats.embedded_chunks, 0);
        assert_eq!(new.stats.retokenized_chunks, 0);
        assert_eq!(vector_bytes(&new), vector_bytes(&old));
        assert_eq!(vector_bytes(&new), vector_bytes(&clean));
    }

    #[test]
    fn a_declared_title_rename_reuses_every_row_even_when_order_changes() {
        let hash = embedder(8);
        let old_docs = [
            document("a.md", "Alpha", "alpha cache"),
            document("z.md", "Zeta", "zeta store"),
        ];
        let old = vector_plan(&old_docs, None, &hash);
        // The new paths put Zeta before Alpha, so row positions move while the
        // declared titles, and therefore the embedded texts, do not.
        let new_docs = [
            document("a.md", "Zeta", "zeta store"),
            document("zz.md", "Alpha", "alpha cache"),
        ];
        let incremental = vector_plan(&new_docs, Some(&previous_of(&old)), &hash);
        let clean = vector_plan(&new_docs, None, &hash);

        assert_eq!(incremental.stats.embedded_chunks, 0);
        assert_eq!(incremental.stats.retokenized_chunks, 0);
        assert_eq!(
            incremental.stats.reused_vector_chunks,
            incremental.snapshot.chunks.len()
        );
        assert_eq!(vector_bytes(&incremental), vector_bytes(&clean));
        assert_same_as_clean(&incremental, &clean);
    }

    #[test]
    fn a_derived_title_rename_reembeds_only_that_document() {
        let hash = embedder(8);
        let old_docs = [
            derived_document("cache.md", "cache", "alpha cache"),
            document("beta.md", "Beta", "beta store"),
        ];
        let old = vector_plan(&old_docs, None, &hash);
        let new_docs = [
            derived_document("renamed.md", "renamed", "alpha cache"),
            document("beta.md", "Beta", "beta store"),
        ];
        let incremental = vector_plan(&new_docs, Some(&previous_of(&old)), &hash);
        let clean = vector_plan(&new_docs, None, &hash);

        assert_eq!(incremental.stats.embedded_chunks, 1);
        assert_eq!(incremental.stats.reused_vector_chunks, 1);
        assert_eq!(incremental.stats.retokenized_chunks, 1);
        assert_eq!(vector_bytes(&incremental), vector_bytes(&clean));
        assert_same_as_clean(&incremental, &clean);
    }

    #[test]
    fn an_embedder_change_reembeds_every_row_and_reuses_every_token() {
        let old_hash = embedder(8);
        let new_hash = embedder(16);
        let docs = [
            document("a.md", "Alpha", "alpha cache"),
            document("b.md", "Beta", "beta store"),
        ];
        let old = vector_plan(&docs, None, &old_hash);
        let incremental = vector_plan(&docs, Some(&previous_of(&old)), &new_hash);
        let clean = vector_plan(&docs, None, &new_hash);

        assert_eq!(
            incremental.stats.embedded_chunks,
            incremental.snapshot.chunks.len()
        );
        assert_eq!(incremental.stats.reused_vector_chunks, 0);
        assert_eq!(incremental.stats.retokenized_chunks, 0);
        assert_eq!(incremental.vector_id.as_ref().map(|id| id.dim), Some(16));
        assert_eq!(vector_bytes(&incremental), vector_bytes(&clean));
    }

    #[test]
    fn identical_bodies_with_different_titles_get_two_different_rows() {
        let hash = embedder(8);
        let docs = [
            document("a.md", "Alpha", "shared cache body"),
            document("b.md", "Beta", "shared cache body"),
        ];
        let clean = vector_plan(&docs, None, &hash);
        let set = clean.vectors.as_ref().expect("rows");
        assert_eq!(set.rows().len(), 2);
        assert_ne!(
            set.rows().row(0),
            set.rows().row(1),
            "the title is in the embedded text, so the rows differ"
        );
    }

    #[test]
    fn an_eol_rewrite_reembeds_and_reuses_nothing() {
        let hash = embedder(8);
        let old_body = "line one\r\nline two  \r\n";
        let new_body = "line one\nline two\n";
        let old = vector_plan(&[document("a.md", "Alpha", old_body)], None, &hash);
        let new = vector_plan(
            &[document("a.md", "Alpha", new_body)],
            Some(&previous_of(&old)),
            &hash,
        );
        let clean = vector_plan(&[document("a.md", "Alpha", new_body)], None, &hash);

        assert_eq!(
            old.snapshot.chunks.iter().map(|c| c.id).collect::<Vec<_>>(),
            new.snapshot.chunks.iter().map(|c| c.id).collect::<Vec<_>>(),
            "normalised text keeps every ChunkId"
        );
        assert_eq!(new.stats.retokenized_chunks, new.snapshot.chunks.len());
        assert_eq!(new.stats.embedded_chunks, new.snapshot.chunks.len());
        assert_eq!(new.stats.reused_vector_chunks, 0);
        assert_eq!(vector_bytes(&new), vector_bytes(&clean));
    }

    #[test]
    fn a_section_reorder_keeps_row_i_on_chunk_i() {
        // Regression guard: `body_hash` alone already denies this reuse, so the
        // case is evidence for the invariant, not for the sequence conjunct.
        let hash = embedder(8);
        let old_body = "# A\n\nalpha text\n\n# B\n\nbeta text\n";
        let new_body = "# B\n\nbeta text\n\n# A\n\nalpha text\n";
        let old = vector_plan(&[document("a.md", "Alpha", old_body)], None, &hash);
        let new = vector_plan(
            &[document("a.md", "Alpha", new_body)],
            Some(&previous_of(&old)),
            &hash,
        );
        let clean = vector_plan(&[document("a.md", "Alpha", new_body)], None, &hash);

        assert!(new.snapshot.chunks.len() > 1);
        assert_eq!(new.stats.reused_vector_chunks, 0);
        assert_eq!(
            new.snapshot.chunks.iter().map(|c| c.id).collect::<Vec<_>>(),
            clean
                .snapshot
                .chunks
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>()
        );
        assert_eq!(vector_bytes(&new), vector_bytes(&clean));
    }

    #[test]
    fn a_dry_run_plans_the_embeds_without_calling_the_embedder() {
        let old = vector_plan(
            &[
                document("a.md", "Alpha", "alpha cache"),
                document("b.md", "Beta", "beta store"),
            ],
            None,
            &embedder(8),
        );
        let new_docs = [
            document("a.md", "Alpha", "alpha cache changed"),
            document("b.md", "Beta", "beta store"),
        ];
        let previous = previous_of(&old);
        let counting = CountingEmbedder::new(8);
        let dry = plan_forced_dry_run(
            &config(),
            &mut new_docs.to_vec(),
            Some(&previous),
            None,
            today(),
            &BTreeSet::new(),
            Some(&counting),
            &no_progress,
        )
        .expect("a dry run is only a plan");

        assert_eq!(dry.stats.embedded_chunks, 1);
        assert_eq!(dry.stats.reused_vector_chunks, 1);
        assert_eq!(dry.vector_id.as_ref().map(|id| id.dim), Some(8));
        assert!(dry.vectors.is_none(), "a dry run produces no rows");
        assert_eq!(counting.calls(), 0, "a dry run never calls embed");

        let real = plan_forced(
            &config(),
            &mut new_docs.to_vec(),
            Some(&previous),
            None,
            today(),
            &BTreeSet::new(),
            Some(&counting),
            &no_progress,
        )
        .expect("the hash double never fails");
        assert_eq!(counting.calls(), 1, "one batch holds the one changed chunk");
        assert_eq!(real.stats.embedded_chunks, 1);
    }

    #[test]
    fn a_tokenizer_fingerprint_bump_retokenizes_everything() {
        let docs = [
            document("a.md", "Alpha", "alpha cache"),
            document("b.md", "Beta", "beta store"),
        ];
        let mut old = built(&docs, None).snapshot;
        let chunks = old.chunks.len();
        old.tokenizer.algorithm = "v2";

        let incremental = built(&docs, Some(&Previous::tokens(old)));
        let clean = built(&docs, None);
        assert_eq!(incremental.stats.retokenized_chunks, chunks);
        assert_eq!(incremental.stats.reused_documents, 0);
        assert_eq!(incremental.stats.rebuilt_documents, docs.len());
        assert_same_as_clean(&incremental, &clean);
    }

    #[test]
    fn the_body_hash_is_the_raw_body_never_a_normalised_one() {
        // CRLF plus trailing spaces: `chunk_id` hashes the normalised text, so
        // a normalised witness would make an EOL-only rewrite look unchanged
        // (V-23, V-26).
        let body = "line one\r\nline two  \r\n";
        let docs = [document("a.md", "Alpha", body)];
        let computed = built(&docs, None);

        let stored = computed.snapshot.docs[0].body_hash;
        assert_eq!(stored, Blake3Hash::of_bytes(body.as_bytes()));
        assert_ne!(
            stored,
            Blake3Hash::of_bytes(b"line one\nline two"),
            "a normalised body must not produce the same witness"
        );
        // `content_hash` stays the whole-file fact, which a test document sets
        // to the body: the two agree here only because nothing else is hashed.
        assert_eq!(computed.snapshot.docs[0].content_hash, stored);
    }

    #[test]
    fn a_build_without_an_embedder_writes_no_vectors() {
        let docs = [document("a.md", "Alpha", "alpha cache")];
        let computed = built(&docs, None);

        assert!(computed.vectors.is_none());
        assert_eq!(computed.stats.embedded_chunks, 0);
        assert_eq!(computed.stats.reused_vector_chunks, 0);
    }

    #[test]
    fn an_embedder_gets_every_chunk_and_the_rows_align_with_it() {
        let embedder = crate::index::embed::HashEmbedder::new(8).expect("8 is valid");
        let docs = [
            document("a.md", "Alpha", "alpha cache body"),
            document("b.md", "Beta", "# Heading\n\nbeta store body"),
        ];
        let computed = plan(
            &config(),
            &mut docs.to_vec(),
            None,
            None,
            today(),
            Some(&embedder),
            &no_progress,
        )
        .expect("the double cannot fail");
        let chunks = computed.snapshot.chunks.len();
        assert_eq!(chunks, 2, "one chunk per document at this chunk size");
        assert_eq!(computed.stats.embedded_chunks, chunks);

        let rows = computed.vectors.as_ref().expect("vectors").rows();
        assert_eq!(rows.len(), chunks, "one row per chunk occurrence");
        assert_eq!(rows.dim(), 8);
        for index in 0..chunks {
            assert_eq!(rows.row(index).map(<[f32]>::len), Some(8));
        }
    }

    #[test]
    fn computing_a_plan_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docs = [document("a.md", "Alpha", "alpha cache")];
        let computed = built(&docs, None);

        assert!(computed.stats.retokenized_chunks > 0);
        assert!(!dir.path().join(".janus").exists());
        assert_eq!(
            std::fs::read_dir(dir.path()).expect("read dir").count(),
            0,
            "a plan is the --dry-run shape and performs no I/O"
        );
    }

    #[test]
    fn moving_the_vault_changes_only_built_root() {
        let first = tempfile::tempdir().expect("first root");
        let second = tempfile::tempdir().expect("second root");
        let docs = [document("a.md", "Alpha", "alpha cache")];
        let computed = built(&docs, None);
        let first_manifest = computed
            .manifest(first.path(), "2025-04-01T00:00:00Z", None, None)
            .expect("first manifest");
        let second_manifest = computed
            .manifest(second.path(), "2025-04-01T00:00:00Z", None, None)
            .expect("second manifest");

        assert_ne!(first_manifest.built_root, second_manifest.built_root);
        let mut adjusted = first_manifest.clone();
        adjusted.built_root = second_manifest.built_root.clone();
        assert_eq!(adjusted, second_manifest);
        assert_eq!(first_manifest.index_sha, second_manifest.index_sha);
        assert_eq!(first_manifest.docs, second_manifest.docs);
        assert_eq!(first_manifest.chunks, second_manifest.chunks);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        /// The invariant: after an incremental build, row *i* is the embedded
        /// text of snapshot chunk *i*, not a row borrowed from another position.
        #[test]
        fn every_incremental_row_matches_its_new_chunk(
            generated in prop::collection::vec(("[A-Za-z]{1,8}", "[a-z ]{1,60}"), 1..4),
            edit in 0usize..4,
            suffix in "[a-z]{1,8}",
        ) {
            let hash = embedder(8);
            let old_docs: Vec<Document> = generated
                .iter()
                .enumerate()
                .map(|(index, (title, body))| {
                    document(&format!("{index}.md"), title, body)
                })
                .collect();
            let old = vector_plan(&old_docs, None, &hash);

            let mut new_docs = old_docs.clone();
            let index = edit % new_docs.len();
            match edit % 3 {
                0 => new_docs[index].body.push_str(&suffix),
                1 => {
                    new_docs[index].title.push_str(&suffix);
                    new_docs[index].frontmatter.title = Some(new_docs[index].title.clone());
                }
                _ => {
                    new_docs[index].content_hash =
                        Blake3Hash::of_bytes(suffix.as_bytes());
                }
            }

            let incremental = vector_plan(&new_docs, Some(&previous_of(&old)), &hash);
            let clean = vector_plan(&new_docs, None, &hash);
            let rows = incremental.vectors.as_ref().expect("rows").rows();
            prop_assert_eq!(rows.len(), incremental.snapshot.chunks.len());
            prop_assert_eq!(vector_bytes(&incremental), vector_bytes(&clean));

            let mut ordered: Vec<&Document> = new_docs.iter().collect();
            ordered.sort_by(|a, b| a.rel_path.cmp(&b.rel_path).then_with(|| a.id.cmp(&b.id)));
            let mut expected_ids = Vec::new();
            let mut expected_rows = Vec::new();
            for doc in ordered {
                let (chunks, _) = chunk_body(doc.id, &doc.body, &config().chunking);
                for chunk in chunks {
                    expected_ids.push(chunk.id);
                    let text = chunk.embed_text(&doc.body, &doc.title);
                    let mut row = hash.embed(&[text.as_str()]).expect("the hash double never fails")[0].clone();
                    let _ = normalize(&mut row);
                    expected_rows.push(row);
                }
            }
            prop_assert_eq!(
                incremental.snapshot.chunks.iter().map(|chunk| chunk.id).collect::<Vec<_>>(),
                expected_ids
            );
            for (index, expected) in expected_rows.iter().enumerate() {
                prop_assert_eq!(rows.row(index), Some(expected.as_slice()));
            }
        }
    }

    #[test]
    fn a_move_with_a_new_title_and_date_gets_the_hand_derived_hint() {
        // Eight old chunks and ten new, all eight old ids reappearing:
        //   0.50 * 8/10 + 0.20 * 0 (titles differ) + 0.15 * 1 (stems equal)
        //   + 0.15 * 0 (no tags) = 0.55. The basis, not `Previous`, supplies
        // the evidence, which is the `--rebuild` shape.
        let old = dated(
            "notes/2024-01-01-plan.md",
            "Old plan",
            NaiveDate::from_ymd_opt(2024, 1, 1).expect("date"),
            &parts("shared", 8),
        );
        let first = built(std::slice::from_ref(&old), None);

        let new = dated(
            "notes/2025-02-02-plan.md",
            "New plan",
            NaiveDate::from_ymd_opt(2025, 2, 2).expect("date"),
            &parts("shared", 10),
        );
        let second = rebuilt_from(std::slice::from_ref(&new), &first.snapshot);

        let hint = second.snapshot.docs[0].hint.expect("one hint");
        assert_eq!(hint.id, first.snapshot.docs[0].id);
        assert!((hint.chunk_jaccard - 0.8).abs() < 1e-6, "{hint:?}");
        assert!((hint.title_match - 0.0).abs() < 1e-6, "{hint:?}");
        assert!((hint.basename_match - 1.0).abs() < 1e-6, "{hint:?}");
        assert!((hint.tag_jaccard - 0.0).abs() < 1e-6, "{hint:?}");
        assert!((hint.confidence - 0.55).abs() < 1e-6, "{hint:?}");

        // A second rebuild reads the hint's own generation. The candidate has
        // no `DocMeta` row left, so the recorded pair must survive.
        let third = rebuilt_from(&[new], &second.snapshot);
        let carried = third.snapshot.docs[0].hint.expect("the hint survived");
        assert_eq!(carried, hint);
    }

    #[test]
    fn a_move_below_the_floor_stores_no_hint() {
        // Ten old chunks and two new, both known: 0.50 * 2/10 + 0.15 = 0.25.
        let old = dated(
            "notes/2024-01-01-plan.md",
            "Old plan",
            NaiveDate::from_ymd_opt(2024, 1, 1).expect("date"),
            &parts("shared", 10),
        );
        let first = built(&[old], None);

        let new = dated(
            "notes/2025-02-02-plan.md",
            "New plan",
            NaiveDate::from_ymd_opt(2025, 2, 2).expect("date"),
            &parts("shared", 2),
        );
        let second = rebuilt_from(&[new], &first.snapshot);
        assert!(second.snapshot.docs[0].hint.is_none());
    }
}
