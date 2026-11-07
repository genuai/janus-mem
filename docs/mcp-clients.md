# Connect an agent to janus-mem

janus-mem answers over the Model Context Protocol (MCP) on stdio. The controller
starts `janus mcp` and the two processes exchange JSON-RPC 2.0 messages, one per
line. The server opens no port, needs no network and keeps no state between
processes.

Two paths appear in the snippets below. Replace both with your own: the binary,
which `command -v janus` prints, and the vault root, which is the folder that
holds `janus.toml`.

## Claude Desktop

Open the config file:

- macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
- Windows: `%APPDATA%\Claude\claude_desktop_config.json`

Add one entry to `mcpServers` and restart the app:

```json
{
  "mcpServers": {
    "janus-mem": {
      "command": "/Users/me/.cargo/bin/janus",
      "args": ["mcp", "--vault", "/Users/me/vault"],
      "env": { "RUST_LOG": "warn" }
    }
  }
}
```

`RUST_LOG=warn` keeps the server quiet on stderr. The answers travel on stdout,
so a diagnostic level cannot corrupt the protocol.

## Hermes

Hermes takes the same two facts: the command and its arguments. Point its MCP
server list at

```console
/Users/me/.cargo/bin/janus mcp --vault /Users/me/vault
```

A build that reads a JSON server map takes the block above unchanged; only the
name of the entry matters, and `janus-mem` is a good one.

## What the agent can then do

The agent gets five read tools: `search` for ranked evidence with the declared
and effective status of each row, `get_lineage` for the chain that replaced a
decision, `get_authoritative_facts` for the active principles and decisions
inside their validity window, `read_document` for the text of one document, and
`janus_status` for index freshness before it asks anything. Every answer carries
`api`, `ok` and a stable `code`, so a controller branches on data and not on a
message. Writes are refused in this release: a call that names one answers
`code: "refused"` and a hint that names the release which adds them.

## Try it without a controller

`janus rpc` is the same five tools on one JSON object per line, with no protocol
layer. It is the shortest way to check that a vault answers before you register
it:

```bash
printf '{"id":1,"tool":"janus_status"}\n' | janus --vault /Users/me/vault rpc
```
