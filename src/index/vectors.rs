//! The flat `vectors.f32` payload (D17, SPEC §7.2, §8.3, §8.3.1).
//!
//! The file is **headerless**: raw little-endian `f32` values, row-major, row
//! `i` belonging to snapshot chunk `i`. The shape lives in the manifest
//! (`EmbedderId::dim` and the chunk count), never in the file, because a header
//! would duplicate `dim` and force M8's zero-copy cast to skip a prefix. Row
//! storage is one flat [`Vec<f32>`] and every row is a slice of it, following
//! §8.3.1's rule against `Vec<Vec<f32>>`.
//!
//! A value is valid only when the whole array is valid. [`Vectors::new`] and
//! [`Vectors::decode`] therefore reject, before any row is used: a zero
//! dimension, a byte length that is not a multiple of four, a byte length that
//! does not equal `rows × dim × 4`, a non-finite value, and a row whose norm is
//! neither zero nor within [`NORM_TOLERANCE`] of one. Corruption is a rebuild,
//! never a panic (§3.1).
//!
//! [`normalize`] is the writer-side operation: it divides by the L2 norm and
//! returns that norm. A zero row stays zero and reports `0.0`; any other input
//! is normalised in index order, so the bytes a deterministic embedder produces
//! stay deterministic. On rows that are already normalised, [`dot`] is the
//! cosine (§7.2).

/// The payload filename inside `.janus/` (§8.3).
pub const VECTORS_FILE: &str = "vectors.f32";

/// How far a row's norm may sit from `1.0` and still be accepted.
///
/// `1e-3` is the plan's §3.1 tolerance. It is wide enough for an `f32` norm
/// pass over a real embedding and narrow enough that a row normalised with the
/// wrong dimension fails. A norm of exactly `0.0` is accepted separately: an
/// empty text has no direction, and §3.2 requires the hash double to keep it.
pub const NORM_TOLERANCE: f32 = 1e-3;

/// Why a vector array cannot be trusted.
///
/// Every variant names the value that failed, so a corrupt file produces a
/// diagnostic rather than "invalid input" (§5.1).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Corrupt {
    /// A zero-width row cannot be decoded or normalised.
    #[error("vectors dimension is zero")]
    ZeroDim,

    /// The flat length is not a whole number of rows.
    #[error("vectors length {len} is not a multiple of dimension {dim}")]
    RowWidth {
        /// Number of `f32` values supplied.
        len: usize,
        /// Configured row width.
        dim: u32,
    },

    /// A byte array does not end on an `f32` boundary.
    #[error("vectors byte length {len} is not a multiple of 4")]
    NotF32Aligned {
        /// Supplied byte length.
        len: usize,
    },

    /// The bytes are not the length the manifest's shape requires.
    #[error(
        "vectors byte length {len} does not match {rows} rows × {dim} dimensions × 4 bytes = {expected}"
    )]
    WrongByteLength {
        /// Supplied byte length.
        len: usize,
        /// Row count the caller expects.
        rows: usize,
        /// Row width the caller expects.
        dim: u32,
        /// The one byte length those two values permit.
        expected: usize,
    },

    /// `NaN` or `±inf` is corruption, not a score.
    #[error("vectors value at index {index} is not finite: {value}")]
    NonFinite {
        /// Flat index of the offending value.
        index: usize,
        /// The offending value.
        value: f32,
    },

    /// A row is neither normalised nor zero, so the writer was not this code.
    #[error("vectors row {row} has norm {norm}, expected 0.0 or within {NORM_TOLERANCE} of 1.0")]
    Unnormalised {
        /// Zero-based row index.
        row: usize,
        /// The norm that failed.
        norm: f32,
    },
}

/// A validated, row-major vector array for `vectors.f32`.
///
/// Fields are private so the validation in [`Vectors::new`] and
/// [`Vectors::decode`] is the only way to construct a value outside this
/// module. `dim` is stored as `u32` because that is the width `EmbedderId`
/// records; the public accessor returns `usize` because that is what slicing
/// wants.
#[derive(Debug, Clone, PartialEq)]
pub struct Vectors {
    dim: u32,
    rows: Vec<f32>,
}

impl Vectors {
    /// Validates a flat row-major array and returns it.
    ///
    /// `rows.len()` must be a multiple of `dim`, every value must be finite,
    /// and every row's norm must be `0.0` or within [`NORM_TOLERANCE`] of
    /// `1.0`. A zero-row array is valid: an empty corpus has an empty payload,
    /// not a missing one.
    ///
    /// # Errors
    ///
    /// [`Corrupt::ZeroDim`], [`Corrupt::RowWidth`], [`Corrupt::NonFinite`] or
    /// [`Corrupt::Unnormalised`] when the array cannot describe a
    /// `vectors.f32` generation.
    pub fn new(dim: u32, rows: Vec<f32>) -> Result<Self, Corrupt> {
        if dim == 0 {
            return Err(Corrupt::ZeroDim);
        }

        let width = dim as usize;
        if !rows.len().is_multiple_of(width) {
            return Err(Corrupt::RowWidth {
                len: rows.len(),
                dim,
            });
        }

        for (index, value) in rows.iter().enumerate() {
            if !value.is_finite() {
                return Err(Corrupt::NonFinite {
                    index,
                    value: *value,
                });
            }
        }

        for (row, values) in rows.chunks_exact(width).enumerate() {
            let norm = l2_norm(values);
            if norm != 0.0 && (norm - 1.0).abs() > NORM_TOLERANCE {
                return Err(Corrupt::Unnormalised { row, norm });
            }
        }

        Ok(Self { dim, rows })
    }

    /// Encodes the array as little-endian `f32` bytes.
    ///
    /// The caller hashes exactly these bytes into `vectors_sha` (§3.1), so the
    /// encoding is the file format: no header, no padding, no alignment.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.rows.len() * 4);
        for value in &self.rows {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    /// Decodes and validates a payload whose shape the manifest supplies.
    ///
    /// `dim` comes from `EmbedderId`; `rows` comes from the snapshot's chunk
    /// count. The manifest's `vectors_len` must equal `rows × dim × 4`, which is
    /// the length checked here.
    ///
    /// # Errors
    ///
    /// [`Corrupt::ZeroDim`], [`Corrupt::NotF32Aligned`],
    /// [`Corrupt::WrongByteLength`], or any error [`Vectors::new`] reports after
    /// the words are decoded.
    #[allow(clippy::chunks_exact_to_as_chunks)] // §5 P1a names `chunks_exact(4)`.
    pub fn decode(bytes: &[u8], dim: u32, rows: usize) -> Result<Self, Corrupt> {
        if dim == 0 {
            return Err(Corrupt::ZeroDim);
        }

        if !bytes.len().is_multiple_of(4) {
            return Err(Corrupt::NotF32Aligned { len: bytes.len() });
        }

        // Saturating arithmetic keeps an impossible shape total: no real byte
        // slice can equal a saturated `usize::MAX`, so it falls through to the
        // length mismatch below.
        let expected = rows.saturating_mul(dim as usize).saturating_mul(4);

        if bytes.len() != expected {
            return Err(Corrupt::WrongByteLength {
                len: bytes.len(),
                rows,
                dim,
                expected,
            });
        }

        let mut values = Vec::with_capacity(bytes.len() / 4);
        for chunk in bytes.chunks_exact(4) {
            let mut word = [0u8; 4];
            word.copy_from_slice(chunk);
            values.push(f32::from_le_bytes(word));
        }

        Self::new(dim, values)
    }

    /// Returns row `index`, or `None` when it is out of range.
    #[must_use]
    pub fn row(&self, index: usize) -> Option<&[f32]> {
        let width = self.dim as usize;
        let start = index.checked_mul(width)?;
        let end = start.checked_add(width)?;
        self.rows.get(start..end)
    }

    /// Number of rows, which equals the number of chunks in the generation.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len() / self.dim as usize
    }

    /// True when the payload holds no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Row width in `f32` values.
    #[must_use]
    pub fn dim(&self) -> usize {
        self.dim as usize
    }
}

/// Divides `row` by its L2 norm and returns that norm.
///
/// A zero row is left untouched and reports `0.0`, as §3.2 requires for empty
/// text. A non-finite norm is returned without dividing: the read path rejects
/// non-finite values first, so this only keeps the function total when a caller
/// normalises an unchecked slice.
///
/// The sum is a single pass in index order, with `f32` arithmetic. That order
/// is part of [`super::embed::HashEmbedder`]'s byte-identical guarantee.
#[must_use]
pub fn normalize(row: &mut [f32]) -> f32 {
    let norm = l2_norm(row);
    if norm > 0.0 && norm.is_finite() {
        for value in row.iter_mut() {
            *value /= norm;
        }
    }
    norm
}

/// Computes the dot product of two rows.
///
/// On L2-normalised rows this is the cosine (§7.2). Both slices must have the
/// same width: a query vector and a row come from one
/// [`super::embed::EmbedderId`]. A mismatch is a bug, so debug builds assert it;
/// release builds keep the zip and stay panic-free.
#[must_use]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "dot widths must come from one EmbedderId");
    a.iter().zip(b).map(|(left, right)| left * right).sum()
}

/// `sqrt(Σ value²)`, in slice order.
fn l2_norm(row: &[f32]) -> f32 {
    row.iter().map(|value| value * value).sum::<f32>().sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(values: &[f32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    #[test]
    fn new_accepts_the_unit_rows_and_zero_rows_from_the_format() {
        let vectors = Vectors::new(2, vec![1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
            .expect("unit rows and a zero row are valid");

        assert_eq!(vectors.dim(), 2);
        assert_eq!(vectors.len(), 3);
        assert!(!vectors.is_empty());
        assert_eq!(vectors.row(0), Some([1.0, 0.0].as_slice()));
        assert_eq!(vectors.row(1), Some([0.0, 1.0].as_slice()));
        assert_eq!(vectors.row(2), Some([0.0, 0.0].as_slice()));
        assert_eq!(vectors.row(3), None);
    }

    #[test]
    fn new_accepts_an_empty_payload() {
        let vectors = Vectors::new(3, Vec::new()).expect("an empty corpus has no rows");

        assert_eq!(vectors.dim(), 3);
        assert_eq!(vectors.len(), 0);
        assert!(vectors.is_empty());
        assert_eq!(vectors.row(0), None);
    }

    #[test]
    fn new_rejects_a_zero_dimension() {
        assert_eq!(Vectors::new(0, Vec::new()), Err(Corrupt::ZeroDim));
        assert_eq!(Vectors::new(0, vec![1.0]), Err(Corrupt::ZeroDim));
    }

    #[test]
    fn new_rejects_a_partial_row() {
        assert_eq!(
            Vectors::new(2, vec![1.0]),
            Err(Corrupt::RowWidth { len: 1, dim: 2 })
        );
    }

    #[test]
    fn new_rejects_non_finite_values_before_checking_the_norm() {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(
                matches!(
                    Vectors::new(1, vec![value]),
                    Err(Corrupt::NonFinite { index: 0, .. })
                ),
                "{value} must be corruption"
            );
        }
    }

    #[test]
    fn new_rejects_an_unnormalised_row_but_accepts_the_tolerance_edge() {
        // [3, 4] has norm 5, nowhere near 1.
        assert_eq!(
            Vectors::new(2, vec![3.0, 4.0]),
            Err(Corrupt::Unnormalised { row: 0, norm: 5.0 })
        );

        // sqrt(1² + 0.002²) = 1.000002, inside the 1e-3 tolerance.
        let inside = Vectors::new(2, vec![1.0, 0.002]).expect("inside the tolerance");
        assert_eq!(inside.len(), 1);

        // sqrt(1² + 0.05²) = 1.001249..., outside it.
        assert!(matches!(
            Vectors::new(2, vec![1.0, 0.05]),
            Err(Corrupt::Unnormalised { row: 0, .. })
        ));
    }

    #[test]
    fn encode_writes_little_endian_f32_without_a_header() {
        let vectors = Vectors::new(1, vec![1.0, -1.0]).expect("one-dimensional unit rows");
        let bytes = vectors.encode();

        assert_eq!(bytes, vec![0x00, 0x00, 0x80, 0x3f, 0x00, 0x00, 0x80, 0xbf]);
        assert_eq!(bytes.len(), 8);
    }

    #[test]
    fn encode_and_decode_round_trip_a_known_row_pair() {
        let values = vec![0.6, 0.8, 0.8, -0.6];
        let vectors = Vectors::new(2, values).expect("both rows have norm 1");
        let bytes = vectors.encode();

        let decoded = Vectors::decode(&bytes, 2, 2).expect("the bytes we just wrote are valid");
        assert_eq!(decoded, vectors);
        assert_eq!(decoded.row(0), Some([0.6, 0.8].as_slice()));
        assert_eq!(decoded.row(1), Some([0.8, -0.6].as_slice()));
    }

    #[test]
    fn decode_rejects_a_byte_length_that_is_not_a_whole_number_of_f32() {
        assert_eq!(
            Vectors::decode(&[0x00, 0x00, 0x80], 1, 1),
            Err(Corrupt::NotF32Aligned { len: 3 })
        );
    }

    #[test]
    fn decode_rejects_a_byte_length_the_shape_does_not_allow() {
        // Four bytes can only be one row of dim 1, not one row of dim 2.
        assert_eq!(
            Vectors::decode(&encoded(&[1.0]), 2, 1),
            Err(Corrupt::WrongByteLength {
                len: 4,
                rows: 1,
                dim: 2,
                expected: 8,
            })
        );

        // Eight bytes are one row of dim 2, not two rows of dim 2.
        assert_eq!(
            Vectors::decode(&encoded(&[1.0, 0.0]), 2, 2),
            Err(Corrupt::WrongByteLength {
                len: 8,
                rows: 2,
                dim: 2,
                expected: 16,
            })
        );
    }

    #[test]
    fn decode_rejects_zero_dim_before_reading_any_byte() {
        assert_eq!(Vectors::decode(&[], 0, 0), Err(Corrupt::ZeroDim));
    }

    #[test]
    fn decode_validates_the_values_it_read() {
        assert!(matches!(
            Vectors::decode(&encoded(&[f32::NAN]), 1, 1),
            Err(Corrupt::NonFinite { index: 0, .. })
        ));
        assert!(matches!(
            Vectors::decode(&encoded(&[2.0]), 1, 1),
            Err(Corrupt::Unnormalised { row: 0, norm: 2.0 })
        ));
    }

    #[test]
    fn decode_accepts_an_empty_payload_for_an_empty_snapshot() {
        let vectors = Vectors::decode(&[], 4, 0).expect("no rows need no bytes");

        assert_eq!(vectors.dim(), 4);
        assert_eq!(vectors.len(), 0);
    }

    #[test]
    fn normalize_divides_by_hand_computed_norms() {
        let mut row = [3.0, 4.0];
        let norm = normalize(&mut row);

        assert_eq!(norm, 5.0);
        assert!((row[0] - 0.6).abs() < 1e-6);
        assert!((row[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn normalize_leaves_a_zero_row_zero_and_reports_zero() {
        let mut row = [0.0, 0.0, 0.0];

        assert_eq!(normalize(&mut row), 0.0);
        assert_eq!(row, [0.0, 0.0, 0.0]);

        let mut empty: [f32; 0] = [];
        assert_eq!(normalize(&mut empty), 0.0);
    }

    #[test]
    fn normalize_leaves_a_unit_row_unchanged() {
        let mut row = [1.0, 0.0];

        assert_eq!(normalize(&mut row), 1.0);
        assert_eq!(row, [1.0, 0.0]);
    }

    #[test]
    fn dot_is_the_cosine_on_normalised_rows() {
        let first = [0.6, 0.8];
        let second = [0.8, -0.6];

        assert!((dot(&first, &first) - 1.0).abs() < 1e-6);
        assert!(
            dot(&first, &second).abs() < 1e-6,
            "these rows are orthogonal"
        );
        assert!((dot(&first, &[1.0, 0.0]) - 0.6).abs() < 1e-6);
    }
}
