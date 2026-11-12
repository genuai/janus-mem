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

use std::collections::BTreeMap;

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
            hint: hint(err),
        }
    }
}

/// The advice one failure carries, when the error itself holds it.
///
/// A refused write with no etag names the keys that would change, in one line
/// (M7-13). The full preview is a `dry_run` call, because §9.4 says a failure
/// carries no `results`.
fn hint(err: &JanusError) -> Option<String> {
    match err {
        JanusError::PreconditionRequired { preview, .. } => Some(preview.clone()),
        _ => None,
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
    /// The corpora to read: `primary` (default), `all`, or a workspace id.
    pub corpus: Option<String>,
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
    /// The corpora to read: `primary` (default), `all`, or a workspace id.
    pub corpus: Option<String>,
}

/// The parameters of one `get_lineage` call (M6 §2.5): exactly one target.
///
/// `corpus` names the corpora the chain may reach through a qualified link, and
/// a `corpus:path` in `path` names the same thing (M7-37).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LineageParams {
    /// The corpus path of the document whose chain to report.
    pub path: Option<String>,
    /// A `current` query whose top result the chain is reported for.
    pub topic: Option<String>,
    /// The corpora to read: `primary` (default), `all`, or a workspace id.
    pub corpus: Option<String>,
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
    /// The corpora to read: `primary` (default), or a workspace corpus id. A
    /// path may carry the same name as a `corpus:path` qualifier instead.
    pub corpus: Option<String>,
}

/// The parameters of one `update_frontmatter` call (§9.3).
///
/// `patch` is one object of key to string-or-null. A `null` writes `null`; a
/// key is never deleted (M7-6). `expected_hash` is the `content_hash` a read
/// returned, and the write refuses without it. The map is a `BTreeMap`, so the
/// keys arrive in one order whatever the caller sent.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateParams {
    /// The corpus path of the document; required.
    pub path: String,
    /// The keys to write, and the value of each. `null` writes `null`.
    pub patch: BTreeMap<String, Option<String>>,
    /// The etag a read returned. Required for a write that is not a dry run.
    pub expected_hash: Option<String>,
    /// Insert a block when the document has none. Default false.
    pub create_frontmatter: Option<bool>,
    /// Report what would change and write nothing.
    pub dry_run: Option<bool>,
}

impl UpdateParams {
    /// The patch as the writer's sorted key list.
    #[must_use]
    pub fn keys(&self) -> Vec<(String, Option<String>)> {
        self.patch
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

/// The mode of one `write_document` call (§9.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WriteMode {
    /// Write a path that does not exist. An existing path is `path_exists`.
    Create,
    /// Replace a file the caller has read. `expected_hash` is required.
    Overwrite,
}

/// The parameters of one `write_document` call (§9.3).
///
/// `frontmatter` is one object of key to string-or-null, and `body` is written
/// byte for byte after the block. `mode` decides which half of the contract
/// runs: `create` refuses an existing path, and `overwrite` requires the
/// `content_hash` a read returned.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteParams {
    /// The corpus path of the document; required.
    pub path: String,
    /// The keys to write, and the value of each. `null` writes `null`.
    pub frontmatter: BTreeMap<String, Option<String>>,
    /// The text written after the closing fence, byte for byte.
    pub body: String,
    /// `create` or `overwrite`.
    pub mode: WriteMode,
    /// The etag a read returned. Required for an overwrite that writes.
    pub expected_hash: Option<String>,
    /// Report what would change and write nothing.
    pub dry_run: Option<bool>,
}

/// The parameters of one `supersede_document` call (§9.3, M7-6).
///
/// `expected_hash` is the old document's etag. `new_path` must not exist: the
/// wire contract has one etag, so the created document cannot be guarded too
/// (M7-35). `append_only` creates the new document and leaves the old one
/// byte-identical. `new_title` writes the one optional key the new document
/// asks for; `supersedes` is always written.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SupersedeParams {
    /// The corpus path of the document being retired.
    pub old_path: String,
    /// The etag a read returned for the old document.
    pub expected_hash: Option<String>,
    /// The corpus path of the document that replaces it. It must not exist.
    pub new_path: String,
    /// The title to write on the new document, when one is wanted.
    pub new_title: Option<String>,
    /// Create the new document and leave the old one untouched. Default false.
    pub append_only: Option<bool>,
    /// Report what would change and write nothing.
    pub dry_run: Option<bool>,
}

/// The mode of one `promote_document` call (§6.11, §9.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PromoteMode {
    /// Patch the primary document's `supersedes` with the qualified source.
    Link,
    /// Create a primary document that carries the source's text.
    Adopt,
}

/// The parameters of one `promote_document` call (§6.11, §9.3, M7-23).
///
/// `source` is `corpus:path`, and it is read and never written. `target_path`
/// names the primary document that gains the link in `link` mode, and the path
/// the copy is written at in `adopt` mode; it defaults to the source's own path
/// when a copy is made. `expected_hash` is the target document's etag, so a
/// `link` needs one and an `adopt` has nothing to guard.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PromoteParams {
    /// The source document, as `corpus:path`.
    pub source: String,
    /// The primary document the call writes.
    pub target_path: Option<String>,
    /// `link` or `adopt`.
    pub mode: PromoteMode,
    /// The etag a read returned for the target document.
    pub expected_hash: Option<String>,
    /// Report what would change and write nothing.
    pub dry_run: Option<bool>,
}

/// One write's receipt (§2.3, M7-14).
///
/// A receipt is not an envelope: it carries the shared head and these fields,
/// and no `results` and no `notes`, because it describes bytes and not rows.
/// `new_hash` is the hash of the bytes written, so a caller can present it as the
/// next `expected_hash` (M7-18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Receipt {
    /// The corpus-relative path of the document that was written.
    pub path: String,
    /// Keys whose bytes differ after the patch, sorted (M7-44).
    pub changed_keys: Vec<String>,
    /// Keys that were absent and are now written, sorted.
    pub appended_keys: Vec<String>,
    /// blake3 of the bytes before the write; `null` for a create.
    pub old_hash: Option<String>,
    /// blake3 of the bytes after the write.
    pub new_hash: String,
    /// True when nothing was written.
    pub dry_run: bool,
    /// The file's length before the write, in bytes.
    pub bytes_before: usize,
    /// The file's length after the write, in bytes.
    pub bytes_after: usize,
    /// True when this write inserted the frontmatter block.
    pub created_block: bool,
}

/// The receipt of one `promote_document` call (§2.3, §3.7).
///
/// The shared fields describe the one file the call wrote, and the two added
/// fields name the corpus the source lives in, so a receipt says which archive a
/// reader must look in (case 7.2 of §3.9). Both fields are additive, so `api`
/// stays 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PromoteReceipt {
    /// The file the call wrote, flattened into the standard fields.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// The corpus the source document lives in.
    pub source_corpus: String,
    /// The source, as the written link names it: `corpus:path`.
    pub source: String,
}

/// The receipt of one `supersede_document` call (§2.3, M7-6).
///
/// The shared fields describe the first file the call wrote: the old document
/// in place, or the new one in append-only mode. `receipts` carries one receipt
/// per file, in the order they were written, and `chain` is the resolved chain
/// oldest first. Both fields are additive, so `api` stays 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SupersedeReceipt {
    /// The first file the call wrote, flattened into the standard fields.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// One receipt per file, in the order they were written.
    pub receipts: Vec<Receipt>,
    /// The resolved supersession chain, oldest first.
    pub chain: Vec<String>,
}
