//! The pure part of the writer: a patch becomes new bytes, or a refusal
//! (SPEC §7.3).
//!
//! Three jobs, in this order:
//!
//! 1. [`scan`] finds the byte range of every top-level key's value, and refuses
//!    the shapes it does not read.
//! 2. [`render_value`] decides how one value is written: bare, or double-quoted
//!    with a small escape set.
//! 3. [`plan`] replaces the smallest byte range that holds each value and copies
//!    everything else.
//!
//! # What this module refuses to do
//!
//! It reads no file, writes no file and takes no lock. It does not compare an
//! `expected_hash`, it does not build a receipt, and it does not map an error to
//! an exit code. Those belong to the phases that own IO and the surface (M7-3,
//! M7-4). A refusal returns [`PatchError`] and no bytes, so a caller cannot
//! write a half-patched file.
//!
//! # Creating a block
//!
//! `create_frontmatter` puts one block after any BOM and before the body. A file
//! that opens with `---` and never closes is body-only to the reader, so the new
//! block goes before it and the stray fence becomes the body's first line. The
//! alternative — refusing — would forbid a real document whose body opens with a
//! horizontal rule.
//!
//! # Invariants this module keeps (SPEC §7.3)
//!
//! * I1 — every byte outside the patched spans is identical.
//! * I2 — the newline style and the BOM are preserved.
//! * I3 — a refused patch produces no output at all.
//! * I4 — every accepted patch re-reads as valid frontmatter plus a body.
//! * I5 — other keys, key order, comments and quoting are untouched.

mod patch;
mod scan;
mod value;

pub use patch::{Patch, Plan, plan};
pub use scan::reason;
pub use scan::{KeySpan, Scan, ValueShape, ValueSpan, is_patch_key, scan};
pub use value::render_value;

use thiserror::Error;

/// Why a patch was refused.
///
/// One variant per refusal, and no path: the caller that owns the path adds it
/// (M7-4). Every variant is a refusal, never a partial write.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PatchError {
    /// The request names no key, which is a usage error.
    #[error("the patch names no key")]
    EmptyRequest,
    /// The request names one key twice, which is a usage error.
    #[error("the patch names `{key}` twice")]
    DuplicateRequestKey {
        /// The key that appears twice.
        key: String,
    },
    /// A patch key is outside `[A-Za-z0-9][A-Za-z0-9._-]*`.
    ///
    /// A key is written as a line, so a newline, `: `, `#` or a quote inside a
    /// key would add or split a key (case 1.12c).
    #[error("`{key}` is not a key: use letters, digits, dot, underscore or dash")]
    KeyShape {
        /// The offending key, as the request wrote it.
        key: String,
    },
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
    /// The document has no frontmatter block and the request did not ask for one.
    #[error("the document has no frontmatter; create it first or ask for one")]
    NoFrontmatter,
}
