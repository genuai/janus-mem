//! The read tool descriptors and the `tools/call` dispatch (M6 §3.7).
//!
//! A schema is written by hand, here, and nothing is generated at run time. The
//! schema and the parameter struct of [`crate::api::dto`] must describe the same
//! fields, so one test compares them: a field that moves in either place fails
//! that test instead of becoming a silent default (§11).
//!
//! `tools/call` asks the engine for one answer and wraps it twice: as one text
//! block and as structured content. A legal shape with a bad combination stays a
//! tool result with `ok: false`, because the caller must read `code`, not the
//! transport error (M6-8).

use serde_json::{Map, Value, json};

use crate::api::{Call, Tool};
use crate::engine::Engine;
use crate::mcp::protocol::{self, Error, INVALID_PARAMS};

/// The order `tools/list` reports, and the only tools this server serves.
///
/// It is the order of SPEC §9.2, which differs from [`Tool::ALL`]: no surface may
/// invent a name, and this list decides the order a client reads (§3.6).
/// `search` and `read_document` answer here; the three query tools arrive with
/// the rest of the phase's surface.
const LISTED: [Tool; 2] = [Tool::Search, Tool::ReadDocument];

/// The answer to one `tools/list` request.
#[must_use]
pub fn descriptors() -> Value {
    let tools: Vec<Value> = LISTED.iter().filter_map(|tool| descriptor(*tool)).collect();
    json!({ "tools": tools })
}

/// Answers one `tools/call` request.
///
/// # Errors
///
/// [`INVALID_PARAMS`] when the request has no legal shape: a missing tool name,
/// an `arguments` value that is not an object, or a name this server does not
/// serve.
pub fn call(engine: &Engine, params: &Value) -> Result<Value, Error> {
    let fields = params.as_object().ok_or_else(|| {
        Error::new(
            INVALID_PARAMS,
            "params must be an object with a tool name and its arguments",
        )
    })?;
    let name = fields.get("name").and_then(Value::as_str).ok_or_else(|| {
        Error::new(
            INVALID_PARAMS,
            "name must be a string naming one tool of tools/list",
        )
    })?;
    let arguments = match fields.get("arguments") {
        None => Value::Object(Map::new()),
        Some(value) if value.is_object() => value.clone(),
        Some(other) => {
            return Err(Error::new(
                INVALID_PARAMS,
                format!("arguments must be an object; got {}", protocol::kind(other)),
            ));
        }
    };
    let tool = Tool::parse(name).map_err(|error| Error::new(INVALID_PARAMS, error.to_string()))?;
    if !LISTED.contains(&tool) {
        return Err(Error::new(
            INVALID_PARAMS,
            format!("{name} is not a tool of this server; see tools/list"),
        ));
    }
    let response = engine.call(&Call::new(tool, arguments));
    Ok(tool_result(&response.json))
}

/// One tool result: the envelope as text and as structured content.
///
/// An older client that reads only `content` still gets the whole answer, and a
/// tool failure adds `isError` (case 7.5).
fn tool_result(envelope: &Value) -> Value {
    let mut result = Map::new();
    result.insert(
        "content".to_owned(),
        json!([{ "type": "text", "text": envelope.to_string() }]),
    );
    result.insert("structuredContent".to_owned(), envelope.clone());
    if envelope["ok"] == Value::Bool(false) {
        result.insert("isError".to_owned(), Value::Bool(true));
    }
    Value::Object(result)
}

/// One tool descriptor: its wire name, its sentence, and its schema.
fn descriptor(tool: Tool) -> Option<Value> {
    let (Some(description), Some(input_schema)) = (describe(tool), schema(tool)) else {
        return None;
    };
    Some(json!({
        "name": tool.wire_name(),
        "description": description,
        "inputSchema": input_schema,
    }))
}

/// What one tool answers, in one sentence.
fn describe(tool: Tool) -> Option<&'static str> {
    match tool {
        Tool::Search => Some(
            "Ranked documents for one query: scores, spans and status, never text. \
             Use read_document for the text.",
        ),
        Tool::ReadDocument => Some(
            "One document's frontmatter and body, or one chunk's span. The only tool \
             that returns text.",
        ),
        Tool::Lineage | Tool::AuthoritativeFacts | Tool::Status => None,
    }
}

/// The schema of one tool's arguments, hand-written (§2.5, §9.2).
///
/// `None` names a tool this server does not serve yet: the tool exists in the
/// engine, and nothing may list it before its schema and its dispatch do.
fn schema(tool: Tool) -> Option<Value> {
    match tool {
        Tool::Search => Some(json!({
            "type": "object",
            "properties": {
                "query": { "type": "string",
                    "description": "The query text." },
                "limit": { "type": "integer",
                    "description": "Maximum documents returned. Default 10." },
                "intent": { "type": "string",
                    "description": "current, history, evolution or exhaustive. An unknown value \
                                    is a tool failure, not a schema error. Default current." },
                "as_of": { "type": "string",
                    "description": "The valid-time date: YYYY-MM-DD, YYYY-MM or YYYY. \
                                    Default: no window." },
                "scope": { "type": "array", "items": { "type": "string" },
                    "description": "A document matches any one of these scopes. Default: none." },
                "scope_only": { "type": "boolean",
                    "description": "Exclude global and shared-only matches; requires scope. \
                                    Default false." },
                "types": { "type": "array", "items": { "type": "string" },
                    "description": "Canonical type names or configured aliases. \
                                    Default: no restriction." },
                "tags": { "type": "array", "items": { "type": "string" },
                    "description": "A document matches any one of these tags. Default: none." },
                "include_superseded": { "type": "boolean",
                    "description": "Admit declared-superseded documents under current. \
                                    Default false." },
                "decay_half_life_days": { "type": "number",
                    "description": "Override the per-type decay half-life, in days. \
                                    Default: the corpus setting." }
            },
            "required": ["query"],
            "additionalProperties": false
        })),
        Tool::ReadDocument => Some(json!({
            "type": "object",
            "properties": {
                "path": { "type": "string",
                    "description": "The corpus path of the document." },
                "max_chars": { "type": "integer",
                    "description": "The text cap in characters; 0 means no limit. Default: the \
                                    [mcp] max_chars setting, 4000 when unset." },
                "chunk_id": { "type": "string",
                    "description": "Return one chunk's span text instead of the whole body." }
            },
            "required": ["path"],
            "additionalProperties": false
        })),
        Tool::Lineage | Tool::AuthoritativeFacts | Tool::Status => None,
    }
}
