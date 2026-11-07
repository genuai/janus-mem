//! The client registration of SPEC §16.3: `docs/mcp-clients.md` holds a block a
//! controller can paste, and that block is JSON naming `janus mcp`.
//!
//! The test parses the block. It does not match the text with a pattern: a
//! snippet that looks right and does not parse is the failure this prevents.

use std::fs;
use std::path::Path;

use serde_json::Value;

/// Every fenced `json` block in `text`, in order, without its fences.
fn json_blocks(text: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut open = false;
    let mut start = 0;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let fence = line.trim_end();
        if !open && fence == "```json" {
            open = true;
            start = offset + line.len();
        } else if open && fence == "```" {
            open = false;
            blocks.push(&text[start..offset]);
        }
        offset += line.len();
    }
    assert!(!open, "a json block is never closed");
    blocks
}

#[test]
fn the_client_snippet_parses_and_names_janus_mcp() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/mcp-clients.md");
    let text = fs::read_to_string(&path).expect("docs/mcp-clients.md must exist");
    let blocks = json_blocks(&text);
    assert!(!blocks.is_empty(), "no json block in {}", path.display());

    let mut servers = 0;
    for block in &blocks {
        let value: Value = serde_json::from_str(block)
            .unwrap_or_else(|err| panic!("the block is not JSON ({err}):\n{block}"));
        let Some(map) = value.get("mcpServers").and_then(Value::as_object) else {
            continue;
        };
        for (name, server) in map {
            let command = server["command"].as_str().expect("a command path");
            assert!(
                command.starts_with('/') && command.ends_with("/janus"),
                "{name}: the snippet must name the binary by absolute path: {command}"
            );
            let args: Vec<&str> = server["args"]
                .as_array()
                .expect("args")
                .iter()
                .map(|arg| arg.as_str().expect("a string argument"))
                .collect();
            assert_eq!(args.first().copied(), Some("mcp"), "{name}: {args:?}");
            let vault = args
                .windows(2)
                .find(|pair| pair[0] == "--vault")
                .map(|pair| pair[1])
                .unwrap_or_else(|| panic!("{name} must name a vault: {args:?}"));
            assert!(vault.starts_with('/'), "{name}: {vault}");
            servers += 1;
        }
    }
    assert!(servers >= 1, "no mcpServers entry in {}", path.display());

    for client in ["Claude Desktop", "Hermes"] {
        assert!(text.contains(client), "the docs must register {client}");
    }
}
