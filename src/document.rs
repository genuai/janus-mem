//! The in-memory shape of a parsed document (SPEC §6.2, §6.3).
//!
//! Two layers, deliberately kept apart:
//!
//! * [`Frontmatter`] is the *typed view over a subset of keys*, filled straight
//!   from YAML with no inference. Every field is optional because a vault is
//!   allowed to omit anything.
//! * The resolved fields on [`Document`] (`title`, `type_name`, `status`,
//!   `date`, `scope`) are the result of applying the §6.3 hierarchy against a
//!   [`Config`](crate::config::Config). Resolution is a separate step so the
//!   raw view stays a faithful record of what the file said.
//!
//! Nothing here can fail loudly: an unparseable value degrades to a default and
//! records a [`DocNote`]. Reads fail *open*; only writes (M7) fail closed.

use std::fmt;
use std::path::PathBuf;
use std::time::SystemTime;

use chrono::NaiveDate;
use serde::Serialize;

use crate::config::TypeSpec;
use crate::id::{Blake3Hash, DocId};

/// One parsed Markdown document and everything the scan resolved about it.
#[derive(Debug, Clone)]
pub struct Document {
    /// Stable identity (§6.4). Derived on this scan; persisted only in `.janus/`.
    pub id: DocId,
    /// Which rung of the identity ladder produced [`Document::id`].
    pub id_source: IdentityTier,
    /// Absolute, canonical path.
    pub path: PathBuf,
    /// Vault-relative path, always `/`-separated, so output is OS-independent
    /// (§8.7). This is the only path form that may appear in JSON or the index.
    pub rel_path: String,
    /// Hash of the full file bytes; doubles as the etag for writes (D18).
    pub content_hash: Blake3Hash,
    /// Typed view over the recognised frontmatter keys. Never inferred.
    pub frontmatter: Frontmatter,
    /// The frontmatter block exactly as it appeared in the file, fences
    /// included, so the writer never re-serializes what it did not change
    /// (§7.3). `None` when the file has no frontmatter.
    pub raw_frontmatter: Option<String>,
    /// Markdown body, verbatim, starting just after the closing fence.
    pub body: String,
    /// Byte offsets of the frontmatter region within the file; serves the
    /// span-preserving patcher in M7.
    pub span: FrontmatterSpan,
    /// Filesystem modification time. Diagnostic only: never used for identity,
    /// and only a last-resort date source (§6.3).
    pub mtime: SystemTime,
    /// File size in bytes, as observed during the read.
    pub size: u64,

    // ── resolved metadata (phase 3 applies the hierarchy) ──
    /// Frontmatter title, else the filename stem (else the first H1 from M2).
    pub title: String,
    /// Canonical type name, e.g. `"decision"`. Kept alongside the spec because
    /// an unknown type legitimately keeps its own name (§6.6.1).
    pub type_name: String,
    /// Effective authority/decay behaviour for [`Document::type_name`].
    pub type_spec: TypeSpec,
    /// Declared status, defaulted and normalised per §6.3.
    pub status: Status,
    /// Authored date, if any source produced one.
    pub date: Option<NaiveDate>,
    /// Where [`Document::date`] came from, surfaced to the agent so it can judge
    /// confidence (§6.3).
    pub date_source: DateSource,
    /// Valid-time start (§6.10). Never derived from `mtime`.
    pub effective_from: Option<NaiveDate>,
    /// Valid-time end, exclusive (§6.10).
    pub effective_to: Option<NaiveDate>,
    /// Owning scope, normalised; `"global"` when absent (§6.12).
    pub scope: String,
    /// Retrieval tags, deduplicated and trimmed, in first-seen order.
    ///
    /// A set for matching purposes, but a `Vec` because a set has no order to
    /// preserve and output has to be deterministic. Never part of identity: a
    /// document sharing a tag must not be re-keyed by adding one (§6.12 rule 2).
    pub shared_with: Vec<String>,
    /// Everything that had to be defaulted, skipped or tolerated while
    /// producing this document.
    pub notes: Vec<DocNote>,
}

/// The region of the file occupied by its frontmatter.
///
/// Both offsets are byte indices into the original file, not character indices,
/// because the writer splices bytes (§7.3). For a file with no frontmatter both
/// offsets are `0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrontmatterSpan {
    /// Inclusive start offset of the opening fence (after any BOM).
    pub start: usize,
    /// Exclusive end offset, immediately after the closing fence's newline.
    pub end: usize,
}

impl FrontmatterSpan {
    /// Number of bytes covered by the frontmatter region.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// True when there is no frontmatter to patch.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The typed view over a subset of frontmatter keys (§6.3).
///
/// Every field is optional and nothing is inferred: this is what the file said,
/// not what the engine concluded. Unknown keys are not represented here at all
/// — they survive because [`Document::raw_frontmatter`] keeps the original text.
///
/// `Deserialize` is intentionally *not* derived. Frontmatter is read from an
/// `oneyaml::Value` map through the configurable alias lists, so a single bad
/// value (`doc_version: "2.0"`, `tags: rust`) degrades that one key instead of
/// discarding the whole document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(default)]
pub struct Frontmatter {
    /// Declared identity, if the author pinned one (§6.4, rung 1).
    pub id: Option<String>,
    /// Provenance; opaque, never inferred, not scored in v1.
    pub author: Option<String>,
    /// Authored title.
    pub title: Option<String>,
    /// Declared type. An open registry name, not an enum (§6.6.1).
    ///
    /// The `rename` keeps the serialized key `type`; without it serde would
    /// emit the raw identifier `r#type`, since that is the field's real name.
    #[serde(rename = "type")]
    pub r#type: Option<String>,
    /// Declared status, as written. Normalisation happens during resolution.
    pub status: Option<String>,
    /// Authored/decided date.
    pub date: Option<NaiveDate>,
    /// Valid-time start (§6.10).
    pub effective_from: Option<NaiveDate>,
    /// Valid-time end, exclusive (§6.10).
    pub effective_to: Option<NaiveDate>,
    /// Paths this document supersedes (§6.7).
    #[serde(default)]
    pub supersedes: Vec<String>,
    /// Paths that supersede this document, as declared (§6.7).
    #[serde(default)]
    pub superseded_by: Vec<String>,
    /// Human-facing content revision. A string because real vaults use `"2.0"`,
    /// `"v3"` and `"draft-2"`; it is a fallback match key, never identity (D26).
    pub doc_version: Option<String>,
    /// Frontmatter schema revision, distinct from both of the above (§6.8).
    pub schema_version: Option<u32>,
    /// Free-form tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Explicit opt-out of time decay; overrides the per-type default.
    pub timeless: Option<bool>,
    /// Owning scope; `None` means global (§6.12).
    pub scope: Option<String>,
    /// Retrieval tags only; never part of identity (§6.12, D43).
    #[serde(default)]
    pub shared_with: Vec<String>,
}

/// Where a document's date came from (§6.3).
///
/// Surfaced in every response so an agent can tell an authored date from a
/// filesystem accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DateSource {
    /// Parsed from a frontmatter date key.
    Frontmatter,
    /// Parsed from a `YYYY-MM-DD` prefix in the filename.
    Filename,
    /// Fell back to the filesystem modification time. The weakest source, and
    /// the only one excluded from identity (§6.4).
    Mtime,
    /// No date at all. The document is excluded from `as_of` queries (§6.10
    /// rule 9).
    #[default]
    None,
}

impl DateSource {
    /// The stable lowercase name used in JSON output (§9.4).
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Frontmatter => "frontmatter",
            Self::Filename => "filename",
            Self::Mtime => "mtime",
            Self::None => "none",
        }
    }
}

impl fmt::Display for DateSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A document's declared status (§6.3, §6.6).
///
/// Modelled as an enum because only these five values change behaviour. An
/// unrecognised value is not an error — it becomes [`Status::Unknown`] with a
/// 0.8× gate factor and a doctor note, so one typo cannot hide a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    /// Current and in force.
    #[default]
    Active,
    /// Work in progress; included by `current` at 0.7×.
    Draft,
    /// Replaced by a successor (§6.7).
    Superseded,
    /// Retired with no declared successor.
    Deprecated,
    /// Unrecognised declared value.
    Unknown,
}

impl Status {
    /// Parses a declared status, case-insensitively, mapping anything
    /// unrecognised to [`Status::Unknown`].
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "active" => Self::Active,
            "draft" => Self::Draft,
            "superseded" => Self::Superseded,
            "deprecated" => Self::Deprecated,
            _ => Self::Unknown,
        }
    }

    /// The stable lowercase name used in JSON output and frontmatter.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draft => "draft",
            Self::Superseded => "superseded",
            Self::Deprecated => "deprecated",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which rung of the identity ladder produced a [`DocId`] (§6.4).
///
/// Reported by `janus ids` so a user can see *why* a document has the identity
/// it has. Ordered strongest to weakest to match the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IdentityTier {
    /// Rung 1: a declared `id` (or an alias of it).
    DeclaredId,
    /// Rung 2: a byte-identical move — proof, not inference.
    ContentHash,
    /// Rung 3: `title + date`, namespaced by corpus and scope.
    TitleDate,
    /// Rung 4: `title + doc_version`, reached only when no date is available.
    TitleVersion,
    /// Rung 5: a globally unique title.
    Title,
    /// Rung 6: the vault-relative path — last resort, and fail-closed: such a
    /// document loses move continuity rather than risking a bad merge.
    RelPath,
}

impl IdentityTier {
    /// The stable lowercase name used in JSON output.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::DeclaredId => "declared_id",
            Self::ContentHash => "content_hash",
            Self::TitleDate => "title_date",
            Self::TitleVersion => "title_version",
            Self::Title => "title",
            Self::RelPath => "rel_path",
        }
    }
}

impl fmt::Display for IdentityTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One tolerated irregularity found while parsing a document.
///
/// Notes are how the engine keeps §6.3's "fail open on read" honest: the
/// document still parses, and the reason it parsed *differently than the file
/// suggests* is recorded rather than silently swallowed. `doctor` (M5) promotes
/// the serious ones into warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocNote {
    /// The declared type is not in the registry; weighted 0.80 (§6.6.1).
    UnknownType {
        /// The name as declared.
        value: String,
    },
    /// The declared status is not one of the five known values.
    UnknownStatus {
        /// The value as declared.
        value: String,
    },
    /// The scope did not match the allowed pattern and fell back to `global`
    /// (§6.12 rule 9).
    InvalidScope {
        /// The value as declared.
        value: String,
    },
    /// Frontmatter looked like YAML but could not be parsed at all. The raw
    /// block is still preserved for the writer.
    YamlUnparseable {
        /// Parser message, already human-readable.
        message: String,
    },
    /// Frontmatter parsed with a warning (duplicate key, leniency, …).
    YamlWarning {
        /// Parser message.
        message: String,
    },
    /// An opening fence had no matching closing fence, so the file was treated
    /// as body-only.
    NoClosingFence,
    /// The file is not valid UTF-8 and was skipped.
    NonUtf8Content,
    /// The path is not valid UTF-8 and was skipped.
    NonUtf8Path,
    /// The file could not be read (permissions, transient IO).
    Unreadable {
        /// Underlying error, already rendered.
        message: String,
    },
    /// The file changed while it was being read and retries were exhausted.
    ChangedDuringRead,
    /// A symlink pointed outside the corpus root and was skipped (§8.5).
    SymlinkEscape {
        /// The link target, as resolved.
        target: String,
    },
    /// A match key was ambiguous, so this document fell through to a weaker
    /// rung or to `rel_path` (§6.4).
    DuplicateIdentity {
        /// The ambiguous key, rendered for humans.
        key: String,
    },
    /// One block could not be split and is larger than the configured chunk
    /// limit (§6.5 rule 2). A fence, table, quote or single list item bigger
    /// than `max_chars` becomes one oversized chunk; cutting it would corrupt
    /// what the text means, so the exception is reported instead (CH-6).
    OversizedBlock {
        /// The chunk's length in characters.
        chars: usize,
    },
    /// The body is larger than `u32::MAX` bytes, so its chunk spans cannot be
    /// represented in the §6.5 shape. No Markdown document is that large; the
    /// note exists so the conversion is a decision, not a truncation.
    BodyTooLarge,
    /// The document produced more than `u16::MAX` chunks, so chunk ordinals
    /// saturate. Ordering stays stable; the last chunks share an ordinal.
    TooManyChunks,
}

impl DocNote {
    /// The stable machine-readable code used in JSON output.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownType { .. } => "unknown_type",
            Self::UnknownStatus { .. } => "unknown_status",
            Self::InvalidScope { .. } => "invalid_scope",
            Self::YamlUnparseable { .. } => "yaml_unparseable",
            Self::YamlWarning { .. } => "yaml_warning",
            Self::NoClosingFence => "no_closing_fence",
            Self::NonUtf8Content => "non_utf8_content",
            Self::NonUtf8Path => "non_utf8_path",
            Self::Unreadable { .. } => "unreadable",
            Self::ChangedDuringRead => "changed_during_read",
            Self::SymlinkEscape { .. } => "symlink_escape",
            Self::DuplicateIdentity { .. } => "duplicate_identity",
            Self::OversizedBlock { .. } => "oversized_block",
            Self::BodyTooLarge => "body_too_large",
            Self::TooManyChunks => "too_many_chunks",
        }
    }

    /// A one-line human explanation, including the offending value where there
    /// is one.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::UnknownType { value } => {
                format!("unknown type `{value}`; using the fallback weight 0.80")
            }
            Self::UnknownStatus { value } => {
                format!("unknown status `{value}`; using the fallback factor 0.8")
            }
            Self::InvalidScope { value } => {
                format!("invalid scope `{value}`; falling back to `global`")
            }
            Self::YamlUnparseable { message } => {
                format!("frontmatter could not be parsed: {message}")
            }
            Self::YamlWarning { message } => message.clone(),
            Self::NoClosingFence => {
                "frontmatter fence was never closed; treating the file as body-only".to_owned()
            }
            Self::NonUtf8Content => "file is not valid UTF-8; skipped".to_owned(),
            Self::NonUtf8Path => "path is not valid UTF-8; skipped".to_owned(),
            Self::Unreadable { message } => format!("could not read file: {message}"),
            Self::ChangedDuringRead => {
                "file kept changing while being read; skipped this pass".to_owned()
            }
            Self::SymlinkEscape { target } => {
                format!("symlink resolves outside the corpus ({target}); skipped")
            }
            Self::DuplicateIdentity { key } => {
                format!("identity key `{key}` was ambiguous; fell through to a weaker key")
            }
            Self::OversizedBlock { chars } => format!(
                "a block of {chars} characters could not be split; kept as one oversized chunk"
            ),
            Self::BodyTooLarge => {
                "body is larger than 4 GiB; chunk spans cannot address it, so it was not chunked"
                    .to_owned()
            }
            Self::TooManyChunks => {
                "more than 65535 chunks; ordinals saturate at the maximum".to_owned()
            }
        }
    }
}

impl fmt::Display for DocNote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parses_known_values_case_insensitively() {
        assert_eq!(Status::parse("ACTIVE"), Status::Active);
        assert_eq!(Status::parse("  Draft "), Status::Draft);
        assert_eq!(Status::parse("superseded"), Status::Superseded);
        assert_eq!(Status::parse("deprecated"), Status::Deprecated);
    }

    #[test]
    fn status_never_fails_on_unknown_values() {
        assert_eq!(Status::parse("archived"), Status::Unknown);
        assert_eq!(Status::parse(""), Status::Unknown);
    }

    #[test]
    fn status_display_round_trips_through_parse() {
        for status in [
            Status::Active,
            Status::Draft,
            Status::Superseded,
            Status::Deprecated,
        ] {
            assert_eq!(Status::parse(status.as_str()), status);
        }
    }

    #[test]
    fn identity_tiers_are_ordered_strongest_first() {
        let tiers = [
            IdentityTier::DeclaredId,
            IdentityTier::ContentHash,
            IdentityTier::TitleDate,
            IdentityTier::TitleVersion,
            IdentityTier::Title,
            IdentityTier::RelPath,
        ];
        let mut sorted = tiers;
        sorted.sort();
        assert_eq!(sorted, tiers, "ladder order must match SPEC §6.4");
    }

    #[test]
    fn frontmatter_serializes_type_without_raw_identifier_prefix() {
        let frontmatter = Frontmatter {
            r#type: Some("decision".to_owned()),
            ..Frontmatter::default()
        };
        let json = serde_json::to_string(&frontmatter).unwrap();
        assert!(json.contains("\"type\":\"decision\""), "{json}");
        assert!(!json.contains("r#type"), "{json}");
    }

    #[test]
    fn doc_notes_expose_a_stable_code() {
        assert_eq!(
            DocNote::UnknownStatus {
                value: "archived".to_owned()
            }
            .code(),
            "unknown_status"
        );
        assert_eq!(DocNote::NoClosingFence.code(), "no_closing_fence");
    }

    #[test]
    fn span_reports_length_and_emptiness() {
        let span = FrontmatterSpan { start: 3, end: 20 };
        assert_eq!(span.len(), 17);
        assert!(!span.is_empty());
        assert!(FrontmatterSpan::default().is_empty());
    }
}
