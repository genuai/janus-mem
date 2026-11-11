//! The splice: one patch in, the whole new file out, or a refusal (SPEC §7.3).
//!
//! [`plan`] is pure. It reads no file, takes no lock and writes no file: it takes
//! the bytes the caller already read and returns the bytes to write (M7-11). A
//! test can therefore change a file between the plan and the commit with no
//! thread and no sleep, and a rollback is possible later, because
//! [`Plan::original`] keeps the bytes the plan was made from (M7-31).

use std::path::PathBuf;

use super::PatchError;
use crate::id::Blake3Hash;

use super::scan::{self, Scan, ValueShape};
use super::value::render_value;

/// One request to change frontmatter keys.
///
/// `expected_hash`, `dry_run` and `path` are carried, not enforced: they belong
/// to the caller of this module (M7-3, M7-4). Nothing here compares a hash or
/// reads a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    /// The document's path, for the caller's diagnostics.
    pub path: PathBuf,
    /// The keys to write. `None` writes `null`. Sorted before use.
    pub keys: Vec<(String, Option<String>)>,
    /// The hash the caller saw on a read. Not checked in this phase.
    pub expected_hash: Option<String>,
    /// Create a block when the document has none.
    pub create_frontmatter: bool,
    /// Report what would change without changing a file. Not used in this phase.
    pub dry_run: bool,
}

/// The new bytes, and everything the receipt reports about them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The bytes the plan was made from, kept for a rollback (M7-31).
    pub original: Vec<u8>,
    /// Length of the input, in bytes.
    pub bytes_before: usize,
    /// Length of the output, in bytes.
    pub bytes_after: usize,
    /// blake3 of the input bytes.
    pub old_hash: String,
    /// blake3 of the output bytes.
    pub new_hash: String,
    /// The whole new file.
    pub text: String,
    /// Keys whose bytes differ after the patch, sorted. Empty when the patch
    /// writes what the file already said.
    pub changed_keys: Vec<String>,
    /// Keys that were absent and are now written, sorted.
    pub appended_keys: Vec<String>,
}

/// Plans the new bytes for `patch` against `text`, or refuses.
///
/// A refusal returns no bytes at all, so a caller that treats `Err` as "do not
/// write" cannot write a half-patched file (I3).
pub fn plan(text: &str, patch: &Patch) -> Result<Plan, PatchError> {
    if patch.keys.is_empty() {
        return Err(PatchError::EmptyRequest);
    }

    let mut keys: Vec<&(String, Option<String>)> = patch.keys.iter().collect();
    keys.sort_by(|left, right| left.0.cmp(&right.0));
    for pair in keys.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(PatchError::DuplicateRequestKey {
                key: pair[0].0.clone(),
            });
        }
    }
    for (key, _) in &keys {
        if !scan::is_patch_key(key) {
            return Err(PatchError::KeyShape { key: key.clone() });
        }
    }

    let scan = scan::scan(text)?;
    if scan.span.is_empty() && !patch.create_frontmatter {
        return Err(PatchError::NoFrontmatter);
    }

    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut appended: Vec<String> = Vec::new();
    let mut changed_keys: Vec<String> = Vec::new();
    let mut appended_keys: Vec<String> = Vec::new();

    for (key, value) in keys {
        let rendered = render_value(value.as_deref());
        match scan.keys.iter().find(|span| span.key == *key) {
            Some(span) => {
                if let ValueShape::Unsupported(reason) = span.shape {
                    return Err(PatchError::UnsupportedShape {
                        key: key.clone(),
                        reason,
                    });
                }
                // An empty value has no bytes to replace, so the rendered value
                // brings its own separator space.
                let insert = if span.value.start == span.value.end {
                    format!(" {rendered}")
                } else {
                    rendered
                };
                if text[span.value.start..span.value.end] != insert {
                    edits.push((span.value.start, span.value.end, insert));
                    changed_keys.push(key.clone());
                }
            }
            // The key is absent: one line, before the closing fence (M7-8).
            None => {
                appended.push(format!("{key}: {rendered}"));
                changed_keys.push(key.clone());
                appended_keys.push(key.clone());
            }
        }
    }

    let new_text = if scan.span.is_empty() {
        create_block(text, &scan, &appended)
    } else {
        splice(text, &scan, &mut edits, &appended)
    };

    Ok(Plan {
        original: text.as_bytes().to_vec(),
        bytes_before: text.len(),
        bytes_after: new_text.len(),
        old_hash: hash(text.as_bytes()),
        new_hash: hash(new_text.as_bytes()),
        text: new_text,
        changed_keys,
        appended_keys,
    })
}

/// Replaces every planned span, and inserts the appended lines before the fence.
///
/// Edits are applied last to first, so an earlier offset is never shifted by a
/// later one. The append point is past every value span, so it goes first.
fn splice(
    text: &str,
    scan: &Scan,
    edits: &mut [(usize, usize, String)],
    appended: &[String],
) -> String {
    let mut out = text.to_owned();
    if !appended.is_empty() {
        let block = lines(appended, scan.newline);
        out.replace_range(scan.yaml_end..scan.yaml_end, &block);
    }
    edits.sort_by_key(|(start, _, _)| *start);
    for (start, end, insert) in edits.iter().rev() {
        out.replace_range(*start..*end, insert);
    }
    out
}

/// Wraps `lines` in a new block, after the prefix and before the body (M7-10).
fn create_block(text: &str, scan: &Scan, appended: &[String]) -> String {
    let mut out = String::with_capacity(text.len() + 32);
    out.push_str(&text[..scan.span.start]);
    out.push_str(scan::FENCE);
    out.push_str(scan.newline);
    out.push_str(&lines(appended, scan.newline));
    out.push_str(scan::FENCE);
    out.push_str(scan.newline);
    out.push_str(&text[scan.span.start..]);
    out
}

/// The key lines as one string, each ended by the file's newline.
fn lines(lines: &[String], newline: &str) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push_str(newline);
    }
    out
}

/// The hash a read reports for these bytes: the etag a receipt hands back.
fn hash(bytes: &[u8]) -> String {
    Blake3Hash::of_bytes(bytes).to_hex()
}
