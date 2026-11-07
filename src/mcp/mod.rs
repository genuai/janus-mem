//! `janus mcp`: the read tools over JSON-RPC 2.0 on stdio (M6 §3.6, §3.7).
//!
//! The server reads one message per line, answers every request once, and never
//! answers a notification. It keeps one piece of state, the handshake flag: a
//! request before `initialize` is `-32002` and the loop stays alive (§3.6).
//!
//! It is a thin adapter. [`crate::engine::Engine::call`] computes every tool
//! answer, and [`crate::api::write_refusal`] refuses every write name, so MCP
//! cannot disagree with `janus rpc` or the CLI (§9.5).
//!
//! # Stdout discipline
//!
//! Stdout carries answers and nothing else. A `warning:` or `refresh:` line is a
//! diagnostic, and the engine writes those to stderr (§3.10, case 10.2). This
//! module never writes a diagnostic, so a client can parse every line it reads.

pub mod protocol;
pub mod tools;

use std::io::{BufRead, Write};

use serde_json::{Value, json};

use crate::engine::Engine;
use crate::mcp::protocol::{Error, Message};

/// Serves one client until its input ends (M6 §3.6).
///
/// An empty line answers nothing. A line that does not parse answers one error
/// and the loop continues, so one bad line never ends a session. End of input
/// ends the loop; a failed write, such as a closed pipe, ends it too, because
/// the client has stopped reading.
///
/// `allow_write` travels to the shared refusal and changes no answer in this
/// release: a write tool is refused in both modes (M6-7).
pub fn run(engine: &Engine, allow_write: bool, input: &mut dyn BufRead, out: &mut dyn Write) {
    let mut session = Session {
        engine,
        allow_write,
        initialized: false,
    };
    // One buffer, cleared per line: `read_line` appends, so a buffer that is
    // never cleared would answer the whole session's input at once.
    let mut line = String::new();
    loop {
        line.clear();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let Some(answer) = session.handle(&line) else {
            continue;
        };
        if write_answer(out, &answer).is_err() {
            return;
        }
    }
}

/// One open session: the engine, the flag, and the one handshake fact.
struct Session<'a> {
    engine: &'a Engine,
    allow_write: bool,
    /// True once `initialize` has been answered.
    initialized: bool,
}

impl Session<'_> {
    /// The answer to one line, or `None` for a notification.
    fn handle(&mut self, line: &str) -> Option<Value> {
        let request = match protocol::parse(line) {
            Ok(request) => request,
            // A line that is not a request names no `id`, so the answer carries
            // `null`; a malformed request that names itself gets its own back.
            Err(error) => return Some(protocol::error_answer(&error.id, &error)),
        };
        let Message::Request { id, method, params } = request else {
            // No `id` means no answer, including for an unknown notification.
            return None;
        };
        Some(match self.answer(&method, &params) {
            Ok(result) => protocol::result(&id, result),
            Err(error) => protocol::error_answer(&id, &error),
        })
    }

    /// The result of one request, or the protocol error that answers it.
    fn answer(&mut self, method: &str, params: &Value) -> Result<Value, Error> {
        match method {
            "initialize" => {
                let result = handshake(params)?;
                self.initialized = true;
                Ok(result)
            }
            // A ping is legal before the handshake (§3.6).
            "ping" => Ok(json!({})),
            "tools/list" => {
                self.ready()?;
                Ok(tools::descriptors())
            }
            "tools/call" => {
                self.ready()?;
                tools::call(self.engine, params, self.allow_write)
            }
            other => Err(Error::new(
                protocol::METHOD_NOT_FOUND,
                format!("unknown method {other:?}"),
            )),
        }
    }

    /// Refuses a request that arrived before the handshake (§3.6 case 6.5).
    fn ready(&self) -> Result<(), Error> {
        if self.initialized {
            return Ok(());
        }
        Err(Error::new(
            protocol::NOT_INITIALIZED,
            "send initialize first; the server is not initialized",
        ))
    }
}

/// The result of one `initialize` request (§3.6).
///
/// A requested revision this server accepts is echoed. Any other revision is
/// answered with the newest one this server speaks, and the client decides
/// whether to continue.
fn handshake(params: &Value) -> Result<Value, Error> {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::new(
                protocol::INVALID_PARAMS,
                "initialize: \"protocolVersion\" must be a string",
            )
        })?;
    let version = if protocol::SUPPORTED_VERSIONS.contains(&requested) {
        requested
    } else {
        protocol::PROTOCOL_VERSION
    };
    Ok(json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "janus-mem", "version": env!("CARGO_PKG_VERSION") },
    }))
}

/// Writes one answer line and flushes it, so the client sees it at once.
fn write_answer(out: &mut dyn Write, answer: &Value) -> std::io::Result<()> {
    writeln!(out, "{answer}")?;
    out.flush()
}
