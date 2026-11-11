//! The pure part of the writer: frontmatter key spans, or a refusal (SPEC §7.3).
//!
//! [`scan`] finds the byte range of every top-level key's value in a document,
//! and [`ValueShape`] says whether that value can be replaced by one splice.
//! The splice itself is not here: this module locates bytes, it does not move
//! them.
//!
//! # What this module refuses to do
//!
//! It reads no file, writes no file and takes no lock. It does not compare an
//! `expected_hash`, build a receipt, or map an error to an exit code. Those
//! belong to the phases that own IO and the surface (M7-3, M7-4).
//!
//! # The two rules a caller can rely on
//!
//! * A key is read at column 0 only, because a nested key is not a key the
//!   config reads (M7-2).
//! * [`ValueShape`] is closed. A shape the scanner does not read is reported
//!   with its reason, and never guessed at (M7-3).

mod scan;

pub use scan::reason;
pub use scan::{KeySpan, Scan, ValueShape, ValueSpan, is_patch_key, scan};

use thiserror::Error;

/// Why a document cannot be scanned, or a value cannot be replaced.
///
/// One variant per refusal, and no path: the caller that owns the path adds it
/// (M7-4). Every variant is a refusal, never a partial write.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PatchError {
    /// The key's value has a shape the scanner does not read.
    ///
    /// The reason comes from [`reason`] and stays stable for consumers.
    #[error("`{key}` holds a value the writer cannot splice: {reason}")]
    UnsupportedShape {
        /// The key whose value is refused.
        key: String,
        /// Why it is refused, from [`reason`].
        reason: &'static str,
    },
}
