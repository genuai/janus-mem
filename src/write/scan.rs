//! The frontmatter line scanner: which bytes hold which key's value (SPEC §7.3).
//!
//! The scanner reads a document and reports one [`KeySpan`] per top-level key.
//! A key is read at column 0 only, because a nested key is not a key the config
//! reads (M7-2). Everything else on a line is located, never re-emitted, so a
//! comment, an anchor or a quote on a line no patch names survives byte for
//! byte.
//!
//! [`ValueShape`] is a closed list. A shape the scanner does not understand is
//! refused with a reason, and a refusal costs one key, not the file (M7-3).
//!
//! The block is found the same way [`crate::vault::frontmatter`] finds it: the
//! same BOM, the same fence test, the same line scan. The two are checked
//! against each other on every fixture, so they cannot drift apart silently.

use std::collections::BTreeMap;

use crate::document::FrontmatterSpan;
use crate::write::PatchError;

/// The fence that opens and closes a frontmatter block.
pub(crate) const FENCE: &str = "---";

/// Length of a UTF-8 BOM in bytes.
const BOM_LEN: usize = 3;

/// The newline a block uses when its lines end in `\n`.
pub(crate) const LF: &str = "\n";

/// The newline a block uses when its lines end in `\r\n`.
pub(crate) const CRLF: &str = "\r\n";

/// Why a value cannot be replaced by one splice (plan §2.1).
///
/// The strings are public and stable: a refusal carries one in
/// [`ValueShape::Unsupported`], and the error layer puts it in `reason`.
pub mod reason {
    /// A block sequence: `tags:` followed by `- a` lines.
    pub const BLOCK_SEQUENCE: &str = "BlockSequence";
    /// A block mapping: `owner:` followed by `name: me` lines.
    pub const BLOCK_MAPPING: &str = "BlockMapping";
    /// A flow mapping: `owner: {name: me}`.
    pub const FLOW_MAPPING: &str = "FlowMapping";
    /// A block scalar: `summary: |` or `note: >-`.
    pub const MULTILINE_SCALAR: &str = "MultilineScalar";
    /// An anchored value: `team: &platform infra`.
    pub const ANCHOR: &str = "Anchor";
    /// An alias: `reviewer: *platform`.
    pub const ALIAS: &str = "Alias";
    /// A key defined more than once at column 0.
    pub const DUPLICATE_KEY: &str = "DuplicateKey";
    /// A line holds a value with no key, or a value whose end cannot be found.
    pub const NO_VALUE: &str = "NoValue";
    /// A structure the scanner does not read: a nested collection, a tag, or a
    /// key that is not a plain name.
    pub const NESTED: &str = "Nested";
}

/// A top-level key and the byte range of its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySpan {
    /// The key's text, as it is written in the file.
    pub key: String,
    /// 1-based line number of the key, used by a refusal message.
    pub line: usize,
    /// Where the value's bytes are. Empty when the value is null.
    pub value: ValueSpan,
    /// What the scanner found, and whether it can be replaced.
    pub shape: ValueShape,
}

/// The byte range of one value, and the comment that follows it.
///
/// `start..end` covers the value only. The whitespace before a trailing comment
/// sits outside the range, so a patch keeps it (M7-4). A null value has no bytes:
/// both offsets then sit directly after the key's colon, which is where the
/// rendered value goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueSpan {
    /// First byte of the value.
    pub start: usize,
    /// One past the last byte of the value.
    pub end: usize,
    /// Where a trailing comment starts, if the line has one.
    pub comment: Option<usize>,
}

impl ValueSpan {
    /// The span of a value that starts at `start` and is `length` bytes long.
    fn new(start: usize, length: usize, comment: Option<usize>) -> Self {
        Self {
            start,
            end: start + length,
            comment,
        }
    }

    /// No bytes: a null value, or the point a value the scanner refuses starts at.
    fn empty(start: usize) -> Self {
        Self::new(start, 0, None)
    }
}

/// What a key's line holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueShape {
    /// A scalar on the key's line, including a quoted scalar and a null value.
    Scalar,
    /// A flow sequence of scalars: `[db, rust]`.
    FlowSequence,
    /// A shape the writer refuses, with the reason from [`reason`].
    Unsupported(&'static str),
}

/// What the scanner found in one document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scan {
    /// Byte range of the whole block, fences included. Empty when the document
    /// has no block; `start` is then the byte after any BOM, which is where a
    /// new block goes.
    pub span: FrontmatterSpan,
    /// First byte of the YAML text between the fences.
    pub yaml_start: usize,
    /// One past the last byte of the YAML text: the start of the closing fence
    /// line, and the byte a new key line goes before.
    pub yaml_end: usize,
    /// The newline the block uses, `"\n"` or `"\r\n"`.
    pub newline: &'static str,
    /// One span per top-level key, in file order.
    pub keys: Vec<KeySpan>,
}

/// Reads the top-level keys of a document's frontmatter.
///
/// A document with no fenced block is not an error here: it scans as zero keys
/// with an empty span. Whether that is a refusal depends on the request, which
/// is why the caller decides.
pub fn scan(text: &str) -> Result<Scan, PatchError> {
    let bom_len = bom_length(text);
    let Some(block) = isolate(text, bom_len) else {
        return Ok(Scan {
            span: FrontmatterSpan {
                start: bom_len,
                end: bom_len,
            },
            yaml_start: bom_len,
            yaml_end: bom_len,
            newline: match text[bom_len..].split_once('\n') {
                Some((line, _)) if line.ends_with('\r') => CRLF,
                _ => LF,
            },
            keys: Vec::new(),
        });
    };

    let mut keys = Vec::new();
    let mut cursor = block.yaml_start;
    while let Some(line) = line_at(text, cursor) {
        if line.start >= block.yaml_end {
            break;
        }
        cursor = line.next;

        let content = without_cr(line.content);
        if content.trim().is_empty() || content.starts_with('#') {
            continue;
        }
        if content.starts_with([' ', '\t']) {
            // An indented line belongs to the key above it. That key was refused
            // already when its value was not on its own line, so there is no
            // key here to report.
            continue;
        }

        let Some(colon) = key_colon(content) else {
            // A value at column 0 with no key: the block is not a flat mapping,
            // so no key in it can be spliced safely.
            return Err(PatchError::UnsupportedShape {
                key: content.trim().to_owned(),
                reason: reason::NO_VALUE,
            });
        };
        let key = &content[..colon];
        let (value, shape) = if is_patch_key(key) {
            value_on_line(text, &line, content, colon, block.yaml_end)
        } else {
            // A quoted key, or a name with a space. It is a real key, but no
            // patch can name it, so it is reported as unpatchable and left
            // alone.
            (
                ValueSpan::empty(line.start + colon + 1),
                ValueShape::Unsupported(reason::NESTED),
            )
        };

        keys.push(KeySpan {
            key: key.to_owned(),
            line: line_number(text, line.start),
            value,
            shape,
        });
    }

    mark_duplicates(&mut keys);

    Ok(Scan {
        span: block.span,
        yaml_start: block.yaml_start,
        yaml_end: block.yaml_end,
        newline: block.newline,
        keys,
    })
}

/// Whether `key` matches the patch-key pattern `[A-Za-z0-9][A-Za-z0-9._-]*`.
///
/// A key is written as a line, so a newline, `: `, `#` or a quote inside a key
/// would add or split a key. That is an injection, not a patch.
#[must_use]
pub fn is_patch_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// A located block: its fence span, the YAML text inside, and its newline.
struct Block {
    span: FrontmatterSpan,
    yaml_start: usize,
    yaml_end: usize,
    newline: &'static str,
}

/// Locates the `---` … `---` region, if the file starts with one.
fn isolate(text: &str, bom_len: usize) -> Option<Block> {
    let rest = &text[bom_len..];

    let opening = line_at(rest, 0)?;
    if !is_fence(opening.content) {
        return None;
    }
    let newline = if opening.content.ends_with('\r') {
        CRLF
    } else {
        LF
    };

    let mut cursor = opening.next;
    loop {
        let line = line_at(rest, cursor)?;
        if is_fence(line.content) {
            return Some(Block {
                span: FrontmatterSpan {
                    start: bom_len,
                    end: bom_len + line.next,
                },
                yaml_start: bom_len + opening.next,
                yaml_end: bom_len + line.start,
                newline,
            });
        }
        cursor = line.next;
    }
}

/// One line of `text`, with byte offsets and no trailing `\n`.
struct Line<'a> {
    /// Byte offset of the line's first byte.
    start: usize,
    /// The line without its `\n`. A CRLF line still ends in `\r`.
    content: &'a str,
    /// Byte offset of the next line, or `text.len()` for a final line.
    next: usize,
}

/// Returns the line beginning at `from`, or `None` at end of input.
fn line_at(text: &str, from: usize) -> Option<Line<'_>> {
    if from >= text.len() {
        return None;
    }
    let rest = &text[from..];
    match rest.find('\n') {
        Some(offset) => Some(Line {
            start: from,
            content: &rest[..offset],
            next: from + offset + 1,
        }),
        None => Some(Line {
            start: from,
            content: rest,
            next: text.len(),
        }),
    }
}

/// Whether a line is a fence: `---`, optionally followed by whitespace.
fn is_fence(line: &str) -> bool {
    line.trim_end() == FENCE
}

/// Length of a leading UTF-8 BOM, if any.
fn bom_length(text: &str) -> usize {
    if text.starts_with('\u{feff}') {
        BOM_LEN
    } else {
        0
    }
}

/// The line's content without the `\r` of a CRLF ending.
fn without_cr(content: &str) -> &str {
    content.strip_suffix('\r').unwrap_or(content)
}

/// The 1-based line number of a byte offset.
fn line_number(text: &str, offset: usize) -> usize {
    text[..offset].bytes().filter(|byte| *byte == b'\n').count() + 1
}

/// The offset of the `:` that ends a key, or `None` when the line has no key.
///
/// A key's colon is followed by a space or ends the line; `a:b` is one scalar,
/// not a key and a value.
fn key_colon(content: &str) -> Option<usize> {
    let bytes = content.as_bytes();
    (1..bytes.len()).find(|index| {
        bytes[*index] == b':' && bytes.get(index + 1).is_none_or(u8::is_ascii_whitespace)
    })
}

/// The next line that is neither blank nor a comment, before the closing fence.
fn next_content_line<'a>(text: &'a str, from: usize, yaml_end: usize) -> Option<Line<'a>> {
    let mut cursor = from;
    while let Some(line) = line_at(text, cursor) {
        if line.start >= yaml_end {
            return None;
        }
        let content = without_cr(line.content);
        if !content.trim().is_empty() && !content.trim_start().starts_with('#') {
            return Some(line);
        }
        cursor = line.next;
    }
    None
}

/// The shape and span of the value on one key line.
fn value_on_line(
    text: &str,
    line: &Line<'_>,
    content: &str,
    colon: usize,
    yaml_end: usize,
) -> (ValueSpan, ValueShape) {
    let after = &content[colon + 1..];
    let value_start = colon + 1 + (after.len() - after.trim_start_matches([' ', '\t']).len());
    let value = &content[value_start..];
    let absolute = line.start + value_start;

    // A key with no value bytes is a null value, and one splice replaces it.
    // The empty span sits directly after the colon: the rendered value brings
    // its own separator space, so the spacing before a trailing comment stays
    // where the file put it.
    if value.is_empty() || value.starts_with('#') {
        let comment = if value.starts_with('#') {
            Some(absolute)
        } else {
            None
        };
        let empty = ValueSpan::new(line.start + colon + 1, 0, comment);
        return match block_shape(text, line.next, yaml_end) {
            Some(reason) => (empty, ValueShape::Unsupported(reason)),
            None => (empty, ValueShape::Scalar),
        };
    }

    let unsupported =
        |reason: &'static str| (ValueSpan::empty(absolute), ValueShape::Unsupported(reason));

    match value.as_bytes()[0] {
        b'|' | b'>' => unsupported(reason::MULTILINE_SCALAR),
        b'&' => unsupported(reason::ANCHOR),
        b'*' => unsupported(reason::ALIAS),
        b'{' => unsupported(reason::FLOW_MAPPING),
        b'!' | b'%' | b'`' | b'@' => unsupported(reason::NESTED),
        // `? x` and `- x` open a structure; `?x` and `-x` are scalars.
        b'?' | b'-' if value.len() == 1 || value.as_bytes()[1].is_ascii_whitespace() => {
            unsupported(reason::NESTED)
        }
        quote @ (b'"' | b'\'') => match quoted_end(value, quote) {
            Some(length) => delimited(content, line, value_start, length, ValueShape::Scalar),
            None => unsupported(reason::NO_VALUE),
        },
        b'[' => match flow_sequence_end(value) {
            Ok(length) => delimited(content, line, value_start, length, ValueShape::FlowSequence),
            Err(reason) => unsupported(reason),
        },
        _ => {
            let (length, comment) = plain_end(value);
            let comment = comment.map(|offset| line.start + value_start + offset);
            (
                ValueSpan::new(absolute, length, comment),
                ValueShape::Scalar,
            )
        }
    }
}

/// The span of a value whose end was found, plus the comment that follows it.
///
/// Anything but spaces and a comment after the value means the value could not
/// be delimited, so the key is refused rather than guessed at.
fn delimited(
    content: &str,
    line: &Line<'_>,
    value_start: usize,
    length: usize,
    shape: ValueShape,
) -> (ValueSpan, ValueShape) {
    let start = line.start + value_start;
    match tail_comment(content, value_start + length) {
        Ok(comment) => (
            ValueSpan::new(start, length, comment.map(|offset| line.start + offset)),
            shape,
        ),
        Err(()) => (
            ValueSpan::empty(start),
            ValueShape::Unsupported(reason::NESTED),
        ),
    }
}

/// The refusal reason for indented lines under a key with no value on its line.
fn block_shape(text: &str, from: usize, yaml_end: usize) -> Option<&'static str> {
    let next = next_content_line(text, from, yaml_end)?;
    let content = without_cr(next.content);
    if !content.starts_with([' ', '\t']) {
        return None;
    }
    let item = content.trim_start();
    let is_item = item == "-" || item.starts_with("- ");
    Some(if is_item {
        reason::BLOCK_SEQUENCE
    } else {
        reason::BLOCK_MAPPING
    })
}

/// The offset one past the closing quote of a quoted scalar, if it closes.
fn quoted_end(value: &str, quote: u8) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if quote == b'"' => index += 2,
            byte if byte == quote => {
                // `''` is one escaped quote inside a single-quoted scalar.
                if quote == b'\'' && bytes.get(index + 1) == Some(&b'\'') {
                    index += 2;
                    continue;
                }
                return Some(index + 1);
            }
            _ => index += 1,
        }
    }
    None
}

/// What follows a value: spaces, then an optional comment, and nothing else.
///
/// `Ok(Some(offset))` is the offset of the `#` within `content`.
fn tail_comment(content: &str, from: usize) -> Result<Option<usize>, ()> {
    let rest = &content[from..];
    let trimmed = rest.trim_start_matches([' ', '\t']);
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.starts_with('#') {
        return Ok(Some(from + rest.len() - trimmed.len()));
    }
    Err(())
}

/// The length of a plain scalar and the offset of a trailing comment.
fn plain_end(value: &str) -> (usize, Option<usize>) {
    let comment = value
        .char_indices()
        .find(|(index, c)| *c == '#' && (*index == 0 || value[..*index].ends_with([' ', '\t'])));
    let limit = match comment {
        Some((index, _)) => index,
        None => value.len(),
    };
    let length = value[..limit].trim_end_matches([' ', '\t']).len();
    (length, comment.map(|(index, _)| index))
}

/// The length of a flow sequence of scalars, or the reason it is refused.
fn flow_sequence_end(value: &str) -> Result<usize, &'static str> {
    let bytes = value.as_bytes();
    let mut quote: Option<u8> = None;
    let mut index = 1;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(open) => {
                if open == b'"' && byte == b'\\' {
                    index += 1;
                } else if byte == open {
                    quote = None;
                }
            }
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b']' => {
                    let inner = &value[1..index];
                    if inner.contains(['[', '{', '}', ':']) {
                        return Err(reason::NESTED);
                    }
                    return Ok(index + 1);
                }
                b'[' | b'{' | b'}' => return Err(reason::NESTED),
                _ => {}
            },
        }
        index += 1;
    }
    Err(reason::NO_VALUE)
}

/// Marks every key that is defined more than once at column 0.
///
/// Two definitions make the file's meaning depend on the parser, so neither
/// span can be spliced (M7-7).
fn mark_duplicates(keys: &mut [KeySpan]) {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for key in keys.iter() {
        *counts.entry(key.key.clone()).or_default() += 1;
    }
    for key in keys.iter_mut() {
        if counts.get(&key.key).is_some_and(|count| *count > 1) {
            key.shape = ValueShape::Unsupported(reason::DUPLICATE_KEY);
        }
    }
}
