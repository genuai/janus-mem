//! BM25 over the flat inverted index (S-3, S-4, §3.3).
//!
//! The score is the textbook Robertson/Sparck Jones BM25 with the
//! always-positive idf:
//!
//! ```text
//! bm25(t, c) = idf(t) * tf * (k1 + 1) / (tf + k1 * (1 - b + b * dl / avgdl))
//! idf(t)     = ln(1 + (N - df(t) + 0.5) / (df(t) + 0.5))
//! s          = sum over query terms of bm25(t, c)
//! s_ref      = sum over query terms of idf(t)
//! lexical    = s / (s + s_ref)
//! ```
//!
//! `s_ref` is the score of a hypothetical chunk that matches every query term
//! once at average length. Dividing by it makes [`squash`] scale-free: the same
//! `(document, query)` pair maps to the same `lexical` value in a 40-document
//! and a 20 000-document corpus. A fixed midpoint cannot do that, because `idf`
//! grows like `ln N` (§3.3).
//!
//! Units are `f64` throughout. All arithmetic is pure: a build tokenizes with
//! the phase-1 [`Tokenizer`], a query tokenizes nothing.

use std::collections::BTreeMap;

use crate::tokenize::{Tokenizer, Tokens};

use super::{Posting, StringPool, TermMeta};

/// BM25 term-frequency saturation. §3.3 fixes this at 1.2.
const K1: f64 = 1.2;

/// BM25 length normalization. §3.3 fixes this at 0.75.
const B: f64 = 0.75;

/// An in-memory lexical index over chunks (§3.3, S-3, S-4).
///
/// Build it with [`LexicalIndex::build`] from each chunk's indexed text, which
/// is [`Chunk::embed_text`](crate::chunk::Chunk::embed_text) rather than the
/// chunk body: the title and breadcrumb must be visible to the same tokenizer
/// as the query.
///
/// The value is a shaped cache: terms are interned, `df` and the posting ranges
/// are fixed at build time, and `avgdl` is computed once. Querying never
/// mutates it and never tokenizes.
#[derive(Debug, Clone)]
pub struct LexicalIndex {
    /// Term texts and their ids. Ids are dense; `terms[id]` describes term `id`.
    pool: StringPool,
    /// Per-term `df` and posting range, indexed by term id.
    terms: Vec<TermMeta>,
    /// All postings, grouped by term id and ascending by chunk within a term.
    postings: Vec<Posting>,
    /// Token count per chunk (BM25's `dl`), in chunk order.
    lengths: Vec<u32>,
    /// Mean token count over all chunks. Zero for an empty index.
    avgdl: f64,
}

impl LexicalIndex {
    /// Tokenizes every chunk's indexed text and builds the inverted index.
    ///
    /// Each element of `texts` is one chunk, in the order callers want scores
    /// back; pass [`Chunk::embed_text`](crate::chunk::Chunk::embed_text) so the
    /// title and breadcrumb are indexed alongside the body (S-3).
    ///
    /// Determinism: term ids follow first appearance, postings follow chunk
    /// order within a term, and terms are flattened in id order. No `HashMap`
    /// iteration reaches the result.
    ///
    /// The conversion failures below are unreachable for any corpus that fits
    /// in memory (more than four billion chunks, postings, or one chunk's
    /// tokens). They stop the build rather than wrap an index; a truncated index
    /// is still internally consistent.
    #[must_use]
    pub fn build(texts: &[&str]) -> Self {
        let tokenizer = Tokenizer::new();
        let mut tokens = Tokens::default();
        let mut pool = StringPool::new();

        // One bucket per term id, filled in chunk order. `Vec` rather than a map
        // so the flattening step below cannot depend on iteration order.
        let mut buckets: Vec<Vec<Posting>> = Vec::new();
        let mut lengths: Vec<u32> = Vec::with_capacity(texts.len());

        for (index, text) in texts.iter().enumerate() {
            let Ok(chunk) = u32::try_from(index) else {
                break;
            };

            tokenizer.tokenize_into(text, &mut tokens);
            lengths.push(u32::try_from(tokens.len()).unwrap_or(u32::MAX));

            // Term frequencies for this chunk. `BTreeMap` by term id gives the
            // stable order, even though a term appears at most once per bucket
            // after the flattening step.
            let mut counts: BTreeMap<u32, u32> = BTreeMap::new();
            for term in tokens.iter() {
                let id = pool.intern(term);
                let count = counts.entry(id.as_u32()).or_insert(0);
                *count = count.saturating_add(1);
            }

            // Interning happened while scanning this chunk, so new terms need a
            // bucket before their posting can be pushed.
            buckets.resize(pool.len(), Vec::new());
            for (term, tf) in counts {
                if let Some(bucket) = buckets.get_mut(term as usize) {
                    bucket.push(Posting { chunk, tf });
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

        let total: u64 = lengths.iter().map(|&dl| u64::from(dl)).sum();
        let avgdl = if lengths.is_empty() {
            0.0
        } else {
            total as f64 / lengths.len() as f64
        };

        Self {
            pool,
            terms,
            postings,
            lengths,
            avgdl,
        }
    }

    /// Number of chunks in the index, which is also `N` in the idf formula.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lengths.len()
    }

    /// True when the index has no chunks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lengths.is_empty()
    }

    /// Mean chunk length in tokens, computed once at build.
    #[must_use]
    pub fn avgdl(&self) -> f64 {
        self.avgdl
    }

    /// Per-term metadata, indexed by [`StringId`](super::StringId).
    #[must_use]
    pub fn terms(&self) -> &[TermMeta] {
        &self.terms
    }

    /// The flat posting list, in term-id order.
    #[must_use]
    pub fn postings(&self) -> &[Posting] {
        &self.postings
    }

    /// The always-positive idf for a term with document frequency `df`.
    ///
    /// The form is `ln(1 + (N - df + 0.5) / (df + 0.5))`, so `df == N` is small
    /// but positive rather than zero or negative. `df` above the chunk count is
    /// clamped: a built index cannot produce one, and clamping keeps the public
    /// accessor total instead of returning `NaN` for an impossible input.
    #[must_use]
    pub fn idf(&self, df: u32) -> f64 {
        let n = self.lengths.len() as f64;
        let df = f64::from(df.min(u32::try_from(self.lengths.len()).unwrap_or(u32::MAX)));
        (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
    }

    /// Raw BM25 for every chunk, in build order, plus the query's `s_ref`.
    ///
    /// `terms` are already-tokenized query terms, normally collected from
    /// [`Tokenizer::tokenize`](crate::tokenize::Tokenizer::tokenize); this
    /// method does not fold, split, or stem them, because the query must pass
    /// through exactly the same tokenizer as the indexed text.
    ///
    /// A query term that is not in the index contributes nothing to any score
    /// **and nothing to `s_ref`**: it cannot be part of the reference match, so
    /// `s_ref` stays the sum of the idf values that were actually looked up. If
    /// no query term exists, every score is `0.0` and `s_ref` is `0.0`.
    ///
    /// A repeated query term contributes its `bm25` again, so repetition is
    /// linear; document `tf` is not, because of the saturation denominator.
    #[must_use]
    pub fn score_query(&self, terms: &[&str]) -> (Vec<f64>, f64) {
        let mut scores = vec![0.0; self.lengths.len()];
        let mut s_ref = 0.0;

        for term in terms {
            let Some(id) = self.pool.lookup(term) else {
                continue;
            };
            let Some(meta) = self.terms.get(id.as_usize()) else {
                continue;
            };

            let idf = self.idf(meta.df);
            s_ref += idf;

            let start = meta.postings.0 as usize;
            let end = meta.postings.1 as usize;
            let Some(term_postings) = self.postings.get(start..end) else {
                continue;
            };

            for posting in term_postings {
                let Some(&dl) = self.lengths.get(posting.chunk as usize) else {
                    continue;
                };
                let tf = f64::from(posting.tf);
                let denominator = tf + K1 * (1.0 - B + B * f64::from(dl) / self.avgdl);
                let Some(score) = scores.get_mut(posting.chunk as usize) else {
                    continue;
                };
                *score += idf * tf * (K1 + 1.0) / denominator;
            }
        }

        (scores, s_ref)
    }
}

/// Squashes a raw BM25 score into `[0, 1)` relative to the query's `s_ref`.
///
/// `squash(s, s_ref) = s / (s + s_ref)` for positive `s` and `s_ref`, and `0.0`
/// otherwise. The short-circuit makes `squash(0, 0) == 0` and keeps the function
/// total for absent terms; it also maps `NaN` inputs to `0.0` rather than
/// propagating them. For a real match `s_ref > 0`, so the result is
/// `logistic(ln(s / s_ref))` and `0.5` means “exactly the reference match”.
#[must_use]
pub fn squash(score: f64, reference: f64) -> f64 {
    if score > 0.0 && reference > 0.0 {
        score / (score + reference)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::Chunk;
    use crate::id::{ChunkId, DocId};
    use proptest::prelude::*;

    /// `n` identical chunks, for tests that only need a fixed `N`.
    fn index_with_n_chunks(n: usize) -> LexicalIndex {
        let texts = vec!["filler"; n];
        LexicalIndex::build(&texts)
    }

    #[test]
    fn idf_matches_independent_literals() {
        // Values derived from ln(1 + (N - df + 0.5) / (df + 0.5)) by hand.
        let cases: &[(usize, u32, f64)] = &[
            (3, 1, 0.980_829),
            (3, 2, 0.470_004),
            (3, 3, 0.133_531),
            (40, 1, 3.308_107),
            (40, 10, 1.362_197),
        ];

        for &(n, df, expected) in cases {
            let index = index_with_n_chunks(n);
            let actual = index.idf(df);
            assert!(
                (actual - expected).abs() < 1e-6,
                "N={n} df={df}: expected {expected}, got {actual}"
            );
            assert!(actual > 0.0, "N={n} df={df}: idf must stay positive");
        }
    }

    #[test]
    fn document_tf_saturates_toward_k1_plus_one() {
        // One chunk of length 10, so N, df, avgdl and dl are fixed at every
        // step and only tf changes. Filler words replace cache occurrences to
        // keep dl == avgdl == 10. The ratio is tf * 2.2 / (tf + 1.2):
        // 1.0, 1.5714285714..., 1.9642857142...
        let cases: &[(usize, f64)] = &[
            (1, 1.0),
            (3, 1.571_428_571_428_571_4),
            (10, 1.964_285_714_285_714_2),
        ];

        let mut previous = 0.0;
        for &(tf, expected) in cases {
            let mut text = "cache ".repeat(tf);
            for filler in 0..(10 - tf) {
                text.push_str(&format!("f{tf}_{filler} "));
            }
            let index = LexicalIndex::build(&[text.as_str()]);
            assert!((index.avgdl() - 10.0).abs() < 1e-12);

            let (scores, _) = index.score_query(&["cache"]);
            let ratio = scores[0] / index.idf(1);
            assert!(
                (ratio - expected).abs() < 1e-9,
                "tf={tf}: expected {expected}, got {ratio}"
            );
            assert!(ratio > previous, "tf={tf}: must strictly increase");
            assert!(ratio < K1 + 1.0, "tf={tf}: must stay below k1 + 1");
            previous = ratio;
        }
    }

    #[test]
    fn query_term_repetition_is_linear_unlike_tf() {
        let index = LexicalIndex::build(&["cache"]);
        let (single, single_ref) = index.score_query(&["cache"]);
        let (double, double_ref) = index.score_query(&["cache", "cache"]);

        // Each query occurrence contributes bm25(t, c) again, with no saturation.
        assert!((double[0] - 2.0 * single[0]).abs() < 1e-12);
        assert!((double_ref - 2.0 * single_ref).abs() < 1e-12);
    }

    #[test]
    fn longer_average_document_raises_this_normalized_score() {
        // Chunk 0 is always 10 tokens and is the only chunk containing "cache",
        // so df=1 in both indexes. Chunk 1 is 10 or 30 tokens, making avgdl
        // 10 or 20.
        let target = "cache f0 f1 f2 f3 f4 f5 f6 f7 f8";
        let ten = "g0 g1 g2 g3 g4 g5 g6 g7 g8 g9";
        let thirty = "g0 g1 g2 g3 g4 g5 g6 g7 g8 g9 g10 g11 g12 g13 g14 g15 g16 g17 g18 g19 g20 g21 g22 g23 g24 g25 g26 g27 g28 g29";

        let avgdl_10 = LexicalIndex::build(&[target, ten]);
        let avgdl_20 = LexicalIndex::build(&[target, thirty]);
        assert!((avgdl_10.avgdl() - 10.0).abs() < 1e-12);
        assert!((avgdl_20.avgdl() - 20.0).abs() < 1e-12);

        let (scores_10, _) = avgdl_10.score_query(&["cache"]);
        let (scores_20, _) = avgdl_20.score_query(&["cache"]);
        let ratio_10 = scores_10[0] / avgdl_10.idf(1);
        let ratio_20 = scores_20[0] / avgdl_20.idf(1);

        // 1.0 at dl/avgdl=1; 2.2 / (1 + 1.2*(0.25 + 0.75*0.5)) = 1.257142857...
        assert!((ratio_10 - 1.0).abs() < 1e-9);
        assert!((ratio_20 - 1.257_142_857_142_857).abs() < 1e-9);
        assert!(ratio_20 > ratio_10);
    }

    #[test]
    fn longer_document_is_penalized() {
        // Both indexes have two chunks and 20 tokens total, so avgdl is 10 and
        // df("cache") is 1. Only chunk 0's length changes: 10 versus 20.
        let short = "cache f0 f1 f2 f3 f4 f5 f6 f7 f8";
        let long = "cache f0 f1 f2 f3 f4 f5 f6 f7 f8 f9 f10 f11 f12 f13 f14 f15 f16 f17 f18";
        let short_index = LexicalIndex::build(&[short, "g0 g1 g2 g3 g4 g5 g6 g7 g8 g9"]);
        let long_index = LexicalIndex::build(&[long, ""]);
        assert!((short_index.avgdl() - 10.0).abs() < 1e-12);
        assert!((long_index.avgdl() - 10.0).abs() < 1e-12);

        let (short_scores, _) = short_index.score_query(&["cache"]);
        let (long_scores, _) = long_index.score_query(&["cache"]);
        let short_ratio = short_scores[0] / short_index.idf(1);
        let long_ratio = long_scores[0] / long_index.idf(1);

        // 2.2 / (1 + 1.2*(0.25 + 0.75*2)) = 2.2 / 3.1.
        assert!((short_ratio - 1.0).abs() < 1e-9);
        assert!((long_ratio - 0.709_677_419_354_838_7).abs() < 1e-9);
        assert!(long_ratio < short_ratio);
    }

    #[test]
    fn squash_table_matches_literals() {
        // lexical = r / (1 + r), with s_ref = 1 so r = s.
        let cases: &[(f64, f64)] = &[
            (0.25, 0.2),
            (0.5, 0.333_333_333_333_333_3),
            (1.0, 0.5),
            (1.5, 0.6),
            (2.0, 0.666_666_666_666_666_6),
            (3.0, 0.75),
            (5.0, 0.833_333_333_333_333_4),
            (10.0, 0.909_090_909_090_909_1),
        ];

        for &(ratio, expected) in cases {
            let actual = squash(ratio, 1.0);
            assert!(
                (actual - expected).abs() < 1e-9,
                "r={ratio}: expected {expected}, got {actual}"
            );
        }

        // The short-circuit: zero raw score is zero, even with no reference.
        assert!((squash(0.0, 0.0) - 0.0).abs() < 1e-12);
        assert!((squash(0.0, 5.0) - 0.0).abs() < 1e-12);
        assert!((squash(f64::NAN, 1.0) - 0.0).abs() < 1e-12);
    }

    #[test]
    fn three_document_fixture_matches_hand_arithmetic() {
        let texts = ["cache cache store", "cache index", "store index disk"];
        let index = LexicalIndex::build(&texts);

        assert_eq!(index.len(), 3);
        // (3 + 2 + 3) / 3 = 8/3.
        assert!((index.avgdl() - 8.0 / 3.0).abs() < 1e-12);

        let cache = index.pool.lookup("cache").unwrap().as_usize();
        let store = index.pool.lookup("store").unwrap().as_usize();
        let idx = index.pool.lookup("index").unwrap().as_usize();
        let disk = index.pool.lookup("disk").unwrap().as_usize();

        assert_eq!(index.terms[cache].df, 2);
        assert_eq!(index.terms[store].df, 2);
        assert_eq!(index.terms[idx].df, 2);
        assert_eq!(index.terms[disk].df, 1);

        let range = |term: usize| {
            let (start, end) = index.terms[term].postings;
            (start as usize, end as usize)
        };
        let (start, end) = range(cache);
        assert_eq!(
            &index.postings[start..end],
            &[Posting { chunk: 0, tf: 2 }, Posting { chunk: 1, tf: 1 }]
        );
        let (start, end) = range(store);
        assert_eq!(
            &index.postings[start..end],
            &[Posting { chunk: 0, tf: 1 }, Posting { chunk: 2, tf: 1 }]
        );
        let (start, end) = range(disk);
        assert_eq!(&index.postings[start..end], &[Posting { chunk: 2, tf: 1 }]);

        // Query "cache store": df=2 for both, N=3, so
        // idf = ln(1 + 1.5/2.5) = ln(1.6) = 0.47000362924573563.
        // avgdl = 8/3; chunk 0 dl=3, so dl/avgdl = 9/8.
        // cache: 2 + 1.2*(0.25 + 0.75*9/8) = 3.3125, so
        //   0.47000362924573563 * 2 * 2.2 / 3.3125 = 0.6243067075264112.
        // store: 1 + 1.2*(0.25 + 0.75*9/8) = 2.3125, so
        //   0.47000362924573563 * 2.2 / 2.3125 = 0.44713858782297017.
        let (scores, s_ref) = index.score_query(&["cache", "store"]);
        assert!((scores[0] - 1.071_445_295_349_381_4).abs() < 1e-9);
        assert!((scores[1] - 0.523_548_346_501_579).abs() < 1e-9);
        assert!((scores[2] - 0.447_138_587_822_970_17).abs() < 1e-9);
        assert!((s_ref - 0.940_007_258_491_471_3).abs() < 1e-9);
        assert!((squash(scores[0], s_ref) - 0.532_672_417_901_911_3).abs() < 1e-9);
    }

    #[test]
    fn empty_query_and_empty_index_are_total() {
        let index = LexicalIndex::build(&["cache store"]);
        let (scores, s_ref) = index.score_query(&[]);
        assert_eq!(scores.len(), 1);
        assert!((scores[0] - 0.0).abs() < 1e-12);
        assert!((s_ref - 0.0).abs() < 1e-12);

        let empty = LexicalIndex::build(&[]);
        assert!(empty.is_empty());
        assert!((empty.avgdl() - 0.0).abs() < 1e-12);
        let (scores, s_ref) = empty.score_query(&["cache"]);
        assert!(scores.is_empty());
        assert!((s_ref - 0.0).abs() < 1e-12);
    }

    #[test]
    fn a_term_in_every_document_stays_positive() {
        let index = LexicalIndex::build(&["cache", "cache", "cache"]);
        let idf = index.idf(3);
        assert!((idf - 0.133_531).abs() < 1e-6);
        assert!(idf > 0.0);

        let (scores, s_ref) = index.score_query(&["cache"]);
        assert!((s_ref - idf).abs() < 1e-12);
        for score in scores {
            assert!(score > 0.0);
        }
    }

    #[test]
    fn prefix_terms_do_not_substring_match() {
        let index = LexicalIndex::build(&["caching", "cache"]);
        let (absent, absent_ref) = index.score_query(&["cach"]);
        assert!((absent[0] - 0.0).abs() < 1e-12);
        assert!((absent[1] - 0.0).abs() < 1e-12);
        assert!((absent_ref - 0.0).abs() < 1e-12);

        let (exact, _) = index.score_query(&["cache"]);
        assert!((exact[0] - 0.0).abs() < 1e-12);
        assert!(exact[1] > 0.0);
    }

    #[test]
    fn absent_query_term_is_skipped_entirely() {
        let index = LexicalIndex::build(&["cache store"]);
        let (without, ref_without) = index.score_query(&["cache"]);
        let (with, ref_with) = index.score_query(&["cache", "missing"]);

        assert!((with[0] - without[0]).abs() < 1e-12);
        assert!((ref_with - ref_without).abs() < 1e-12);
    }

    #[test]
    fn a_chunk_with_no_tokens_scores_zero() {
        let index = LexicalIndex::build(&["cache", ""]);
        let (scores, _) = index.score_query(&["cache"]);
        assert!(scores[0] > 0.0);
        // tf = 0 for chunk 1: a constant zero, not a very small score.
        assert!((scores[1] - 0.0).abs() < 1e-12);
    }

    #[test]
    fn indexed_text_ranks_its_own_chunk_first() {
        let docs = [
            (
                "Cache eviction",
                "The cache evicts the least recently used entry when capacity is reached.",
            ),
            (
                "Write locking",
                "A writer holds an exclusive lock while publishing the snapshot.",
            ),
            (
                "Tokenizer rules",
                "Terms are lowercased and punctuation separates them.",
            ),
        ];

        let chunks: Vec<Chunk> = docs
            .iter()
            .enumerate()
            .map(|(i, (_, body))| Chunk {
                id: ChunkId::from_bytes([i as u8; 32]),
                doc_id: DocId::from_bytes([i as u8; 16]),
                heading_path: Vec::new(),
                span: (0, u32::try_from(body.len()).unwrap()),
                ordinal: 0,
            })
            .collect();
        let indexed: Vec<String> = chunks
            .iter()
            .zip(docs.iter())
            .map(|(chunk, (title, body))| chunk.embed_text(body, title))
            .collect();
        let texts: Vec<&str> = indexed.iter().map(String::as_str).collect();
        let index = LexicalIndex::build(&texts);

        // The query is exactly chunk 0's indexed text, tokenized by the same
        // phase-1 Tokenizer the build used (T-5, now through the index).
        let tokenizer = Tokenizer::new();
        let query = tokenizer.tokenize(&indexed[0]);
        let terms: Vec<&str> = query.iter().collect();
        let (scores, s_ref) = index.score_query(&terms);

        assert!(s_ref > 0.0);
        assert_eq!(scores.len(), 3);
        assert!(
            scores[0] > scores[1] && scores[0] > scores[2],
            "query text must rank its own chunk first: {scores:?}"
        );
    }

    /// Builds `n` chunks totalling `total` tokens. Chunk 0 has `dl` tokens,
    /// `tf` of them `"cache"`; chunks 1..df each hold one `"cache"` so the
    /// document frequency is exactly `df`; the remaining tokens are unique
    /// fillers so only `"cache"` can match the query.
    fn length_fixture(n: usize, df: usize, dl: u32, total: u32, tf: u32) -> LexicalIndex {
        assert!(n >= 1 && df >= 1 && df <= n);
        assert!(tf >= 1 && tf <= dl);
        assert!(total >= dl);
        assert!(total - dl >= u32::try_from(df - 1).unwrap());

        let mut filler = 0u32;
        let mut text = |cache: u32, len: u32| {
            let mut out = String::new();
            for _ in 0..cache {
                out.push_str("cache ");
            }
            for _ in 0..(len - cache) {
                out.push_str(&format!("f{filler} "));
                filler += 1;
            }
            out
        };

        let mut texts = Vec::with_capacity(n);
        texts.push(text(tf, dl));
        if n > 1 {
            if df == 1 {
                texts.push(text(0, total - dl));
            } else {
                let extra = total - dl - u32::try_from(df - 1).unwrap();
                texts.push(text(1, 1 + extra));
                for _ in 2..df {
                    texts.push(text(1, 1));
                }
            }
        }
        while texts.len() < n {
            texts.push(String::new());
        }

        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        LexicalIndex::build(&refs)
    }

    /// Generates a valid `(n, df, avgdl, dl, tf)` for [`length_fixture`]: the
    /// reserved `df-1` tokens outside chunk 0 always fit, so no assumption has
    /// to reject a generated case.
    fn bm25_fixture_params() -> impl Strategy<Value = (usize, usize, u32, u32, u32)> {
        (1usize..6, 1u32..30).prop_flat_map(|(n, avgdl)| {
            let total = avgdl * u32::try_from(n).unwrap();
            (1usize..=n).prop_flat_map(move |df| {
                let min_spare = u32::try_from(df - 1).unwrap();
                // With one chunk there is nowhere to put tokens outside chunk 0,
                // so its length must be the whole average.
                let spare_range = if n == 1 { 0..1 } else { min_spare..total };
                spare_range.prop_flat_map(move |spare| {
                    let dl = total - spare;
                    (1u32..=dl).prop_map(move |tf| (n, df, avgdl, dl, tf))
                })
            })
        })
    }

    proptest! {
        #[test]
        fn bm25_axes_move_in_the_right_direction(
            (n, df, avgdl, dl, tf) in bm25_fixture_params(),
        ) {
            let total = avgdl * u32::try_from(n).unwrap();
            let spare = total - dl;
            let base = length_fixture(n, df, dl, total, tf);

            // tf: N, df, avgdl and dl fixed; strictly increasing, bounded.
            if tf < dl {
                let higher_tf = length_fixture(n, df, dl, total, tf + 1);
                let (low, _) = base.score_query(&["cache"]);
                let (high, _) = higher_tf.score_query(&["cache"]);
                prop_assert!(high[0] > low[0]);
                prop_assert!(high[0] < base.idf(1) * (K1 + 1.0));
            }

            // dl: N, df, avgdl and tf fixed; strictly decreasing. Moving one
            // token into chunk 0 needs one spare token outside it.
            if spare >= u32::try_from(df).unwrap() {
                let longer = length_fixture(n, df, dl + 1, total, tf);
                let (short_scores, _) = base.score_query(&["cache"]);
                let (long_scores, _) = longer.score_query(&["cache"]);
                prop_assert!(long_scores[0] < short_scores[0]);
            }

            // avgdl: N, df, dl and tf fixed; strictly increasing. A single
            // chunk leaves nowhere to put the extra average length, so the
            // property is only constructible with at least two chunks.
            if n > 1 {
                let sparser_total = (avgdl + 1) * u32::try_from(n).unwrap();
                let sparser = length_fixture(n, df, dl, sparser_total, tf);
                let (dense_scores, _) = base.score_query(&["cache"]);
                let (sparse_scores, _) = sparser.score_query(&["cache"]);
                prop_assert!(sparse_scores[0] > dense_scores[0]);
            }

            // idf: N fixed; strictly decreasing in df.
            if df < n {
                let df32 = u32::try_from(df).unwrap();
                prop_assert!(base.idf(df32) > base.idf(df32 + 1));
            }
        }

        #[test]
        fn scores_and_postings_are_finite_and_in_range(
            texts in prop::collection::vec("[A-Za-z0-9 _-]{0,40}", 0..6),
            query in "[A-Za-z0-9 _-]{0,20}",
        ) {
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            let index = LexicalIndex::build(&refs);
            let tokenizer = Tokenizer::new();
            let query_tokens = tokenizer.tokenize(&query);
            let terms: Vec<&str> = query_tokens.iter().collect();
            let (scores, s_ref) = index.score_query(&terms);

            prop_assert_eq!(scores.len(), index.len());
            prop_assert!(s_ref.is_finite() && s_ref >= 0.0);
            for score in &scores {
                prop_assert!(score.is_finite() && *score >= 0.0);
            }
            for term in index.terms() {
                prop_assert!(term.postings.0 <= term.postings.1);
                prop_assert!((term.postings.1 as usize) <= index.postings().len());
            }
            for posting in index.postings() {
                prop_assert!((posting.chunk as usize) < index.len());
                prop_assert!(posting.tf >= 1);
            }
        }
    }
}
