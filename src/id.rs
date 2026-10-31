//! Stable, content-addressed identifiers (SPEC §6.4, §8.3.1).
//!
//! Three newtypes, deliberately distinct:
//!
//! * [`DocId`] — a *document's* identity. Derived from a metadata match key
//!   (`blake3(key)[..16]`) and carried forward while the path is stable.
//! * [`Blake3Hash`] — the hash of a file's full bytes. Serves as the etag every
//!   read tool hands out and every write tool demands back (§9.3, D18).
//! * [`ChunkId`] — a *chunk's* identity: `blake3(breadcrumb ‖ 0x00 ‖ text)`.
//!   Content-addressed and deliberately **independent of `DocId`**, so an
//!   unchanged chunk keeps its embedding across edits, moves and copies (D24).
//!
//! They are separate types rather than `String`/`[u8; 32]` for one reason: the
//! compiler should reject a chunk id passed where a doc id is expected. A
//! `String` would not.

use std::fmt;
use std::str::FromStr;

use blake3::Hash;

/// Byte length of a [`DocId`]. 16 bytes (128 bits) of blake3 output — far more
/// than a local vault can exhaust, and small enough to sort and store cheaply.
pub const DOC_ID_LEN: usize = 16;

/// Byte length of a full blake3 digest, used by [`Blake3Hash`] and [`ChunkId`].
pub const BLAKE3_LEN: usize = 32;

/// Hex digits needed to render [`DOC_ID_LEN`] bytes.
const DOC_ID_HEX_LEN: usize = DOC_ID_LEN * 2;

/// Hex digits needed to render [`BLAKE3_LEN`] bytes.
const BLAKE3_HEX_LEN: usize = BLAKE3_LEN * 2;

/// A document's stable identity (§6.4).
///
/// Nothing in the vault stores this: it is derived on every scan from the best
/// available match key and only persisted inside `.janus/`. It must never be
/// confused with a path, which changes, or a content hash, which also changes
/// whenever the document is edited.
///
/// Ordered and `Hash` so it can key maps (`path_history`) and sort output
/// deterministically.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocId([u8; DOC_ID_LEN]);

impl DocId {
    /// Derives an id from a match key (§6.4): `blake3(key)[..16]`.
    ///
    /// `key` is the namespaced identity string the caller built — for example
    /// `"main\x00org:acme\x00Q4 plan\x002026-01-01"`. Truncation to 128 bits is
    /// intentional; the full digest buys nothing against accidental collision
    /// and costs 16 extra bytes per document.
    #[must_use]
    pub fn from_match_key(key: &[u8]) -> Self {
        let digest = blake3::hash(key);
        let mut bytes = [0u8; DOC_ID_LEN];
        bytes.copy_from_slice(&digest.as_bytes()[..DOC_ID_LEN]);
        Self(bytes)
    }

    /// Wraps raw bytes, e.g. when loading a persisted id from the index.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; DOC_ID_LEN]) -> Self {
        Self(bytes)
    }

    /// The raw bytes, for serialization into the index.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; DOC_ID_LEN] {
        &self.0
    }

    /// Lowercase hex, the form used in JSON output and on the command line.
    #[must_use]
    pub fn to_hex(self) -> String {
        hex_encode(&self.0)
    }
}

impl fmt::Display for DocId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for DocId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DocId({self})")
    }
}

impl FromStr for DocId {
    type Err = IdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(hex_decode(s, DOC_ID_HEX_LEN)?))
    }
}

/// The blake3 hash of a document's full file bytes (§6.2).
///
/// This is the vault's *etag*: `read_document` returns it and
/// `update_frontmatter` refuses to write without it, so an agent can never
/// silently overwrite a file it read before someone else changed it (D18).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Blake3Hash([u8; BLAKE3_LEN]);

impl Blake3Hash {
    /// Hashes a file's bytes. The vault hashes exactly the bytes it read, never
    /// trusting `mtime` (§6.9).
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self::from_digest(blake3::hash(bytes))
    }

    /// Wraps an already-computed digest.
    #[must_use]
    pub const fn from_digest(digest: Hash) -> Self {
        Self(*digest.as_bytes())
    }

    /// Wraps raw bytes, e.g. when loading a persisted hash from the index.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; BLAKE3_LEN]) -> Self {
        Self(bytes)
    }

    /// The raw bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; BLAKE3_LEN] {
        &self.0
    }

    /// Lowercase hex, the form used for etags in JSON.
    #[must_use]
    pub fn to_hex(self) -> String {
        hex_encode(&self.0)
    }
}

impl fmt::Display for Blake3Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Blake3Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Blake3Hash({self})")
    }
}

impl FromStr for Blake3Hash {
    type Err = IdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(hex_decode(s, BLAKE3_HEX_LEN)?))
    }
}

/// A chunk's identity: `blake3(heading_breadcrumb ‖ 0x00 ‖ text)` (§6.4).
///
/// Deliberately **excludes** `DocId`, which is what makes the embedding cache
/// free across edits, moves, renames and copies: the same text in two documents
/// is one chunk, and one embedding (D24). Filled in by the chunker in M2.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkId([u8; BLAKE3_LEN]);

impl ChunkId {
    /// Wraps raw bytes, e.g. when loading a persisted chunk id from the index.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; BLAKE3_LEN]) -> Self {
        Self(bytes)
    }

    /// The raw bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; BLAKE3_LEN] {
        &self.0
    }

    /// Lowercase hex.
    #[must_use]
    pub fn to_hex(self) -> String {
        hex_encode(&self.0)
    }
}

impl fmt::Display for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChunkId({self})")
    }
}

impl FromStr for ChunkId {
    type Err = IdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(hex_decode(s, BLAKE3_HEX_LEN)?))
    }
}

/// Why a hex id could not be parsed.
///
/// Kept local to this module rather than folded into `JanusError`: parsing an
/// identifier is a self-contained fallible conversion, and `id` must stay
/// usable by `error` without a cycle.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdParseError {
    /// The string did not have the exact number of hex digits required.
    #[error("expected {expected} hex digits, found {found}")]
    WrongLength {
        /// Digits required by the id type.
        expected: usize,
        /// Characters actually present.
        found: usize,
    },

    /// The string had the right length but contained a non-hex character.
    #[error("invalid hex digit {digit:?} at byte {index}")]
    InvalidHex {
        /// The offending character.
        digit: char,
        /// Its character index in the input.
        index: usize,
    },
}

/// Renders bytes as lowercase hex without allocating per byte.
fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";

    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Decodes exactly `N` bytes of hex, rejecting wrong lengths and stray
/// characters.
///
/// `N` is a const generic so [`DocId`] (16 bytes) and [`Blake3Hash`] (32 bytes)
/// share one implementation instead of two near-identical ones.
fn hex_decode<const N: usize>(
    text: &str,
    expected_hex_len: usize,
) -> Result<[u8; N], IdParseError> {
    if text.len() != expected_hex_len {
        return Err(IdParseError::WrongLength {
            expected: expected_hex_len,
            found: text.len(),
        });
    }

    let mut bytes = [0u8; N];
    let (pairs, _remainder) = text.as_bytes().as_chunks::<2>();

    // `iter_mut().zip` cannot overrun either buffer, so a wrong
    // `expected_hex_len` would produce garbage rather than a panic.
    for (index, (slot, pair)) in bytes.iter_mut().zip(pairs).enumerate() {
        let high = hex_value(pair[0], index * 2)?;
        let low = hex_value(pair[1], index * 2 + 1)?;
        *slot = (high << 4) | low;
    }
    Ok(bytes)
}

/// Maps one ASCII hex digit to its value.
fn hex_value(digit: u8, index: usize) -> Result<u8, IdParseError> {
    match digit {
        b'0'..=b'9' => Ok(digit - b'0'),
        b'a'..=b'f' => Ok(digit - b'a' + 10),
        b'A'..=b'F' => Ok(digit - b'A' + 10),
        _ => Err(IdParseError::InvalidHex {
            // Non-ASCII bytes are rendered through Latin-1 (`char::from(u8)`),
            // which is accurate for the ASCII case that actually matters and
            // merely cosmetic for anything else.
            digit: char::from(digit),
            index,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_id_is_deterministic_and_key_sensitive() {
        assert_eq!(
            DocId::from_match_key(b"title\x002026-01-01"),
            DocId::from_match_key(b"title\x002026-01-01")
        );
        assert_ne!(
            DocId::from_match_key(b"title\x002026-01-01"),
            DocId::from_match_key(b"title\x002026-01-02")
        );
    }

    #[test]
    fn doc_id_hex_round_trips() {
        let id = DocId::from_match_key(b"anything");
        assert_eq!(id.to_hex().len(), DOC_ID_HEX_LEN);
        assert_eq!(id.to_string().parse::<DocId>(), Ok(id));
    }

    #[test]
    fn blake3_hash_hex_round_trips() {
        let hash = Blake3Hash::of_bytes(b"hello");
        assert_eq!(hash.to_hex().len(), BLAKE3_HEX_LEN);
        assert_eq!(hash.to_string().parse::<Blake3Hash>(), Ok(hash));
    }

    #[test]
    fn hash_changes_with_content() {
        assert_ne!(
            Blake3Hash::of_bytes(b"hello"),
            Blake3Hash::of_bytes(b"hello ")
        );
    }

    #[test]
    fn uppercase_hex_parses() {
        let id = DocId::from_match_key(b"x");
        let upper = id.to_hex().to_uppercase();
        assert_eq!(upper.parse::<DocId>(), Ok(id));
    }

    #[test]
    fn wrong_length_is_rejected() {
        let err = "abcd".parse::<DocId>().unwrap_err();
        assert_eq!(
            err,
            IdParseError::WrongLength {
                expected: DOC_ID_HEX_LEN,
                found: 4
            }
        );
    }

    #[test]
    fn non_hex_is_rejected_with_position() {
        let mut text = DocId::from_match_key(b"x").to_hex();
        text.replace_range(3..4, "z");
        let err = text.parse::<DocId>().unwrap_err();
        assert_eq!(
            err,
            IdParseError::InvalidHex {
                digit: 'z',
                index: 3
            }
        );
    }

    #[test]
    fn ids_are_ordered_for_deterministic_output() {
        let mut ids = [
            DocId::from_match_key(b"c"),
            DocId::from_match_key(b"a"),
            DocId::from_match_key(b"b"),
        ];
        ids.sort();
        let mut expected = ids;
        expected.sort_by_key(|id| id.to_hex());
        assert_eq!(ids, expected);
    }
}
