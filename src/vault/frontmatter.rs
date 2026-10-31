//! Frontmatter isolation and parsing (SPEC §6.3, §7.3).
//!
//! Two jobs, deliberately in this order:
//!
//! 1. **Isolate** the frontmatter region byte-exactly, so the writer of M7 never
//!    has to re-serialize what it did not change. The region is described by a
//!    [`FrontmatterSpan`], and the contract is:
//!
//!    ```text
//!    text[..span.start]   prefix — never touched (may be a UTF-8 BOM)
//!    text[span.start..span.end]   frontmatter, fences included
//!    text[span.end..]     body — verbatim
//!    ```
//!
//!    Nothing here normalizes line endings, strips trailing whitespace or
//!    reformats YAML. A patch has to show up as one changed line in `git diff`,
//!    so untouched bytes stay untouched (§8.6 rule 5).
//!
//! 2. **Read** a typed subset of it through the config's alias lists, coercing
//!    tolerantly and recording a [`DocNote`] for everything that had to be
//!    guessed.
//!
//! # Fail open, never fail
//!
//! Malformed YAML does not fail a scan. It produces an empty typed view plus a
//! note, and the raw text is still preserved for the writer. Reads fail open;
//! only writes (M7) fail closed. A vault full of hand-edited frontmatter is the
//! expected input, not an edge case.

use chrono::NaiveDate;
use oneyaml::{DuplicateKeys, Mapping, Settings, Value};

use crate::config::{Config, Field};
use crate::document::{DocNote, Frontmatter, FrontmatterSpan};

/// The fence that opens and closes a frontmatter block.
const FENCE: &str = "---";

/// Length of a UTF-8 BOM in bytes.
const BOM_LEN: usize = 3;

/// A document's frontmatter region and the typed view read from it.
///
/// All borrowed fields point into the text passed to [`read`], so nothing is
/// copied that the caller already owns.
#[derive(Debug)]
pub struct FrontmatterRead<'a> {
    /// Byte offsets of the frontmatter block within the file.
    pub span: FrontmatterSpan,
    /// The block exactly as written, fences included. `None` when the file has
    /// no frontmatter at all.
    pub raw: Option<&'a str>,
    /// Everything after the closing fence, verbatim. Excludes the BOM.
    pub body: &'a str,
    /// Bytes skipped before the block (0 or 3). Part of the prefix, never of the
    /// span.
    pub bom_len: usize,
    /// The typed view: what the file said, with nothing inferred.
    pub frontmatter: Frontmatter,
    /// Everything that had to be defaulted, coerced or tolerated.
    pub notes: Vec<DocNote>,
}

/// Isolates and reads a document's frontmatter.
///
/// `text` must already be valid UTF-8; rejecting non-UTF-8 files is the caller's
/// decision, because only the caller knows whether that is a skip or an error.
#[must_use]
pub fn read<'a>(text: &'a str, config: &Config) -> FrontmatterRead<'a> {
    let bom_len = bom_length(text);
    let mut notes = Vec::new();

    let Some(block) = isolate(text, bom_len, &mut notes) else {
        // No frontmatter: the whole document is body, and the span is an empty
        // region sitting *after* the BOM. Splicing a block in later therefore
        // preserves the BOM where it belongs — before the frontmatter.
        return FrontmatterRead {
            span: FrontmatterSpan {
                start: bom_len,
                end: bom_len,
            },
            raw: None,
            body: &text[bom_len..],
            bom_len,
            frontmatter: Frontmatter::default(),
            notes,
        };
    };

    let frontmatter = parse_into(block.yaml, config, &mut notes);

    FrontmatterRead {
        span: block.span,
        raw: Some(&text[block.span.start..block.span.end]),
        body: &text[block.span.end..],
        bom_len,
        frontmatter,
        notes,
    }
}

/// The located block: its span and the YAML text between the fences.
struct Block<'a> {
    span: FrontmatterSpan,
    yaml: &'a str,
}

/// Locates the `---` … `---` region, if the file starts with one.
///
/// Returns `None` for "no frontmatter", which is a normal, fully supported state
/// (§6.3) — a document without frontmatter is never mutated to add one.
fn isolate<'a>(text: &'a str, bom_len: usize, notes: &mut Vec<DocNote>) -> Option<Block<'a>> {
    let rest = &text[bom_len..];

    // The opening fence must be the first line. A `---` further down is a
    // Markdown setext heading or a thematic break, never frontmatter.
    let opening = line_at(rest, 0)?;
    if !is_fence(opening.content) {
        return None;
    }

    // Everything after the opening fence's newline is candidate YAML.
    let mut cursor = opening.next;
    loop {
        let Some(line) = line_at(rest, cursor) else {
            // The fence was never closed. Treating a truncated block as
            // frontmatter would silently swallow a document whose body contains
            // a horizontal rule, so the file is body-only and says so.
            notes.push(DocNote::NoClosingFence);
            return None;
        };

        if is_fence(line.content) {
            return Some(Block {
                span: FrontmatterSpan {
                    start: bom_len,
                    end: bom_len + line.next,
                },
                yaml: &rest[opening.next..line.start],
            });
        }

        cursor = line.next;
    }
}

/// One line of `text`, with byte offsets and no trailing `\n`.
struct Line<'a> {
    /// Byte offset of the line's first byte.
    start: usize,
    /// The line without its `\n` (a `\r` is still present on CRLF input).
    content: &'a str,
    /// Byte offset of the next line, or `text.len()` for a final line.
    next: usize,
}

/// Returns the line beginning at `from`, or `None` at end of input.
///
/// `from == text.len()` is `None` rather than an empty final line, so a file
/// ending in a newline does not appear to have a phantom last line.
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
///
/// `trim_end` handles both CRLF (`"---\r"`) and trailing spaces (`"---  "`),
/// which real editors produce. Anything else on the line disqualifies it, so
/// `----` and `--- # comment` are not fences.
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

/// Parses the YAML between the fences into the typed view.
///
/// Duplicate keys are tolerated with the last value winning: hand-edited
/// frontmatter hits this constantly, and refusing to read a document because a
/// key appears twice is a worse outcome than reading it and saying so.
fn parse_into(yaml: &str, config: &Config, notes: &mut Vec<DocNote>) -> Frontmatter {
    if yaml.trim().is_empty() {
        // `---\n---\n` is a legal, empty block.
        return Frontmatter::default();
    }

    let settings = Settings::new().duplicate_keys(DuplicateKeys::LastWins);

    let (value, diagnostics) = match oneyaml::from_str_with_diagnostics::<Value>(yaml, &settings) {
        Ok(parsed) => parsed,
        Err(error) => {
            notes.push(DocNote::YamlUnparseable {
                message: error.to_string(),
            });
            return Frontmatter::default();
        }
    };

    // Warnings are the parser telling us it leniently accepted something
    // (duplicate keys, `%YAML` directives). They are worth surfacing; they are
    // not worth failing over.
    for diagnostic in diagnostics {
        notes.push(DocNote::YamlWarning {
            message: diagnostic.message,
        });
    }

    let Some(mapping) = value.as_mapping() else {
        notes.push(DocNote::YamlUnparseable {
            message: format!("frontmatter is {}, not a mapping of keys", describe(&value)),
        });
        return Frontmatter::default();
    };

    from_mapping(mapping, config)
}

/// Builds the typed view from a YAML mapping through the config's alias lists.
///
/// Aliases are read in configured order with the first match winning, which is
/// what makes `date`/`created`/`decided` resolve predictably instead of by map
/// iteration order.
fn from_mapping(mapping: &Mapping, config: &Config) -> Frontmatter {
    let supersedes = string_list(mapping, config, Field::Supersedes);
    let superseded_by = string_list(mapping, config, Field::SupersededBy);

    Frontmatter {
        id: scalar_string(mapping, config, Field::Id),
        author: scalar_string(mapping, config, Field::Author),
        title: scalar_string(mapping, config, Field::Title),
        r#type: scalar_string(mapping, config, Field::Type),
        status: scalar_string(mapping, config, Field::Status),
        date: date_value(mapping, config, Field::Date),
        effective_from: date_value(mapping, config, Field::EffectiveFrom),
        effective_to: date_value(mapping, config, Field::EffectiveTo),
        supersedes,
        superseded_by,
        doc_version: scalar_string(mapping, config, Field::Version),
        schema_version: schema_version(mapping),
        tags: string_list(mapping, config, Field::Tags),
        // `timeless` is deliberately not aliasable: it is a janus-specific
        // switch, not a key other tools write under another spelling.
        timeless: mapping.get_str("timeless").and_then(as_bool),
        scope: scalar_string(mapping, config, Field::Scope),
        shared_with: string_list(mapping, config, Field::SharedWith),
    }
}

/// The first alias of `field` present in the mapping.
fn lookup<'a>(mapping: &'a Mapping, config: &Config, field: Field) -> Option<&'a Value> {
    config
        .aliases(field)
        .iter()
        .find_map(|alias| mapping.get_str(alias))
}

/// A single scalar rendered as a string, coercing numbers and booleans.
///
/// `id: 123` is a perfectly ordinary way to write an id, and `doc_version: 2.0`
/// parses as a float in YAML 1.2 — both must survive, because the alternative is
/// silently dropping identity evidence. Sequences and mappings are *not* coerced:
/// they mean the file said something structural where we expected a value.
fn scalar_string(mapping: &Mapping, config: &Config, field: Field) -> Option<String> {
    lookup(mapping, config, field).and_then(scalar_to_string)
}

/// One scalar to a string, or `None` for nulls and structures.
fn scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        // `null` means "explicitly absent", not the string "null". Structures
        // mean the file said something structural where a value was expected.
        // `Value` is `#[non_exhaustive]`, so the catch-all is required and also
        // does the right thing for any future variant.
        _ => None,
    }
}

/// A list of scalars, accepting a single scalar as a one-element list.
///
/// `supersedes: decisions/old.md` and `tags: rust` are both idiomatic in
/// hand-written frontmatter, and §6.3's own example writes `supersedes` as a
/// scalar. Rejecting them would make the engine unusable on exactly the vaults
/// it targets.
fn string_list(mapping: &Mapping, config: &Config, field: Field) -> Vec<String> {
    let Some(value) = lookup(mapping, config, field) else {
        return Vec::new();
    };

    match value {
        Value::Sequence(items) => items.iter().filter_map(scalar_to_string).collect(),
        Value::Null => Vec::new(),
        other => scalar_to_string(other).into_iter().collect(),
    }
}

/// A date, accepting a full timestamp by taking its date part.
///
/// Returns `None` for anything unparseable rather than failing the document; the
/// date hierarchy (§6.3) then falls through to the filename and finally to
/// `mtime`, and `date_source` reports which rung won.
fn date_value(mapping: &Mapping, config: &Config, field: Field) -> Option<NaiveDate> {
    parse_date(&scalar_string(mapping, config, field)?)
}

/// Parses `YYYY-MM-DD`, or the date part of an ISO-8601 timestamp.
#[must_use]
pub fn parse_date(text: &str) -> Option<NaiveDate> {
    let text = text.trim();

    // `date: 2026-10-29T10:00:00Z` is common enough in generated frontmatter
    // that refusing it would be pedantic. The first ten bytes are ASCII-safe to
    // slice only after confirming the shape.
    let date_part = if text.len() > 10 {
        text.get(..10).unwrap_or(text)
    } else {
        text
    };

    NaiveDate::parse_from_str(date_part, "%Y-%m-%d").ok()
}

/// A boolean, accepting the string spellings YAML 1.1 writers still use.
///
/// YAML 1.2 reads `yes` as a string, so a vault migrated from a 1.1-era tool
/// would otherwise lose `timeless: yes` silently.
fn as_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(flag) => Some(*flag),
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Some(true),
            "false" | "no" | "off" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// `schema_version` is deliberately unaliased: it is janus-adjacent metadata
/// with one spelling, and silently accepting `schemaVersion` or `schema` would
/// invent a vocabulary nobody agreed to.
fn schema_version(mapping: &Mapping) -> Option<u32> {
    let value = mapping.get_str("schema_version")?;
    u32::try_from(value.as_u64()?).ok()
}

/// A short description of a value's shape, for error messages.
fn describe(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Sequence(_) => "a list",
        Value::Mapping(_) => "a mapping",
        Value::Tagged(_) => "a tagged value",
        _ => "an unsupported value",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn config() -> Config {
        Config::from_toml("", Path::new("<test>")).expect("default config must parse")
    }

    /// An owned copy of a [`FrontmatterRead`], so tests can return it without
    /// carrying the borrow of the input text.
    struct Read {
        span: FrontmatterSpan,
        raw: Option<String>,
        body: String,
        bom_len: usize,
        frontmatter: Frontmatter,
        notes: Vec<DocNote>,
    }

    /// Reads with the default config, asserting the byte contract holds:
    /// prefix, span and body must partition the input exactly, and `raw` and
    /// `body` must be the corresponding slices of it.
    fn read_str(text: &str) -> Read {
        let config = config();
        let parsed = read(text, &config);

        assert_eq!(
            parsed.bom_len + parsed.span.len() + parsed.body.len(),
            text.len(),
            "prefix, span and body must partition the text"
        );
        assert_eq!(
            parsed.span.end,
            parsed.bom_len + parsed.span.len(),
            "span must follow the BOM"
        );
        assert_eq!(&text[parsed.span.end..], parsed.body);
        if let Some(raw) = parsed.raw {
            assert_eq!(&text[parsed.span.start..parsed.span.end], raw);
        }

        Read {
            span: parsed.span,
            raw: parsed.raw.map(str::to_owned),
            body: parsed.body.to_owned(),
            bom_len: parsed.bom_len,
            frontmatter: parsed.frontmatter,
            notes: parsed.notes,
        }
    }

    // ──────────────────────────── isolation ────────────────────────────

    #[test]
    fn a_file_without_frontmatter_is_all_body() {
        let text = "# Just a heading\n\nSome prose.\n";
        let parsed = read_str(text);

        assert!(parsed.raw.is_none());
        assert_eq!(parsed.body, text);
        assert_eq!(parsed.span.len(), 0);
        assert!(parsed.notes.is_empty());
        assert_eq!(parsed.frontmatter, Frontmatter::default());
    }

    #[test]
    fn a_plain_block_is_isolated_byte_exactly() {
        let text = "---\ntitle: Hello\ntags: [a, b]\n---\n\n# Body\n";
        let parsed = read_str(text);

        assert_eq!(
            parsed.raw.as_deref(),
            Some("---\ntitle: Hello\ntags: [a, b]\n---\n")
        );
        assert_eq!(parsed.body, "\n# Body\n");
        assert_eq!(parsed.frontmatter.title.as_deref(), Some("Hello"));
        assert_eq!(parsed.frontmatter.tags, ["a", "b"]);
    }

    #[test]
    fn crlf_is_preserved_verbatim() {
        let text = "---\r\ntitle: Windows\r\n---\r\n\r\n# Body\r\n";
        let parsed = read_str(text);

        assert_eq!(
            parsed.raw.as_deref(),
            Some("---\r\ntitle: Windows\r\n---\r\n"),
            "CRLF must survive byte-for-byte; M7 splices these bytes back"
        );
        assert_eq!(parsed.body, "\r\n# Body\r\n");
        assert_eq!(parsed.frontmatter.title.as_deref(), Some("Windows"));
    }

    #[test]
    fn a_bom_is_preserved_outside_the_span() {
        let text = "\u{feff}---\ntitle: Bom\n---\nbody\n";
        let parsed = read_str(text);

        assert_eq!(parsed.bom_len, BOM_LEN);
        assert_eq!(
            parsed.span.start, BOM_LEN,
            "the BOM is prefix, not frontmatter"
        );
        assert_eq!(parsed.raw.as_deref(), Some("---\ntitle: Bom\n---\n"));
        assert_eq!(parsed.body, "body\n");
        assert_eq!(parsed.frontmatter.title.as_deref(), Some("Bom"));
    }

    #[test]
    fn a_bom_without_frontmatter_keeps_the_body_intact() {
        let text = "\u{feff}# Body\n";
        let parsed = read_str(text);

        assert_eq!(parsed.bom_len, BOM_LEN);
        assert_eq!(parsed.span.start, BOM_LEN);
        assert_eq!(parsed.body, "# Body\n");
    }

    #[test]
    fn trailing_whitespace_after_a_fence_is_accepted() {
        for text in [
            "--- \ntitle: x\n---\nbody",
            "---\ntitle: x\n--- \nbody",
            "---\t\r\ntitle: x\r\n---\r\nbody",
        ] {
            let parsed = read_str(text);
            assert_eq!(
                parsed.frontmatter.title.as_deref(),
                Some("x"),
                "should parse: {text:?}"
            );
        }
    }

    #[test]
    fn an_unclosed_fence_makes_the_file_body_only() {
        let text = "---\ntitle: x\n\n# Body\n";
        let parsed = read_str(text);

        assert!(
            parsed.raw.is_none(),
            "a truncated block must not be frontmatter"
        );
        assert_eq!(parsed.body, text);
        assert!(parsed.notes.contains(&DocNote::NoClosingFence));
        assert_eq!(parsed.frontmatter.title, None);
    }

    #[test]
    fn a_lone_fence_with_no_newline_is_body() {
        let parsed = read_str("---");
        assert!(parsed.raw.is_none());
        assert_eq!(parsed.body, "---");
        assert!(parsed.notes.contains(&DocNote::NoClosingFence));
    }

    #[test]
    fn a_fence_that_is_not_the_first_line_is_not_frontmatter() {
        let text = "# Title\n\n---\n\nProse.\n";
        let parsed = read_str(text);

        assert!(parsed.raw.is_none());
        assert_eq!(parsed.body, text);
        assert!(parsed.notes.is_empty());
    }

    #[test]
    fn horizontal_rules_in_the_body_do_not_confuse_the_parser() {
        let text = "---\ntitle: x\n---\n\nAbove.\n\n---\n\nBelow.\n";
        let parsed = read_str(text);

        assert_eq!(parsed.raw.as_deref(), Some("---\ntitle: x\n---\n"));
        assert_eq!(parsed.body, "\nAbove.\n\n---\n\nBelow.\n");
    }

    #[test]
    fn an_empty_block_is_valid() {
        let parsed = read_str("---\n---\nbody");

        assert_eq!(parsed.raw.as_deref(), Some("---\n---\n"));
        assert_eq!(parsed.body, "body");
        assert_eq!(parsed.frontmatter, Frontmatter::default());
        assert!(parsed.notes.is_empty());
    }

    #[test]
    fn an_empty_file_has_no_frontmatter() {
        let parsed = read_str("");
        assert!(parsed.raw.is_none());
        assert_eq!(parsed.body, "");
        assert!(parsed.notes.is_empty());
    }

    #[test]
    fn multibyte_text_around_the_fences_is_safe() {
        let text = "---\ntitle: 日本語のメモ 🎌\n---\n🦀 body ✨\n";
        let parsed = read_str(text);

        assert_eq!(parsed.frontmatter.title.as_deref(), Some("日本語のメモ 🎌"));
        assert_eq!(parsed.body, "🦀 body ✨\n");
    }

    // ───────────────────────────── parsing ─────────────────────────────

    #[test]
    fn yaml_comments_are_accepted() {
        let text = "---\n# a comment\ntitle: x  # trailing\n---\n";
        let parsed = read_str(text);
        assert_eq!(parsed.frontmatter.title.as_deref(), Some("x"));
        assert!(parsed.notes.is_empty(), "{:?}", parsed.notes);
    }

    #[test]
    fn duplicate_keys_are_tolerated_last_wins() {
        let text = "---\ntitle: first\ntitle: second\n---\n";
        let parsed = read_str(text);

        assert_eq!(parsed.frontmatter.title.as_deref(), Some("second"));
        assert!(
            parsed
                .notes
                .iter()
                .any(|note| note.code() == "yaml_warning"),
            "the duplicate must be reported, not silently resolved: {:?}",
            parsed.notes
        );
    }

    #[test]
    fn unparseable_yaml_fails_open_with_a_note() {
        let text = "---\ntitle: [unclosed\n---\nbody";
        let parsed = read_str(text);

        assert_eq!(parsed.frontmatter, Frontmatter::default());
        assert!(
            parsed
                .notes
                .iter()
                .any(|note| note.code() == "yaml_unparseable"),
            "{:?}",
            parsed.notes
        );
        assert_eq!(
            parsed.raw.as_deref(),
            Some("---\ntitle: [unclosed\n---\n"),
            "the raw block must still be preserved for the writer"
        );
    }

    #[test]
    fn a_non_mapping_block_fails_open() {
        let parsed = read_str("---\n- just\n- a list\n---\nbody");

        assert_eq!(parsed.frontmatter, Frontmatter::default());
        assert!(
            parsed
                .notes
                .iter()
                .any(|note| note.code() == "yaml_unparseable")
        );
    }

    // ───────────────────────────── coercion ─────────────────────────────

    #[test]
    fn scalars_are_coerced_to_strings() {
        let text = "---\nid: 123\ndoc_version: 2.0\ntitle: plain\n---\n";
        let parsed = read_str(text);

        assert_eq!(parsed.frontmatter.id.as_deref(), Some("123"));
        assert_eq!(parsed.frontmatter.doc_version.as_deref(), Some("2.0"));
        assert_eq!(parsed.frontmatter.title.as_deref(), Some("plain"));
    }

    #[test]
    fn a_quoted_version_stays_verbatim() {
        let parsed = read_str("---\ndoc_version: \"2.0\"\n---\n");
        assert_eq!(parsed.frontmatter.doc_version.as_deref(), Some("2.0"));
    }

    #[test]
    fn a_scalar_supersedes_becomes_a_one_element_list() {
        // §6.3's own example writes a scalar here.
        let parsed = read_str("---\nsupersedes: decisions/2024-11-02-postgres-cache.md\n---\n");
        assert_eq!(
            parsed.frontmatter.supersedes,
            ["decisions/2024-11-02-postgres-cache.md"]
        );
    }

    #[test]
    fn a_scalar_tag_becomes_a_one_element_list() {
        let parsed = read_str("---\ntags: rust\n---\n");
        assert_eq!(parsed.frontmatter.tags, ["rust"]);
    }

    #[test]
    fn a_sequence_of_non_scalars_skips_the_structures() {
        let parsed = read_str("---\ntags: [ok, {nested: map}, 7]\n---\n");
        assert_eq!(parsed.frontmatter.tags, ["ok", "7"]);
    }

    #[test]
    fn nulls_mean_absent() {
        let parsed = read_str("---\ntitle: null\nsuperseded_by: null\n---\n");
        assert_eq!(parsed.frontmatter.title, None);
        assert!(parsed.frontmatter.superseded_by.is_empty());
    }

    #[test]
    fn dates_parse_from_plain_and_timestamp_forms() {
        let parsed = read_str("---\ndate: 2026-10-29\neffective_to: 2027-01-01T00:00:00Z\n---\n");

        assert_eq!(
            parsed.frontmatter.date,
            NaiveDate::from_ymd_opt(2026, 10, 29)
        );
        assert_eq!(
            parsed.frontmatter.effective_to,
            NaiveDate::from_ymd_opt(2027, 1, 1),
            "an ISO timestamp should contribute its date part"
        );
    }

    #[test]
    fn an_invalid_date_is_absent_not_fatal() {
        let parsed = read_str("---\ndate: someday\ntitle: still readable\n---\n");

        assert_eq!(parsed.frontmatter.date, None);
        assert_eq!(parsed.frontmatter.title.as_deref(), Some("still readable"));
    }

    #[test]
    fn parse_date_rejects_partial_and_absurd_dates() {
        assert_eq!(
            parse_date("2026-10-29"),
            NaiveDate::from_ymd_opt(2026, 10, 29)
        );
        assert_eq!(
            parse_date("2026-10-29T10:00:00Z"),
            NaiveDate::from_ymd_opt(2026, 10, 29)
        );
        assert_eq!(parse_date("2026-10"), None);
        assert_eq!(parse_date("2026-02-30"), None, "30 February is not a date");
        assert_eq!(parse_date(""), None);
    }

    #[test]
    fn timeless_accepts_yaml_1_1_spellings() {
        for (text, expected) in [
            ("yes", true),
            ("true", true),
            ("no", false),
            ("false", false),
        ] {
            let parsed = read_str(&format!("---\ntimeless: {text}\n---\n"));
            assert_eq!(
                parsed.frontmatter.timeless,
                Some(expected),
                "timeless: {text}"
            );
        }
    }

    #[test]
    fn schema_version_must_be_a_number() {
        assert_eq!(
            read_str("---\nschema_version: 1\n---\n")
                .frontmatter
                .schema_version,
            Some(1)
        );
        assert_eq!(
            read_str("---\nschema_version: \"one\"\n---\n")
                .frontmatter
                .schema_version,
            None
        );
    }

    #[test]
    fn aliases_are_matched_in_configured_order() {
        let config = config();
        let parsed = read("---\ndate: 2026-01-01\ncreated: 2020-05-05\n---\n", &config);
        assert_eq!(
            parsed.frontmatter.date,
            NaiveDate::from_ymd_opt(2026, 1, 1),
            "`date` comes before `created` in the default alias list"
        );
    }

    #[test]
    fn a_configured_alias_override_is_honoured() {
        let config = Config::from_toml(
            "[frontmatter]\ntitle = [\"headline\"]\n",
            Path::new("<test>"),
        )
        .unwrap();

        let parsed = read("---\nheadline: from alias\ntitle: ignored\n---\n", &config);
        assert_eq!(parsed.frontmatter.title.as_deref(), Some("from alias"));
    }

    #[test]
    fn unknown_keys_are_ignored_but_preserved_in_raw() {
        let text = "---\nfuture_key: 42\ntitle: x\n---\n";
        let parsed = read_str(text);

        assert_eq!(parsed.frontmatter.title.as_deref(), Some("x"));
        assert!(
            parsed.raw.unwrap().contains("future_key: 42"),
            "unknown keys survive because raw does"
        );
    }
}
