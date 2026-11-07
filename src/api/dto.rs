//! The one success head and the one failure object (M6 §2.1).
//!
//! [`Ok`] flattens its payload, so a report keeps every field it has today at
//! the top level. It gains `api` and `ok` and nothing else, which keeps
//! [`crate::scan::API_VERSION`] at `1`.
//!
//! [`Failure`] carries no `results`, no `taken_ms` and no `vault`. A failure has
//! no partial answer, and an object that sometimes holds a list invites a reader
//! to read an empty one.

use serde::{Deserialize, Serialize};

use crate::api::error::ApiCode;
use crate::error::JanusError;
use crate::scan::API_VERSION;

/// A successful answer: the shared head, then the tool's own fields.
#[derive(Debug, Serialize)]
pub struct Ok<T> {
    /// The response contract version (§9.5).
    pub api: u32,
    /// Always `true`. An agent branches on this field first.
    pub ok: bool,
    /// The tool's fields, unchanged and at the top level.
    #[serde(flatten)]
    pub payload: T,
}

impl<T> Ok<T> {
    /// Wraps one payload in the success head.
    ///
    /// The version comes from [`API_VERSION`] in one place, so no adapter can
    /// announce a version the others do not.
    pub fn new(payload: T) -> Self {
        Self {
            api: API_VERSION,
            ok: true,
            payload,
        }
    }
}

/// A failed answer: what went wrong, and what the caller can do about it.
#[derive(Debug, Serialize)]
pub struct Failure {
    /// The response contract version (§9.5).
    pub api: u32,
    /// Always `false` here. An agent branches on this field first.
    pub ok: bool,
    /// The stable code; see [`ApiCode`].
    pub code: &'static str,
    /// The failure, in the words the error type already uses.
    pub message: String,
    /// Advice for the agent, when the failure has some. Never for the runner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Failure {
    /// Builds the failure object for one error, from the code table.
    ///
    /// The exit code is the caller's: [`ApiCode::of`] holds it, and the CLI is
    /// the only surface that exits on a failure (§9.1).
    pub fn from_error(err: &JanusError) -> Self {
        Self {
            api: API_VERSION,
            ok: false,
            code: ApiCode::of(err).code,
            message: err.to_string(),
            hint: None,
        }
    }
}

// ─────────────────────────── the tool parameters ────────────────────────────
//
// One struct per tool, with `deny_unknown_fields`: a parameter the schema does
// not list is a usage failure, never a silent ignore (M6 §2.5, case 7.6). The
// names are the wire names §9.2 states, in `snake_case`, with no camelCase pass.
//
// The structs are `Serialize` as well as `Deserialize` so the MCP tool schemas
// and these fields can be compared by one test: an encoded struct names every
// field it holds, and a field that moves fails that test (§3.7).

/// The parameters of one `search` call (M6 §2.5).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchParams {
    /// The query text; required.
    pub query: String,
    /// Maximum documents returned; the default is [`crate::search::DEFAULT_LIMIT`].
    pub limit: Option<usize>,
    /// `current`, `history`, `evolution` or `exhaustive`.
    pub intent: Option<String>,
    /// The valid-time date: `YYYY-MM-DD`, `YYYY-MM` or `YYYY`.
    pub as_of: Option<String>,
    /// The scope set; a document matches any one of them.
    pub scope: Option<Vec<String>>,
    /// Exclude global and shared-only matches; requires `scope`.
    pub scope_only: Option<bool>,
    /// Canonical type names or configured aliases.
    pub types: Option<Vec<String>>,
    /// Frontmatter tags; a document matches any one of them.
    pub tags: Option<Vec<String>>,
    /// Admit declared-superseded documents under `current`.
    pub include_superseded: Option<bool>,
    /// Override the per-type decay half-life, in days.
    pub decay_half_life_days: Option<f64>,
}

/// The parameters of one `get_authoritative_facts` call (M6 §2.5).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FactsParams {
    /// Narrows the listing by relevance and reports the score.
    pub query: Option<String>,
    /// Frontmatter tags; a document matches any one of them.
    pub tags: Option<Vec<String>>,
    /// Canonical type names or configured aliases.
    pub types: Option<Vec<String>>,
    /// The scope set; a document matches any one of them.
    pub scope: Option<Vec<String>>,
    /// Maximum facts returned.
    pub limit: Option<usize>,
}

/// The parameters of one `get_lineage` call (M6 §2.5): exactly one is required.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LineageParams {
    /// The corpus path of the document whose chain to report.
    pub path: Option<String>,
    /// A `current` query whose top result the chain is reported for.
    pub topic: Option<String>,
}

/// The parameters of one `janus_status` call: none (M6 §2.5).
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StatusParams {}

/// The parameters of one `read_document` call (M6 §2.5).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadParams {
    /// The corpus path of the document; required.
    pub path: String,
    /// The text cap in characters; `0` means no limit.
    pub max_chars: Option<usize>,
    /// Return one chunk's span text instead of the whole body.
    pub chunk_id: Option<String>,
}
