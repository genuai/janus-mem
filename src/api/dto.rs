//! The one success head and the one failure object (M6 §2.1).
//!
//! [`Ok`] flattens its payload, so a report keeps every field it has today at
//! the top level. It gains `api` and `ok` and nothing else, which keeps
//! [`crate::scan::API_VERSION`] at `1`.
//!
//! [`Failure`] carries no `results`, no `taken_ms` and no `vault`. A failure has
//! no partial answer, and an object that sometimes holds a list invites a reader
//! to read an empty one.

use serde::Serialize;

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
