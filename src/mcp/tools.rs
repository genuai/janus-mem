//! The five read tool descriptors and the `tools/call` dispatch (M6 §3.7).
//!
//! A schema is written by hand, here, and nothing is generated at run time. The
//! schema and the parameter struct of [`crate::api::dto`] must describe the same
//! fields, so one test compares them: a field that moves in either place fails
//! that test instead of becoming a silent default (§11).
//!
//! `tools/call` checks the arguments against the schema it just published, so an
//! unknown or mistyped parameter is `-32602` rather than a value the engine
//! ignores (M6-8, case 7.6). A legal shape with a bad combination stays a tool
//! result with `ok: false`, because the caller must read `code`, not the
//! transport error.

use serde_json::{Map, Value, json};

use crate::api::{Call, Failure, Tool, write_refusal};
use crate::engine::Engine;
use crate::mcp::protocol::{self, Error, INTERNAL_ERROR, INVALID_PARAMS};

/// The order `tools/list` reports, and the only tools this release lists.
///
/// It is the order of SPEC §9.2, which differs from [`Tool::ALL`]: no surface may
/// invent a name, and this list decides the order a client reads (§3.6 case 6.7).
const LISTED: [Tool; 5] = [
    Tool::Search,
    Tool::Lineage,
    Tool::AuthoritativeFacts,
    Tool::ReadDocument,
    Tool::Status,
];

/// The answer to one `tools/list` request.
#[must_use]
pub fn descriptors() -> Value {
    let tools: Vec<Value> = LISTED.iter().map(|tool| descriptor(*tool)).collect();
    json!({ "tools": tools })
}

/// Answers one `tools/call` request.
///
/// A write tool name is refused through [`crate::api::write_refusal`], before the
/// tool table is consulted, so MCP and rpc answer the same name alike (§3.8).
///
/// # Errors
///
/// [`INVALID_PARAMS`] when the request has no legal shape: a missing tool name,
/// an `arguments` value that is not an object, a name this server does not serve,
/// or an argument the tool's schema does not accept.
pub fn call(engine: &Engine, params: &Value, allow_write: bool) -> Result<Value, Error> {
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
    // The name is legal and the release is not: no write tool is served, in
    // either `--allow-write` mode (M6-7, case 8.2).
    if let Some(failure) = write_refusal(name, allow_write) {
        return Ok(tool_result(&encode(failure)?));
    }
    let tool = Tool::parse(name).map_err(|error| Error::new(INVALID_PARAMS, error.to_string()))?;
    validate(tool, &arguments)?;
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

/// One refusal as the object the caller reads.
fn encode(failure: Failure) -> Result<Value, Error> {
    serde_json::to_value(failure).map_err(|error| {
        Error::new(
            INTERNAL_ERROR,
            format!("cannot encode the refusal: {error}"),
        )
    })
}

/// Checks one arguments object against the tool's own schema.
///
/// The schema is the single source: an argument is legal when it names a
/// property, and its value is legal when it fits that property's `type`. A
/// missing property named in `required` is illegal too.
fn validate(tool: Tool, arguments: &Value) -> Result<(), Error> {
    let schema = schema(tool);
    let properties = &schema["properties"];
    for (field, value) in arguments.as_object().into_iter().flatten() {
        let Some(spec) = properties.get(field) else {
            return Err(invalid(tool, &format!("unknown parameter {field:?}")));
        };
        if !fits(spec, value) {
            return Err(invalid(
                tool,
                &format!("{field:?} must be {}", type_name(spec)),
            ));
        }
    }
    for field in schema["required"].as_array().into_iter().flatten() {
        let Some(field) = field.as_str() else {
            continue;
        };
        if arguments.get(field).is_none() {
            return Err(invalid(tool, &format!("{field:?} is required")));
        }
    }
    Ok(())
}

/// One `-32602` that names the tool and the field.
fn invalid(tool: Tool, detail: &str) -> Error {
    Error::new(INVALID_PARAMS, format!("{}: {detail}", tool.wire_name()))
}

/// True when `value` fits the JSON type one property spec states.
fn fits(spec: &Value, value: &Value) -> bool {
    match spec["type"].as_str() {
        Some("string") => value.is_string(),
        // `as_u64` rejects a negative number and a fraction, so a count cannot
        // arrive as `-1` or `1.5` and reach the engine as a cast.
        Some("integer") => value.as_u64().is_some(),
        Some("number") => value.is_number(),
        Some("boolean") => value.is_boolean(),
        Some("array") => value
            .as_array()
            .is_some_and(|items| items.iter().all(|item| fits(&spec["items"], item))),
        Some("object") => value.is_object(),
        _ => true,
    }
}

/// The JSON type one property spec states, for an error message.
fn type_name(spec: &Value) -> &str {
    spec["type"].as_str().unwrap_or("a valid value")
}

/// One tool descriptor: its wire name, its sentence, and its schema.
fn descriptor(tool: Tool) -> Value {
    json!({
        "name": tool.wire_name(),
        "description": describe(tool),
        "inputSchema": schema(tool),
    })
}

/// What one tool answers, in one sentence.
fn describe(tool: Tool) -> &'static str {
    match tool {
        Tool::Search => {
            "Ranked documents for one query: scores, spans and status, never text. \
             Use read_document for the text."
        }
        Tool::Lineage => {
            "The supersession chain that holds one document, root first, with its \
             terminal state. Name the document with path or with topic, exactly one."
        }
        Tool::AuthoritativeFacts => {
            "Active decisions, specs and principles only: canonical type, effective \
             status active, inside the validity window."
        }
        Tool::ReadDocument => {
            "One document's frontmatter and body, or one chunk's span. The only tool \
             that returns text."
        }
        Tool::Status => {
            "Index freshness, the embedder this process resolved, and the cache \
             counters. Builds nothing and takes no lock."
        }
    }
}

/// The schema of one tool's arguments, hand-written (§2.5, §9.2).
fn schema(tool: Tool) -> Value {
    match tool {
        Tool::Search => json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "The query text." },
                "limit": {
                    "type": "integer",
                    "description": "Maximum documents returned. Default 10."
                },
                "intent": {
                    "type": "string",
                    "description": "current, history, evolution or exhaustive. An unknown \
                                    value is a tool failure, not a schema error. Default current."
                },
                "as_of": {
                    "type": "string",
                    "description": "The valid-time date: YYYY-MM-DD, YYYY-MM or YYYY. \
                                    Default: no window."
                },
                "scope": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "A document matches any one of these scopes. Default: none."
                },
                "scope_only": {
                    "type": "boolean",
                    "description": "Exclude global and shared-only matches; requires scope. \
                                    Default false."
                },
                "types": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Canonical type names or configured aliases. \
                                    Default: no restriction."
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "A document matches any one of these tags. Default: none."
                },
                "include_superseded": {
                    "type": "boolean",
                    "description": "Admit declared-superseded documents under current. \
                                    Default false."
                },
                "decay_half_life_days": {
                    "type": "number",
                    "description": "Override the per-type decay half-life, in days. \
                                    Default: the corpus setting."
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
        Tool::Lineage => json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The corpus path of the document whose chain to report."
                },
                "topic": {
                    "type": "string",
                    "description": "A current query; the chain of its top result is reported."
                }
            },
            "required": [],
            "additionalProperties": false
        }),
        Tool::AuthoritativeFacts => json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Narrows the listing by relevance and reports the score. \
                                    Default: listing order."
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "A fact matches any one of these tags. Default: none."
                },
                "types": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Canonical type names or configured aliases. \
                                    Default: decision, spec and principle."
                },
                "scope": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "A fact matches any one of these scopes. Default: none."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum facts returned. Default 10."
                }
            },
            "required": [],
            "additionalProperties": false
        }),
        Tool::ReadDocument => json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The corpus path of the document."
                },
                "max_chars": {
                    "type": "integer",
                    "description": "The text cap in characters; 0 means no limit. \
                                    Default: the [mcp] max_chars setting, 4000 when unset."
                },
                "chunk_id": {
                    "type": "string",
                    "description": "Return one chunk's span text instead of the whole body."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        Tool::Status => json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        }),
    }
}
