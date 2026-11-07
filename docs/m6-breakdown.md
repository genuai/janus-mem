# M6 — The controller surface (read-only)

> Branch: `m6-plan` (this document), then one branch per phase from `main`.
> Spec: §8.3, §9.1–§9.5, §11 (M6), §16.3. §6.11 is read and deferred (§0.3).
> Evidence: the code as merged at `48c22e7`, and the handoff in
> `docs/m5-breakdown.md` §10.
>
> **Done when:** an agent answers W1–W3 through `janus mcp` with `grep` disabled, the
> same five tools answer over `janus rpc`, and a call naming a write tool is refused
> in both `--allow-write` modes.

**Words used here.** **Tool**: one named read operation (`search`, `facts`,
`lineage`, `read`, `status`). **Call**: one tool invocation with parameters.
**Envelope**: the shared outer JSON object every surface returns. **Adapter**: the
CLI, the rpc loop, or the MCP server. **Generation**: one validated index, as
`src/index/store.rs` loads it. **Core**: the sync library below every adapter.

## 0. Scope

**Decision: M6 adds one controller surface and one `read` tool. It adds no corpus
federation and no write.**

Today the engine is reachable only through `janus` subcommands. Each command owns
its own JSON report and writes to stdout itself. Measured against the code:

| M6 item | state today | evidence |
|---|---|---|
| `janus mcp` | absent | `src/cli.rs:50`–`243`: twelve commands, none named `mcp` |
| `janus rpc` | absent | same list |
| one shared DTO module | absent; two private, command-local reports | `src/search/mod.rs:1109`, `src/lineage.rs:375` |
| a core callable without stdout | absent; `run` renders | `src/search/mod.rs:1062`, `src/lineage.rs:98` |
| `read` tool / `janus read` | absent | no `Read` variant in `src/cli.rs`; the parsed shape exists at `src/document.rs:28` |
| read refresh and refusal | exists and is reusable | `src/search/mod.rs:424`, `src/index/refresh.rs:311` |
| busy writer is a skip | exists | `Decision::SkippedBusy` at `src/index/refresh.rs:278` |
| write gate | absent | M7 owns every write |
| `janus_status` / `status` | absent | — |
| corpora and workspaces | absent; no workspace file is parsed anywhere | `grep -rn workspace src/` returns only prose |

### 0.1 In scope

1. One DTO module: the envelope, the five tool parameter shapes, the stable error
   codes.
2. One sync `Engine` with one method, `call`, that returns a DTO and never prints.
3. `janus read` and `janus status`, so every tool has a CLI test surface.
4. `janus rpc`: one JSON object per line on stdin, one per line on stdout.
5. `janus mcp`: JSON-RPC 2.0 over stdio, with the five read tools.
6. A provable write refusal, and the client configuration snippets.

### 0.2 Not in scope

| Item | Owner | Why not here |
|---|---|---|
| `update_frontmatter`, `write_document`, `supersede_document`, `promote_document` | M7 | they need the span-preserving patcher and the lock protocol |
| `--corpus`, `janus corpora`, `workspace.toml`, archived roles | M6b (§0.3) | they change the data model, not the protocol |
| `rmcp` and any async runtime | nobody, until M8 measures a need | M6-1 |
| warm-index invalidation by file events | M8 (`watch`) | M6 caches a generation; it does not watch the corpus |
| HTTP or SSE transport | M8 or later | stdio is the only transport an agent host needs today |
| `max_chars` on `search` | M6b, if the walkthrough asks | the generation stores spans, not text (§3.7 case 7.6) |

### 0.3 Why corpora are M6b, not M6

`docs/m5-breakdown.md` §10 hands cross-corpus links, `--corpus` and workspaces to
M6. The plan splits them out. Three reasons:

1. **The M6 done-when does not need them.** W1–W3 are answered from one corpus.
2. **They are a data-model change, not an adapter change.** A workspace adds a
   file format, a role, a multi-index merge, an archived hard-exclusion rule and a
   write command (`promote`). Mixing that into the phase that introduces framing
   makes one unreviewable PR and two untestable halves.
3. **`promote` is a write.** It belongs after M7's patcher exists, not before.

So M6 is the protocol milestone. M6b is the federation milestone, planned after
M6 closes. §6.11 stays the design authority for both, unchanged.

### 0.4 Why the milestone splits into five phases

| Phase | Unit | Why this is one reviewable change |
|---|---|---|
| P1 | the envelope and the codes | pure types plus one field on three reports |
| P2 | the engine, `read`, `status` | compute is separated from rendering, once |
| P3 | the rpc loop | one framing, one loop, no protocol state |
| P4 | the MCP server | handshake, tool list, tool dispatch |
| P5 | the refusal and the docs | one predicate plus the client snippets |

## 1. Dependencies

**No dependency is added.** `serde_json` is already a dependency and is the only
crate the rpc loop and the MCP server need. `rmcp` is rejected; M6-1 records why.

## 2. Data model

### 2.1 The envelope (`src/api/dto.rs`, new)

Every surface returns one object with the same head:

```rust
#[derive(Serialize)]
pub struct Ok<T> {
    pub api: u32,          // 1, from scan::API_VERSION
    pub ok: bool,          // always true here
    #[serde(flatten)]
    pub payload: T,        // per-tool fields, unchanged from today
}

#[derive(Serialize)]
pub struct Failure {
    pub api: u32,
    pub ok: bool,          // always false here
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}
```

A failure carries no `results`, no `took_ms` and no `vault`. An agent branches on
`ok`, then on `code`, then on `hint`. Nothing else is promised.

`Ok<T>` flattens, so a search answer keeps every field it has today. It gains
`ok` and nothing else. Adding a field keeps `api` at `1` (§9.5).

### 2.2 The error codes (`src/api/error.rs`, new)

Codes are stable strings, and each maps from one or more `JanusError` variants.
The exit code is the CLI's; rpc and MCP keep running (§3.2 case 2.5).

| `code` | from `JanusError` | exit |
|---|---|---|
| `usage` | `Usage` | 2 |
| `config` | `ConfigExists`, `ConfigMissing`, `ConfigRead`, `ConfigParse` | 1 |
| `corpus` | `VaultResolve`, `VaultNotFound`, `VaultNotDirectory` | 1 |
| `path_escape` | `PathEscape` | 4 |
| `not_found` | `DocumentNotInCorpus`, `TopicNotFound` | 1 |
| `index_missing` | `IndexMissing` | 1 |
| `index_unstable` | `IndexUnstable` | 1 |
| `index_needs_full_build` | `IndexNeedsFullBuild` | 1 |
| `busy` | `LockTimeout` | 1 |
| `unstable_corpus` | `ChangedDuringRead` | 1 |
| `write` | `Write` | 1 |
| `io` | `CreateDir`, `PathResolve` | 1 |

`conflict` and `precondition_required` arrive with M7, which adds the variants.
`refused` is emitted by the MCP layer itself (§3.8); it has no `JanusError`
variant, because a refusal is a policy answer, not a failure of the engine.

### 2.3 The call (`src/api/mod.rs`, new)

```rust
pub struct Call {
    pub tool: Tool,                 // Search, Facts, Lineage, Read, Status
    pub params: serde_json::Value,  // deserialized into the typed shape below
}
```

`Tool::parse(&str)` is the only place a tool name becomes a `Tool`. An unknown
name is rejected before the core runs, so the core never sees a name it cannot
serve. One enum means the MCP tool list, the rpc tool list and the tests cannot
drift apart.

### 2.4 The engine (`src/engine.rs`, new)

```rust
pub struct Engine {
    root: PathBuf,
    config: Config,
    clock: Box<dyn Clock>,
    embedder: Option<Box<dyn Embedder>>,
    cache: RwLock<Cache>,     // one loaded generation
    loads: AtomicU64,         // cache misses, for `janus_status`
    hits: AtomicU64,          // cache hits, for `janus_status`
}

impl Engine {
    pub fn call(&self, call: &Call) -> Response;
}
```

`Response { ok: bool, json: serde_json::Value, code: Option<&'static str> }`.
`call` never prints, never exits, and never panics on a recoverable error.

The cache holds the loaded `Snapshot`, the `LexicalIndex` and the optional
`Vectors`, keyed by two values (M6-5):

1. the corpus signature digest of this call's walk (`Signature::of(...).digest()`,
   `src/index/refresh.rs:84`), and
2. the loaded manifest's `index_sha` (`src/index/store.rs:67`).

Both inputs are needed. The digest alone misses a generation republished by
another process; `index_sha` alone misses an edited document. `today` is not a key:
decay and windows are computed per query, never stored in the generation.

### 2.5 Tool parameters (`src/api/dto.rs`)

| Tool | Parameters (`camelCase` on the wire) |
|---|---|
| `search` | `query` (required), `limit?`, `intent?`, `as_of?`, `scope?`, `scope_only?`, `types?`, `tags?`, `include_superseded?`, `decay_half_life_days?` |
| `facts` | `query?`, `tags?`, `types?`, `scope?`, `limit?` |
| `lineage` | `path?` **or** `topic?`, exactly one |
| `read` | `path` (required), `max_chars?`, `chunk_id?` |
| `status` | none |

The wire uses `camelCase` (`#[serde(rename_all = "camelCase")]`); the Rust fields
keep `snake_case`. `no_rebuild`, `no_embeddings` and `progress` are **not** tool
parameters: an MCP read always rebuilds when the corpus changed (§9.2) and never
reports progress on a shared stdout. `decay_half_life_days` maps to
`Options::decay_half_life`; `as_of` is parsed with `search::parse_as_of`
(`src/search/mod.rs:661`), so rpc and MCP accept exactly the CLI's date shapes.

### 2.6 The status DTO

```json
{ "api": 1, "ok": true, "vault": "/abs/root", "corpus_id": "main",
  "writes_enabled": false, "embeddings": { "available": true, "model": "…" },
  "index": { "present": true, "stale": false, "docs": 24, "chunks": 210,
             "built_at": "…", "format_version": 6, "index_sha": "…" },
  "cache": { "hits": 2, "misses": 1 } }
```

`embeddings.model` is the manifest's `EmbedderId` when the generation has vectors,
and `null` otherwise. `index.present` is false before the first build, and then
every `index` field except `present` is `null`.

## 3. Contracts

Every case below is a test. A case number names one test function.

### 3.1 The envelope

**Case 1.1.** `janus search "authority" --json` returns an object with
`ok: true` and `api: 1`. Every other key is byte-identical to the `main` output.

**Case 1.2.** `janus facts --json` and `janus lineage <path> --json` likewise.

**Case 1.3.** The three committed search snapshots
(`tests/snapshots/golden_search__*.snap`) move by exactly one line each: the
added `"ok": true`. `api` stays `1`. The diff is reviewed line by line.

**Case 1.4.** A failure on any surface is `{api, ok: false, code, message}` plus
`hint` when one exists. It has no `results` and no `took_ms`.

### 3.2 Codes and exit codes

**Case 2.1.** `--scope-only` with no `--scope` gives `code: "usage"` on rpc and
exit `2` on the CLI.

**Case 2.2.** `lineage` with neither `path` nor `topic` gives `code: "usage"`;
with both it also gives `usage`.

**Case 2.3.** A path outside the corpus gives `code: "path_escape"` on every
surface.

**Case 2.4.** A generation whose manifest `format_version` is `999` gives
`code: "index_needs_full_build"`, and the `message` contains
`index needs a full build: run janus index`.

**Case 2.5.** A failed call never ends the rpc process or the MCP server. The
next line or request is answered normally. The server exits `0` on end of input.

**Case 2.6.** The mapping is total: a `match` over `JanusError` in
`src/api/error.rs` has no `_` arm, so a new variant fails to compile until it has
a code.

### 3.3 `janus read` and `read_document`

The read path returns text, so it is the only place document content reaches a
response. It reads through the same confined walk as search, and it never opens a
file outside the corpus.

**Case 3.1.** `janus read decisions/2024-11-02-postgres-cache.md --json` returns
`frontmatter` as a JSON object with `title: "Postgres cache"` and
`status: "superseded"`, and `body` equal to the file bytes after the closing
fence. The expected body is written out by hand in the test.

**Case 3.2.** `read` of `.janus/manifest.json` gives `path_escape`.

**Case 3.3.** `read` of `../outside.md` gives `path_escape`.

**Case 3.4.** `read` of a path that does not exist gives `not_found`, and the
message names the path.

**Case 3.5.** `read` with `max_chars: 10` on a body that starts with a two-byte
character cuts after a character boundary, never inside one, and sets
`truncated: true`. The test body starts with `é`, so a byte cut would panic or
return invalid UTF-8.

**Case 3.6.** `read` with `chunk_id` set returns that chunk's `span` text, its
`breadcrumb`, and the document's `frontmatter`. A `chunk_id` from another
generation gives `not_found`.

**Case 3.7.** `read` never returns a `text` field for a document it was not asked
for. `search` returns spans, not text, so a caller reads text in exactly one way.

### 3.4 The engine and its cache

**Case 4.1.** Two `search` calls on an unchanged corpus give equal results.
`cache.hits` is `1` and `cache.misses` is `1`.

**Case 4.2.** A document edit between two calls makes the second call a miss, and
its result reflects the edit. The signature digest is the cause, so the test
changes one byte and moves no mtime.

**Case 4.3.** Two threads call `search` on one `Engine` at once. Both answers
equal the single-threaded answer. No result depends on which thread arrived first.

**Case 4.4.** The cache never holds a `today`: `status` and `search` compute decay
from the injected clock on every call. A `FixedClock` moved between calls changes
the decay factor and not the cached generation.

**Case 4.5.** A cache hit does no build. `.janus/` is not written, and its mtimes
do not move.

### 3.5 `janus rpc`

Framing: one JSON object per line on stdin, one JSON object per line on stdout.
An empty line is ignored. End of input ends the loop with exit `0`.

**Case 5.1.** Two request lines give two response lines, in order, and nothing
else on stdout.

**Case 5.2.** A malformed JSON line gives one line with `ok: false`,
`code: "usage"`, `id: null`. The next line is answered normally.

**Case 5.3.** A request is `{"id": <number|string|null>, "tool": "<name>",
"params": {...}}`. The response echoes `id` unchanged. A request with no `id`
answers `id: null`.

**Case 5.4.** An unknown `tool` gives `code: "usage"` and a `hint` listing the five
names.

**Case 5.5.** A request that changes the corpus prints its `refresh:` line on
stderr only. `stdout` holds responses only (case 10.1).

**Case 5.6.** `{"tool": "status"}` is answered without a build and without a lock.

**Case 5.7.** The same five calls over rpc and over MCP return equal JSON, apart
from `id` and `took_ms`.

### 3.6 The MCP handshake

Protocol version `2025-06-18` (M6-9). The server is a JSON-RPC 2.0 peer on stdio,
one message per line.

**Case 6.1.** `initialize` with `protocolVersion: "2025-06-18"` answers the same
version, `capabilities: {"tools": {}}` and
`serverInfo: {"name": "janus-mem", "version": "<crate version>"}`.

**Case 6.2.** `initialize` with an older supported version echoes the client's
version. The supported set is written in one constant, and the test names every
member of it.

**Case 6.3.** `notifications/initialized` produces no response line.

**Case 6.4.** A request before `initialize` returns error `-32002` and the server
stays alive.

**Case 6.5.** `tools/list` returns exactly five tools, in the fixed order
`search`, `facts`, `lineage`, `read`, `status`. Each has `description` and
`inputSchema.type == "object"`.

**Case 6.6.** An unknown method returns `-32601`. A `tools/call` whose params do
not match the schema returns `-32602` and names the missing field.

### 3.7 Tool dispatch

**Case 7.1.** `search` with `{"query": "cache"}` returns the envelope of case 1.1.

**Case 7.2.** `facts` with `{"tags": ["principle"]}` returns only effective-active
facts, using the same predicate as the CLI (no second inclusion rule).

**Case 7.3.** `lineage` with neither or both of `path` and `topic` returns
`ok: false, code: "usage"`, not a protocol error, because the shape is legal and
the combination is not.

**Case 7.4.** `search` with an unknown `intent` returns `ok: false`,
`code: "usage"`, and `isError: true` in the MCP result.

**Case 7.5.** A tool result carries the envelope twice: as `structuredContent`
and as one `content` entry of type `text` holding the same JSON. An older client
that reads only `text` still gets the answer.

**Case 7.6.** `search` rejects a `max_chars` parameter with `-32602`, because the
schema does not list it. The caller reads text with `read`.

**Case 7.7.** A cold mismatch (case 2.4) returns `ok: false` with
`code: "index_needs_full_build"`; a missing index is built on the first call.

**Case 7.8.** A busy writer (another process holds `.janus/lock`) is a skip, not
an error: the call answers from the generation it loaded, and one `refresh:
skipped, index busy` line goes to stderr.

### 3.8 The write refusal

M6 serves no write. This is a planned, temporary state, and M7 reverses it.

**Case 8.1.** `tools/list` never lists a write tool, with or without
`--allow-write`.

**Case 8.2.** `tools/call` naming `update_frontmatter`, `write_document`,
`supersede_document` or `promote_document` returns `ok: false`,
`code: "refused"`, and a `hint` that names the release that adds writes.

**Case 8.3.** `janus mcp --allow-write` starts and prints one line to stderr:
`writes are not implemented yet; --allow-write has no effect`. `janus_status`
reports `writes_enabled: false` in both modes.

**Case 8.4.** The refusal is pinned by a test that names M7 as the phase that
reverses it, so M7 cannot land without editing that test.

### 3.9 `janus status`

**Case 9.1.** Before the first build, `index.present` is false, every other
`index` field is `null`, and the call writes no file.

**Case 9.2.** After a build, `index.docs` and `index.chunks` equal the manifest's
numbers, and `index.stale` is false. After one edit, `index.stale` is true and no
build runs.

**Case 9.3.** `status` never locks. Run it while another process holds
`.janus/lock`; it answers immediately with the loaded generation's facts.

**Case 9.4.** `embeddings.available` is false and `model` is null when the binary
has no `embeddings` feature, whatever `janus.toml` says.

### 3.10 Stdout discipline

**Case 10.1.** A corpus with a config warning and a stale index produces no
`warning:` and no `refresh:` line on stdout, on any of `rpc`, `mcp` or a `--json`
command. Both lines appear on stderr.

**Case 10.2.** Every stdout line of `janus mcp` parses as a JSON-RPC message. The
test feeds a whole session and parses all lines.

**Case 10.3.** No code path below `src/api/` and `src/engine.rs` calls
`println!` or writes to `stdout`. A test greps the two directories for
`println!` and `stdout()`.

### 3.11 The walkthrough (the milestone done-when)

The session is one test against a copy of `tests/fixtures/graph-vault`. The copy
is temp, so a test never writes to a committed fixture.

**Case 11.1.** W1: `search` with `{"query": "cache decision", "intent":
"current"}` returns a non-empty list, every row has `effective_status: active`,
and `chain/2022-01-01-cache-returns.md` is in it. The four other chain documents
declare `status: superseded`, so none of them may appear in `current` (M5 §3.7).
The test asserts the set, not the rank: rank depends on scoring, which M6 does not
change.

**Case 11.2.** W2: `lineage` on `chain/2022-01-01-cache-returns.md` returns the
five nodes at depths 0–4 in this order: `adopt-postgres`, `move-to-redis`,
`cache-in-front`, `drop-cache`, `cache-returns`, with terminal `active`. Then
`read` on the top path returns a body that contains `measured budget`. Both
values come from the fixture files.

**Case 11.3.** W3: the test adds one file to its temp copy,
`principles/pricing.md`, with `type: principle`, `status: active` and
`tags: [pricing]`. `facts` with `{"tags": ["pricing"]}` returns exactly that one
path. The committed fixtures hold no principle, so the test supplies one rather
than asserting an empty list.

### 3.12 What must not change

- No ranking, gating, window or decay rule moves. M6 changes where the answer is
  written, not what it says.
- `format_version` stays `6`. M6 adds no persisted field.
- `api` stays `1`. Every change is additive.
- The CLI's human output stays. A table is not a protocol.
- `janus index` output stays. M6 does not touch the build.

## 4. Decisions

| ID | Decision | Why |
|---|---|---|
| M6-1 | Hand-rolled JSON-RPC 2.0; no `rmcp`, no Tokio | Five methods and five tools need about 300 lines and one dependency already present. `rmcp` brings an async runtime, a churning API (§13 risk table) and a version pinned to a protocol revision. D11 wants async at one edge; M6 needs none. |
| M6-2 | One `Engine::call`; adapters parse and format only | §9.5. Two adapters that each parse params drift within one release. |
| M6-3 | Every object carries `api` and `ok`; failures carry `code`, `message`, `hint` | One contract an agent learns once. Additive to every existing report, so `api` stays `1`. |
| M6-4 | `janus rpc` uses `{id, tool, params}`, not JSON-RPC | The rpc loop exists for `subprocess` + `json` callers. A flat tool envelope is one line of Python; JSON-RPC adds error envelopes and ids for no caller benefit. MCP keeps JSON-RPC because that is its protocol. |
| M6-5 | One in-process generation cache, keyed by signature digest and `index_sha` | Removes the per-call 300 MB load from an agent session. Both keys are needed: each one alone misses a real invalidation. |
| M6-6 | `status` walks the corpus but never builds and never locks | An agent must be able to ask "is this fresh?" before it asks a question, and a status call must never be the thing that starts a three-minute build. |
| M6-7 | Write tools are refused in both `--allow-write` modes until M7 | M6 has no writer. A listed tool that cannot work is worse than an absent one. Pinned by case 8.4. |
| M6-8 | A legal shape with a bad combination is `ok: false`; an illegal shape is `-32602` | The agent must read `code`, not the transport error. `lineage` with both args is legal JSON and illegal usage. |
| M6-9 | MCP protocol `2025-06-18`; echo any supported version the client asks for | Newest revision with structured tool output; older hosts must still connect. |
| M6-10 | `read` truncates at `max_chars` on a character boundary, default 8000 | A byte cut panics or corrupts. A default keeps a 2 MB document from filling an agent's context. |
| M6-11 | Corpora, workspaces and `promote` are M6b | §0.3. |
| M6-12 | `search` returns spans, never text; `read` is the one text path | The generation stores spans (§8.5), and one text path cannot disagree with itself about truncation. §9.2's `max_chars` on `search` is corrected in §9. |

## 5. Phases

Each phase: one branch from `main`, one PR, one report. Do not poll CI.

### Phase 1 — The envelope, the codes, and the DTO head

**Files.** `src/api/mod.rs` (new), `src/api/dto.rs` (new), `src/api/error.rs`
(new), `src/lib.rs`, `src/search/mod.rs`, `src/search/facts.rs`, `src/lineage.rs`,
`tests/api_envelope.rs` (new), `tests/snapshots/golden_search__*.snap`.

**Build.** `Ok<T>` and `Failure`; `ApiCode` from `JanusError`; `API_VERSION`
re-exported. The three command reports gain `ok`, and their human output is
unchanged.

**Tests.** Cases 1.1–1.4, 2.1–2.6, 10.1 (the three `--json` commands only).

**Handoff.** A shared DTO head exists. Every command still renders inside `run`.

**Accept.** Three snapshots move by one line each. `api` is `1` everywhere. The
error match has no `_` arm.

### Phase 2 — The engine, `janus read`, `janus status`

**Files.** `src/engine.rs` (new), `src/read.rs` (new), `src/status.rs` (new),
`src/api/dto.rs`, `src/search/mod.rs` (split compute from `render`),
`src/search/facts.rs`, `src/lineage.rs`, `src/cli.rs` (two new commands),
`src/main.rs`, `tests/cli_read.rs` (new), `tests/engine_cache.rs` (new),
`tests/cli_status.rs` (new).

**Build.** `Engine::call` with the five tools and the cache of M6-5. `search`,
`facts` and `lineage` expose a `compute` function returning the DTO; `run` becomes
`compute` + a renderer. `read` and `status` are new commands.

**Tests.** Cases 3.1–3.7, 4.1–4.5, 9.1–9.4, 10.3.

**Handoff.** `Engine::call` answers all five tools with no stdout. The CLI uses it
for `read` and `status`; the other three still call their own `compute` + render.

**Accept.** The `search` and `lineage` human tables and JSON are unchanged. One
test deletes `.janus/index.bin` and proves the second call answers from the cache.

**Split seam if over budget.** Part 1: the engine and the three query tools. Part
2: `read`, `status` and the cache counters. Both are reviewable alone.

### Phase 3 — `janus rpc`

**Files.** `src/rpc.rs` (new), `src/api/mod.rs`, `src/cli.rs`, `src/main.rs`,
`tests/cli_rpc.rs` (new).

**Build.** One line in, one line out. `id` echoed. Empty line skipped. Malformed
line answered, then the loop continues. Startup failures (no corpus, no config)
exit `1` before the loop; every later failure is a response line.

**Tests.** Cases 5.1–5.7, 10.1.

**Handoff.** A language-agnostic surface exists. The MCP adapter reuses
`Engine::call` and nothing else.

**Accept.** A test drives `janus rpc` with a `serde_json`-built script and parses
every stdout line. No test sleeps.

**Split seam.** Part 1: the loop and codes. Part 2: the five-call parity test.

### Phase 4 — `janus mcp`

**Files.** `src/mcp/mod.rs` (new), `src/mcp/protocol.rs` (new),
`src/mcp/tools.rs` (new), `src/cli.rs`, `src/main.rs`, `tests/cli_mcp.rs` (new),
`tests/mcp_walkthrough.rs` (new).

**Build.** JSON-RPC 2.0 framing, `initialize`, `notifications/initialized`,
`tools/list`, `tools/call`. Schemas are `serde_json::Value` literals with
`additionalProperties: false`, so an extra param is `-32602` (case 7.6).

**Tests.** Cases 6.1–6.6, 7.1–7.8, 8.1–8.2, 10.2, 11.1–11.3.

**Handoff.** The server answers W1–W3. The write refusal exists.

**Accept.** A whole scripted session parses as JSON-RPC. The walkthrough asserts
the hand-derived first path and the five-link chain.

**Split seam.** Part 1: handshake, list, dispatch, `search` and `read`. Part 2:
`facts`, `lineage`, `status`, the walkthrough.

### Phase 5 — The gate, the snippets, and the corrections

**Files.** `src/mcp/tools.rs`, `src/main.rs`, `README.md`,
`docs/mcp-clients.md` (new), `SPEC.md`, `docs/m6-breakdown.md` (this plan).

**Build.** The refusal of §3.8, the `--allow-write` stderr line, and the client
configuration for Claude Desktop and Hermes (§16.3). The SPEC corrections of §9
land here.

**Tests.** Case 8.3, and a test that reads the snippets and checks the JSON is
valid and names `janus mcp`.

**Handoff.** M7 can add write tools to one list in `src/mcp/tools.rs`.

**Accept.** `cargo doc` is warning-free. The README quickstart mentions MCP.

## 6. Cross-cutting requirements

- **Gates.** `cargo fmt --all --check`; `cargo clippy --all-targets -- -D
  warnings`; `cargo test --all-targets`; `cargo doc --no-deps`; the Windows
  cross-check `cargo check --target aarch64-pc-windows-msvc --all-targets
  --features blake3/pure`. The `embeddings` matrix stays as `AGENTS.md` states.
- **Determinism.** Every test uses `FixedClock`. No test reads the wall clock,
  sleeps, or binds a port.
- **No `unwrap()`** in library code. A malformed line is a `Result`.
- **Comments in Simplified Technical English.** Short sentences, plain words.
- **Windows.** Stdio framing uses `BufRead::read_line` on `String`. No
  `std::os::unix`; no `#[cfg]` without a Windows arm.
- **Exit codes.** Only the CLI maps an error to a code. The servers exit `0` on
  clean end of input and `1` on a startup failure.

## 7. Definition of done

1. `janus mcp` and `janus rpc` answer `search`, `facts`, `lineage`, `read` and
   `status` from one `Engine::call`. Evidence: `tests/cli_rpc.rs` parity test.
2. W1–W3 are answered from the fixtures with no `grep`. Evidence:
   `tests/mcp_walkthrough.rs`, three assertions with hand-derived paths.
3. A write tool call is refused with `--allow-write` and without it. Evidence:
   case 8.2.
4. stdout carries protocol only. Evidence: case 10.2, one whole session parsed.
5. `janus read` and `janus status` exist and are tested. Evidence:
   `tests/cli_read.rs`, `tests/cli_status.rs`.
6. No golden moved except the one added `ok` line per snapshot. Evidence: the P1
   diff and `git diff --stat tests/snapshots`.
7. `cargo doc --no-deps` has zero warnings. Evidence: the command's output.
8. `format_version` stays `6` and `api` stays `1`. Evidence: `git diff
   src/index/store.rs` is empty.

## 8. PR plan and size budget

| phase | logic ceiling | estimate | test ceiling | estimate | base |
|---|---|---|---|---|---|
| P1 envelope + codes | 400 | ~300 | 600 | ~350 | `main` |
| P2 engine + read + status | 600 | ~500 | 900 | ~600 | `main` |
| P3 rpc | 350 | ~250 | 500 | ~300 | `main` |
| P4 mcp | 550 | ~450 | 800 | ~550 | `main` |
| P5 gate + docs | 250 | ~150 | 400 | ~150 | `main` |

Estimates run low by a factor of two to three. The split seam in each phase is
named above; split before the ceiling, never shave a comment or drop a test.

## 9. SPEC corrections

These amendments ship in the phase that owns them. Each is in the same PR.

1. **§9.4 envelope.** State that `api` and `ok` are on every object, that a
   failure carries `code`, `message` and optional `hint` and no results, and that
   the envelope is shared by CLI `--json`, rpc and MCP. (P1.)
2. **§9.5 rpc shape.** Add the request shape `{id, tool, params}` and state that
   rpc is **not** JSON-RPC, while MCP is. The current text says "same shapes as
   the MCP tools" without saying which layer owns framing. (P3.)
3. **§9.1 exit codes.** State that exit codes belong to the CLI. A tool failure
   inside `janus rpc` or `janus mcp` is a response, not an exit. (P3.)
4. **§9.2 `search` parameters.** Remove `max_chars` from the `search` row and
   state that `read_document` is the one text path, addressed by `chunk_id`. The
   generation stores spans, not text, and §8.5 keeps it that way. (P2.)
5. **§9.2 `read_document`.** State the default `max_chars` (8000), the
   character-boundary rule, and that `chunk_id` selects a span of the current
   generation. (P2.)
6. **§9.2 `janus_status`.** Add the response shape of §2.6, including
   `writes_enabled`, `embeddings`, `index` and `cache`. (P2.)
7. **§11 M6.** Move corpora, workspaces and `promote` out of M6 into M6b, and say
   that M6's done-when is the scripted transcript of §3.11. Add that writes are
   refused until M7. (P5.)
8. **§9.1 command list and §11 M8.** Add `janus status`, and remove the
   "register in Claude Desktop and Hermes" line from M8, which moves to M6 P5. (P5.)
9. **§9.2 refresh.** State that a cold mismatch refuses but a missing index is
   built on the first call, and that the generation cache is keyed by the corpus
   signature and the manifest's `index_sha`. (P2.)

## 10. Handoff to M7

- `Engine::call` is the one place a write tool is added. `Tool` gains a variant,
  `src/mcp/tools.rs` gains one descriptor, and `--allow-write` becomes a real
  gate instead of a refusal.
- `JanusError` gains `Conflict` and `PreconditionRequired`; `src/api/error.rs`
  gains two rows and no `_` arm, so the compiler finds the call sites.
- The `read` DTO already returns `content_hash`, which is the etag M7 requires.
  P2 must include it, or M7 has to re-open every read tool.
- The cache of M6-5 must be invalidated after a write. The writer publishes a new
  `index_sha`, so the key already changes; M7 adds a test that proves it.

## 11. Implementation logs

Each phase appends a log here when it merges: what deviated, why, and the size
numbers.

### Phase 1 — the envelope, the codes, and the DTO head

_(empty)_

### Phase 2 — the engine, `janus read`, `janus status`

_(empty)_

### Phase 3 — `janus rpc`

_(empty)_

### Phase 4 — `janus mcp`

_(empty)_

### Phase 5 — the gate, the snippets, and the corrections

_(empty)_
