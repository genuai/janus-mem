//! One tokenizer for documents and queries (S-1, S-2, §3.7).
//!
//! Lexical retrieval has two sides — indexing a chunk and scoring a query — and
//! they only agree if both call the same function. This module is that
//! definition: [`Tokenizer::tokenize`] is the only way the rest of the engine
//! turns text into terms.
//!
//! # Ownership
//!
//! [`Tokens`] **owns** the folded bytes (`folded: String`) and lends terms as
//! slices of it. [`Tokens::iter`] is therefore tied to `&Tokens`, never to the
//! input string or to the [`Tokenizer`]. No lifetime parameter, no `unsafe`, no
//! self-referential struct: the buffer and the ranges live in the same value.
//!
//! [`Tokens`] is reusable through [`Tokenizer::tokenize_into`], so an index
//! build keeps one buffer for every chunk instead of allocating per chunk.
//!
//! # Rules (S-1)
//!
//! * Text is lowercased with [`char::to_lowercase`], and **every** character an
//!   expansion produces is kept. `İ` becomes `i` followed by a combining dot,
//!   not a bare `i`.
//! * Unicode alphanumerics and `_` are term characters; every other character
//!   separates terms. Digits are term characters, so `blake3` stays one term
//!   and `m1-scan-parse` becomes `m1`, `scan`, `parse`.
//! * A term longer than [`MAX_TERM_BYTES`] (64) bytes is dropped and counted in
//!   [`Tokens::dropped`]. Terms are measured after folding, in UTF-8 bytes.
//!
//! # Known limitation (phase 1)
//!
//! A run of CJK characters is **one term**, because this milestone has no
//! language-specific segmenter. `東京タワー` is a single term rather than
//! `東京` + `タワー`; recall on spaced Japanese is fine, recall on unspaced
//! Japanese is not. A real segmenter is not part of M3 and changing this rule
//! changes the tokenizer fingerprint.
//!
//! # Passing the fingerprint on (S-2a)
//!
//! [`Tokenizer::fingerprint`] is `Copy + Eq` and names both the tokenization
//! algorithm (`"v1"`) and the stemmer ([`Stemmer::None`]). Phase 4 stores it in
//! the manifest and hashes it into the document reuse key, so enabling Porter
//! later is a deliberate fingerprint change that forces a rebuild rather than a
//! silent mismatch between old postings and new query terms. Stemming is off in
//! M3 (S-2); this enum deliberately has exactly one variant.
//!
//! # Example
//!
//! The terms borrow from `tokens`, so no lifetime annotation is needed:
//!
//! ```
//! use janus_mem::tokenize::Tokenizer;
//!
//! let tokenizer = Tokenizer::new();
//! let tokens = tokenizer.tokenize("JANUS_WRITE m1-scan-parse");
//! let terms: Vec<&str> = tokens.iter().collect();
//! assert_eq!(terms, ["janus_write", "m1", "scan", "parse"]);
//! ```

use std::ops::Range;

/// Longest term kept, in folded UTF-8 bytes.
///
/// 64 bytes is long enough for SHA-256-sized hex identifiers and short enough
/// that a pathologically long alphanumeric run cannot dominate the index.
pub const MAX_TERM_BYTES: usize = 64;

/// The tokenization algorithm name, part of every fingerprint.
const ALGORITHM: &str = "v1";

/// Upper bound on the folded buffer, because ranges are stored as `u32`.
///
/// A folded input would have to exceed 4 GiB to reach this; documents are
/// chunks of a few thousand characters. When the bound is reached, folding
/// stops at the last complete term rather than panicking or emitting a range
/// that does not fit.
const MAX_FOLDED_BYTES: usize = u32::MAX as usize;

/// The folded text and the ranges of its terms (S-1, §3.7).
///
/// Terms are slices of the owned `folded` string; nothing borrows from the
/// input or from the [`Tokenizer`]. Construct one with
/// [`Tokenizer::tokenize`], or reuse one with [`Tokenizer::tokenize_into`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tokens {
    /// Lowercased text, owned for as long as the terms are borrowed.
    folded: String,
    /// Half-open term ranges into `folded`, in text order and non-overlapping.
    ranges: Vec<Range<u32>>,
    /// Terms over [`MAX_TERM_BYTES`], counted for diagnostics.
    dropped: u32,
}

impl Tokens {
    /// Iterates over the terms, borrowing them from this value.
    ///
    /// The iterator's lifetime is tied to `&self`, so `tokens` must outlive the
    /// terms. Copy a term into the caller's storage if it must be kept.
    ///
    /// The `unwrap_or_default` fallback is unreachable: `commit` only stores
    /// char-boundary offsets into `folded`, so it keeps the function total rather
    /// than handling a real invalid range.
    pub fn iter(&self) -> impl Iterator<Item = &str> + '_ {
        self.ranges.iter().map(|range| {
            self.folded
                .get(range.start as usize..range.end as usize)
                .unwrap_or_default()
        })
    }

    /// Number of kept terms. Dropped terms are not counted; see
    /// [`Tokens::dropped`].
    #[must_use]
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// True when no term was kept, including for empty or separator-only input.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Number of terms over [`MAX_TERM_BYTES`] that were discarded.
    #[must_use]
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Empties the buffers while keeping their allocations for reuse.
    fn clear(&mut self) {
        self.folded.clear();
        self.ranges.clear();
        self.dropped = 0;
    }

    /// Records the term starting at `start` and ending at the current length.
    ///
    /// The caller guarantees both offsets are UTF-8 boundaries inside `folded`.
    fn commit(&mut self, start: usize) {
        let end = self.folded.len();
        if end - start > MAX_TERM_BYTES {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        let (Ok(start), Ok(end)) = (u32::try_from(start), u32::try_from(end)) else {
            // Unreachable while `MAX_FOLDED_BYTES` bounds `folded`; refusing the
            // term keeps the iterator panic-free if that invariant ever breaks.
            return;
        };
        self.ranges.push(start..end);
    }
}

/// Which stemming rule the tokenizer applies (S-2, S-2a).
///
/// Exactly one variant in M3: stemming is off. It is part of
/// [`TokenizerFingerprint`] so adding `Porter` later cannot silently reuse an
/// index built with a different rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stemmer {
    /// No stemming: terms are the folded words themselves.
    None,
}

/// The stable identity of the tokenizer that produced an index (S-2a, §3.4).
///
/// `Copy + Eq` so it can be stored in the manifest and hashed into the reuse
/// key without cloning. The fields are the whole contract: an algorithm change
/// or a stemmer change must produce a different value, and therefore a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenizerFingerprint {
    /// The tokenization rules, e.g. `"v1"`.
    pub algorithm: &'static str,
    /// The stemming rule applied after tokenization.
    pub stemmer: Stemmer,
}

/// Turns text into [`Tokens`] (S-1, §3.7).
///
/// The tokenizer is stateless apart from its [`Stemmer`], so one instance can
/// serve both the index build and every query.
#[derive(Debug, Clone, Copy)]
pub struct Tokenizer {
    stemmer: Stemmer,
}

impl Tokenizer {
    /// Creates the M3 tokenizer: algorithm `"v1"`, no stemming.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            stemmer: Stemmer::None,
        }
    }

    /// The tokenization algorithm reported by [`Tokenizer::fingerprint`].
    #[must_use]
    pub const fn algorithm(&self) -> &'static str {
        ALGORITHM
    }

    /// The value an index stores to detect a tokenizer change (S-2a).
    #[must_use]
    pub const fn fingerprint(&self) -> TokenizerFingerprint {
        TokenizerFingerprint {
            algorithm: ALGORITHM,
            stemmer: self.stemmer,
        }
    }

    /// Tokenizes `text` into a fresh [`Tokens`] that owns the folded bytes.
    ///
    /// The returned value does **not** borrow `self` or `text`, so it can
    /// outlive both; its terms are borrowed from the returned value itself.
    #[must_use]
    pub fn tokenize(&self, text: &str) -> Tokens {
        let mut tokens = Tokens::default();
        self.tokenize_into(text, &mut tokens);
        tokens
    }

    /// Tokenizes `text` into an existing [`Tokens`], reusing its allocations.
    ///
    /// Any previous contents are cleared. A buffer that has already held a
    /// string of the same size does not grow again on later calls.
    pub fn tokenize_into(&self, text: &str, out: &mut Tokens) {
        out.clear();
        let mut current: Option<usize> = None;

        let bytes = text.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            // ASCII is the overwhelmingly common case. `to_ascii_lowercase` is
            // exactly `char::to_lowercase` for it (one char, no expansion), and
            // scanning bytes avoids decoding each one. The Unicode branch below
            // stays the definition for every non-ASCII character.
            if byte.is_ascii() {
                index += 1;
                if byte.is_ascii_alphanumeric() || byte == b'_' {
                    if current.is_none() {
                        current = Some(out.folded.len());
                    }
                    if out.folded.len() >= MAX_FOLDED_BYTES {
                        return;
                    }
                    out.folded.push(char::from(byte.to_ascii_lowercase()));
                } else if let Some(start) = current.take() {
                    out.commit(start);
                }
                continue;
            }

            let Some(character) = text[index..].chars().next() else {
                return;
            };
            index += character.len_utf8();
            if is_term_char(character) {
                if current.is_none() {
                    current = Some(out.folded.len());
                }
                for folded in character.to_lowercase() {
                    let Some(next) = out.folded.len().checked_add(folded.len_utf8()) else {
                        return;
                    };
                    if next > MAX_FOLDED_BYTES {
                        return;
                    }
                    out.folded.push(folded);
                }
            } else if let Some(start) = current.take() {
                out.commit(start);
            }
        }

        if let Some(start) = current {
            out.commit(start);
        }
    }
}

impl Default for Tokenizer {
    fn default() -> Self {
        Self::new()
    }
}

/// True for the characters that continue a term (S-1).
///
/// `is_alphanumeric` is Unicode-aware, so accented Latin, Greek, Cyrillic and
/// CJK characters all continue a term; punctuation, whitespace and emoji
/// separate it. `_` is explicit because identifiers are common in a developer
/// vault.
fn is_term_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::chunk_body;
    use crate::config::ChunkingConfig;
    use crate::id::DocId;
    use proptest::prelude::*;

    fn terms_of(text: &str) -> Vec<String> {
        terms_with(&Tokenizer::new(), text)
    }

    fn terms_with(tokenizer: &Tokenizer, text: &str) -> Vec<String> {
        let tokens = tokenizer.tokenize(text);
        tokens.iter().map(ToOwned::to_owned).collect()
    }

    #[test]
    fn splitting_table_covers_the_documented_rules() {
        let cases: &[(&str, &[&str])] = &[
            ("JANUS_WRITE", &["janus_write"]),
            ("m1-scan-parse", &["m1", "scan", "parse"]),
            ("v2.0", &["v2", "0"]),
            ("  foo, bar! (baz)?  ", &["foo", "bar", "baz"]),
            ("café", &["café"]),
            ("İ", &["i\u{307}"]),
            ("blake3", &["blake3"]),
            ("_", &["_"]),
        ];
        for (text, expected) in cases {
            assert_eq!(terms_of(text), *expected, "input {text:?}");
        }
    }

    #[test]
    fn empty_and_whitespace_only_input_yields_no_terms() {
        for text in ["", " ", "\n\t\r  ", "\u{2003}\u{2003}"] {
            let tokens = Tokenizer::new().tokenize(text);
            assert!(tokens.is_empty(), "input {text:?}");
            assert_eq!(tokens.len(), 0, "input {text:?}");
            assert_eq!(tokens.iter().count(), 0, "input {text:?}");
            assert_eq!(tokens.dropped(), 0, "input {text:?}");
        }
    }

    #[test]
    fn emoji_separates_terms_and_is_not_a_dropped_term() {
        let tokens = Tokenizer::new().tokenize("hello 🌍 world");
        assert_eq!(tokens.iter().collect::<Vec<_>>(), ["hello", "world"]);
        assert_eq!(tokens.dropped(), 0);

        let only_emoji = Tokenizer::new().tokenize("🌍🛰️");
        assert!(only_emoji.is_empty());
        assert_eq!(only_emoji.dropped(), 0);
    }

    #[test]
    fn a_cjk_run_is_one_term_known_limitation() {
        // Pinned as the documented phase-1 limitation: no segmenter, so pulling
        // 東京 and タワー apart is a fingerprint change, not a test tweak.
        let tokens = Tokenizer::new().tokenize("東京タワー");
        assert_eq!(tokens.iter().collect::<Vec<_>>(), ["東京タワー"]);

        let spaced = Tokenizer::new().tokenize("東京 タワー");
        assert_eq!(spaced.iter().collect::<Vec<_>>(), ["東京", "タワー"]);
    }

    #[test]
    fn terms_over_64_bytes_are_dropped_and_counted() {
        let kept = "a".repeat(MAX_TERM_BYTES);
        let long = "a".repeat(MAX_TERM_BYTES + 1);
        let tokens = Tokenizer::new().tokenize(&format!("{kept} {long} b"));

        assert_eq!(tokens.iter().collect::<Vec<_>>(), [kept.as_str(), "b"]);
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens.dropped(), 1, "the 65-byte term is counted");

        // The limit is bytes, not characters: 33 two-byte characters are 66.
        let unicode_long = "é".repeat(33);
        let unicode = Tokenizer::new().tokenize(&unicode_long);
        assert!(unicode.is_empty());
        assert_eq!(unicode.dropped(), 1);
    }

    #[test]
    fn tokenize_into_reuses_the_folded_buffer() {
        let tokenizer = Tokenizer::new();
        let text = "alpha ".repeat(512);
        let mut tokens = Tokens::default();

        tokenizer.tokenize_into(&text, &mut tokens);
        let folded_capacity = tokens.folded.capacity();
        let ranges_capacity = tokens.ranges.capacity();
        assert!(folded_capacity >= text.trim_end().len());

        for _ in 0..16 {
            tokenizer.tokenize_into(&text, &mut tokens);
            assert_eq!(tokens.folded.capacity(), folded_capacity);
            assert_eq!(tokens.ranges.capacity(), ranges_capacity);
        }
    }

    #[test]
    fn document_and_query_paths_produce_identical_terms() {
        let tokenizer = Tokenizer::new();
        let config = ChunkingConfig {
            max_chars: 4096,
            overlap: 0.0,
        };
        let cases = [
            "JANUS_WRITE",
            "m1-scan-parse",
            "v2.0: punctuation, everywhere!",
            "café",
            "İSTANBUL",
            "東京タワー",
            "hello 🌍 world",
            "blake3_m1",
        ];

        for text in cases {
            // The document path sees `Chunk::embed_text`: an empty title here,
            // then two newlines, then the chunk body. The query path sees the
            // bare text. Separators do not change terms, so both must agree.
            let (chunks, _) = chunk_body(DocId::from_match_key(b"tokenize-shared"), text, &config);
            assert_eq!(chunks.len(), 1, "{text:?} should be one chunk");
            let document_text = chunks[0].embed_text(text, "");
            assert_eq!(
                terms_with(&tokenizer, &document_text),
                terms_with(&tokenizer, text),
                "document and query paths diverged for {text:?}"
            );
        }
    }

    #[test]
    fn fingerprint_names_the_algorithm_and_stemmer() {
        let tokenizer = Tokenizer::new();
        assert_eq!(tokenizer.algorithm(), "v1");
        let fingerprint = tokenizer.fingerprint();
        assert_eq!(
            fingerprint,
            TokenizerFingerprint {
                algorithm: "v1",
                stemmer: Stemmer::None,
            }
        );
        assert_eq!(fingerprint, tokenizer.fingerprint());
    }

    fn arbitrary_text() -> impl Strategy<Value = String> {
        prop::collection::vec(any::<char>(), 0..300)
            .prop_map(|characters| characters.into_iter().collect())
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

        #[test]
        fn arbitrary_text_yields_well_formed_tokens(text in arbitrary_text()) {
            let tokens = Tokenizer::new().tokenize(&text);
            let mut previous_end = 0usize;

            for range in &tokens.ranges {
                let start = range.start as usize;
                let end = range.end as usize;
                prop_assert!(start < end, "empty range {start}..{end}");
                prop_assert!(end <= tokens.folded.len(), "range past folded");
                prop_assert!(tokens.folded.is_char_boundary(start));
                prop_assert!(tokens.folded.is_char_boundary(end));

                let term = &tokens.folded[start..end];
                prop_assert!(!term.is_empty());
                prop_assert!(term.len() <= MAX_TERM_BYTES);
                prop_assert!(start >= previous_end, "ranges overlap or go backwards");
                previous_end = end;
            }

            prop_assert_eq!(tokens.len(), tokens.ranges.len());
            prop_assert_eq!(tokens.iter().count(), tokens.len());
        }
    }
}
