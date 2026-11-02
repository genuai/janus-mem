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

use crate::chunk::{Chunk, chunk_body};
use crate::config::Config;
use crate::document::Document;
use crate::id::{Blake3Hash, ChunkId, DocId};
use crate::tokenize::{Stemmer, Tokenizer, TokenizerFingerprint, Tokens};

use super::embed::Embedder;
use super::store::{Corrupt, Manifest, Payloads, VectorSet};
use super::vectors::{Vectors, normalize};
use super::{
    ChunkMeta, ChunkingFingerprint, DocMeta, LexicalIndex, Posting, Snapshot, StringId, StringPool,
    TermMeta,
};

/// What one build did, for the phase-6 receipt and the §3.4 tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuildStats {
    /// Chunks that actually went through the tokenizer.
    pub retokenized_chunks: usize,
    /// Documents whose tokens came from the previous snapshot.
    pub reused_documents: usize,
    /// Documents whose chunks were tokenized again.
    pub rebuilt_documents: usize,
    /// Chunks that actually went through `Embedder::embed`.
    pub embedded_chunks: usize,
    /// Rows copied from the previous generation. Always zero in P1b: the reuse
    /// predicate and the carry-forward belong to P4.
    pub reused_vector_chunks: usize,
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
    /// The rows to publish beside the snapshot, or `None` when no embedder was
    /// resolved (§3.1).
    pub vectors: Option<VectorSet>,
}

impl BuildPlan {
    /// Builds the manifest without writing anything.
    ///
    /// # Errors
    ///
    /// [`Corrupt`] when the snapshot cannot be encoded.
    pub fn manifest(&self, root: &Path, built_at: &str) -> Result<Manifest, Corrupt> {
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
        ))
    }
}

/// Computes the next snapshot, reusing tokens where §3.4 allows.
///
/// An invalid `previous` is treated as absent; computing a plan performs no I/O.
/// Every chunk is embedded on every build (§5 P1b item 7): reuse, `body_hash`
/// and the carry-forward arrive in P4.
///
/// # Errors
///
/// [`Corrupt`] when the embedder breaks its contract (wrong row count, wrong
/// width, a non-finite value). The build publishes nothing in that case: a
/// substituted zero row would be invisible quality loss (§3.2 V-7).
pub fn plan(
    config: &Config,
    docs: &[Document],
    previous: Option<&Snapshot>,
    embedder: Option<&dyn Embedder>,
) -> Result<BuildPlan, Corrupt> {
    plan_forced(config, docs, previous, &BTreeSet::new(), embedder)
}

/// [`plan`] with a set of documents whose tokens must be recomputed anyway.
///
/// `janus index PATHS` uses this for a targeted refresh: reuse still applies to
/// every other document, so the index stays complete and the save is real.
///
/// # Errors
///
/// Exactly [`plan`]'s.
pub fn plan_forced(
    config: &Config,
    docs: &[Document],
    previous: Option<&Snapshot>,
    force: &BTreeSet<DocId>,
    embedder: Option<&dyn Embedder>,
) -> Result<BuildPlan, Corrupt> {
    let tokenizer = Tokenizer::new();
    let tokenizer_fp = tokenizer.fingerprint();
    let chunker = ChunkingFingerprint {
        max_chars: config.chunking.max_chars,
        overlap: config.chunking.overlap,
    };

    let previous = previous.filter(|snapshot| snapshot.validate().is_ok());
    let old_docs: BTreeMap<DocId, usize> = previous
        .map(|snapshot| {
            snapshot
                .docs
                .iter()
                .enumerate()
                .map(|(index, doc)| (doc.id, index))
                .collect()
        })
        .unwrap_or_default();
    let old_forward = previous.map(forward_index).unwrap_or_default();

    let mut ordered: Vec<&Document> = docs.iter().collect();
    ordered.sort_by(|a, b| a.rel_path.cmp(&b.rel_path).then_with(|| a.id.cmp(&b.id)));

    let mut stats = BuildStats::default();
    let mut tokens = Tokens::default();
    let mut drafts = Vec::with_capacity(ordered.len());
    // Every chunk's embedded text, in snapshot order: row `i` of `vectors.f32`
    // belongs to chunk `i`, so this sequence is the payload's key. P4 replaces
    // the unconditional walk with the §3.4 reuse predicate.
    let mut embed_texts = Vec::new();

    for doc in ordered {
        let (chunks, _notes) = chunk_body(doc.id, &doc.body, &config.chunking);
        let key = lex_key(&tokenizer_fp, &chunker, &doc.title);
        let old_match = previous.and_then(|snapshot| {
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

        let old_range = old_match.map(|(_, old_doc)| old_doc.chunk_range);
        let mut chunk_drafts = Vec::with_capacity(chunks.len());
        for (ordinal, chunk) in chunks.iter().enumerate() {
            let text = chunk.embed_text(&doc.body, &doc.title);
            let (terms, length) = if reused {
                let index = old_range
                    .and_then(|(start, _)| start.checked_add(u32::try_from(ordinal).ok()?));
                let pairs = index
                    .and_then(|index| old_forward.get(index as usize))
                    .map(|pairs| old_terms(previous, pairs))
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
            embed_texts.push(text);
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
            effective_from: doc.effective_from,
            timeless: doc.type_spec.timeless,
            content_hash: doc.content_hash,
            // Raw body bytes: a normalised body would fold line endings and
            // reopen the hole V-23 closes (V-26).
            body_hash: Blake3Hash::of_bytes(doc.body.as_bytes()),
            lex_key: draft.lex_key,
            chunk_range: (start, end),
        });
    }

    let index = LexicalIndex::from_parts(pool, terms, postings, lengths, avgdl);
    let snapshot = Snapshot::from_index(
        &index,
        config.corpus_id.clone(),
        tokenizer_fp,
        docs_meta,
        chunks_meta,
    );
    let vectors = match embedder {
        Some(embedder) => {
            let set = embed_all(embedder, &embed_texts, config.embedding.batch_size)?;
            stats.embedded_chunks = embed_texts.len();
            Some(set)
        }
        None => None,
    };
    Ok(BuildPlan {
        snapshot,
        stats,
        chunker,
        vectors,
    })
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
) -> Result<VectorSet, Corrupt> {
    let id = embedder.id();
    let dim = id.dim as usize;
    let mut flat: Vec<f32> = Vec::with_capacity(texts.len().saturating_mul(dim));
    let step = batch_size.max(1);

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
        }
    }

    let rows = Vectors::new(id.dim, flat)?;
    VectorSet::new(id.clone(), rows)
}

/// One document being assembled, before term ids and the string pool exist.
struct DocDraft<'a> {
    doc: &'a Document,
    lex_key: [u8; 32],
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

/// The five §3.4 conjuncts. Any false result means “rebuild this document”.
fn reusable(
    previous: &Snapshot,
    old_doc: &DocMeta,
    old_forward: &[Vec<(u32, u32)>],
    doc: &Document,
    chunks: &[Chunk],
    tokenizer: &TokenizerFingerprint,
    chunker: &ChunkingFingerprint,
) -> bool {
    if old_doc.lex_key != lex_key(tokenizer, chunker, &doc.title)
        || old_doc.content_hash != doc.content_hash
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
    use chrono::NaiveDate;
    use std::path::PathBuf;
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

    /// A plan built with no embedder: the token-side cases do not embed.
    fn built(docs: &[Document], previous: Option<&Snapshot>) -> BuildPlan {
        plan(&config(), docs, previous, None).expect("a build without an embedder cannot fail")
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
        let incremental = built(&new_docs, Some(&old));
        let clean = built(&new_docs, None);

        assert!(incremental.stats.retokenized_chunks > 0);
        assert_same_as_clean(&incremental, &clean);
    }

    #[test]
    fn a_derived_title_rename_rebuilds_to_the_clean_snapshot() {
        let old_docs = [document("cache.md", "cache", "alpha cache body")];
        let old = built(&old_docs, None).snapshot;
        let new_docs = [document("renamed.md", "renamed", "alpha cache body")];
        let incremental = built(&new_docs, Some(&old));
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
        let incremental = built(&moved_docs, Some(&old));
        let clean = built(&moved_docs, None);

        assert_eq!(incremental.stats.retokenized_chunks, 0);
        assert_eq!(incremental.stats.reused_documents, 1);
        assert_eq!(incremental.stats.rebuilt_documents, 0);
        assert_same_as_clean(&incremental, &clean);
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
        let incremental = built(&new_docs, Some(&old));
        let clean = built(&new_docs, None);

        assert_eq!(incremental.stats.retokenized_chunks, 1);
        assert_eq!(incremental.stats.reused_documents, 1);
        assert_eq!(incremental.stats.rebuilt_documents, 1);
        assert_same_as_clean(&incremental, &clean);
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

        let incremental = built(&docs, Some(&old));
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
        let computed =
            plan(&config(), &docs, None, Some(&embedder)).expect("the double cannot fail");
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
            .manifest(first.path(), "2025-04-01T00:00:00Z")
            .expect("first manifest");
        let second_manifest = computed
            .manifest(second.path(), "2025-04-01T00:00:00Z")
            .expect("second manifest");

        assert_ne!(first_manifest.built_root, second_manifest.built_root);
        let mut adjusted = first_manifest.clone();
        adjusted.built_root = second_manifest.built_root.clone();
        assert_eq!(adjusted, second_manifest);
        assert_eq!(first_manifest.index_sha, second_manifest.index_sha);
        assert_eq!(first_manifest.docs, second_manifest.docs);
        assert_eq!(first_manifest.chunks, second_manifest.chunks);
    }
}
