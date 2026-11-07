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

pub mod dto;
pub mod error;

pub use crate::scan::API_VERSION;
pub use dto::{Failure, Ok};
pub use error::ApiCode;

use serde_json::Value;

use crate::error::JanusError;

/// One read tool this surface serves (M6 §2.3).
///
/// One enum means the MCP tool list, the rpc tool list and the tests cannot
/// drift apart. Phase 2b adds `ReadDocument` and `Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// `search`: ranked documents for one query.
    Search,
    /// `get_authoritative_facts`: active decisions, specs and principles.
    AuthoritativeFacts,
    /// `get_lineage`: the supersession chain around one document.
    Lineage,
}

impl Tool {
    /// Every tool this phase serves, in the order a surface lists them.
    pub const ALL: &'static [Self] = &[Self::Search, Self::AuthoritativeFacts, Self::Lineage];

    /// The wire name an agent sends and reads (§9.2).
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::AuthoritativeFacts => "get_authoritative_facts",
            Self::Lineage => "get_lineage",
        }
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
