# M6 — The controller surface (read-only)

> Branch: `m6-plan` (this document), then one branch per phase from `main`.
> Spec: §8.3, §9.1–§9.5, §11 (M6), §16.1–§16.3. §6.11 is read and deferred (§0.3).
> Evidence: the code as merged at `48c22e7`, and the handoff in
> `docs/m5-breakdown.md` §10.
>
> **Done when:** an agent answers W1–W3 through `janus mcp` with `grep` disabled, the
> same five tools answer over `janus rpc`, and a call naming a write tool is refused
> in both `--allow-write` modes.

**Words used here.** **Tool**: one named read operation (`search`,
`get_lineage`, `get_authoritative_facts`, `read_document`, `janus_status`).
**Call**: one tool invocation with parameters. **Envelope**: the shared outer JSON
object every tool surface returns. **Adapter**: the CLI, the rpc loop, or the MCP
server. **Generation**: one validated index, as `src/index/store.rs` loads it.
**Core**: the sync library below every adapter.

## 0. Scope

**Decision: M6 adds one read-only controller surface. It adds no corpus
federation and no write.**

Today the engine is reachable only through `janus` subcommands. Search already
separates computation from rendering. Facts and lineage render inside `run`. Read
and status are absent. Measured against the code:

| M6 item | state today | evidence |
|---|---|---|
| `janus mcp` | absent | `src/cli.rs:50`–`280`: eleven commands, none named `mcp` |
| `janus rpc` | absent | same list |
| one shared DTO module | absent; two private, command-local reports | `src/search/mod.rs:1109`, `src/lineage.rs:375` |
| a tool body callable without stdout | partial: `search` has one, `facts` and `lineage` render inside `run` | `src/search/mod.rs:178`; `src/search/facts.rs:86`; `src/lineage.rs:56` |
| `read` tool / `janus read` | absent | no `Read` variant in `src/cli.rs`; the parsed shape exists at `src/document.rs:28` |
| read refresh and refusal | exists and is reusable | `src/search/mod.rs:424`, `src/index/refresh.rs:311` |
| busy writer is a skip | exists | `Decision::SkippedBusy` at `src/index/refresh.rs:278` |
| `content_hash` per document | already stored | `DocMeta::content_hash` at `src/index/mod.rs:273` |
| `[mcp] allow_write`, `max_chars` | already parsed | `src/config.rs:477`–`480` |
| write gate | absent | M7 owns every write |
| `janus_status` / `status` | absent | — |
| corpora and workspaces | absent; no workspace file is parsed anywhere | `grep -rn workspace src/` returns prose only |

### 0.1 In scope

1. One DTO module: the envelope, the five tool parameter shapes, the stable error
   codes, the response matrix.
2. One sync `Engine` with one method, `call`, that returns a DTO and never prints.
3. `janus read` and `janus status`, so every tool has a CLI test surface.
4. `janus rpc`: one JSON object per line on stdin, one per line on stdout.
5. `janus mcp`: JSON-RPC 2.0 over stdio, with the five read tools.
6. A provable write refusal, and the client registration snippets.

### 0.2 Not in scope

| Item | Owner | Why not here |
|---|---|---|
| `update_frontmatter`, `write_document`, `supersede_document`, `promote_document` | M7 | they need the span-preserving patcher and the lock protocol |
| `--corpus`, `janus corpora`, `workspace.toml`, archived roles | M6b (§0.3) | they change the data model, not the protocol |
| `rmcp` and any async runtime | nobody, until M8 measures a need | M6-1 |
| warm-index invalidation by file events | M8 (`watch`) | M6 caches one load; it does not watch the corpus |
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
| P1 | the envelope, the codes, the response matrix | pure types plus one field on three reports |
| P2 | the engine, `read`, `status` | compute separated from rendering, once; ships as two PRs |
| P3 | the rpc loop | one framing, one loop, no protocol state |
| P4 | the MCP server | handshake, tool list, tool dispatch |
| P5 | the refusal and the docs | one predicate plus the client snippets |

## 1. Dependencies

**No dependency is added.** `serde_json` is already a dependency and is the only
crate the rpc loop and the MCP server need. `rmcp` is rejected; M6-1 records why.

## 2. Data model

### 2.1 The envelope (`src/api/dto.rs`, new)

Every tool surface returns one object with the same head:

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

A failure carries no `results`, no `taken_ms` and no `vault`. An agent branches on
`ok`, then on `code`, then on `hint`. Nothing else is promised.

`Ok<T>` flattens, so a search answer keeps every field it has today. It gains
`ok` and nothing else. Adding a field keeps `api` at `1` (§9.5).

### 2.2 The error codes (`src/api/error.rs`, new)

Codes are stable strings. Each maps from one `JanusError` variant, and the exit
code is copied from `JanusError::exit_code` (`src/error.rs:207`), not invented:

| `code` | from `JanusError` | exit |
|---|---|---|
| `usage` | `Usage` | 2 |
| `config_exists` | `ConfigExists` | 4 |
| `vault_not_directory` | `VaultNotDirectory` | 4 |
| `path_escape` | `PathEscape` | 4 |
| `config_missing` | `ConfigMissing` | 1 |
| `config_read` | `ConfigRead` | 1 |
| `config_parse` | `ConfigParse` | 1 |
| `vault_resolve` | `VaultResolve` | 1 |
| `vault_not_found` | `VaultNotFound` | 1 |
| `path_resolve` | `PathResolve` | 1 |
| `not_found` | `DocumentNotInCorpus`, `TopicNotFound` | 1 |
| `index_missing` | `IndexMissing` | 1 |
| `index_unstable` | `IndexUnstable` | 1 |
| `index_needs_full_build` | `IndexNeedsFullBuild` | 1 |
| `busy` | `LockTimeout` | 1 |
| `unstable_corpus` | `ChangedDuringRead` | 1 |
| `write` | `Write` | 1 |
| `create_dir` | `CreateDir` | 1 |
| `io` | `Io` | 1 |
| `refused` | (no variant; the adapter's policy answer) | 4 |

`conflict` and `precondition_required` arrive with M7, which adds the variants.
`refused` is shared by every surface and comes from the core (§3.8), not from one
adapter.

### 2.3 The call (`src/api/mod.rs`, new)

```rust
pub struct Call {
    pub tool: Tool,
    pub params: serde_json::Value,  // deserialized into the typed shape below
}

pub enum Tool {
    Search,               // wire: "search"
    AuthoritativeFacts,   // wire: "get_authoritative_facts"
    Lineage,              // wire: "get_lineage"
    ReadDocument,         // wire: "read_document"
    Status,               // wire: "janus_status"
}
```

`Tool::parse(&str)` and `Tool::wire_name()` are the only places a name becomes a
`Tool`. An unknown name is rejected before the core runs. One enum means the MCP
tool list, the rpc tool list and the tests cannot drift apart.

The CLI keeps its own command names (`facts`, `lineage`, `read`, `status`) and
maps them to these five. That mapping is the adapter's only naming decision.

### 2.4 The engine (`src/engine.rs`, new)

```rust
pub struct Engine {
    root: PathBuf,
    config: Config,
    clock: Box<dyn Clock>,
    embedder: Option<Box<dyn Embedder>>,
    cache: RwLock<Option<Cached>>,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl Engine {
    pub fn call(&self, call: &Call) -> Response;
}
```

`Response { ok: bool, json: serde_json::Value, code: Option<&'static str> }`.
`call` never prints, never exits, and never panics on a recoverable error. It
writes nothing to stdout; the one `refresh:` line a declined refresh produces is
a stderr diagnostic, which case 7.8 requires (M6-19).

`Clock` gains `Send + Sync` (`src/clock.rs:12`), because the cache and the
`RwLock` make the engine shared across threads.

**The cache holds one load, not one answer.** Every call still walks the corpus
and still runs the refresh decision, so freshness is a fact of this call. The
cache only avoids re-reading `index.bin` and `vectors.f32`. A cached load is
reused when both of these hold (M6-5):

1. the on-disk `manifest.json` equals the cached manifest field for field,
   including `index_sha`, `vectors_sha`, `corpus_sig`, `format_version`,
   `schema_version`, `corpus_id` and the three fingerprints, and
2. the walk digest of this call equals `corpus_sig`.

`index_sha` alone is not enough: `vectors.f32` is published separately, so a
vector-only rewrite keeps `index_sha` and changes only `vectors_sha`
(`src/index/store.rs:104`, `src/index/store.rs:220`). A stale
generation is never served from cache: its digest differs from `corpus_sig`, so
the call falls through to `prepare()`, which retries the busy lock.

**One load serves every tool** (M6-18). The load asks for the vector rows when
the configuration weights them and an embedder exists, whichever tool asked, so
a `get_authoritative_facts` call cannot leave a load that a later `search` call
would answer with zero semantics. A lexical-only tool ignores the rows it did
not need.

**Two counters, `hits` and `misses`** (M6-19). A hit answers from the load; a
miss reads the payload from disk. `janus_status` reports both.

**The key compares nine fields** (M6-17): `format_version`, `schema_version`,
`corpus_id`, `index_sha`, `vectors_sha`, `tokenizer`, `chunker`, `embedder` and
`corpus_sig`. Every other manifest field is a diagnostic (`built_at`,
`built_root`) or a number derived from a payload hash (`docs`, `chunks`, `terms`,
`postings`, `avgdl`, `index_len`, `vectors_len`). Two further rules keep the
cache honest: a load that declined a refresh is never cached, and neither is a
load whose manifest names another root, so a pending repair is never skipped.

### 2.5 Tool parameters (`src/api/dto.rs`)

Wire names and parameter names follow §9.2 exactly. Parameters are
`snake_case`, as the spec writes them. There is no `camelCase` pass.

| Tool | Parameters |
|---|---|
| `search` | `query` (string, required), `limit?` (int), `intent?` (string), `as_of?` (string), `scope?` (list of string), `scope_only?` (bool), `types?` (list), `tags?` (list), `include_superseded?` (bool), `decay_half_life_days?` (number) |
| `get_authoritative_facts` | `query?`, `tags?`, `types?`, `scope?`, `limit?` |
| `get_lineage` | `path?` **or** `topic?`, exactly one |
| `read_document` | `path` (required), `max_chars?` (int), `chunk_id?` (hex string) |
| `janus_status` | none |

`no_rebuild`, `no_embeddings` and `progress` are **not** tool parameters: an MCP
read always rebuilds when the corpus changed (§9.2) and never reports progress on
a shared stdout. `decay_half_life_days` maps to `Options::decay_half_life`;
`as_of` is parsed with `search::parse_as_of` (`src/search/mod.rs:661`), so rpc
and MCP accept exactly the CLI's date shapes.

`get_authoritative_facts` gains a working `types` filter. Today `facts` passes an
empty type list to `Filter::new` (`src/search/facts.rs:140`), so the parameter
would be accepted and ignored. P2a adds the filter and one CLI flag `--types`, so
one predicate serves both surfaces.

### 2.6 The status DTO

```json
{ "api": 1, "ok": true, "vault": "/abs/root", "corpus_id": "main",
  "writes_enabled": false,
  "embeddings": { "available": true, "model": "hash-64", "degraded": null },
  "index": { "present": true, "stale": false, "docs": 24, "chunks": 210,
             "built_at": "…", "format_version": 6, "index_sha": "…" },
  "cache": { "hits": 2, "misses": 1 } }
```

`embeddings.available` means **this process can embed a query now**, so it comes
from `embed::resolve`. A `hash-<n>` model resolves without the `embeddings`
feature (`src/index/embed.rs:267`), so it is available on every build.
`embeddings.model` is the validated manifest's `EmbedderId` when the generation
has vectors, and `null` otherwise. The two fields answer two questions and may
disagree: a feature-on binary with `enabled = false` can read stored vectors but
cannot embed a query.

### 2.7 The response matrix

The envelope does not cover every report in the program yet. This table is the
contract, and P1 adds it to §9.4.

| Surface | `api` + `ok` | `content_hash` on document rows |
|---|---|---|
| `search`, `get_lineage`, `get_authoritative_facts`, `read_document`, `janus_status` over MCP and rpc | yes | yes |
| `janus search --json`, `facts --json`, `lineage --json`, `read --json`, `status --json` | yes | yes |
| `janus index --json`, `scan`, `chunks`, `doctor`, `ids`, `moves`, `suggest-links` | unchanged in M6 | not applicable |
| a failing CLI command with `--json` | yes (`ok: false`) on stdout, then the exit code | not applicable |
| a failing CLI command without `--json` | unchanged: one `error:` line on stderr | not applicable |

`content_hash` is the field name, copied from `DocMeta::content_hash`
(`src/index/mod.rs:273`), because M7's `expected_hash` is that value. No format
bump: the value is already stored.

Two rules keep the hash honest:

1. **`read_document` hashes the bytes it returns.** It takes `content_hash` from
the same file read that supplies `body` and `frontmatter`
(`Document::content_hash`, `src/document.rs:39`), not from `DocMeta`. A stale
generation or a skipped refresh must not label old bytes with a new hash.
2. **A retired lineage node has `content_hash: null`.** A retired document has a
`TombstoneMeta` row, and that row stores no hash
(`src/index/mod.rs:207`). A live node carries the indexed hash.

## 3. Contracts

Every case below is a test. A case number names one test function.

### 3.1 The envelope

**Case 1.1.** `janus search "authority" --json` returns an object with
`ok: true` and `api: 1`, and every document row carries `content_hash`. Every
other key is byte-identical to the `main` output.

**Case 1.2.** `janus facts --json` and `janus lineage <path> --json` likewise.

**Case 1.3.** The three committed search snapshots
(`tests/snapshots/golden_search__*.snap`) move by exactly two changes each: the
added `"ok": true` and one `content_hash` per row. `api` stays `1`. The diff is
reviewed line by line against `tests/fixtures/vault`.

**Case 1.4.** A failing command with `--json` prints `{api, ok: false, code,
message}` plus `hint` when one exists, on stdout, and exits with the code of
§2.2. Without `--json` the output is unchanged.

**Case 1.5.** A failure object has no `results`, no `taken_ms` and no `vault`.

**Case 1.6.** A command outside the matrix (`janus scan --json`) is unchanged.

### 3.2 Codes and exit codes

**Case 2.1.** `--scope-only` with no `--scope` gives `code: "usage"` on rpc and
exit `2` on the CLI.

**Case 2.2.** `lineage` with neither `path` nor `topic` gives `code: "usage"`;
with both it also gives `usage`.

**Case 2.3.** A path outside the corpus gives `code: "path_escape"` on every
surface and exit `4` on the CLI.

**Case 2.4.** A generation whose manifest `format_version` is `999` gives
`code: "index_needs_full_build"`, and the `message` contains
`index needs a full build: run janus index`.

**Case 2.5.** A failed call never ends the rpc process or the MCP server. The
next line or request is answered normally. The server exits `0` on end of input.

**Case 2.6.** The mapping is total and faithful. A `match` over `JanusError` in
`src/api/error.rs` has no `_` arm, and one test asserts, for every variant, that
the table's exit code equals `JanusError::exit_code()`. No exit code moves in M6.

### 3.3 `janus read` and `read_document`

The read path returns text, so it is the only place document content reaches a
response. It reads through the same confined walk as search, and it never opens a
file outside the corpus.

**Case 3.1.** `janus read decisions/2024-11-02-postgres-cache.md --json` returns
`frontmatter` as a JSON object with `title: "Postgres cache"` and
`status: "superseded"`, `body` equal to the file bytes after the closing fence,
and `content_hash` equal to the blake3 of the whole file. The test computes that
hash from the file, so the assertion is about the bytes, not about the index. The
expected body is written out by hand.

**Case 3.1b.** With another process holding `.janus/lock`, edit only the
frontmatter, then call `read_document`. The response's `content_hash` matches the
edited file and differs from `DocMeta::content_hash`, because the refresh was
skipped. The body is unchanged. This case is the reason rule 1 exists.

**Case 3.2.** `read` of `.janus/manifest.json` gives `path_escape`.

**Case 3.3.** `read` of `../outside.md` gives `path_escape`.

**Case 3.4.** `read` of a path that does not exist gives `not_found`, and the
message names the path.

**Case 3.5.** `max_chars` counts characters, not bytes. The test body is
`"éééééé"` (six characters, twelve bytes). `max_chars: 5` returns `"ééééé"`
(five characters) and `truncated: true`; `max_chars: 6` returns the whole body
and `truncated: false`. A byte counter would return `"ééé"` for the first call,
so the two implementations disagree on a value the test states literally.

**Case 3.6.** `max_chars` defaults to `config.mcp.max_chars` (4000,
`src/config.rs:479`). An explicit request value overrides it; `0` means no limit
and sets `truncated: false`. One case per rule, each with an exact expected
`body`.

**Case 3.7.** `chunk_id` is resolved as `(path, chunk_id)` inside the current
generation, because a chunk id excludes the document id and survives a rebuild
(`src/id.rs:163`). A surviving id returns the chunk's `span` text and
`breadcrumb`; a body edit that moves the span makes `body_hash`
(`src/index/mod.rs:280`) disagree with the file, and the call returns
`not_found` with a note naming the changed document. Two cases: an unchanged
chunk across a rebuild succeeds, an edited document fails.

**Case 3.8.** `read` never returns text for a document it was not asked for, and
`search` returns spans, not text. Text has one path.

### 3.4 The engine and its cache

**Case 4.1.** Two `search` calls on an unchanged corpus give equal results.
`cache.hits` is `1` and `cache.misses` is `1`.

**Case 4.2.** A document edit between two calls makes the second call a miss, and
its result reflects the edit. The test writes the file and advances its mtime
explicitly, because the signature reads path, size and mtime only
(`src/index/refresh.rs:64`).

**Case 4.2b.** An edit that keeps path, size and mtime is invisible to the cache
key and to freshness, by design (§8.3). The test asserts the old answer, then
runs `janus index --rebuild`, then asserts the new answer. This pins the
documented limit instead of hiding it.

**Case 4.3.** Two threads call `search` on one `Engine` at once. Both answers
equal the single-threaded answer. `Clock` is `Send + Sync` for this case.

**Case 4.4.** The cache never holds a `today`: `status` and `search` compute decay
from the injected clock on every call. A `FixedClock` moved between calls changes
the decay factor and not the cached generation.

**Case 4.5.** A cache hit writes nothing: every `.janus/` mtime is unchanged.

**Case 4.6.** The cache key compares the manifest field by field. A unit test in
`src/engine.rs` holds two manifests that differ in exactly one field and asserts
the cached load is not reusable, in both directions: a cached manifest that moved
and an on-disk one that moved. The rows are the nine compared fields of §2.4. A
second test lists the compared fields and the deliberately ignored ones, and
compares that union with the keys of `serde_json::to_value(Manifest)`, so a new
field fails a test instead of silently missing the key (M6-17).

The case cannot be driven end to end through one `Engine`: the engine keeps the
embedder it was built with, so a corpus re-indexed under another model is a
compatibility mismatch, not a cache question. The predicate is the thing that
must be right, so the predicate is what the test calls.

**Case 4.7.** A busy lock is a skip, and the next call retries. Setup: edit the
corpus first, then take `.janus/lock` in another process, then call `search`.
Call one answers from the loaded generation, prints
`refresh: skipped, index busy` on stderr, and reports `stale: true`. Release the
lock and call `search` again with no further edit: the digest still differs from
`corpus_sig`, so call two refreshes. `cache.misses` is `2`, and the answer
reflects the edit.

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

**Case 5.4.** An unknown `tool` gives `code: "usage"` and a `hint` listing the
five wire names.

**Case 5.5.** A request that changes the corpus prints its `refresh:` line on
stderr only. `stdout` holds responses only (case 10.1).

**Case 5.6.** `{"tool": "janus_status"}` is answered without a build and without
a lock.

**Case 5.7.** The five calls over rpc and over the CLI `--json` commands return
equal JSON, after two normalizations: `taken_ms` becomes `0`, and for
`janus_status` the `cache` object becomes `null`. Both are process-local facts:
one is a duration, the other counts calls in this process. Each process's
counters are asserted separately against its own call history. The MCP parity
case is 7.9.

**Case 5.8.** A write tool name over rpc returns the core's refusal row
(case 8.2): `ok: false`, `code: "refused"`. The refusal is decided in
`src/api/mod.rs`, so this call proves rpc did not re-implement it.

### 3.6 The MCP handshake

Protocol version `2025-06-18` (M6-9). The server is a JSON-RPC 2.0 peer on stdio,
one message per line.

**Case 6.1.** `initialize` with `protocolVersion: "2025-06-18"` answers the same
version, `capabilities: {"tools": {}}` and
`serverInfo: {"name": "janus-mem", "version": "<crate version>"}`.

**Case 6.2.** `initialize` with an older supported version echoes the client's
version. The supported set is one constant, and the test names every member.

**Case 6.3.** `initialize` with an unsupported version answers the server's latest
version, and the client may continue or disconnect (MCP lifecycle rule). The test
asserts the answer names `2025-06-18`.

**Case 6.4.** No notification produces a response. The test sends
`notifications/initialized`, `notifications/cancelled` and an unknown
`notifications/…` method, and asserts stdout stays empty for all three.

**Case 6.5.** A request before `initialize` returns error `-32002` and the server
stays alive.

**Case 6.6.** A line that is not JSON returns `-32700` with `id: null`. A line
that is JSON but not a valid request object returns `-32600`. A known method with
a bad `params` object returns `-32602` and names the field. An unknown method
returns `-32601`. The loop survives all four.

**Case 6.7.** `tools/list` returns exactly five tools, in the fixed wire order
`search`, `get_lineage`, `get_authoritative_facts`, `read_document`,
`janus_status`. Each has `description` and `inputSchema.type == "object"` with
`additionalProperties: false`.

### 3.7 Tool dispatch

**Case 7.1.** `search` with `{"query": "cache"}` returns the envelope of case 1.1.

**Case 7.2.** `get_authoritative_facts` with `{"tags": ["principle"]}` returns
only effective-active facts, using the same predicate as the CLI (no second
inclusion rule).

**Case 7.2b.** `get_authoritative_facts` with `{"types": ["decision", "principle"]}`
on a temp vault holding one of each plus one `spec` returns the two named types
only. The test's expected set is written by hand from the three files.

**Case 7.3.** `get_lineage` with neither or both of `path` and `topic` returns
`ok: false, code: "usage"`, not a protocol error, because the shape is legal and
the combination is not.

**Case 7.4.** `search` with an unknown `intent` returns `ok: false`,
`code: "usage"`, and `isError: true` in the MCP result.

**Case 7.5.** A tool result carries the envelope twice: as `structuredContent`
and as one `content` entry of type `text` holding the same JSON. An older client
that reads only `text` still gets the answer.

**Case 7.6.** `search` rejects a `max_chars` parameter with `-32602`, because the
schema does not list it. The caller reads text with `read_document`.

**Case 7.7.** A cold mismatch (case 2.4) returns `ok: false` with
`code: "index_needs_full_build"`; a missing index is built on the first call.

**Case 7.8.** A busy writer (another process holds `.janus/lock`) is a skip, not
an error: the call answers from the generation it loaded, and one
`refresh: skipped, index busy` line goes to stderr.

**Case 7.9.** The five calls over MCP equal the five calls over rpc, after
`taken_ms` is replaced by `0`. Both adapters call `Engine::call`, so a difference
is a defect in an adapter and nowhere else.

**Case 7.10.** A lineage chain that contains one live node and one retired node
returns `content_hash` as a hex string on the live node and `null` on the
retired one. The fixture chain and one deleted document give both states in one
call.

### 3.8 The write refusal

M6 serves no write. This is a planned, temporary state, and M7 reverses it.

**Case 8.1.** `tools/list` never lists a write tool, with or without
`--allow-write`.

**Case 8.2.** A call naming `update_frontmatter`, `write_document`,
`supersede_document` or `promote_document` returns `ok: false`,
`code: "refused"`, and a `hint` that names the release that adds writes. The same
answer comes from rpc (case 5.8), because `src/api/mod.rs` owns the refusal, not
an adapter.

**Case 8.3.** `janus mcp --allow-write` starts and prints one line to stderr:
`writes are not implemented yet; --allow-write has no effect`. `janus_status`
reports `writes_enabled: false` in both modes, including when `janus.toml` sets
`[mcp] allow_write = true`.

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

**Case 9.4.** With `[embedding] model = "hash-64"` and a binary without the
`embeddings` feature, `available` is true and `model` is `"hash-64"`. Hash
embedding does not need the feature (`src/index/embed.rs:267`).

**Case 9.4b.** With a model name that needs the feature and a binary without it,
`available` is false, `model` is `null`, and `degraded` names the missing
feature.

**Case 9.4c.** After a vector build, set `[embedding] enabled = false`: `model`
is still the manifest's id, and `available` is false. The two fields disagree on
purpose (§2.6).

### 3.10 Stdout discipline

**Case 10.1.** A corpus with a config warning and a stale index produces no
`warning:` and no `refresh:` line on stdout, on any of `rpc`, `mcp` or a `--json`
command. Both lines appear on stderr.

**Case 10.2.** Every stdout line of `janus mcp` parses as a JSON-RPC message. The
test captures stdout across a whole session, including the first build, and
parses every line.

**Case 10.3.** No code path below `src/api/` and `src/engine.rs` writes to
stdout. The test greps the crate for `println!` and `stdout()` and asserts the
only hits are the CLI renderers and `src/main.rs`.

### 3.11 The walkthrough (the milestone done-when)

The session is one test against a copy of `tests/fixtures/graph-vault`. The copy
is temp, so a test never writes to a committed fixture.

**Case 11.1.** W1: `search` with `{"query": "cache decision", "intent":
"current"}` returns a non-empty list, every row has `effective_status: active`,
and `chain/2022-01-01-cache-returns.md` is in it. The four other chain documents
declare `status: superseded`, so none of them may appear in `current` (M5 §3.7).
The test asserts the set, not the rank: rank depends on scoring, which M6 does not
change.

**Case 11.2.** W2: `get_lineage` on `chain/2022-01-01-cache-returns.md` returns
the five nodes at depths 0–4 in this order: `adopt-postgres`, `move-to-redis`,
`cache-in-front`, `drop-cache`, `cache-returns`, with terminal `active`. Then
`read_document` on the same path returns a body that contains `measured budget`.
Both values come from the fixture files.

**Case 11.3.** W3: the test adds one file to its temp copy,
`principles/pricing.md`, with `type: principle`, `status: active` and
`tags: [pricing]`. `get_authoritative_facts` with `{"tags": ["pricing"]}` returns
exactly that one path. The committed fixtures hold no principle, so the test
supplies one rather than asserting an empty list.

### 3.12 What must not change

- No ranking, gating, window or decay rule moves. M6 changes where the answer is
  written, not what it says.
- `format_version` stays `6`. M6 adds no persisted field.
- `api` stays `1`. Every change is additive.
- Every exit code in `JanusError::exit_code` stays as it is.
- The CLI's human output stays. A table is not a protocol.
- `janus index`, `scan`, `chunks`, `doctor`, `ids`, `moves` and `suggest-links`
  output stays.

## 4. Decisions

| ID | Decision | Why |
|---|---|---|
| M6-1 | Hand-rolled JSON-RPC 2.0; no `rmcp`, no Tokio | Five methods and five tools need about 300 lines and one dependency already present. `rmcp` brings an async runtime, a churning API (§13 risk table) and a version pinned to a protocol revision. |
| M6-2 | One `Engine::call`; adapters parse and format only | §9.5. Two adapters that each parse params drift within one release. |
| M6-3 | Every tool object carries `api` and `ok`; failures carry `code`, `message`, `hint` | One contract an agent learns once. Additive to every existing report, so `api` stays `1`. |
| M6-4 | `janus rpc` uses `{id, tool, params}`, not JSON-RPC | The rpc loop exists for `subprocess` + `json` callers. A flat tool envelope is one line of Python. MCP keeps JSON-RPC because that is its protocol. |
| M6-5 | The cache holds one **load**, keyed by manifest equality plus the walk digest; freshness runs on every call | Removes the per-call payload read from an agent session without hiding a stale corpus. `index_sha` alone misses a vector-only publish, which changes only `vectors_sha`. |
| M6-6 | `status` walks the corpus but never builds and never locks | An agent must be able to ask "is this fresh?" before it asks a question, and a status call must never start a build. |
| M6-7 | Write tools are refused in both `--allow-write` modes until M7 | M6 has no writer. A listed tool that cannot work is worse than an absent one. Pinned by case 8.4. |
| M6-8 | A legal shape with a bad combination is `ok: false`; an illegal shape is `-32602` | The agent must read `code`, not the transport error. |
| M6-9 | MCP protocol `2025-06-18`; echo any supported version, otherwise offer the newest | Current revision with structured tool output; older hosts must still connect. |
| M6-10 | `read_document` truncates on a character boundary; default `config.mcp.max_chars`, `0` means no limit | A configured limit already exists (`src/config.rs:479`). A byte cut panics or corrupts. |
| M6-11 | Corpora, workspaces and `promote` are M6b | §0.3. |
| M6-12 | `search` returns spans, never text; `read_document` is the one text path | The generation stores spans (§8.5), and one text path cannot disagree with itself about truncation. §9.2's `max_chars` on `search` is corrected in §9. |
| M6-13 | Wire names and parameter names follow §9.2 exactly | The spec is the public contract. Renaming a tool in an implementation plan is a breaking change nobody asked for. |
| M6-14 | The envelope covers the five tool surfaces and their CLI counterparts, not the whole program | `janus index --json` is a receipt, not an answer. Widening the envelope there would move goldens for no agent benefit. |
| M6-15 | Case 2.3's CLI half belongs to the `read` phase (P2b) | No tool command of P1 raises `path_escape`: `lineage` answers `not_found` for an escaping path today, and `janus read` is the first command that refuses one (cases 3.2 and 3.3). |
| M6-16 | The SPEC §9.4 amendment ships in P5, not in P1 | The P1 file list has no `SPEC.md`. One phase must own that paragraph, so all §9 text lands together. |
| M6-17 | The cache key compares nine manifest fields; a second test names the compared and the ignored fields | `built_at` and `built_root` are diagnostics, and the counts derive from a payload hash. An exhaustive field table would cache on a wall-clock string. The test fails when `Manifest` gains a field, so the key cannot silently miss one. |
| M6-18 | One load serves all three tools, so it asks for vector rows whenever the configuration weights them and an embedder exists | A load chosen by the tool would leave a vectorless cache that a later `search` answers with zero semantics. Reading rows a lexical tool ignores costs one file read per process. |
| M6-19 | The counters are `hits` and `misses`; a declined refresh still prints its one `refresh:` line to stderr | A hit and a miss are the two things a caller can observe, and `janus_status` reports both. `stderr` is a diagnostic stream, not the answer stream: case 7.8 and §9.4 require the line, and case 10.1 keeps it off stdout. |
| M6-20 | Phase 2a ships as two pull requests: the compute/render split, then the engine | The phase is 1149 logic lines, and its named seam put 736 on one side, over the 600 ceiling. The split chosen keeps part one free of behaviour change, so its diff can be reviewed against an unmoved golden. |

## 5. Phases

Each phase: one branch from `main`, one PR, one report. Do not poll CI.

### Phase 1 — The envelope, the codes, the response matrix

**Files.** `src/api/mod.rs` (new), `src/api/dto.rs` (new), `src/api/error.rs`
(new), `src/lib.rs`, `src/main.rs`, `src/search/mod.rs`, `src/search/facts.rs`,
`src/lineage.rs`, `tests/api_envelope.rs` (new),
`tests/snapshots/golden_search__*.snap`.

**Build.** `Ok<T>` and `Failure`; `ApiCode` from `JanusError` with the exact exit
codes; `API_VERSION` re-exported; `content_hash` on document rows; a `--json`
failure prints the failure envelope on stdout before the exit code.

**Tests.** Cases 1.1–1.6, 2.1–2.6, 10.1 for the three `--json` commands only. No
rpc or MCP test here; those adapters do not exist yet.

**Handoff.** A shared DTO head exists. Every command still renders inside `run`.

**Accept.** Three snapshots move by the two declared changes only. `api` is `1`.
No exit code moves. The error match has no `_` arm.

**Split seam.** Types first (`api/dto.rs`), then wire mapping and the CLI failure
path. Split before the ceiling.

### Phase 2 — The engine, `read`, `status` (two PRs)

Mandatory split (M6 §8). P2a is dispatched first; P2b branches from `main` after
P2a merges.

#### Phase 2a — the engine and the three query tools

**Files.** `src/engine.rs` (new), `src/api/mod.rs`, `src/api/dto.rs`,
`src/clock.rs` (`Clock: Send + Sync`), `src/search/mod.rs` (split compute from
`render`), `src/search/facts.rs` (compute + the `types` filter),
`src/lineage.rs`, `src/cli.rs` (`facts --types`), `src/main.rs`,
`tests/engine_cache.rs` (new), `tests/api_hashes.rs` (new),
`tests/cli_search.rs`, `tests/common/mod.rs`.

**Build.** `Engine::call` for `search`, `get_authoritative_facts` and
`get_lineage`, plus the cache of M6-5 and its counters. `facts` and `lineage`
gain a `compute` that returns a DTO; `run` becomes `compute` + a renderer.

**Tests.** Cases 4.1–4.7, 7.2b, 1.1–1.3 for unchanged output.

**Handoff.** `Engine::call` answers three tools with no stdout. `read` and
`status` are not yet in the engine.

**Accept.** The `search`, `facts` and `lineage` human tables and JSON are
unchanged apart from `content_hash`. `Clock` compiles with the bounds.

**Split, as delivered.** The seam below was drawn the other way round and did not
fit the ceiling: extracted compute plus the engine is 736 logic lines, not 520.
The phase ships as two stacked pull requests instead (M6-20):

| part | contents | logic | tests |
|---|---|---|---|
| 2a-i | the compute/render split of `search`, `facts` and `lineage`, the owned report, and `facts --types` | 591 | 41 |
| 2a-ii | the engine, its cache, `Tool`/`Call`/parameters, and `Clock: Send + Sync` | 558 | 665 |

Part 2a-i changes no output: no golden moves. Part 2a-ii adds the second caller.

#### Phase 2b — `read`, `status`, and the two CLI commands

**Files.** `src/read.rs` (new), `src/status.rs` (new), `src/engine.rs`,
`src/api/mod.rs`, `src/cli.rs`, `src/main.rs`, `tests/cli_read.rs` (new),
`tests/cli_status.rs` (new).

**Build.** `read_document` and `janus_status` in the engine; `janus read` and
`janus status` as thin adapters. The chunk-span guard of case 3.7.

**Tests.** Cases 3.1–3.8, 9.1–9.4c, 10.3.

**Handoff.** All five tools answer from the engine. The rpc loop has nothing left
to decide.

**Accept.** `read` refuses every escape and cuts on a character boundary.
`status` never locks.

### Phase 3 — `janus rpc`

**Files.** `src/rpc.rs` (new), `src/api/mod.rs`, `src/cli.rs`, `src/main.rs`,
`tests/cli_rpc.rs` (new).

**Build.** One line in, one line out. `id` echoed. Empty line skipped. Malformed
line answered, then the loop continues. Startup failures (no corpus, no config)
exit `1` before the loop; every later failure is a response line.

**Tests.** Cases 5.1–5.8, 10.1. Case 5.7 compares rpc with the CLI, which exists;
cross-adapter parity is case 7.9 in P4.

**Handoff.** A language-agnostic surface exists. The MCP adapter reuses
`Engine::call` and nothing else.

**Accept.** A test drives `janus rpc` with a `serde_json`-built script and parses
every stdout line. No test sleeps.

**Split seam.** Part 1: the loop and codes. Part 2: the five-call parity test.

### Phase 4 — `janus mcp`

**Files.** `src/mcp/mod.rs` (new), `src/mcp/protocol.rs` (new),
`src/mcp/tools.rs` (new), `src/cli.rs`, `src/main.rs`, `tests/cli_mcp.rs` (new),
`tests/mcp_walkthrough.rs` (new).

**Build.** JSON-RPC 2.0 framing, `initialize`, notifications, `tools/list`,
`tools/call`. Schemas are `serde_json::Value` literals with
`additionalProperties: false`, so an extra param is `-32602` (case 7.6).

**Tests.** Cases 6.1–6.7, 7.1–7.10, 8.1–8.2, 10.2, 11.1–11.3.

**Handoff.** The server answers W1–W3. The write refusal exists.

**Accept.** A whole scripted session parses as JSON-RPC. The walkthrough asserts
the hand-derived first path and the five-link chain.

**Split seam.** Part 1: handshake, framing, list, dispatch, `search` and
`read_document`. Part 2: `get_authoritative_facts`, `get_lineage`, `janus_status`,
the walkthrough.

### Phase 5 — The gate, the snippets, and the corrections

**Files.** `src/mcp/tools.rs`, `src/main.rs`, `README.md`,
`docs/mcp-clients.md` (new), `SPEC.md`, `docs/m6-breakdown.md` (this plan).

**Build.** The refusal of §3.8, the `--allow-write` stderr line, and the client
registration for Claude Desktop and Hermes (§16.3). The SPEC corrections of §9
land here.

**Tests.** Case 8.3, and a test that reads the snippet and checks the JSON is
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
- **Reply format.** Each phase reports: PR number, size numbers, gates, decisions,
  and anything this plan got wrong.

## 7. Definition of done

1. `janus mcp` and `janus rpc` answer all five tools from one `Engine::call`.
   Evidence: `tests/cli_rpc.rs` parity test.
2. W1–W3 are answered from the fixtures with no `grep`. Evidence:
   `tests/mcp_walkthrough.rs`, three assertions with hand-derived values.
3. A write tool call is refused with `--allow-write` and without it, on both
   surfaces. Evidence: cases 8.2 and 5.8.
4. stdout carries protocol only. Evidence: case 10.2, one whole session parsed.
5. `janus read` and `janus status` exist and are tested. Evidence:
   `tests/cli_read.rs`, `tests/cli_status.rs`.
6. No golden moved except `ok` and `content_hash`. Evidence: the P1 diff and
   `git diff --stat tests/snapshots`.
7. `cargo doc --no-deps` has zero warnings. Evidence: the command's output.
8. `format_version` stays `6` (`src/index/mod.rs:49`) and `api` stays `1`
   (`src/scan.rs:46`). Evidence: both constants are unchanged in
   `git diff main`.
9. No exit code moves. Evidence: case 2.6, one table compared with
   `JanusError::exit_code`.

## 8. PR plan and size budget

| phase | logic ceiling | estimate | test ceiling | estimate | base |
|---|---|---|---|---|---|
| P1 envelope + codes + matrix | 400 | ~320 | 600 | ~380 | `main` |
| P2a engine + 3 query tools | 600 | ~520, actual 1149 in two PRs | 900 | ~650, actual 706 | `main` |
| P2b read + status + engine wiring | 350 | ~280 | 550 | ~400 | `main` |
| P3 rpc | 350 | ~250 | 500 | ~300 | `main` |
| P4 mcp | 550 | ~450 | 800 | ~550 | `main` |
| P5 gate + docs | 250 | ~150 | 400 | ~150 | `main` |

The P2 split is mandatory, not a contingency: three compute extractions, a cache,
a concurrency bound and two commands are not one reviewable unit. M5's lineage
phase alone took 652 logic lines.

Estimates run low by a factor of two to three in this repository, and P2a proved
it: the phase is 1149 logic lines against a 520 estimate, and its named seam put
736 on one side. Both P2 parts carry a named split seam; split before the
ceiling, never shave a comment or drop a test. No phase ceiling moves: a phase
that does not fit becomes two pull requests.

## 9. SPEC corrections

These amendments ship in the phase that owns them. Each is in the same PR.

1. **§9.4 envelope.** State that `api` and `ok` are on the five tool surfaces and
   their five CLI counterparts, that `janus index` and the other reports keep
   their shape, that a failure carries `code`, `message` and optional `hint` with
   no results, that a `--json` failure prints that object on stdout before the
   exit code, and that `content_hash` is the etag field name. (P5; M6-16.)
2. **§9.5 rpc shape.** Add the request shape `{id, tool, params}` and state that
   rpc is **not** JSON-RPC, while MCP is. The current text says "same shapes as
   the MCP tools" without saying which layer owns framing. (P3.)
3. **§9.1 exit codes.** State that exit codes belong to the CLI. A tool failure
   inside `janus rpc` or `janus mcp` is a response, not an exit. Add `janus
   status [--json]` to the command list. (P3 and P5.)
4. **§9.2 `search` parameters.** Remove `max_chars` from the `search` row and
   state that `read_document` is the one text path, addressed by `chunk_id`. The
   generation stores spans, not text, and §8.5 keeps it that way. (P2b.)
5. **§9.2 `read_document`.** State that `max_chars` defaults to `[mcp] max_chars`
   (§16.1, 4000), that a request overrides it, that `0` means no limit, that the
   cut is on a character boundary, and that `chunk_id` is resolved against
   `(path, chunk_id)` in the current generation and checked against `body_hash`.
   (P2b.)
6. **§9.2 `janus_status`.** Add the response shape of §2.6, including
   `writes_enabled`, `embeddings` (`available` from the resolved embedder,
   `model` from the manifest), `index` and `cache`. (P2b.)
7. **§9.2 refresh.** State that a cold mismatch refuses but a missing index is
   built on the first call, and that a process may cache one load, reused only
   when the on-disk manifest is unchanged and the walk digest matches
   `corpus_sig`. (P2a.)
8. **§9.1 `janus facts`.** Add `--types T,..` so the CLI and
   `get_authoritative_facts` share one predicate. §9.2 already gives the tool the
   parameter. (P2a.)
9. **§11 M6.** State that the done-when is the scripted transcript of §11 here,
   that writes are refused until M7, and that corpora, workspaces and `promote`
   are M6b. The deferral source is the handoff in `docs/m5-breakdown.md` §10, not
   the M6 paragraph. M6 keeps "register in Claude Desktop and Hermes"; M8 keeps
   the configuration snippets. (P5.)

## 10. Handoff to M7

- `Engine::call` is the one place a write tool is added. `Tool` gains a variant,
  `src/mcp/tools.rs` gains one descriptor, and `--allow-write` becomes a real gate
  instead of a refusal.
- `JanusError` gains `Conflict` and `PreconditionRequired`; `src/api/error.rs`
  gains two rows and no `_` arm, so the compiler finds the call sites.
- The document DTOs already return `content_hash`, which is the etag M7 requires.
- The cache of M6-5 must be invalidated after a write. The writer publishes a new
  `index_sha` or `vectors_sha`, so the key changes; M7 adds a test that proves it.
- The refusal of §3.8 and its pinned test are deleted in M7, in the same PR that
  adds the first write tool.

## 11. Implementation logs

Each phase appends a log here when it merges: what deviated, why, and the size
numbers.

### Review round 1 — twelve findings, all accepted

An independent review of this plan produced twelve findings. All twelve were
verified against the code before acceptance; none was rejected. The four that
changed a contract:

1. The error table had invented exit codes. `ConfigExists`, `VaultNotDirectory`
   and `PathEscape` exit `4` (`src/error.rs:214`). The table now copies
   `exit_code` and case 2.6 compares the two.
2. Case 4.2 could not fail: the signature reads path, size and mtime, not content
   (`src/index/refresh.rs:64`). The case now moves the mtime, and a new case 4.2b
   pins the documented blind spot.
3. Chunk ids exclude the document id (`src/id.rs:163`) and survive a rebuild, so
   "an id from another generation is not found" was wrong. `read_document` now
   resolves `(path, chunk_id)` and checks `body_hash` before slicing.
4. The cache key was `index_sha` alone, which a vector-only publish keeps
   (`src/index/store.rs:104`). The key is now manifest equality plus the walk
   digest.

The other eight: the tool names now follow §9.2, `get_authoritative_facts` gains
a real `types` filter, `max_chars` follows `[mcp] max_chars`
(`src/config.rs:479`), `available` comes from the resolver rather than the
feature flag, four JSON-RPC conformance cases were added, the P2 split became
mandatory and `Clock` gained `Send + Sync`, the `é` truncation case was replaced
with an exact character count, and four evidence citations were corrected.

### Review round 2 — six findings, all accepted

The re-check found six defects, five of them introduced by the round-1 fixes.
All six were verified and accepted.

1. The Unicode case still counted bytes: `"ééééé"` is five characters, so a
   five-character limit truncates nothing. The case now uses six characters and
   asserts `"ééééé"` for a limit of five.
2. `read_document` was told to copy `DocMeta::content_hash` while returning the
   file's own bytes. A skipped refresh would then label old bytes with a new
   hash. The hash now comes from `Document::content_hash`
   (`src/document.rs:39`), and case 3.1b edits the frontmatter under a held lock.
3. A retired lineage node has no hash to offer: `TombstoneMeta` stores none
   (`src/index/mod.rs:207`). The rule is now `null` on a retired node, with case
   7.10.
4. Case 4.6 could not create its own state: `janus index --no-embeddings` keeps
   `vectors_sha` (`src/index/build.rs:579`). The case now changes the embedder
   model, and the test asserts the same `index_sha` with a different
   `vectors_sha` before it queries.
5. Case 4.7 now edits before the locked call, so the retry is what refreshes.
6. P3 compared rpc with MCP, which P4 builds. Case 5.7 now compares rpc with the
   CLI, and cross-adapter parity is case 7.9 in P4.

The round-2 re-check also caught a wrong count in the introduction: only `search`
already separated compute from rendering. The text now says so.

### Review round 3 — two findings, both accepted

The re-check found two defects, both introduced by round 2.

1. Case 4.6 changed `[embedding] model` to create a vector-only publish. That
   cannot work: the cached engine keeps the embedder it was built with, so the
   new manifest is a compatibility mismatch and not a cache question
   (`src/index/store.rs:865`). The case is now a unit test on the key predicate,
   with one row per manifest field and a second test that fails when a new field
   has no row. That covers more fields than the integration test could.
2. Case 5.7 compared `janus_status` across two processes while its `cache`
   counters count calls in one. The normalization now also nulls `cache`, and
   each process's counters are asserted against its own history.

### Review closure

A fourth round could not run: the independent reviewer reached its usage limit.
Both round-3 findings were fixed exactly as the reviewer prescribed, and the
facts behind them were re-checked here: the engine keeps its embedder, so a
re-index under another model is a compatibility mismatch (`src/index/store.rs:865`),
and the cache counters count calls in one process. The review is closed on that
evidence, with round 4 as a known gap.

### Phase 1 — the envelope, the codes, the response matrix

**Sizes.** logic 269 / 400, tests 599 / 600. No dependency was added.

**What landed.** `src/api/` holds the shared head (`Ok<T>`, `Failure`), the code
table (`ApiCode`, one row per `JanusError` variant, no `_` arm) and the version
re-export. `search`, `facts` and `lineage` report `api` and `ok: true`, and every
document row carries `content_hash` taken from `DocMeta`. A `--json` failure of
one of those three commands prints the failure object on stdout before the exit
code. Without `--json` nothing changes.

**The retired node.** A retired lineage node reports `content_hash: null`. A
`TombstoneMeta` row stores no hash and this phase reads no file from disk, so
there is nothing to report. Case 1.2 pins the null.

**Deviations.**

1. Case 1.1 names the query `authority`. No fixture document contains that word,
   so the query would return no row and prove nothing about a row. The test uses
   the golden's query, `cache`.
2. Case 2.3's CLI half cannot run in this phase. The only tool command with a
   path argument is `lineage`, and an escaping path is `not_found` there, which
   is M5's answer and stays. The test asserts the `path_escape` row of the table
   and pins `lineage`'s answer. The CLI half moves to `janus read` with cases
   3.2 and 3.3 (M6-15).
3. The SPEC §9.4 amendment of §9 item 1 does not ship here. The P1 file list has
   no `SPEC.md`, and the P5 list has it. One phase must own that paragraph, so
   the text ships with the other §9 corrections (M6-16).
4. Case 1.6 asserts the success half only: `scan --json` on a failure is already
   pinned by `cli_scan::scan_json_errors_stay_on_stderr_with_empty_stdout`. Case
   2.5 asserts the CLI half only; the server halves arrive with the rpc loop and
   the MCP server.
5. Case 10.1 also covers the declined-refresh shape in the same test: a read
   under `--no-rebuild` puts its `refresh:` line on stderr and says
   `stale: true` in the object.
6. The `--json` failure object is printed for the five tool commands, not for
   every command (§2.7). `scan --json` keeps an empty stdout on failure, which
   case 1.6 and the existing `scan` test both pin.

**Test budget.** The tests sit one line under the ceiling. Case 2.6 is a table
of twenty variants and costs most of it. That table is the case, so no assertion
was dropped for it: one test was folded into the case it belongs to, and four
short argument lists moved onto one line.

**Golden diff.** The three search snapshots moved by `ok: true` once and one
`content_hash` per row, and by nothing else. Each hash was checked against the
blake3 of its own fixture file. `api` is still 1, `format_version` is still 6,
and no exit code moved.

### Phase 2a — the engine and the three query tools

**Sizes.** Two pull requests, both under the ceiling: the compute/render split is
591 logic and 41 test lines, the engine is 558 logic and 665 test lines. No
dependency was added.

**What landed.** `search`, `facts` and `lineage` each compute an answer and then
render it. The report is owned, the human table reads the same value the JSON
does, and the undated provenance note is part of the answer, so the two renderers
cannot disagree about a row. `Engine::call` answers `search`,
`get_authoritative_facts` and `get_lineage` as one `Response` with no stdout, and
the cache of M6-5 saves the payload read while freshness runs on every call.
`Clock` is `Send + Sync`, and `get_authoritative_facts` honours `types` through
the same predicate the new `janus facts --types` uses.

**Deviations.**

1. The phase ships in two pull requests (M6-20). The phase is 1149 logic lines
   against a 520 estimate, and the named seam put 736 on one side, which is over
   the ceiling on its own. The seam moved to the compute/render split, so part
   one changes no output and part two adds the second caller.
2. Case 4.6's "one row per field" cannot mean every manifest field: a cache key
   on `built_at` would never hit. The rows are the nine compared fields, and a
   second test names those nine plus the nine ignored fields and compares the
   union with the struct's own keys (M6-17).
3. Case 4.6 also asserts both directions of the comparison: a cached manifest
   that moved and an on-disk manifest that moved. One direction alone would pass
   for a predicate that ignores its second argument.
4. The cache asks for vector rows for every tool, not for the rows the calling
   tool needs (M6-18). Without that, a `facts` call could leave a load that a
   later `search` would answer lexically while the configuration weights
   semantics.
5. Case 4.7's stderr half is not asserted in `tests/engine_cache.rs`: a process
   cannot capture its own stderr without a new dependency, which §1 forbids. The
   case asserts the observable answer (`index.stale` is true, the new document is
   absent) and the counters, and `tests/cli_search.rs` pins the exact
   `refresh: skipped, index busy` line for the CLI.
6. `get_authoritative_facts` keeps its fallback trigger corpus-wide. `types`
   narrows the fact list; it does not decide whether the corpus declares a fact
   type, so a filter that hides every fact still gets the fact list and an empty
   answer rather than the newest-documents fallback.
7. The SPEC amendments of §9 items 7 and 8 ship in the second pull request with
   this log, not in the first, because both describe the phase as a whole.

**Test budget.** The engine's cases sit under the ceiling: 665 of 900 test lines
cover the whole engine, including the two-thread case and the manifest table.

**Golden diff.** None. `git diff --stat tests/snapshots` is empty: the split of
compute from rendering moved no score, no order, and no field. `format_version`
stays 6 and `api` stays 1.

**Handoff to 2b.** `Engine::call` serves three tools; `read_document` and
`janus_status` are the two remaining variants of `Tool`, and `Engine::cache`
already answers the `cache` block of §2.6. `src/engine.rs` is the only place a
tool is added, and `src/api/mod.rs` is the only place a wire name is decided.

### Phase 2b — `read`, `status`, and the two CLI commands

_(empty)_

### Phase 3 — `janus rpc`

_(empty)_

### Phase 4 — `janus mcp`

_(empty)_

### Phase 5 — the gate, the snippets, and the corrections

_(empty)_
