//! `janus rpc`: one JSON request per line on stdin, one answer per line on
//! stdout (M6 §3.5).
//!
//! This is deliberately not JSON-RPC. There is no `jsonrpc` field, no method
//! table and no transport error: every answer is the tool envelope of
//! [`crate::api`], so a caller reads `ok` and then `code`, and a line that does
//! not parse is an answer like any other (§9.5).
//!
//! The request shape is `{"id": <number|string|null>, "tool": "<wire name>",
//! "params": {...}}`. The answer echoes `id` unchanged, so a caller may send
//! many requests on one pipe and match each answer to its request.
//!
//! # Stdout discipline
//!
//! This module writes answers and nothing else to stdout. A `warning:` or a
//! `refresh:` line is a diagnostic, and the engine's own reads write those to
//! stderr (§3.10, case 10.1). The loop keeps no second stream, so the two
//! cannot be confused by a caller that reads only stdout.

use std::io::{BufRead, Write};

use serde_json::{Map, Value, json};

use crate::api::{Call, Failure, Tool, write_refusal};
use crate::engine::Engine;
use crate::error::JanusError;

/// Answers every request line until input ends (M6 §3.5).
///
/// An empty or whitespace-only line is skipped and answers nothing. A line that
/// is not a legal request answers one failure line, and the loop continues, so
/// one bad line never ends a session.
///
/// `allow_write` travels to the shared refusal, and it changes no answer in
/// this release: a write tool is refused in both modes (M6-7).
///
/// End of input ends the loop. A failed write on stdout, such as a closed pipe,
/// also ends it: the caller has stopped reading, and that is not an error.
pub fn run(engine: &Engine, allow_write: bool, input: &mut dyn BufRead, out: &mut dyn Write) {
    // One buffer, cleared per line: `read_line` appends, and a buffer that is
    // never cleared would answer the whole session's input at once.
    let mut line = String::new();
    loop {
        line.clear();
        match input.read_line(&mut line) {
            // End of input: a final line without a trailing newline still
            // arrives here once, with a non-zero count.
            Ok(0) => return,
            // A read error means the caller is gone, which is an end too.
            Err(_) => return,
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let (id, answer) = answer(engine, &line, allow_write);
        if respond(out, &id, &answer).is_err() {
            return;
        }
    }
}

/// The echoed `id` and the answer object for one request line.
fn answer(engine: &Engine, line: &str, allow_write: bool) -> (Value, Value) {
    let request = match request(line) {
        Ok(request) => request,
        // The line did not have the request shape, so there is no `id` to echo.
        Err(failure) => return (Value::Null, encode(&failure)),
    };
    // A write name is refused before the tool table is consulted: the name is
    // legal and the release is not (§3.8, case 5.8).
    if let Some(failure) = write_refusal(&request.tool, allow_write) {
        return (request.id, encode(&failure));
    }
    match Tool::parse(&request.tool) {
        Ok(tool) => {
            let response = engine.call(&Call::new(tool, request.params));
            (request.id, response.json)
        }
        Err(error) => {
            let names: Vec<&str> = Tool::ALL.iter().map(|tool| tool.wire_name()).collect();
            let failure = usage(
                error.to_string(),
                Some(format!("one of: {}", names.join(", "))),
            );
            (request.id, encode(&failure))
        }
    }
}

/// One request as its line stated it, before the tool name is resolved.
struct Request {
    /// The value to echo, exactly as it arrived.
    id: Value,
    /// The wire name, which may be unknown or may name a write tool.
    tool: String,
    /// The parameters, `{}` when the line named none.
    params: Value,
}

/// Reads one request line, or the failure that answers it (case 5.2).
///
/// A failure here means the line did not have the request shape, so the answer
/// echoes `id: null`: there is no request to name.
fn request(line: &str) -> Result<Request, Failure> {
    let value: Value = serde_json::from_str(line).map_err(|error| {
        usage(
            format!("the line is not JSON: {error}"),
            Some(request_shape().to_owned()),
        )
    })?;
    let Value::Object(mut fields) = value else {
        return Err(usage(
            format!("a request is one JSON object; got {}", kind(&value)),
            Some(request_shape().to_owned()),
        ));
    };
    let tool = match fields.remove("tool") {
        Some(Value::String(name)) => name,
        Some(other) => {
            return Err(usage(
                format!("tool must be a string; got {}", kind(&other)),
                Some(request_shape().to_owned()),
            ));
        }
        None => {
            return Err(usage(
                "tool is missing".to_owned(),
                Some(request_shape().to_owned()),
            ));
        }
    };
    Ok(Request {
        id: fields.remove("id").unwrap_or(Value::Null),
        tool,
        // A tool with no parameters needs no `params` field in the request
        // (case 5.6), so an absent one is the empty object.
        params: fields
            .remove("params")
            .unwrap_or_else(|| Value::Object(Map::new())),
    })
}

/// The one sentence a malformed line is answered with, as a hint.
fn request_shape() -> &'static str {
    r#"a request is {"id": <number|string|null>, "tool": "<wire name>", "params": {...}}"#
}

/// The name of one JSON value's kind, for a message a human reads.
fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a bool",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// One usage failure: a legal request this surface cannot answer as asked.
fn usage(message: String, hint: Option<String>) -> Failure {
    Failure {
        hint,
        ..Failure::from_error(&JanusError::Usage { message })
    }
}

/// Encodes one failure as the object an adapter writes.
///
/// The failure holds strings only, so an encode that fails would mean the JSON
/// writer itself is broken. The empty object then names no answer at all, which
/// is the one outcome a caller must not read as a success.
fn encode(failure: &Failure) -> Value {
    serde_json::to_value(failure).unwrap_or_else(|_| json!({ "ok": false }))
}

/// Writes one answer line: the echoed `id`, then the envelope (§3.5).
fn respond(out: &mut dyn Write, id: &Value, envelope: &Value) -> std::io::Result<()> {
    let mut answer = Map::new();
    answer.insert("id".to_owned(), id.clone());
    if let Some(fields) = envelope.as_object() {
        answer.extend(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    writeln!(out, "{}", Value::Object(answer))?;
    // One answer leaves the process before the caller sends the next request.
    out.flush()
}
