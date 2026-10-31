//! Corpus config: `<root>/janus.toml` (SPEC §6.1, §6.6.1, §6.12, §16.1).
//!
//! # Two representations, on purpose
//!
//! `RawConfig` mirrors the TOML file one-for-one: every field is optional and
//! every key the user might omit has a serde default. It is a *faithful record
//! of what the file said*, and it is private.
//!
//! [`Config`] is what the rest of the engine consumes: aliases resolved into
//! fixed lists, authority weights merged over the built-in defaults, glob
//! patterns compiled once at load, and type resolution ready to run per
//! document. Turning one into the other is `parse, don't validate`: after
//! construction a `Config` cannot contain an uncompilable glob or an
//! unresolvable alias list, so no later code needs to re-check.
//!
//! # Merge rule
//!
//! Maps merge over the built-in defaults; lists replace them.
//!
//! * `[types]`, `[authority]`, `[decay.by_type]` — merged, so overriding one
//!   weight does not silently drop the other six.
//! * `[frontmatter]`, `[paths]`, `[index] exclude`, `[decay] timeless_types` —
//!   replaced, so a user can genuinely *remove* a default (`timeless_types =
//!   ["principle"]` stops treating decisions as timeless).
//!
//! # Never fatal
//!
//! Unknown keys are tolerated (forward compatibility), and questionable values
//! produce [`ConfigWarning`]s rather than errors. Refusing to start on a
//! slightly odd config would be a worse failure mode than starting and saying
//! what looked wrong.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use globset::{Glob, GlobMatcher, GlobSet, GlobSetBuilder};
use serde::Deserialize;

use crate::document::Status;
use crate::error::JanusError;

/// The corpus config's filename, at the corpus root (§6.1).
pub const CONFIG_FILE: &str = "janus.toml";

/// The directory `.janus/` — index sidecar, always excluded (§8.6 rule 1).
pub const JANUS_DIR: &str = ".janus";

/// Directories the walk never descends into, whatever the config says.
///
/// `.janus/` and `.git/` are deliberately *not* configurable: an index that
/// indexes itself, or a walk that reads git's object database, is never what
/// anyone meant (§6.1, §8.6 rule 1). The rest are the conventional build and
/// editor noise of a Markdown vault.
pub const ALWAYS_EXCLUDED_DIRS: [&str; 5] =
    [JANUS_DIR, ".git", "target", "node_modules", ".obsidian"];

/// The two directories that are forbidden rather than merely ignored.
///
/// `.janus/` is janus-mem's own sidecar and `.git/` belongs to git; neither may
/// ever be read as vault content or written to (§8.6 rule 1). The difference
/// from [`ALWAYS_EXCLUDED_DIRS`] is severity: touching these is a refusal
/// (exit 4), not a skip.
pub const FORBIDDEN_DIRS: [&str; 2] = [JANUS_DIR, ".git"];

/// Authority applied to a type that is not in the registry (§6.6.1):
/// fail-low, never fail-silent.
pub const UNKNOWN_TYPE_AUTHORITY: f64 = 0.80;

/// The shipped example config, written by `janus init` and shipped at the repo
/// root (§8.1).
const DEFAULT_CONFIG: &str = include_str!("../janus.toml");

/// Default number of days at which relevance halves for decaying types (§16.1).
const DEFAULT_HALF_LIFE_DAYS: f64 = 180.0;

/// Upper bound on alias-chain length. A vault with a 16-deep alias chain is not
/// expressing vocabulary, it is expressing a bug; the cap turns a potential
/// infinite loop into a deterministic fallback.
const MAX_ALIAS_HOPS: usize = 16;

// ───────────────────────────── resolved config ─────────────────────────────

/// A fully resolved corpus config. Cheap to read, expensive to build — build
/// once per process (§8.4).
#[derive(Debug, Clone)]
pub struct Config {
    /// Declared, stable corpus identity (§6.11). Never derived from a path.
    pub corpus_id: String,
    /// Index location, relative to the corpus root unless absolute.
    pub index_dir: PathBuf,
    /// Fallbacks applied when frontmatter declares nothing (§6.3).
    pub defaults: Defaults,
    /// Decay parameters (§6.6).
    pub decay: DecayConfig,
    /// Chunking parameters, consumed in M2 (§6.5).
    pub chunking: ChunkingConfig,
    /// Embedding parameters, consumed in M4.
    pub embedding: EmbeddingConfig,
    /// MCP-server parameters, consumed in M6.
    pub mcp: McpConfig,
    /// Everything odd but survivable about the file itself.
    pub warnings: Vec<ConfigWarning>,
    /// Frontmatter key aliases, indexed by [`Field`].
    aliases: Aliases,
    /// Alias → canonical type (§6.6.1). Merged over the defaults.
    types: BTreeMap<String, String>,
    /// Canonical type → authority weight. Merged over the defaults.
    authority: BTreeMap<String, f64>,
    /// Path hints, most specific first (§6.3, D33).
    path_hints: Vec<PathHint>,
    /// Exclusion globs against the vault-relative path (§6.1).
    excludes: GlobSet,
}

impl Config {
    /// Loads and resolves `<root>/janus.toml`.
    ///
    /// # Errors
    ///
    /// [`JanusError::ConfigMissing`] when the file is absent — config is
    /// required, because guessing a vault's conventions would silently misread
    /// the whole corpus (D47). [`JanusError::ConfigRead`] for IO or non-UTF-8,
    /// [`JanusError::ConfigParse`] for invalid TOML.
    pub fn load(root: &Path) -> Result<Self, JanusError> {
        let path = root.join(CONFIG_FILE);

        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Err(JanusError::ConfigMissing { path });
            }
            Err(source) => {
                return Err(JanusError::ConfigRead { path, source });
            }
        };

        let text = String::from_utf8(bytes).map_err(|err| JanusError::ConfigRead {
            path: path.clone(),
            source: io::Error::new(io::ErrorKind::InvalidData, err),
        })?;

        Self::from_toml(&text, &path)
    }

    /// Resolves config text.
    ///
    /// `origin` is used only to make error messages point somewhere useful, so
    /// tests can pass an inline name instead of creating files.
    ///
    /// # Errors
    ///
    /// [`JanusError::ConfigParse`] when the text is not valid TOML or a known
    /// key has the wrong type. Unknown keys are never an error.
    pub fn from_toml(text: &str, origin: &Path) -> Result<Self, JanusError> {
        let raw: RawConfig =
            toml::from_str(strip_bom(text)).map_err(|source| JanusError::ConfigParse {
                path: origin.to_path_buf(),
                source,
            })?;

        Ok(Self::from_raw(raw))
    }

    /// The config `janus init` writes, parsed — a useful self-check that the
    /// shipped example stays valid.
    ///
    /// # Errors
    ///
    /// Only if the compiled-in example is broken, which a test asserts.
    pub fn shipped_example() -> Result<Self, JanusError> {
        Self::from_toml(DEFAULT_CONFIG, Path::new("<shipped janus.toml>"))
    }

    /// The aliases configured for one frontmatter field, in match order.
    #[must_use]
    pub fn aliases(&self, field: Field) -> &[String] {
        self.aliases.get(field)
    }

    /// The alias → canonical type map, for inspection and tests.
    #[must_use]
    pub fn type_aliases(&self) -> &BTreeMap<String, String> {
        &self.types
    }

    /// The canonical type → authority map, for inspection and tests.
    #[must_use]
    pub fn authority_weights(&self) -> &BTreeMap<String, f64> {
        &self.authority
    }

    /// Resolves a declared type name to its canonical name and behaviour.
    ///
    /// Follows alias chains (`adr → decision`), and treats anything unlisted as
    /// a legitimate new type with the fallback weight — types are an open
    /// registry, so an unknown name is information, not an error (§6.6.1).
    ///
    /// Returns [`Cow::Borrowed`] in both branches: the canonical name is
    /// borrowed from the config, the unknown name straight from the caller's
    /// string. Per-document resolution therefore allocates nothing.
    #[must_use]
    pub fn resolve_type<'a>(&'a self, declared: &'a str) -> ResolvedType<'a> {
        let mut name: &'a str = declared.trim();

        let mut seen: [&'a str; MAX_ALIAS_HOPS] = [""; MAX_ALIAS_HOPS];
        seen[0] = name;
        let mut hops = 1;
        while let Some(canonical) = self.types.get(name) {
            if hops == MAX_ALIAS_HOPS || seen[..hops].contains(&canonical.as_str()) {
                // A cycle or an absurd chain. `from_raw` already reported it;
                // stop here with whatever name we reached.
                break;
            }
            name = canonical.as_str();
            seen[hops] = name;
            hops += 1;
        }

        let known = self.authority.contains_key(name);
        let authority = self
            .authority
            .get(name)
            .copied()
            .unwrap_or(UNKNOWN_TYPE_AUTHORITY);

        ResolvedType {
            name: Cow::Borrowed(name),
            spec: TypeSpec {
                authority,
                timeless: self.decay.timeless_types.contains(name),
                half_life_days: self.decay.half_life_for(name),
            },
            known,
        }
    }

    /// Resolves the configured default type, used when frontmatter is silent.
    #[must_use]
    pub fn default_type(&self) -> ResolvedType<'_> {
        self.resolve_type(&self.defaults.type_name)
    }

    /// The type implied by a document's location, if any.
    ///
    /// Consulted only when frontmatter declares no type (§6.3). Most specific
    /// pattern wins, so a nested hint can refine a broader one.
    #[must_use]
    pub fn path_type_hint(&self, rel_path: &str) -> Option<&str> {
        self.path_hints
            .iter()
            .find(|hint| hint.matches(rel_path))
            .map(|hint| hint.type_name.as_str())
    }

    /// Whether `[index] exclude` hides this vault-relative path.
    ///
    /// `rel_path` must use `/` separators (§8.7). Always-excluded directories
    /// are handled separately by [`is_always_excluded_dir`], because they are
    /// pruned during the walk rather than tested per file.
    #[must_use]
    pub fn is_excluded(&self, rel_path: &str) -> bool {
        self.excludes.is_match(rel_path)
    }

    /// Builds the resolved config from the parsed file.
    fn from_raw(raw: RawConfig) -> Self {
        let mut warnings = Vec::new();

        let corpus_id = raw.corpus_id.unwrap_or_else(default_corpus_id);
        validate_corpus_id(&corpus_id, &mut warnings);

        let index_dir = PathBuf::from(
            raw.index_dir
                .filter(|dir| !dir.trim().is_empty())
                .unwrap_or_else(|| JANUS_DIR.to_owned()),
        );

        let aliases = Aliases::resolve(&raw.frontmatter);

        let defaults = Defaults {
            type_name: raw
                .defaults
                .r#type
                .unwrap_or_else(|| "note".to_owned())
                .trim()
                .to_owned(),
            status: raw
                .defaults
                .status
                .as_deref()
                .map_or(Status::Active, Status::parse),
        };

        let mut types: BTreeMap<String, String> = default_type_aliases();
        types.extend(raw.types);
        report_alias_cycles(&types, &mut warnings);

        let mut authority = default_authority();
        authority.extend(raw.authority);
        for (type_name, weight) in &authority {
            // NaN is explicitly rejected too: it compares false against every
            // bound, so it would otherwise poison every score derived from it.
            if weight.is_nan() || *weight <= 0.0 {
                warnings.push(ConfigWarning::NonPositiveAuthority {
                    type_name: type_name.clone(),
                    weight: *weight,
                });
            }
        }

        let half_life_days = raw
            .decay
            .half_life_days
            .filter(|days| *days >= 0.0)
            .unwrap_or(DEFAULT_HALF_LIFE_DAYS);
        if let Some(days) = raw.decay.half_life_days
            && days < 0.0
        {
            warnings.push(ConfigWarning::NegativeHalfLife {
                scope: "decay.half_life_days".to_owned(),
                days,
            });
        }

        let mut by_type: BTreeMap<String, f64> = default_decay_by_type();
        by_type.extend(raw.decay.by_type);
        for (type_name, days) in &by_type {
            if *days < 0.0 {
                warnings.push(ConfigWarning::NegativeHalfLife {
                    scope: format!("decay.by_type.{type_name}"),
                    days: *days,
                });
            }
        }

        let decay = DecayConfig {
            half_life_days,
            timeless_types: raw
                .decay
                .timeless_types
                .map_or_else(default_timeless_types, |types| {
                    types
                        .into_iter()
                        .map(|name| name.trim().to_owned())
                        .collect()
                }),
            by_type,
        };

        let path_hints = build_path_hints(raw.paths, &mut warnings);
        let excludes = build_excludes(raw.index.exclude.as_deref().unwrap_or(&[]), &mut warnings);

        Self {
            corpus_id,
            index_dir,
            defaults,
            decay,
            chunking: ChunkingConfig {
                max_chars: raw.chunking.max_chars.unwrap_or(1800),
                overlap: raw.chunking.overlap.unwrap_or(0.15),
            },
            embedding: EmbeddingConfig {
                enabled: raw.embedding.enabled.unwrap_or(true),
                model: raw
                    .embedding
                    .model
                    .unwrap_or_else(|| "AllMiniLML6V2".to_owned()),
                batch_size: raw.embedding.batch_size.unwrap_or(32),
            },
            mcp: McpConfig {
                allow_write: raw.mcp.allow_write.unwrap_or(false),
                max_chars: raw.mcp.max_chars.unwrap_or(4000),
            },
            warnings,
            aliases,
            types,
            authority,
            path_hints,
            excludes,
        }
    }
}

/// Behaviours a canonical type carries (§6.6.1), resolved once at config load
/// and handed to the scorer so scoring never re-reads config.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TypeSpec {
    /// Multiplier applied after relevance.
    pub authority: f64,
    /// Exempt from time decay.
    pub timeless: bool,
    /// Days at which relevance halves; `<= 0` also disables decay.
    pub half_life_days: f64,
}

impl TypeSpec {
    /// Whether documents of this type lose relevance over time (§7.2).
    #[must_use]
    pub fn decays(&self) -> bool {
        !self.timeless && self.half_life_days > 0.0
    }
}

impl Default for TypeSpec {
    /// The unknown-type fallback. Used where no config is available — tests,
    /// and defensive defaults — never in a real scan.
    fn default() -> Self {
        Self {
            authority: UNKNOWN_TYPE_AUTHORITY,
            timeless: false,
            half_life_days: DEFAULT_HALF_LIFE_DAYS,
        }
    }
}

/// A declared type name resolved to its canonical name and behaviour.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedType<'a> {
    /// Canonical name after following aliases.
    pub name: Cow<'a, str>,
    /// Authority and decay behaviour.
    pub spec: TypeSpec,
    /// Whether the canonical name is in the authority registry. `false` means
    /// the fallback weight applies and a `doctor` note is due (§6.6.1).
    pub known: bool,
}

/// Fallbacks for keys the frontmatter leaves out (§6.3).
#[derive(Debug, Clone)]
pub struct Defaults {
    /// Type used when nothing declares one. `note` unless configured otherwise.
    pub type_name: String,
    /// Status used when nothing declares one. `active`, never `draft`.
    pub status: Status,
}

/// Decay parameters (§6.6).
#[derive(Debug, Clone)]
pub struct DecayConfig {
    /// Global half-life in days; `0` disables decay for every type.
    pub half_life_days: f64,
    /// Types exempt from decay regardless of their half-life.
    pub timeless_types: BTreeSet<String>,
    /// Per-type half-life overrides.
    by_type: BTreeMap<String, f64>,
}

impl DecayConfig {
    /// Half-life for a canonical type: the per-type override, else the global
    /// default.
    #[must_use]
    pub fn half_life_for(&self, type_name: &str) -> f64 {
        self.by_type
            .get(type_name)
            .copied()
            .unwrap_or(self.half_life_days)
    }

    /// Per-type overrides, for inspection and tests.
    #[must_use]
    pub fn overrides(&self) -> &BTreeMap<String, f64> {
        &self.by_type
    }
}

/// Chunking parameters (§6.5). Consumed in M2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChunkingConfig {
    /// Soft ceiling on chunk length in characters.
    pub max_chars: usize,
    /// Fraction of a chunk repeated into the next one when splitting.
    pub overlap: f64,
}

/// Embedding parameters (§8.2). Consumed in M4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingConfig {
    /// Whether to embed at all; `false` runs lexical-only.
    pub enabled: bool,
    /// Model identifier recorded in the index manifest.
    pub model: String,
    /// Texts per embedding call.
    pub batch_size: usize,
}

/// MCP-server parameters (§9.3). Consumed in M6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpConfig {
    /// Whether write tools are exposed at all. Off unless explicitly enabled
    /// (D4).
    pub allow_write: bool,
    /// Response size cap protecting the agent's context window.
    pub max_chars: usize,
}

/// A frontmatter field the engine knows how to read (§6.3).
///
/// Aliases are configured per field, so lookup needs a stable index rather than
/// a string key — and an enum means a typo is a compile error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Field {
    /// `id` — declared identity.
    Id = 0,
    /// `title`.
    Title,
    /// `author`.
    Author,
    /// `type`.
    Type,
    /// `status`.
    Status,
    /// `date`.
    Date,
    /// `effective_from`.
    EffectiveFrom,
    /// `effective_to`.
    EffectiveTo,
    /// `supersedes`.
    Supersedes,
    /// `superseded_by`.
    SupersededBy,
    /// `tags`.
    Tags,
    /// `doc_version`.
    Version,
    /// `scope`.
    Scope,
    /// `shared_with`.
    SharedWith,
}

impl Field {
    /// Number of fields; the length of the alias table.
    pub const COUNT: usize = 14;

    /// Every field, in declaration order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Id,
        Self::Title,
        Self::Author,
        Self::Type,
        Self::Status,
        Self::Date,
        Self::EffectiveFrom,
        Self::EffectiveTo,
        Self::Supersedes,
        Self::SupersededBy,
        Self::Tags,
        Self::Version,
        Self::Scope,
        Self::SharedWith,
    ];

    /// Index into the alias table.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The canonical name, as used in config files and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Title => "title",
            Self::Author => "author",
            Self::Type => "type",
            Self::Status => "status",
            Self::Date => "date",
            Self::EffectiveFrom => "effective_from",
            Self::EffectiveTo => "effective_to",
            Self::Supersedes => "supersedes",
            Self::SupersededBy => "superseded_by",
            Self::Tags => "tags",
            Self::Version => "version",
            Self::Scope => "scope",
            Self::SharedWith => "shared_with",
        }
    }
}

/// Frontmatter aliases, one list per [`Field`].
#[derive(Debug, Clone)]
struct Aliases {
    lists: [Box<[String]>; Field::COUNT],
}

impl Aliases {
    fn get(&self, field: Field) -> &[String] {
        &self.lists[field.index()]
    }

    fn resolve(raw: &RawAliases) -> Self {
        let lists = std::array::from_fn(|index| {
            let field = Field::ALL[index];
            match raw.declared(field) {
                // A present list replaces the default, so `tags = []` genuinely
                // disables the field instead of silently restoring the default.
                Some(list) => list
                    .iter()
                    .map(|alias| alias.trim().to_owned())
                    .filter(|alias| !alias.is_empty())
                    .collect(),
                None => default_aliases(field)
                    .iter()
                    .map(|alias| (*alias).to_owned())
                    .collect(),
            }
        });

        Self { lists }
    }
}

/// A configured `[paths]` hint, compiled once.
#[derive(Debug, Clone)]
struct PathHint {
    pattern: String,
    type_name: String,
    matcher: HintMatcher,
}

impl PathHint {
    fn matches(&self, rel_path: &str) -> bool {
        match &self.matcher {
            HintMatcher::Directory(needle) => contains_path_segments(rel_path, needle),
            HintMatcher::Glob(glob) => glob.is_match(rel_path),
        }
    }
}

/// How a path hint is matched. A plain `folder/` is treated as a directory
/// name; anything with a wildcard is a glob against the vault-relative path.
#[derive(Debug, Clone)]
enum HintMatcher {
    Directory(String),
    Glob(GlobMatcher),
}

/// Something odd about the config that the engine survived.
///
/// Warnings never stop a scan. They are surfaced by `janus doctor` (M5) and by
/// `janus scan` so a misconfiguration is visible instead of silently shaping
/// every score.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigWarning {
    /// Alias chains formed a cycle, so resolution stopped at a hop cap.
    AliasCycle {
        /// A type name participating in the cycle.
        alias: String,
    },
    /// An authority weight was zero, negative or NaN.
    NonPositiveAuthority {
        /// The canonical type name.
        type_name: String,
        /// The configured weight.
        weight: f64,
    },
    /// A half-life was negative, which is almost certainly a typo: `0` is the
    /// documented way to disable decay.
    NegativeHalfLife {
        /// Where the value came from, e.g. `decay.by_type.finding`.
        scope: String,
        /// The configured value.
        days: f64,
    },
    /// A pattern in `[paths]` or `[index] exclude` could not be compiled.
    InvalidGlob {
        /// The offending pattern.
        pattern: String,
        /// The compiler's complaint.
        message: String,
    },
    /// `corpus_id` was empty or contained characters that break qualified
    /// cross-corpus links (`corpus:path`, §6.11).
    InvalidCorpusId {
        /// The configured value.
        value: String,
    },
}

impl fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AliasCycle { alias } => write!(
                f,
                "type alias `{alias}` is part of a cycle; resolution stopped early"
            ),
            Self::NonPositiveAuthority { type_name, weight } => write!(
                f,
                "authority for `{type_name}` is {weight}; it must be greater than zero"
            ),
            Self::NegativeHalfLife { scope, days } => write!(
                f,
                "`{scope}` is {days}; use 0 to disable decay instead of a negative value"
            ),
            Self::InvalidGlob { pattern, message } => {
                write!(f, "invalid glob `{pattern}`: {message}")
            }
            Self::InvalidCorpusId { value } => write!(
                f,
                "corpus_id `{value}` should be non-empty and contain no ':' and no whitespace"
            ),
        }
    }
}

// ──────────────────────────── file representation ───────────────────────────

/// The parsed TOML, before resolution. Private: callers only ever see [`Config`].
///
/// Every field is optional and every table has `#[serde(default)]`, so a
/// minimal M0-era `index_dir = ".janus"` still deserializes (and always will).
/// `deny_unknown_fields` is deliberately absent.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawConfig {
    corpus_id: Option<String>,
    index_dir: Option<String>,
    frontmatter: RawAliases,
    defaults: RawDefaults,
    types: BTreeMap<String, String>,
    authority: BTreeMap<String, f64>,
    decay: RawDecay,
    paths: BTreeMap<String, String>,
    index: RawIndex,
    chunking: RawChunking,
    embedding: RawEmbedding,
    mcp: RawMcp,
}

/// `[frontmatter]`. `None` means "not specified, use the default list"; `Some`
/// — even `Some(vec![])` — means "this is the list".
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawAliases {
    id: Option<Vec<String>>,
    title: Option<Vec<String>>,
    author: Option<Vec<String>>,
    #[serde(rename = "type")]
    r#type: Option<Vec<String>>,
    status: Option<Vec<String>>,
    date: Option<Vec<String>>,
    effective_from: Option<Vec<String>>,
    effective_to: Option<Vec<String>>,
    supersedes: Option<Vec<String>>,
    superseded_by: Option<Vec<String>>,
    tags: Option<Vec<String>>,
    version: Option<Vec<String>>,
    scope: Option<Vec<String>>,
    shared_with: Option<Vec<String>>,
}

impl RawAliases {
    fn declared(&self, field: Field) -> Option<&Vec<String>> {
        let list = match field {
            Field::Id => &self.id,
            Field::Title => &self.title,
            Field::Author => &self.author,
            Field::Type => &self.r#type,
            Field::Status => &self.status,
            Field::Date => &self.date,
            Field::EffectiveFrom => &self.effective_from,
            Field::EffectiveTo => &self.effective_to,
            Field::Supersedes => &self.supersedes,
            Field::SupersededBy => &self.superseded_by,
            Field::Tags => &self.tags,
            Field::Version => &self.version,
            Field::Scope => &self.scope,
            Field::SharedWith => &self.shared_with,
        };
        list.as_ref()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawDefaults {
    #[serde(rename = "type")]
    r#type: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawDecay {
    half_life_days: Option<f64>,
    timeless_types: Option<Vec<String>>,
    by_type: BTreeMap<String, f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawIndex {
    exclude: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawChunking {
    max_chars: Option<usize>,
    overlap: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawEmbedding {
    enabled: Option<bool>,
    model: Option<String>,
    batch_size: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawMcp {
    allow_write: Option<bool>,
    max_chars: Option<usize>,
}

// ───────────────────────────────── defaults ────────────────────────────────

fn default_corpus_id() -> String {
    "main".to_owned()
}

/// Default aliases per field (§16.1). First match wins at read time.
fn default_aliases(field: Field) -> &'static [&'static str] {
    match field {
        Field::Id => &["id", "uid", "uuid", "zettel_id", "permalink"],
        Field::Title => &["title", "name"],
        Field::Author => &["author", "generated_by", "model"],
        Field::Type => &["type", "kind", "doc_type"],
        Field::Status => &["status", "state"],
        // `created` and `decided` are common spellings; a date parsed from any
        // of them is still reported as `date_source: frontmatter`.
        Field::Date => &["date", "created", "decided"],
        Field::EffectiveFrom => &["effective_from", "valid_from", "effective"],
        Field::EffectiveTo => &["effective_to", "valid_to", "expires"],
        Field::Supersedes => &["supersedes", "superseded"],
        Field::SupersededBy => &["superseded_by", "superseded-by"],
        Field::Tags => &["tags", "keywords"],
        Field::Version => &["doc_version", "version"],
        Field::Scope => &["scope", "org", "domain"],
        Field::SharedWith => &["shared_with", "shared", "shared-with"],
    }
}

/// Default alias → canonical mapping (§16.1).
fn default_type_aliases() -> BTreeMap<String, String> {
    let pairs = [
        ("adr", "decision"),
        ("rfc", "spec"),
        ("log", "journal"),
        ("research", "finding"),
        ("proposal", "idea"),
    ];
    pairs
        .iter()
        .map(|(alias, canonical)| ((*alias).to_owned(), (*canonical).to_owned()))
        .collect()
}

/// Default canonical type → authority weight (§6.6).
fn default_authority() -> BTreeMap<String, f64> {
    let pairs = [
        ("decision", 1.50),
        ("spec", 1.35),
        ("principle", 1.20),
        ("finding", 1.10),
        ("journal", 1.00),
        ("note", 0.90),
        ("idea", 0.85),
    ];
    pairs
        .iter()
        .map(|(type_name, weight)| ((*type_name).to_owned(), *weight))
        .collect()
}

/// Default per-type half-life overrides (§6.6). Evidence ages slower than
/// commentary; an unexplored idea ages fastest.
fn default_decay_by_type() -> BTreeMap<String, f64> {
    let pairs = [("finding", 365.0), ("journal", 180.0), ("idea", 90.0)];
    pairs
        .iter()
        .map(|(type_name, days)| ((*type_name).to_owned(), *days))
        .collect()
}

/// Types that never decay by default: a committed choice does not go stale.
fn default_timeless_types() -> BTreeSet<String> {
    ["decision", "spec", "principle"]
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

// ───────────────────────────────── helpers ─────────────────────────────────

/// Drops a leading UTF-8 BOM.
///
/// Editors on Windows and some generators write one; TOML does not allow it, and
/// "your config is invalid" is an unhelpful response to an invisible character.
fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Reports every type name that sits on an alias cycle.
fn report_alias_cycles(types: &BTreeMap<String, String>, warnings: &mut Vec<ConfigWarning>) {
    let mut cyclic = BTreeSet::new();
    for start in types.keys() {
        if let Some(node) = find_cycle(types, start) {
            cyclic.insert(node);
        }
    }
    for alias in cyclic {
        warnings.push(ConfigWarning::AliasCycle { alias });
    }
}

/// Walks the alias chain from `start`, returning a node if the chain revisits
/// itself or exceeds [`MAX_ALIAS_HOPS`].
fn find_cycle(types: &BTreeMap<String, String>, start: &str) -> Option<String> {
    let mut seen: [&str; MAX_ALIAS_HOPS] = [""; MAX_ALIAS_HOPS];
    seen[0] = start;
    let mut hops = 1;
    let mut current = start;

    while let Some(next) = types.get(current) {
        let next = next.as_str();
        if hops == MAX_ALIAS_HOPS || seen[..hops].contains(&next) {
            return Some(next.to_owned());
        }
        seen[hops] = next;
        hops += 1;
        current = next;
    }

    None
}

/// Warns when a declared corpus id would break identity namespacing or
/// qualified links.
fn validate_corpus_id(value: &str, warnings: &mut Vec<ConfigWarning>) {
    let invalid = value.is_empty()
        || value
            .chars()
            .any(|c| c == ':' || c == '/' || c == '\\' || c.is_whitespace());
    if invalid {
        warnings.push(ConfigWarning::InvalidCorpusId {
            value: value.to_owned(),
        });
    }
}

/// Compiles `[paths]` into matchers, most specific first.
fn build_path_hints(
    raw: BTreeMap<String, String>,
    warnings: &mut Vec<ConfigWarning>,
) -> Vec<PathHint> {
    let mut hints = Vec::with_capacity(raw.len());

    for (pattern, type_name) in raw {
        let is_glob = pattern.contains('*') || pattern.contains('?') || pattern.contains('[');
        let matcher = if is_glob {
            match Glob::new(&pattern) {
                Ok(glob) => HintMatcher::Glob(glob.compile_matcher()),
                Err(error) => {
                    warnings.push(ConfigWarning::InvalidGlob {
                        pattern: pattern.clone(),
                        message: error.to_string(),
                    });
                    continue;
                }
            }
        } else {
            let trimmed = pattern.trim_matches('/').trim_matches('\\');
            if trimmed.is_empty() {
                warnings.push(ConfigWarning::InvalidGlob {
                    pattern: pattern.clone(),
                    message: "path hint is empty".to_owned(),
                });
                continue;
            }
            HintMatcher::Directory(trimmed.to_owned())
        };

        hints.push(PathHint {
            pattern,
            type_name: type_name.trim().to_owned(),
            matcher,
        });
    }

    // Longest pattern first, so `decisions/drafts/` beats `decisions/`;
    // lexicographic as a tiebreak keeps the order deterministic for equal
    // lengths (`BTreeMap` alone would put `decisions/` first and shadow the
    // nested hint).
    hints.sort_by(|a, b| {
        b.pattern
            .len()
            .cmp(&a.pattern.len())
            .then_with(|| a.pattern.cmp(&b.pattern))
    });

    hints
}

/// Compiles `[index] exclude` into one matcher. Invalid patterns are reported
/// and skipped: one bad glob must not make the whole vault unscannable.
fn build_excludes(patterns: &[String], warnings: &mut Vec<ConfigWarning>) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        match Glob::new(pattern.trim()) {
            Ok(glob) => {
                builder.add(glob);
            }
            Err(error) => warnings.push(ConfigWarning::InvalidGlob {
                pattern: pattern.clone(),
                message: error.to_string(),
            }),
        }
    }

    builder.build().unwrap_or_default()
}

/// True when `rel_path` contains `needle` as a run of whole path segments.
///
/// Allocation-free and byte-safe: every index tested sits on a `/` boundary,
/// which is always a UTF-8 character boundary. `decisions` matches
/// `decisions/a.md` and `q4/decisions/a.md`, but not `decisions-archive/a.md`.
fn contains_path_segments(rel_path: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }

    let needle_len = needle.len();
    let bytes = rel_path.as_bytes();
    let mut start = 0;

    loop {
        let end = start + needle_len;
        if end <= rel_path.len()
            && rel_path[start..].starts_with(needle)
            && (start == 0 || bytes[start - 1] == b'/')
            && (end == rel_path.len() || bytes[end] == b'/')
        {
            return true;
        }

        match rel_path[start..].find('/') {
            Some(offset) => start += offset + 1,
            None => return false,
        }
    }
}

/// Whether a directory name is excluded unconditionally by the walk.
///
/// Takes an [`OsStr`] because the walk sees directory names before it can build
/// a `String`; a non-UTF-8 name can never equal one of these ASCII names anyway.
#[must_use]
pub fn is_always_excluded_dir(name: &OsStr) -> bool {
    ALWAYS_EXCLUDED_DIRS
        .iter()
        .any(|excluded| name == OsStr::new(excluded))
}

/// Whether a path component belongs to janus-mem's sidecar or to git.
///
/// Used by path confinement: a resolved path containing one of these is a
/// refusal, not a skip.
#[must_use]
pub fn is_forbidden_dir(name: &OsStr) -> bool {
    FORBIDDEN_DIRS
        .iter()
        .any(|forbidden| name == OsStr::new(forbidden))
}

/// The index location resolved against a corpus root.
///
/// `index_dir` is normally relative (`.janus`), but a read-only corpus can point
/// it at a writable cache directory (§6.11), so absolute paths must pass
/// through untouched.
#[must_use]
pub fn resolve_index_dir(root: &Path, index_dir: &Path) -> PathBuf {
    if index_dir.is_absolute() {
        index_dir.to_path_buf()
    } else {
        root.join(index_dir)
    }
}

/// The vault root is the directory that contains `janus.toml`, so it is not
/// repeated inside the config itself.
///
/// Resolves the corpus root from an explicit path, else the current directory.
/// Ancestor discovery is deliberately *not* here yet: it arrives with
/// `--vault`/`JANUS_VAULT` precedence in phase 2 (§6.1).
///
/// # Errors
///
/// [`JanusError::VaultResolve`] when the path cannot be canonicalized,
/// [`JanusError::VaultNotDirectory`] when it exists but is not a directory.
pub fn resolve_vault(explicit: Option<PathBuf>) -> Result<PathBuf, JanusError> {
    let requested = match explicit {
        Some(path) => path,
        None => std::env::current_dir()?,
    };

    let resolved = requested
        .canonicalize()
        .map_err(|source| JanusError::VaultResolve {
            path: requested,
            source,
        })?;

    if !resolved.is_dir() {
        return Err(JanusError::VaultNotDirectory { path: resolved });
    }

    Ok(resolved)
}

/// Initializes a new janus vault at the root path.
///
/// Never overwrites an existing `janus.toml`: the file is created with
/// `create_new`, which fails atomically instead of truncating.
///
/// # Errors
///
/// [`JanusError::ConfigExists`] when a config is already present,
/// [`JanusError::CreateDir`] / [`JanusError::Write`] for IO failures.
pub fn init_vault(root: &Path) -> Result<PathBuf, JanusError> {
    let config_path = root.join(CONFIG_FILE);
    let index_dir = root.join(JANUS_DIR);

    fs::create_dir_all(&index_dir).map_err(|source| JanusError::CreateDir {
        path: index_dir,
        source,
    })?;

    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&config_path)
    {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            return Err(JanusError::ConfigExists { path: config_path });
        }
        Err(source) => {
            return Err(JanusError::Write {
                path: config_path,
                source,
            });
        }
    };

    if let Err(source) = file.write_all(DEFAULT_CONFIG.as_bytes()) {
        // Leave no half-written config behind, then surface the real cause.
        // The file must be closed first: Windows cannot remove an open file.
        drop(file);
        let _ = fs::remove_file(&config_path);
        return Err(JanusError::Write {
            path: config_path,
            source,
        });
    }

    warn_if_not_ignored(root);

    Ok(config_path)
}

/// Prints a hint when the index directory would otherwise be committed.
/// This only reports; it never edits `.gitignore` (SPEC §8.6, rule 3).
fn warn_if_not_ignored(root: &Path) {
    let gitignore = root.join(".gitignore");

    if !gitignore.exists() {
        tracing::info!("no .gitignore found; consider adding one that ignores `.janus/`");
        return;
    }

    match fs::read_to_string(&gitignore) {
        Ok(contents) if gitignore_ignores_janus(&contents) => {}
        Ok(_) => tracing::warn!(
            path = %gitignore.display(),
            "index directory is not git-ignored; add this line: .janus/"
        ),
        Err(error) => tracing::warn!(
            path = %gitignore.display(),
            %error,
            "could not read .gitignore; make sure it ignores `.janus/`"
        ),
    }
}

/// True when some non-comment line ignores the `.janus` directory.
/// Accepts `.janus`, `.janus/`, `/.janus`, and `/.janus/`.
fn gitignore_ignores_janus(contents: &str) -> bool {
    contents.lines().any(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return false;
        }
        line.trim_start_matches('/').trim_end_matches('/') == JANUS_DIR
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Parses config text with a fake origin, for tests that need no filesystem.
    fn parse(text: &str) -> Config {
        Config::from_toml(text, Path::new("<test>")).expect("config should parse")
    }

    /// Parses and returns the warnings, for tests about diagnostics.
    fn parse_warnings(text: &str) -> Vec<ConfigWarning> {
        parse(text).warnings
    }

    // ─────────────────────────── file → config ───────────────────────────

    #[test]
    fn minimal_m0_config_still_parses() {
        let config = parse("index_dir = \".janus\"\n");

        assert_eq!(config.index_dir, PathBuf::from(".janus"));
        assert_eq!(config.corpus_id, "main");
        assert_eq!(config.defaults.type_name, "note");
        assert_eq!(config.defaults.status, Status::Active);
        assert!(config.warnings.is_empty(), "{:?}", config.warnings);
    }

    #[test]
    fn shipped_example_parses_cleanly() {
        let config = Config::shipped_example().expect("shipped janus.toml must be valid");

        assert!(
            config.warnings.is_empty(),
            "shipped example should not warn: {:?}",
            config.warnings
        );
        assert_eq!(config.corpus_id, "main");
        assert_eq!(config.aliases(Field::Id).len(), 5);
        assert_eq!(config.decay.half_life_for("finding"), 365.0);
        assert_eq!(config.decay.half_life_for("note"), 180.0);
    }

    #[test]
    fn unknown_keys_are_tolerated() {
        let config = parse(
            r#"
            index_dir = ".janus"
            future_setting = 42

            [future_table]
            nested = { deeply = true }

            [mcp]
            allow_write = false
            not_a_real_key = "ignore me"
            "#,
        );

        assert!(!config.mcp.allow_write);
    }

    #[test]
    fn a_leading_bom_is_ignored() {
        let config = parse("\u{feff}index_dir = \".janus\"\n");
        assert_eq!(config.index_dir, PathBuf::from(".janus"));
    }

    #[test]
    fn invalid_toml_is_a_parse_error_naming_the_origin() {
        let error = Config::from_toml("index_dir = ", Path::new("/vault/janus.toml"))
            .expect_err("truncated TOML must fail");

        assert!(matches!(error, JanusError::ConfigParse { .. }));
        assert!(error.to_string().contains("janus.toml"), "{error}");
        assert_eq!(error.exit_code(), 1);
    }

    #[test]
    fn wrong_type_for_a_known_key_is_a_parse_error() {
        let error = Config::from_toml("index_dir = 12\n", Path::new("<test>"))
            .expect_err("a number is not a directory");
        assert!(matches!(error, JanusError::ConfigParse { .. }));
    }

    #[test]
    fn missing_config_names_the_path_and_suggests_init() {
        let dir = tempfile::tempdir().unwrap();
        let error = Config::load(dir.path()).expect_err("no config means no scan");

        assert!(matches!(error, JanusError::ConfigMissing { .. }));
        assert!(
            error.to_string().contains("janus.toml"),
            "error must name the file: {error}"
        );
    }

    // ────────────────────────────── aliases ──────────────────────────────

    #[test]
    fn every_field_has_a_non_empty_default_alias_list() {
        let config = parse("");
        for field in Field::ALL {
            assert!(
                !config.aliases(field).is_empty(),
                "{} has no default aliases",
                field.as_str()
            );
        }
    }

    #[test]
    fn a_declared_alias_list_replaces_the_default() {
        let config = parse("[frontmatter]\ntags = [\"kw\", \"topics\"]\n");

        assert_eq!(
            config.aliases(Field::Tags),
            ["kw".to_owned(), "topics".to_owned()]
        );
        // Untouched fields keep their defaults.
        assert!(config.aliases(Field::Title).contains(&"title".to_owned()));
    }

    #[test]
    fn an_empty_alias_list_disables_the_field() {
        let config = parse("[frontmatter]\ntags = []\n");
        assert!(config.aliases(Field::Tags).is_empty());
    }

    #[test]
    fn alias_whitespace_is_trimmed_and_blanks_dropped() {
        let config = parse("[frontmatter]\ntitle = [\" name \", \"\"]\n");
        assert_eq!(config.aliases(Field::Title), ["name".to_owned()]);
    }

    // ──────────────────────────── type registry ───────────────────────────

    #[test]
    fn alias_chains_resolve_to_the_canonical_type() {
        let config = parse("[types]\nadr = \"decision\"\nrfc = \"spec\"\n");

        let resolved = config.resolve_type("adr");
        assert_eq!(resolved.name, "decision");
        assert!(resolved.known);
        assert!((resolved.spec.authority - 1.50).abs() < f64::EPSILON);
        assert!(resolved.spec.timeless, "decisions are timeless by default");

        assert_eq!(config.resolve_type("rfc").name, "spec");
    }

    #[test]
    fn canonical_names_resolve_to_themselves() {
        let config = parse("");
        let resolved = config.resolve_type("principle");
        assert_eq!(resolved.name, "principle");
        assert!(resolved.known);
    }

    #[test]
    fn a_user_alias_can_point_at_another_vocabulary_word() {
        let config = parse("[types]\nadr = \"proposal\"\n");
        // adr -> proposal -> idea, two hops.
        assert_eq!(config.resolve_type("adr").name, "idea");
    }

    #[test]
    fn unknown_types_keep_their_name_and_get_the_fallback_weight() {
        let config = parse("");
        let resolved = config.resolve_type("runbook");

        assert_eq!(resolved.name, "runbook");
        assert!(!resolved.known);
        assert!((resolved.spec.authority - UNKNOWN_TYPE_AUTHORITY).abs() < f64::EPSILON);
        assert!(!resolved.spec.decays() || resolved.spec.half_life_days > 0.0);
    }

    #[test]
    fn type_names_are_trimmed_but_not_case_folded() {
        let config = parse("");
        assert_eq!(config.resolve_type("  decision  ").name, "decision");
        // Types are an open registry, not a case-insensitive enum: an unlisted
        // spelling is a distinct (unknown) type, matching §6.6.1's "keeps its
        // name".
        assert!(!config.resolve_type("Decision").known);
    }

    #[test]
    fn an_alias_cycle_warns_and_still_terminates() {
        let config = parse("[types]\na = \"b\"\nb = \"a\"\n");

        assert!(
            config
                .warnings
                .iter()
                .any(|w| matches!(w, ConfigWarning::AliasCycle { .. })),
            "{:?}",
            config.warnings
        );

        // Resolution must not hang, and must land on a name rather than panic.
        let resolved = config.resolve_type("a");
        assert!(!resolved.name.is_empty());
    }

    #[test]
    fn a_self_alias_is_a_cycle() {
        let config = parse("[types]\nloop = \"loop\"\n");
        assert!(
            config
                .warnings
                .iter()
                .any(|w| matches!(w, ConfigWarning::AliasCycle { .. }))
        );
    }

    // ────────────────────────────── authority ─────────────────────────────

    #[test]
    fn authority_overrides_merge_with_defaults() {
        let config = parse("[authority]\nreport = 1.05\n");

        assert_eq!(config.resolve_type("report").spec.authority, 1.05);
        // The other defaults survive the merge.
        assert_eq!(config.resolve_type("decision").spec.authority, 1.50);
        assert_eq!(
            config.type_aliases().get("adr").map(String::as_str),
            Some("decision")
        );
    }

    #[test]
    fn a_zero_or_negative_authority_warns() {
        for weight in ["0.0", "-1.5"] {
            let warnings = parse_warnings(&format!("[authority]\nweird = {weight}\n"));
            assert!(
                warnings
                    .iter()
                    .any(|w| matches!(w, ConfigWarning::NonPositiveAuthority { .. })),
                "weight {weight} should warn: {warnings:?}"
            );
        }
    }

    #[test]
    fn default_type_is_configurable() {
        let config = parse("[defaults]\ntype = \"journal\"\nstatus = \"draft\"\n");

        assert_eq!(config.defaults.type_name, "journal");
        assert_eq!(config.defaults.status, Status::Draft);
        assert_eq!(config.default_type().name, "journal");
    }

    #[test]
    fn an_unknown_default_status_falls_back_to_unknown() {
        let config = parse("[defaults]\nstatus = \"archived\"\n");
        assert_eq!(config.defaults.status, Status::Unknown);
    }

    // ──────────────────────────────── decay ──────────────────────────────

    #[test]
    fn per_type_half_life_overrides_the_global_default() {
        let config = parse("[decay]\nhalf_life_days = 30\n");
        assert_eq!(config.decay.half_life_for("note"), 30.0);
        // Merged defaults still apply where configured.
        assert_eq!(config.decay.half_life_for("finding"), 365.0);
    }

    #[test]
    fn an_explicit_by_type_override_beats_the_global_value() {
        let config = parse("[decay]\nhalf_life_days = 30\n\n[decay.by_type]\nnote = 7\n");
        assert_eq!(config.decay.half_life_for("note"), 7.0);
        assert_eq!(config.decay.half_life_for("journal"), 180.0);
    }

    #[test]
    fn a_custom_timeless_list_replaces_the_default_set() {
        let config = parse("[decay]\ntimeless_types = [\"principle\"]\n");

        assert!(config.resolve_type("principle").spec.timeless);
        assert!(
            !config.resolve_type("decision").spec.timeless,
            "replacing the list must remove the default entries"
        );
    }

    #[test]
    fn a_zero_half_life_disables_decay_without_warning() {
        let config = parse("[decay]\nhalf_life_days = 0\n");
        assert_eq!(config.decay.half_life_days, 0.0);
        assert!(config.warnings.is_empty(), "{:?}", config.warnings);
    }

    #[test]
    fn a_negative_half_life_warns_and_falls_back() {
        let config = parse("[decay]\nhalf_life_days = -5\n");

        assert_eq!(config.decay.half_life_days, DEFAULT_HALF_LIFE_DAYS);
        assert!(
            config
                .warnings
                .iter()
                .any(|w| matches!(w, ConfigWarning::NegativeHalfLife { .. }))
        );
    }

    // ───────────────────────────── path hints ────────────────────────────

    #[test]
    fn directory_hints_match_root_and_nested_paths() {
        let config = parse("[paths]\n\"decisions/\" = \"decision\"\n");

        assert_eq!(config.path_type_hint("decisions/a.md"), Some("decision"));
        assert_eq!(config.path_type_hint("q4/decisions/a.md"), Some("decision"));
        assert_eq!(config.path_type_hint("notes/a.md"), None);
        // A shared prefix is not a segment match.
        assert_eq!(config.path_type_hint("decisions-archive/a.md"), None);
    }

    #[test]
    fn glob_hints_are_matched_as_globs() {
        let config = parse("[paths]\n\"**/adr/*.md\" = \"decision\"\n");

        assert_eq!(config.path_type_hint("adr/0001.md"), Some("decision"));
        assert_eq!(config.path_type_hint("q4/adr/0001.md"), Some("decision"));
        assert_eq!(config.path_type_hint("adr/0001.txt"), None);
    }

    #[test]
    fn the_most_specific_path_hint_wins() {
        let config =
            parse("[paths]\n\"decisions/\" = \"decision\"\n\"decisions/drafts/\" = \"idea\"\n");

        assert_eq!(config.path_type_hint("decisions/a.md"), Some("decision"));
        assert_eq!(
            config.path_type_hint("decisions/drafts/a.md"),
            Some("idea"),
            "the nested hint must not be shadowed by the broader one"
        );
    }

    #[test]
    fn containing_segments_is_byte_safe() {
        assert!(contains_path_segments("decisions/a.md", "decisions"));
        assert!(contains_path_segments("a/decisions/b/c.md", "decisions"));
        assert!(contains_path_segments("a/decisions", "decisions"));
        assert!(!contains_path_segments("decisionsx/a.md", "decisions"));
        assert!(!contains_path_segments("a.md", "decisions"));
        assert!(!contains_path_segments("a/decisions/b.md", ""));

        // Multi-byte content around the match must not panic or mis-slice.
        assert!(contains_path_segments(
            "日本語/decisions/メモ.md",
            "decisions"
        ));
        assert!(!contains_path_segments("日本語/メモ.md", "decisions"));
    }

    // ────────────────────────────── excludes ─────────────────────────────

    #[test]
    fn exclude_globs_hide_matching_paths() {
        let config = parse("[index]\nexclude = [\"archive/private/**\", \"**/*.draft.md\"]\n");

        assert!(config.is_excluded("archive/private/secret.md"));
        assert!(config.is_excluded("notes/wip.draft.md"));
        assert!(!config.is_excluded("archive/public/ok.md"));
        assert!(!config.is_excluded("notes/wip.md"));
    }

    #[test]
    fn no_exclude_table_excludes_nothing() {
        let config = parse("");
        assert!(!config.is_excluded("archive/private/secret.md"));
    }

    #[test]
    fn an_invalid_exclude_glob_warns_and_is_skipped() {
        let config = parse("[index]\nexclude = [\"[unclosed\", \"ok/**\"]\n");

        assert!(
            config
                .warnings
                .iter()
                .any(|w| matches!(w, ConfigWarning::InvalidGlob { .. }))
        );
        // The valid pattern still works.
        assert!(config.is_excluded("ok/file.md"));
    }

    // ───────────────────────────── corpus id ─────────────────────────────

    #[test]
    fn a_corpus_id_that_breaks_link_syntax_warns() {
        for value in ["\"\"", "\"with space\"", "\"has:colon\"", "\"dir/name\""] {
            let config = parse(&format!("corpus_id = {value}\n"));
            assert!(
                config
                    .warnings
                    .iter()
                    .any(|w| matches!(w, ConfigWarning::InvalidCorpusId { .. })),
                "corpus_id {value} should warn"
            );
        }
    }

    #[test]
    fn a_plain_corpus_id_is_accepted() {
        let config = parse("corpus_id = \"third-brain\"\n");
        assert!(config.warnings.is_empty(), "{:?}", config.warnings);
    }

    // ─────────────────────────────── helpers ─────────────────────────────

    #[test]
    fn always_excluded_dirs_cover_index_and_git() {
        assert!(is_always_excluded_dir(OsStr::new(".janus")));
        assert!(is_always_excluded_dir(OsStr::new(".git")));
        assert!(is_always_excluded_dir(OsStr::new("node_modules")));
        assert!(!is_always_excluded_dir(OsStr::new("notes")));
    }

    #[test]
    fn index_dir_resolves_relative_to_the_root_but_passes_absolute_paths() {
        let root = Path::new("/vault");

        assert_eq!(
            resolve_index_dir(root, Path::new(".janus")),
            PathBuf::from("/vault/.janus")
        );
        assert_eq!(
            resolve_index_dir(root, Path::new("/cache/context")),
            PathBuf::from("/cache/context"),
            "a read-only corpus may point its index elsewhere (§6.11)"
        );
    }

    // ──────────────────────── M0 behaviour, unchanged ─────────────────────

    #[test]
    fn test_resolve_vault_explicit() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().to_path_buf();

        let resolved = resolve_vault(Some(path.clone())).unwrap();
        assert_eq!(resolved, path.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_vault_rejects_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file = temp_dir.path().join("not-a-vault.md");
        fs::write(&file, "hello").unwrap();

        let result = resolve_vault(Some(file));
        assert!(matches!(result, Err(JanusError::VaultNotDirectory { .. })));
    }

    #[test]
    fn test_resolve_vault_missing_path_names_the_path() {
        let temp_dir = tempfile::tempdir().unwrap();
        let missing = temp_dir.path().join("does-not-exist");

        let err = resolve_vault(Some(missing.clone())).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains(&missing.display().to_string()),
            "{message}"
        );
    }

    #[test]
    fn test_init_vault_success() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        let config_path = init_vault(root).unwrap();

        assert!(config_path.exists());
        assert!(root.join(".janus").is_dir());

        let content = fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("index_dir = \".janus\""));
    }

    #[test]
    fn init_writes_a_config_that_loads() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        init_vault(root).unwrap();
        let config = Config::load(root).expect("the config init writes must be loadable");

        assert_eq!(config.corpus_id, "main");
        assert!(config.warnings.is_empty(), "{:?}", config.warnings);
        // The example documents the real defaults, so a fresh vault is explicit.
        assert_eq!(config.resolve_type("adr").name, "decision");
    }

    #[test]
    fn test_init_vault_twice_fails() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        init_vault(root).unwrap();
        let result = init_vault(root);
        assert!(matches!(result, Err(JanusError::ConfigExists { .. })));
    }

    #[test]
    fn test_init_vault_never_overwrites_config() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        init_vault(root).unwrap();
        let config_path = root.join(CONFIG_FILE);
        fs::write(&config_path, "sentinel = true\n").unwrap();

        let result = init_vault(root);

        assert!(matches!(result, Err(JanusError::ConfigExists { .. })));
        assert_eq!(
            fs::read_to_string(&config_path).unwrap(),
            "sentinel = true\n",
            "existing config must be byte-identical after a refused init"
        );
    }

    #[test]
    fn gitignore_recognizes_janus_variants() {
        for contents in [
            ".janus",
            ".janus/",
            "/.janus",
            "/.janus/",
            "target\n.janus\n",
        ] {
            assert!(
                gitignore_ignores_janus(contents),
                "should match: {contents:?}"
            );
        }
    }

    #[test]
    fn gitignore_ignores_noise() {
        for contents in [
            "",
            "# .janus is intentionally committed\n",
            ".janus-old\n",
            "subdir/.janus\n",
        ] {
            assert!(
                !gitignore_ignores_janus(contents),
                "should not match: {contents:?}"
            );
        }
    }
}
