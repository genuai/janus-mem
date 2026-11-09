//! The workspace file and `janus corpora` (SPEC §6.11, §16.2, M6b §2).
//!
//! A workspace names the corpora one query reads together, each one's role, and
//! the federation policy between them. `main` resolves the path and passes it
//! here as a value, so this module reads no environment (SPEC §5.1).
//!
//! A missing `root`, a duplicate `id`, a second primary and an unusable `id` are
//! errors that name the file and the entry. An unknown key is a warning, because
//! a newer janus may write it and a broken federated read must still read what it
//! can (§2.1 of the M6b plan).

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{self, Config};
use crate::error::JanusError;
use crate::index::refresh::Signature;
use crate::index::store::{self, Manifest};
use crate::scan::API_VERSION;
use crate::vault::{self, WalkOptions};

/// The workspace file's name, inside the platform config directory (§6.11).
pub const WORKSPACE_FILE: &str = "workspace.toml";

/// The weight of an archived corpus that a query explicitly includes (§6.11).
pub const DEFAULT_ARCHIVED_WEIGHT: f64 = 0.8;

/// The role a corpus plays in a workspace (§6.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The corpus an unqualified query reads. Exactly one per workspace.
    Primary,
    /// A corpus that is hard-excluded unless a query names it.
    Archived,
}

impl Role {
    /// The name the file and the report use.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Archived => "archived",
        }
    }

    /// An archive is read-only unless its entry says otherwise (§6.11).
    const fn read_only_by_default(self) -> bool {
        matches!(self, Self::Archived)
    }
}

/// One corpus in a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Corpus {
    /// Declared name, unique in the file, and what `--corpus` matches.
    pub id: String,
    /// Absolute, canonical root, resolved once at load (§2.1).
    pub root: PathBuf,
    /// Primary or archived.
    pub role: Role,
    /// Whether a write tool must refuse this corpus. Enforced in M7.
    pub read_only: bool,
    /// Absolute index location: the declared value, else `<root>/.janus`.
    pub index_dir: PathBuf,
}

/// The keys a `[[corpora]]` entry may carry.
const CORPUS_KEYS: [&str; 5] = ["id", "root", "role", "read_only", "index_dir"];

/// The keys `[search]` may carry.
const SEARCH_KEYS: [&str; 2] = ["archived_weight", "allow_cross_corpus_links"];

#[derive(Deserialize)]
struct RawCorpus {
    id: String,
    root: String,
    role: Role,
    read_only: Option<bool>,
    index_dir: Option<String>,
}

#[derive(Deserialize)]
struct RawSearch {
    archived_weight: Option<f64>,
    allow_cross_corpus_links: Option<bool>,
}

/// The federation policy of one workspace (§6.11).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchPolicy {
    /// Weight applied to `final` for an archived corpus a query includes.
    pub archived_weight: f64,
    /// Whether a qualified cross-corpus link resolves (M7).
    pub allow_cross_corpus_links: bool,
}

impl Default for SearchPolicy {
    fn default() -> Self {
        Self {
            archived_weight: DEFAULT_ARCHIVED_WEIGHT,
            allow_cross_corpus_links: false,
        }
    }
}

/// A parsed and validated workspace file: primary first, then archived in file
/// order (§2.2).
#[derive(Debug, Clone)]
pub struct Workspace {
    /// The file this workspace came from.
    pub path: PathBuf,
    /// The corpora, primary first, then archived in file order.
    pub corpora: Vec<Corpus>,
    /// The federation policy of `[search]`.
    pub search: SearchPolicy,
    /// What was ignored or looked wrong, for the caller to print.
    pub warnings: Vec<String>,
}

impl Workspace {
    /// Loads and validates one workspace file.
    ///
    /// # Errors
    ///
    /// [`JanusError::WorkspaceInvalid`] when the file is absent, unreadable, or
    /// cannot be used.
    pub fn load(path: &Path) -> Result<Self, JanusError> {
        let bytes = fs::read(path).map_err(|source| JanusError::WorkspaceInvalid {
            path: path.to_path_buf(),
            message: format!("cannot read the file: {source}"),
        })?;
        let text = String::from_utf8(bytes).map_err(|err| JanusError::WorkspaceInvalid {
            path: path.to_path_buf(),
            message: format!("not UTF-8: {err}"),
        })?;
        Self::from_toml(&text, path)
    }

    /// Resolves workspace text. `origin` names the file in every message and
    /// anchors a relative `root`.
    ///
    /// # Errors
    ///
    /// [`JanusError::WorkspaceInvalid`] for invalid TOML, a missing required key,
    /// a duplicate `id`, a root that does not resolve, or a primary count other
    /// than one.
    pub fn from_toml(text: &str, origin: &Path) -> Result<Self, JanusError> {
        let value: toml::Value = toml::from_str(text.strip_prefix('\u{feff}').unwrap_or(text))
            .map_err(|source| JanusError::WorkspaceInvalid {
                path: origin.to_path_buf(),
                message: source.to_string().trim_end().to_owned(),
            })?;
        let mut parser = Parser {
            origin,
            warnings: Vec::new(),
        };
        let table = value
            .as_table()
            .ok_or_else(|| parser.error("the file must be a table"))?;
        for key in table.keys() {
            if key != "corpora" && key != "search" {
                parser.warn("", &format!("unknown key {key:?}; ignored"));
            }
        }
        let entries = match table.get("corpora") {
            Some(toml::Value::Array(entries)) if !entries.is_empty() => entries,
            Some(toml::Value::Array(_)) | None => {
                return Err(parser.error("no [[corpora]] entry"));
            }
            Some(_) => return Err(parser.error("`corpora` must be an array of tables")),
        };
        let mut corpora = Vec::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            let corpus = parser
                .corpus(entry, index)
                .map_err(|message| parser.error(message))?;
            corpora.push((index, corpus));
        }
        check(&corpora).map_err(|message| parser.error(message))?;
        let search = match table.get("search") {
            Some(value) => parser
                .search(value)
                .map_err(|message| parser.error(message))?,
            None => SearchPolicy::default(),
        };
        // Primary first; `sort_by_key` is stable, so file order survives inside
        // each role.
        corpora.sort_by_key(|(_, corpus)| corpus.role == Role::Archived);

        Ok(Self {
            path: origin.to_path_buf(),
            corpora: corpora.into_iter().map(|(_, corpus)| corpus).collect(),
            search,
            warnings: parser.warnings,
        })
    }
}

/// Resolves which workspace file a command reads (§6.11).
///
/// `explicit` is `--workspace` and `env` is `JANUS_WORKSPACE`; both arrive from
/// `main`. A named path must be a file: a typo is an error, not a fall back to a
/// different corpus set. The platform file is used only when it exists, because
/// its absence is the documented single-corpus mode.
///
/// # Errors
///
/// [`JanusError::WorkspaceInvalid`] for a named path that is not a file, and
/// [`JanusError::Usage`] when no source names one.
pub fn resolve(
    explicit: Option<PathBuf>,
    env: Option<PathBuf>,
    config_dir: &Path,
) -> Result<PathBuf, JanusError> {
    // The flag wins over the variable, which wins over the platform file.
    if let Some(path) = explicit.or(env) {
        if path.is_file() {
            return Ok(path);
        }
        return Err(JanusError::WorkspaceInvalid {
            path,
            message: "no such file".to_owned(),
        });
    }
    let platform = config_dir.join(WORKSPACE_FILE);
    if platform.is_file() {
        return Ok(platform);
    }
    Err(JanusError::Usage {
        message: format!(
            "no workspace found: pass --workspace PATH, set JANUS_WORKSPACE, or create {}",
            platform.display()
        ),
    })
}

// ─────────────────────────────── parsing ───────────────────────────────────

/// One file being parsed: its path for messages, and the warnings so far.
struct Parser<'a> {
    origin: &'a Path,
    warnings: Vec<String>,
}

impl Parser<'_> {
    /// One error naming the file.
    fn error(&self, message: impl Into<String>) -> JanusError {
        JanusError::WorkspaceInvalid {
            path: self.origin.to_path_buf(),
            message: message.into(),
        }
    }

    /// One warning naming the file and, when there is one, the entry.
    fn warn(&mut self, entry: &str, message: &str) {
        let entry = if entry.is_empty() {
            String::new()
        } else {
            format!("{entry}: ")
        };
        self.warnings
            .push(format!("{}: {entry}{message}", self.origin.display()));
    }

    /// Warns about each key of `table` that `known` does not list.
    fn unknown(&mut self, table: &toml::Table, known: &[&str], entry: &str) {
        for key in table.keys() {
            if !known.contains(&key.as_str()) {
                self.warn(entry, &format!("unknown key {key:?}; ignored"));
            }
        }
    }

    /// Parses one `[[corpora]]` entry (§2.1).
    fn corpus(&mut self, value: &toml::Value, index: usize) -> Result<Corpus, String> {
        let position = label(index, None);
        let table = value
            .as_table()
            .ok_or_else(|| format!("{position} must be a table"))?;
        self.unknown(table, &CORPUS_KEYS, &position);
        let raw: RawCorpus = value
            .clone()
            .try_into()
            .map_err(|error| format!("{position}: {}", error.to_string().trim_end()))?;
        let entry = label(index, Some(&raw.id));
        if !valid_id(&raw.id) {
            return Err(format!("{entry}: `id` must match `[a-z0-9][a-z0-9._-]*`"));
        }
        let root = resolve_root(&raw.root, self.origin)
            .map_err(|why| format!("{entry}: root {}: {why}", raw.root))?;
        let index_dir = raw.index_dir.as_deref().map_or_else(
            || root.join(config::JANUS_DIR),
            |dir| config::resolve_index_dir(&root, Path::new(dir)),
        );
        let read_only = raw
            .read_only
            .unwrap_or_else(|| raw.role.read_only_by_default());
        Ok(Corpus {
            id: raw.id,
            root,
            role: raw.role,
            read_only,
            index_dir,
        })
    }

    /// Reads `[search]`, warning about keys and values that cannot mean anything.
    fn search(&mut self, value: &toml::Value) -> Result<SearchPolicy, String> {
        let table = value
            .as_table()
            .ok_or_else(|| "`search` must be a table".to_owned())?;
        self.unknown(table, &SEARCH_KEYS, "[search]");
        let raw: RawSearch = value
            .clone()
            .try_into()
            .map_err(|error| format!("[search]: {}", error.to_string().trim_end()))?;
        let mut policy = SearchPolicy::default();
        if let Some(weight) = raw.archived_weight {
            if weight.is_finite() && (0.0..=1.0).contains(&weight) {
                policy.archived_weight = weight;
            } else {
                self.warn(
                    "[search]",
                    &format!(
                        "`archived_weight` must be a number from 0 to 1, not {weight}; \
                         using {DEFAULT_ARCHIVED_WEIGHT}"
                    ),
                );
            }
        }
        policy.allow_cross_corpus_links = raw.allow_cross_corpus_links.unwrap_or(false);
        Ok(policy)
    }
}

/// How a message names one entry: its position, and its id once that is known.
fn label(index: usize, id: Option<&str>) -> String {
    match id {
        Some(id) => format!("corpora[{index}] (id {id:?})"),
        None => format!("corpora[{index}]"),
    }
}

/// The `id` pattern of §6.11: lowercase, and safe in a `corpus:path` qualifier.
fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    let first = chars.next().unwrap_or(' ');
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-')
        })
}

/// Canonicalizes one root, resolving a relative value against the workspace
/// file's directory (M6b-8).
fn resolve_root(declared: &str, origin: &Path) -> Result<PathBuf, String> {
    let declared = Path::new(declared);
    let candidate = if declared.is_absolute() {
        declared.to_path_buf()
    } else {
        origin.parent().unwrap_or(Path::new("")).join(declared)
    };
    let root = dunce::canonicalize(&candidate).map_err(|error| error.to_string())?;
    if root.is_dir() {
        Ok(root)
    } else {
        Err("not a directory".to_owned())
    }
}

/// Rejects a duplicate `id` or a primary count other than one, naming the entries.
fn check(corpora: &[(usize, Corpus)]) -> Result<(), String> {
    for (position, (index, corpus)) in corpora.iter().enumerate() {
        if let Some((first, _)) = corpora[..position]
            .iter()
            .find(|(_, other)| other.id == corpus.id)
        {
            return Err(format!(
                "{} and {} declare the same id {:?}",
                label(*first, Some(&corpus.id)),
                label(*index, Some(&corpus.id)),
                corpus.id
            ));
        }
    }
    let primaries: Vec<String> = corpora
        .iter()
        .filter(|(_, corpus)| corpus.role == Role::Primary)
        .map(|(index, corpus)| label(*index, Some(&corpus.id)))
        .collect();
    match primaries.len() {
        1 => Ok(()),
        0 => Err("no corpus has role \"primary\"; a workspace needs exactly one".to_owned()),
        _ => Err(format!(
            "{} all have role \"primary\"; a workspace needs exactly one",
            primaries.join(" and ")
        )),
    }
}

// ────────────────────────────── janus corpora ──────────────────────────────

/// How `janus corpora` should be rendered.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Emit one JSON object on stdout instead of the table.
    pub json: bool,
}

/// Runs `janus corpora`: each corpus, its role, and its index freshness.
///
/// One manifest per corpus, and one walk only for the digest. It builds nothing
/// and takes no lock (case 1.5). A corpus whose config or manifest cannot be read
/// reports `index_stale: null`, so one broken corpus does not hide the others.
///
/// # Errors
///
/// Only a serialization failure, which this report cannot cause.
pub fn run(workspace: &Workspace, options: Options) -> Result<(), JanusError> {
    for warning in &workspace.warnings {
        eprintln!("warning: {warning}");
    }
    let rows = rows(workspace);
    let mut out = io::stdout().lock();
    if options.json {
        let report = Report {
            api: API_VERSION,
            workspace: workspace.path.display().to_string(),
            corpora: rows,
        };
        let json = serde_json::to_string_pretty(&report)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        // A closed pipe is not a failure (`janus corpora --json | head`).
        let _ = writeln!(out, "{json}");
        return Ok(());
    }
    let _ = writeln!(out, "workspace  {}", workspace.path.display());
    for row in &rows {
        let read_only = if row.corpus.read_only { "yes" } else { "no" };
        let _ = writeln!(
            out,
            "{}  {}  read-only: {read_only}  index: {}  {}",
            row.corpus.id,
            row.corpus.role.as_str(),
            index_state(row.index_present, row.index_stale),
            row.corpus.root.display()
        );
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct Report {
    api: u32,
    workspace: String,
    corpora: Vec<Row>,
}

/// One corpus's declared facts and the state of its index.
///
/// `janus corpora` prints these rows, and `janus status` in a workspace prints
/// them as its `corpora` block, so the two surfaces report one shape (§3.3 of
/// the M6b plan).
#[derive(Debug, Serialize)]
pub struct Row {
    /// The corpus's declared facts: id, root, role, read-only and index dir.
    #[serde(flatten)]
    pub corpus: Corpus,
    /// Whether the corpus holds a `janus.toml`.
    pub config_present: bool,
    /// Whether the index directory holds a manifest.
    pub index_present: bool,
    /// Whether the corpus changed since that manifest was built; `None` when no
    /// manifest exists or the corpus config cannot be read.
    pub index_stale: Option<bool>,
}

/// One row per corpus, in workspace order.
///
/// Reads at most one manifest and one walk per corpus. It builds nothing and
/// takes no lock, which is what lets a status call run while a writer holds the
/// index (M6-6, case 1.5).
#[must_use]
pub fn rows(workspace: &Workspace) -> Vec<Row> {
    workspace.corpora.iter().map(Row::of).collect()
}

impl Row {
    /// Reads at most one manifest and one walk.
    fn of(corpus: &Corpus) -> Self {
        let manifest = fs::read(corpus.index_dir.join(store::MANIFEST_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).ok());
        let index_stale = manifest
            .as_ref()
            .and_then(|manifest| staleness(&corpus.root, manifest));
        Self {
            corpus: corpus.clone(),
            config_present: corpus.root.join(config::CONFIG_FILE).is_file(),
            index_present: manifest.is_some(),
            index_stale,
        }
    }
}

/// How one index reads in the table, and in a workspace `status` answer.
pub(crate) fn index_state(present: bool, stale: Option<bool>) -> &'static str {
    match (present, stale) {
        (false, _) => "absent; run janus index",
        (true, Some(true)) => "stale",
        (true, Some(false)) => "fresh",
        (true, None) => "unknown",
    }
}

/// Whether the corpus changed since the generation was built: the same one walk
/// and one digest a read makes. `None` when the corpus config cannot be read.
fn staleness(root: &Path, manifest: &Manifest) -> Option<bool> {
    let config = Config::load(root).ok()?;
    let walk = vault::walk(root, &config, WalkOptions::default()).ok()?;
    let digest = Signature::of(&walk.files).digest();
    Some(manifest.corpus_sig.as_deref() != Some(digest.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A root the loader can canonicalize, beside the workspace file.
    fn corpus(dir: &Path, name: &str) -> PathBuf {
        let root = dir.join(name);
        fs::create_dir_all(&root).expect("create the corpus");
        fs::write(root.join(config::CONFIG_FILE), "corpus_id = \"main\"\n")
            .expect("write the corpus config");
        root
    }

    #[test]
    fn a_declared_index_dir_and_weight_are_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        corpus(dir.path(), "vault");
        let path = dir.path().join(WORKSPACE_FILE);
        fs::write(
            &path,
            "[[corpora]]\nid = 'main'\nroot = 'vault'\nrole = 'primary'\n\
             index_dir = '/cache/main'\n\n[search]\narchived_weight = 0.5\n",
        )
        .expect("write the workspace");

        let workspace = Workspace::load(&path).expect("the workspace loads");
        assert_eq!(workspace.corpora[0].index_dir, Path::new("/cache/main"));
        assert_eq!(workspace.search.archived_weight, 0.5);
        assert!(workspace.warnings.is_empty(), "{:?}", workspace.warnings);
    }

    #[test]
    fn an_index_dir_defaults_to_the_janus_directory_under_the_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = corpus(dir.path(), "vault");
        let path = dir.path().join(WORKSPACE_FILE);
        fs::write(
            &path,
            "[[corpora]]\nid = 'main'\nroot = 'vault'\nrole = 'primary'\n",
        )
        .expect("write the workspace");

        let workspace = Workspace::load(&path).expect("the workspace loads");
        assert_eq!(
            workspace.corpora[0].index_dir,
            dunce::canonicalize(root)
                .expect("canonical root")
                .join(config::JANUS_DIR)
        );
        assert_eq!(workspace.search, SearchPolicy::default());
    }
}
