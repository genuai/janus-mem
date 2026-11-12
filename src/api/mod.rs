//! The shared answer head and the stable failure codes (M6 §2, §9.5).
//!
//! Every tool surface returns one object with the same first two fields: `api`,
//! then `ok`. An agent learns that contract once and branches on `ok`, then on
//! `code`, then on `hint`. The CLI, the rpc loop and the MCP server all build
//! these types, so none of them can drift from the others.
//!
//! The envelope lives here, not in an adapter, because the spec makes it public
//! API: adding a field is additive and keeps [`API_VERSION`] at `1`, and
//! renaming one is a breaking change (§9.5).
//!
//! The write gate lives here too ([`Writes`] and [`write_refusal`]), so `janus
//! mcp`, `janus rpc` and the CLI cannot disagree about whether a write is
//! allowed (M7-15, M7-36).

pub mod dto;
pub mod error;

pub use crate::scan::API_VERSION;
pub use dto::{Failure, Ok, Receipt, UpdateParams};
pub use error::ApiCode;

use serde_json::Value;

use crate::error::JanusError;

/// One read tool this surface serves (M6 §2.3).
///
/// One enum means the MCP tool list, the rpc tool list and the tests cannot
/// drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// `search`: ranked documents for one query.
    Search,
    /// `get_authoritative_facts`: active decisions, specs and principles.
    AuthoritativeFacts,
    /// `get_lineage`: the supersession chain around one document.
    Lineage,
    /// `read_document`: the text of one document, or of one chunk of it.
    ReadDocument,
    /// `janus_status`: index freshness, embeddings, and the cache counters.
    Status,
    /// `update_frontmatter`: one surgical patch of a document's frontmatter.
    UpdateFrontmatter,
}

impl Tool {
    /// Every tool this phase serves, in the order a surface lists them.
    pub const ALL: &'static [Self] = &[
        Self::Search,
        Self::AuthoritativeFacts,
        Self::Lineage,
        Self::ReadDocument,
        Self::Status,
        Self::UpdateFrontmatter,
    ];

    /// The wire name an agent sends and reads (§9.2).
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::AuthoritativeFacts => "get_authoritative_facts",
            Self::Lineage => "get_lineage",
            Self::ReadDocument => "read_document",
            Self::Status => "janus_status",
            Self::UpdateFrontmatter => "update_frontmatter",
        }
    }

    /// True when the call names one document in `path`, so a `corpus:path`
    /// qualifier may stand in for the `corpus` parameter (M6b-6, M7-38).
    #[must_use]
    pub const fn names_a_path(self) -> bool {
        matches!(self, Self::ReadDocument | Self::UpdateFrontmatter)
    }

    /// Reads one wire name. This and [`Tool::wire_name`] are the only places a
    /// name becomes a tool, so an unknown name is rejected before any work runs.
    ///
    /// # Errors
    ///
    /// [`JanusError::Usage`] naming the unknown tool and listing the known ones.
    pub fn parse(name: &str) -> Result<Self, JanusError> {
        if let Some(tool) = Self::ALL.iter().find(|tool| tool.wire_name() == name) {
            return Ok(*tool);
        }
        let known: Vec<&str> = Self::ALL.iter().map(|tool| tool.wire_name()).collect();
        Err(JanusError::Usage {
            message: format!("unknown tool {name:?}; expected {}", known.join(", ")),
        })
    }
}

/// One tool invocation: which tool, and the parameters it was given (M6 §2.3).
///
/// The parameters stay untyped here on purpose: an adapter echoes what it
/// received, and the engine decides whether the shape is legal. Deserializing
/// them into a typed struct is the engine's first step.
#[derive(Debug, Clone)]
pub struct Call {
    /// The tool to run.
    pub tool: Tool,
    /// The parameters as they arrived, already one JSON object.
    pub params: Value,
}

impl Call {
    /// A call of one tool with its parameters.
    #[must_use]
    pub const fn new(tool: Tool, params: Value) -> Self {
        Self { tool, params }
    }

    /// A call built from a wire name and its parameters.
    ///
    /// # Errors
    ///
    /// [`JanusError::Usage`] for an unknown tool name.
    pub fn parse(name: &str, params: Value) -> Result<Self, JanusError> {
        Ok(Self::new(Tool::parse(name)?, params))
    }
}

/// The write tools this release names but does not serve yet (§3.8).
///
/// `update_frontmatter` left this list in M7-4, which serves it. A caller that
/// sends one of the three remaining names gets a refusal, not "no such tool":
/// the name is legal and the phase that serves it has not landed. Each phase
/// deletes its own name.
const UNLANDED_WRITE_TOOLS: [&str; 3] =
    ["write_document", "supersede_document", "promote_document"];

/// The four write names of §9.3, served or not.
const WRITE_TOOLS: [&str; 4] = [
    "update_frontmatter",
    "write_document",
    "supersede_document",
    "promote_document",
];

/// What one surface may write, and whether the corpus allows it (M7-15, M7-16).
///
/// This is the only write switch. A surface reads `--allow-write` once, at the
/// edge, and passes the result here; every later check reads this value, so a
/// config key cannot enable a write and no path can skip the gate (M7-36).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Writes {
    /// The surface that answers, e.g. `janus mcp`. A refusal names it.
    pub surface: &'static str,
    /// True when the surface serves writes: the flag, or a typed CLI command.
    pub allowed: bool,
    /// True when the corpus itself refuses every write (M7-28).
    pub read_only: bool,
}

impl Writes {
    /// A surface that was not started with `--allow-write`, over a writable
    /// corpus.
    #[must_use]
    pub const fn off(surface: &'static str) -> Self {
        Self {
            surface,
            allowed: false,
            read_only: false,
        }
    }

    /// The writes one corpus of one surface accepts.
    #[must_use]
    pub const fn new(surface: &'static str, allowed: bool, read_only: bool) -> Self {
        Self {
            surface,
            allowed,
            read_only,
        }
    }

    /// True when a write may reach this corpus through this surface.
    ///
    /// A capability flag that says true on a read-only corpus would invite a
    /// call that must fail (M7-16).
    #[must_use]
    pub const fn enabled(self) -> bool {
        self.allowed && !self.read_only
    }

    /// The refusal this surface owes a write to one corpus, or `None`.
    ///
    /// `named` is the id the caller used and `declared` is the corpus's own
    /// `corpus_id`. They differ in a workspace, where the workspace's name for a
    /// corpus is the one a caller types; the message names the caller's name and
    /// then the declared one, so nobody hunts for a corpus that is not in their
    /// file (§9.2 does the same with a head and a row).
    ///
    /// The flag is checked first: the surface gate is the outer policy, and a
    /// server without the flag answers the same way for every corpus.
    #[must_use]
    pub fn refusal(self, named: &str, declared: &str) -> Option<JanusError> {
        if !self.allowed {
            return Some(JanusError::WriteDisabled {
                surface: self.surface.to_owned(),
            });
        }
        if self.read_only {
            let corpus = if named == declared {
                named.to_owned()
            } else {
                format!("{named} (declared {declared})")
            };
            return Some(JanusError::ReadOnlyCorpus { corpus });
        }
        None
    }
}

/// The refusal for one tool name, or `None` when the name may run.
///
/// The refusal lives here, not in an adapter, so `janus rpc` (case 5.8) and
/// `janus mcp` (case 8.2) cannot answer the same name differently.
///
/// Two rules, in order. Every write name is refused when the surface has no
/// `--allow-write` (case 4.1, case 4.2). A write tool whose phase has not
/// landed is refused even with the flag, because the release does not serve it.
#[must_use]
pub fn write_refusal(name: &str, writes: Writes) -> Option<Failure> {
    if !WRITE_TOOLS.contains(&name) {
        return None;
    }
    if !writes.allowed {
        return Some(Failure::from_error(&JanusError::WriteDisabled {
            surface: writes.surface.to_owned(),
        }));
    }
    if !UNLANDED_WRITE_TOOLS.contains(&name) {
        return None;
    }
    Some(Failure {
        api: API_VERSION,
        ok: false,
        code: ApiCode::refused().code,
        message: format!("{name} is a write tool, and this release does not serve it yet"),
        hint: Some("the phase that serves it is not merged; use a read tool until then".to_owned()),
    })
}
