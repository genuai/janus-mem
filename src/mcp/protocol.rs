//! JSON-RPC 2.0 framing for the MCP server (M6 §3.6).
//!
//! One message per line. A message with an `id` is a request and is answered. A
//! message without an `id` is a notification and is never answered, whatever its
//! method names says, so a cancelled call cannot produce a stray line.
//!
//! The codes are the protocol's own: `-32700` for a line that is not JSON,
//! `-32600` for JSON that is not a request object, `-32601` for a method this
//! server does not serve, and `-32602` for parameters that do not fit the
//! method's shape. JSON-RPC reserves `-32000..=-32099` for an implementation's
//! own errors, and `-32002` is the one this server uses for a request that
//! arrives before the handshake (§3.6 case 6.5).

use serde_json::{Map, Value, json};

/// The newest protocol revision this server speaks (§3.6, M6-9).
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Every revision this server accepts, newest first.
///
/// The two revisions differ in no message this server sends, so echoing either
/// one is honest, and an older host must still connect (§3.6 case 6.2).
pub const SUPPORTED_VERSIONS: [&str; 2] = [PROTOCOL_VERSION, "2025-03-26"];

/// What the client shows the model before the first tool call (§9.2).
///
/// This is the one place an agent learns how the corpus is organised and what
/// to do when a note turns out to matter. It is product copy, not an
/// implementation detail: a rewritten sentence fails a test.
pub const INSTRUCTIONS: &str = "You are reading a janus-mem corpus: Markdown files, each with a type and a date.\n\nThe type sets how much a file counts in a search, from idea at the bottom to decision at the top. The date sets how fast it fades. Principles, specs and decisions never fade.\n\nThe ladder is idea, note, journal, finding, principle, spec, decision.\n\nWhen a note turns out to matter, it is time to promote it. Ask the user to restate it as a principle, a spec or a decision, with its own date. A promoted note keeps answering questions after the note itself would have faded.\n\nTags say what a file is about. The type says how much it counts.\n\nsearch gives you spans and scores. read_document gives you the words.\n\nThis release reads. It does not write.";

/// The line was not JSON.
pub const PARSE_ERROR: i64 = -32700;
/// The JSON was not a valid request object.
pub const INVALID_REQUEST: i64 = -32600;
/// The request named a method this server does not serve.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// The request's parameters do not fit the method or tool schema.
pub const INVALID_PARAMS: i64 = -32602;
/// This server cannot encode the object it wanted to answer with.
pub const INTERNAL_ERROR: i64 = -32603;
/// A request arrived before `initialize` was answered.
pub const NOT_INITIALIZED: i64 = -32002;

/// One message read from one line.
#[derive(Debug, Clone)]
pub enum Message {
    /// A request: it has an `id`, so it earns exactly one answer.
    Request {
        /// The value to echo unchanged.
        id: Value,
        /// The method name, before any table decides whether it is known.
        method: String,
        /// The parameters, `{}` when the message named none.
        params: Value,
    },
    /// A notification: no `id`, so no answer.
    Notification,
}

/// One protocol error, ready to become the `error` object of an answer.
#[derive(Debug, Clone)]
pub struct Error {
    /// The JSON-RPC code, one of the constants above.
    pub code: i64,
    /// One sentence a human can read.
    pub message: String,
    /// The `id` to answer with. It is the message's own `id` when the message
    /// carried one, and `null` when it did not, as JSON-RPC requires.
    pub id: Value,
}

impl Error {
    /// An error with one code and one sentence, answered with `id: null`.
    #[must_use]
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            id: Value::Null,
        }
    }

    /// The same error, answered to the `id` of the message it refuses.
    ///
    /// A request that is malformed but names itself still gets its own `id`
    /// back, so a client can match the answer to the line it sent.
    #[must_use]
    pub fn about(mut self, id: Option<&Value>) -> Self {
        if let Some(id) = id {
            self.id = id.clone();
        }
        self
    }
}

/// Reads one line as one message, or returns the error that answers it.
///
/// A line with no `id` is a notification even when its method is unknown, so
/// nothing is ever written for it (§3.6 case 6.4).
///
/// # Errors
///
/// [`PARSE_ERROR`] for a line that is not JSON, and [`INVALID_REQUEST`] for JSON
/// that is not a request object: an array, a missing `method`, a `method` that
/// is not a string, or a `jsonrpc` field that is not `"2.0"`.
pub fn parse(line: &str) -> Result<Message, Error> {
    let value: Value = serde_json::from_str(line)
        .map_err(|error| Error::new(PARSE_ERROR, format!("the line is not JSON: {error}")))?;
    let mut fields = match value {
        Value::Object(fields) => fields,
        other => {
            return Err(Error::new(
                INVALID_REQUEST,
                format!("a message is one JSON object; got {}", kind(&other)),
            ));
        }
    };
    if !fields.contains_key("id") {
        return Ok(Message::Notification);
    }
    let id = fields.get("id").cloned();
    let version_ok = matches!(fields.get("jsonrpc"), Some(Value::String(text)) if text == "2.0");
    if !version_ok {
        return Err(Error::new(
            INVALID_REQUEST,
            r#"the jsonrpc field must be the string "2.0""#,
        )
        .about(id.as_ref()));
    }
    let method = match fields.remove("method") {
        Some(Value::String(method)) => method,
        Some(other) => {
            return Err(Error::new(
                INVALID_REQUEST,
                format!("method must be a string; got {}", kind(&other)),
            )
            .about(id.as_ref()));
        }
        None => {
            return Err(Error::new(INVALID_REQUEST, "method is missing").about(id.as_ref()));
        }
    };
    Ok(Message::Request {
        id: id.unwrap_or(Value::Null),
        method,
        params: fields
            .remove("params")
            .unwrap_or_else(|| Value::Object(Map::new())),
    })
}

/// One answer that carries a result.
#[must_use]
pub fn result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// One answer that carries an error.
#[must_use]
pub fn error_answer(id: &Value, error: &Error) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": error.code, "message": error.message },
    })
}

/// The name of one JSON value's kind, for a message a human reads.
#[must_use]
pub fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a bool",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}
