# M3 — Lexical search, daily usable: phased breakdown

> Branch: `m3-plan` (this document), then one branch per phase from `main`.
> Spec: `SPEC.md` §6.5, §6.6, §6.6.1, §6.12 (rules 1–5, 9), §6.12.1 (obligations 5,
> 8, 9), §7.1 (intents), §7.2 (scoring), §8.3 (index format, `.janus/lock`), §8.3.1
> (RAM), §9.1, §9.2 (`search`, `get_authoritative_facts`), §9.4 (envelope), §10 (eval,
> property, benchmark rows), §11 (M3), §13, §14.
>
> Milestone statement (SPEC §11): “Hand-written BM25 (inverted index, tokenizer,
> optional stemmer); scoring module with authority, status gates, intents, and the
> scope filter (`--scope`, `--scope-only`, default union); `janus search` and
> `janus facts` with human + JSON output.”
>
> **Done when (verbatim from §11):** “the author uses `janus search` instead of `rg`
> for a day, and the eval set’s ‘active top-1’ rate is ≥80% (lexical only).”
> §7 makes both checkable — one as a manual sign-off item, one as a number with a
> defined denominator — without replacing either.

M2 is merged (`25b995a`). M3 inherits from M1/M2: `Chunk` with byte spans and
content-addressed ids, `Chunk::text`, `Chunk::breadcrumb`, `Chunk::embed_text`
(the string the lexical index should see), `Chunk::char_count`, `Document` with
resolved type/status/scope/date/tags/shared_with, `DocNote`, the `scan`/`chunks`
pipeline seam, and the `{code, message}` DTO pair. It inherits no index, no clock
abstraction, no scoring, and no cross-process coordination.

**Revision note.** This document was revised after review. Seven findings were
resolved by adding explicit contracts (§3), a sixth phase, and acceptance cases
that fail if the contract is violated. §9 maps each finding to its resolution and
lists the SPEC gaps that remain open. No implementation exists yet; nothing here
describes code that has been written.

## 0. Scope

### In scope
- A Unicode-aware tokenizer with an explicit ownership model for normalized text
  and borrowed term iteration (§3.7).
- An in-memory inverted index with BM25 (§3.3): postings, document frequencies,
  lengths, no phrase matching.
- A pure scoring module: authority × status × decay, four intents, the scope filter,
  candidate-set scoping, deterministic tie-breaks, `Clock` injected (§3.2).
- `index.bin` + `manifest.json` under `.janus/`, `format_version = 1`, blake3
  checksums, **cross-process publication under `.janus/lock`**, and incremental
  rebuild keyed on the indexed input rather than on `ChunkId` alone (§3.4, §3.5).
- `janus index [PATHS...] [--rebuild] [--dry-run]`.
- `janus search <QUERY> [--intent …] [--scope …] [--scope-only] [--limit N]
  [--types T,..] [--tags T,..] [--decay-half-life DAYS] [--include-superseded]
  [--json]` and `janus facts [--tags …] [--query Q] [--scope S]… [--json]`.
- The retrieval eval harness: `tests/eval/queries.jsonl` (≥30 queries), reported as
  a **non-blocking** CI number plus an explicit milestone acceptance run (§3.8).

### Explicitly NOT in scope (do not build)
- Embeddings, `Embedder`, `vectors.f32`, `α > 0`, hybrid scoring (M4). `semantic` is
  present in the model as `0.0` so M4 adds a term instead of rewriting `score()`.
- `mmap`, `madvise`, quantization, `watch`, `criterion` benches (M8).
- MCP and `janus rpc` (M6). CLI only.
- Any write to the vault. `janus index` writes `.janus/` only.
- Supersession, transitive lineage, `--as-of`, validity windows, `effective_status`,
  `doctor`, `lineage`, `moves`, `path_history` (M5). M3 gates on **declared**
  `status`; `--as-of` is not exposed at all rather than exposed-and-wrong.
- Multi-corpus queries, `--corpus`, archived weighting (M5/M6).
- The Porter stemmer (`Stemmer::None` only; see S-2).
- `--diversity` / MMR and any multi-chunk-per-document display (§7.2 calls it an
  optional pass, default off; M3 has exactly one result contract, §3.1).
- `janus grep` (rejected: §15 open questions).

## 1. Dependency changes (`Cargo.toml`)

| Crate | Version | Why (one-line justification per §8.2 policy) |
|---|---|---|
| `bincode` | 1.3 (serde-based; verify at implementation time — 2.x drops serde for `Encode`/`Decode`) | `index.bin` is a `bincode` sidecar by D13; a hand-rolled binary format for postings would be a second, worse format. |
| `tempfile` | 3 (promote from dev-dependency) | Publication needs a **unique** temp file per writer that cleans itself up on failure; a shared `index.bin.new` is exactly the race review finding 1 describes. Promoting beats hand-rolling pid/counter names. |
| `fd-lock` | 4 (fallback: `fs4` if its API fits better) | `.janus/lock` is specified by §8.3 and M3 has real cross-process writers (§3.5); advisory locks are platform-specific FFI, which D12 forbids us to write ourselves. |

No new crates for tokenization, stemming (off), stopwords (none), or `criterion`
(M8). `serde`, `serde_json`, `blake3`, `chrono`, `clap` already exist.

## 2. Data model (write these first — §12.3.3)

```rust
// src/tokenize.rs — §3.7. One owner of the folded bytes, borrowed terms.
pub struct Tokens {
    folded: String,              // lowercased text, owned
    ranges: Vec<Range<u32>>,     // term ranges into `folded`; u32 halves the cost
    dropped: u32,                // terms over MAX_TERM_BYTES (64), for diagnostics
}
impl Tokens {
    pub fn iter(&self) -> impl Iterator<Item = &str> + '_;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn dropped(&self) -> u32;
}
pub enum Stemmer { None }        // one variant; adding Porter must not touch call sites
pub struct Tokenizer { /* stemmer: Stemmer */ }
impl Tokenizer {
    pub fn algorithm(&self) -> &'static str;      // "v1", part of the fingerprint
    pub fn fingerprint(&self) -> TokenizerFingerprint; // algorithm + stemmer
    pub fn tokenize(&self, text: &str) -> Tokens;      // owns; no borrow of `self`
    pub fn tokenize_into(&self, text: &str, out: &mut Tokens); // reuse, no realloc
}
```

```rust
// src/index/mod.rs — persisted shape (§8.3). Lexical-only in M3.
pub const FORMAT_VERSION: u16 = 1;

pub struct Manifest {                    // .janus/manifest.json (human-readable)
    pub format_version: u16,
    pub schema_version: u16,
    pub corpus_id: String,
    pub built_root: String,              // canonical; relocation check only (§8.3)
    pub built_at: String,                // RFC 3339 UTC, diagnostic
    pub docs: usize,
    pub chunks: usize,
    pub terms: usize,
    pub postings: usize,
    pub avgdl: f64,
    pub index_sha: String,               // blake3(index.bin) — the generation id
    pub index_len: u64,
    pub tokenizer: TokenizerFingerprint, // algorithm + stemmer: a change rebuilds
    pub chunker: ChunkingFingerprint,    // max_chars + overlap: a change rebuilds
    pub embedder: Option<String>,        // None in M3; Some in M4 (§8.3)
}

pub struct Snapshot {                    // .janus/index.bin (bincode)
    pub format_version: u16,
    pub corpus_id: String,
    pub tokenizer: TokenizerFingerprint,
    pub strings: StringPool,
    pub docs: Vec<DocMeta>,
    pub chunks: Vec<ChunkMeta>,
    pub terms: Vec<TermMeta>,            // term text lives in the pool
    pub postings: Vec<Posting>,          // (chunk_index, tf), grouped by term
    pub avgdl: f64,
}

pub struct DocMeta {
    pub id: DocId,
    pub rel_path: StringId,
    pub title: StringId,
    pub type_name: StringId,
    pub status: StatusCode,              // compact declared status (§3.2)
    pub scope: StringId,
    pub shared_with: Vec<StringId>,
    pub tags: Vec<StringId>,             // --tags after reload (review finding 2)
    pub date: Option<NaiveDate>,
    pub effective_from: Option<NaiveDate>,
    pub timeless: bool,
    pub content_hash: Blake3Hash,
    pub lex_key: [u8; 32],               // §3.4: identity of the *indexed text*
    pub chunk_range: (u32, u32),         // into `chunks`
}

pub struct ChunkMeta {
    pub id: ChunkId,                     // content identity; NOT a postings key (§3.4)
    pub doc: u32,
    pub span: (u32, u32),                // bytes into Document.body
    pub heading_path: Vec<StringId>,
    pub tokens: u32,                     // length norm for BM25
}

pub struct TermMeta { pub df: u32, pub postings: (u32, u32) } // range into `postings`
```

```rust
// src/score.rs — pure, clock-injected (§7.2)
pub trait Clock { fn today(&self) -> NaiveDate; }
pub struct FixedClock(pub NaiveDate);
pub struct SystemClock;

pub struct ScoreInput<'a> {
    pub doc: &'a DocMeta,
    pub lexical: f64,                    // already normalized to [0, 1], §3.3
    pub semantic: f64,                   // 0.0 in M3
    pub intent: Intent,
    pub alpha: f64,                      // 0.0 in M3
    pub half_life_days: Option<f64>,     // from config/type/flag
}

/// Every factor is reported separately, and `authority` is the **product**
/// (§7.2: `authority = AUTHORITY[type] · STATUS_FACTOR[status]`). Review
/// finding 5: an implementer must not be able to compute `status_factor` and
/// forget to apply it.
pub struct Breakdown {
    pub semantic: f64,
    pub lexical: f64,
    pub relevance: f64,
    pub type_authority: f64,
    pub status_factor: f64,
    pub authority: f64,                  // type_authority * status_factor
    pub decay: f64,
    pub final_score: f64,
}
```

```rust
// src/search/mod.rs — one query, one result contract (§3.1)
pub struct SearchQuery {
    pub text: String,
    pub intent: Intent,
    pub scopes: Vec<String>,             // empty = all scopes (§6.12 rule 3)
    pub scope_only: bool,
    pub types: Vec<String>,              // empty = all; OR within, AND across flags
    pub tags: Vec<String>,
    pub limit: usize,                    // default 10
    pub include_superseded: bool,        // accepted, inert until M5 (documented)
}

pub struct Hit {                         // one *document*, not one chunk
    pub doc_id: DocId,
    pub rel_path: String,
    pub title: String,
    pub type_name: String,
    pub declared_status: Status,
    pub scope: String,
    pub date: Option<NaiveDate>,
    pub chunk_id: ChunkId,               // the argmax chunk (§7.2)
    pub span: (u32, u32),
    pub heading_path: Vec<String>,
    pub score: Breakdown,
}
```

`CandidateSet` is a sorted `Vec<u32>` of chunk indices in M3, with a note that
§7.2 permits a bitset; the scorer takes `Option<&CandidateSet>` either way so a
future filter reuses the path.

## 3. Contracts

These are the normative parts of this plan. Each was either implicit or wrong in
the first draft, and each has acceptance cases that fail when violated.

### 3.1 Result contract: one document per result, limit after grouping

Per §7.2 (“a doc’s score is the max over its chunks; the returned chunk is the
argmax”), in this exact order (review finding 4):

1. Score candidate **chunk occurrences** (`lexical → relevance → final`).
2. Group by `DocId`; keep `doc.score = max(final)` and the argmax chunk
   (ties between chunks of one document: lower `ordinal` wins).
3. Filter (types/tags/scope/status) — filters run on documents, before grouping is
   rendered, and are cheap because they use `DocMeta` only.
4. Sort documents: `final` desc, then `date` desc (missing date last), then argmax
   `chunk_id` asc. Total order, no floats compared with `==`.
5. Apply `--limit` to **documents**.
6. Render one row per document, quoting its argmax chunk.

**Not in the default path:** more than one chunk per document, MMR diversity, or a
second display ordering. `--diversity` is deferred (§7.2 keeps it optional and off).

**Acceptance:** a fixture where one document owns the top ten chunk scores and nine
other documents also match — `--limit 10` returns ten **distinct** documents. A
document with many chunks appears once. A test asserts `hits.len() == limit` and
`hits.map(doc_id).dedup() == hits.map(doc_id)`.

### 3.2 Scoring contract: complete equations and a per-intent table

```
lexical   = squash(bm25)                                  ∈ (0, 1]   §3.3
semantic  = 0.0                                            (M3; M4 fills it)
relevance = α · semantic + (1 − α) · lexical               α = 0.0 in M3
authority = AUTHORITY[type] · STATUS_FACTOR[status]         (§6.6, §7.2)
decay     = exp(−ln2 · age_days / half_life_days)          or 1.0 when disabled
final     = relevance · authority · decay
```

Review finding 5 was that the first draft wrote `final = relevance · authority ·
decay` while `Params` stored `status_factor` separately and never said which one
`authority` meant. `Breakdown.authority` is the product, and `type_authority` /
`status_factor` are reported alongside it so a ranking can be explained.

`age_days` = `clock.today() − (effective_from ?? date)`, clamped at 0 (future dates
decay 1.0). `decay = 1.0` when: the type is `timeless`, the half-life is `<= 0`, the
document has no date at all, or the intent disables decay.

| Intent | Decay | Included statuses | `STATUS_FACTOR` |
|---|---|---|---|
| `current` (default) | on (per-type half-life; `--decay-half-life` overrides) | `active`, `draft`, `unknown` | active 1.0, draft 0.7, unknown 0.8 |
| `history` | off | all five | 1.0 for every status |
| `evolution` | off | `active`, `deprecated`, `superseded`, `unknown` | 1.0 for every status |
| `exhaustive` | off | all five, `unknown` included explicitly | 1.0 for every status |

`draft` and `deprecated`/`superseded` exclusion is an **inclusion rule**, not a
weight: an excluded document is absent from the result set, not scored at zero.
Sorting inside `evolution` is oldest→newest on `date` (§7.1), then the §3.1 keys.

**SPEC gap (recorded, not silently extended):** §6.6’s status table has columns for
`current`/`history`/`evolution` only, while §7.1 defines a fourth intent,
`exhaustive`, and §6.6 also omits its factors. The reading above (`exhaustive` =
everything included, every factor 1.0, no decay) follows §7.1’s description (“all,
including `unknown`”) and is flagged in §9 for a future one-row SPEC edit. Until
then this table is the contract, and the phase-3 handoff says so.

**Acceptance (table-driven, one row per cell):** `current` with a `draft` document
scores `relevance · type_authority · 0.7 · decay` and the same document with
`active` scores exactly `1/0.7` times as much, all else equal; `unknown` is `0.8`;
`deprecated` and `superseded` are **absent** under `current` and **present** under
`history`; `history` and `exhaustive` return factor `1.0` for every status and
`decay == 1.0`; `evolution` excludes `draft`. Each assertion compares whole
`Breakdown` values, not just the final number.

### 3.3 Lexical normalization: a sigmoid, defined and scale-free

SPEC §7.2 says `lexical = bm25_norm(q, c) ∈ [0, 1] via sigmoid squash` and names no
constants. The first draft substituted `s / (s + 4.0)`, which review finding 6
correctly called a change to the ranking contract rather than an implementation
detail. Resolution: **keep the sigmoid**, define it, and make its midpoint a
property of the query instead of a magic constant.

```
bm25(t, c) = idf(t) · tf · (k1 + 1) / (tf + k1 · (1 − b + b · dl / avgdl))
idf(t)     = ln(1 + (N − df(t) + 0.5) / (df(t) + 0.5))     always > 0
s          = Σ over query terms of bm25(t, c)
s_ref      = Σ over query terms of idf(t)                  the reference score
lexical    = s / (s + s_ref)      ≡ logistic(ln(s / s_ref))
```

`s_ref` is the score a document earns for matching **every query term exactly once
at average length** (`dl == avgdl` makes the length factor `tf·(k1+1)/(tf+k1) = 1`
at `tf = 1`). So `lexical = 0.5` means “an average match for *this* query”, 0.75
means “three times the reference”, and the value does not depend on corpus size —
which a fixed midpoint cannot offer, because `idf` grows like `ln N`:

| corpus | `df = 1` (N = 40) | `df = 1` (N = 20 000) |
|---|---|---|
| `idf` | 3.308107 | 9.498072 |

A fixed `s + 4.0` would saturate on the 20k vault and barely move on the 40-document
fixture. With `s_ref`, `r = s / s_ref` and `lexical = r / (1 + r)`:

| `r` | 0 | 0.25 | 0.5 | 1 | 1.5 | 2 | 3 | 5 | 10 |
|---|---|---|---|---|---|---|---|---|---|
| `lexical` | 0 (short-circuit) | 0.200 | 0.333 | 0.500 | 0.600 | 0.667 | 0.750 | 0.833 | 0.909 |

Worked example, one-term query `cache` on the fixture vault (N = 40, df = 1,
`idf = 3.308107`, so `s_ref = 3.308107`), `dl = avgdl`, `k1 = 1.2`, `b = 0.75`:

| `tf` | `tf · 2.2 / (tf + 1.2)` | `s` | `r` | `lexical` |
|---|---|---|---|---|
| 1 | 1.0000 | 3.308107 | 1.0000 | 0.5000 |
| 3 | 1.5714 | 5.198454 | 1.5714 | 0.6111 |
| 10 | 1.9643 | 6.498067 | 1.9643 | 0.6627 |
| ∞ | 2.2000 | 7.276961 | 2.2000 | 0.6875 |

**Contracts that fall out of this and are asserted:** `lexical(0) = 0` by
short-circuit (a candidate always has `s > 0`, but the function must be total);
`s_ref > 0` whenever any query term exists in the index, so there is no division by
zero; `lexical` is strictly increasing in `s` and bounded above by 1; and it is
*scale-free* — the same document/query pair in a 40-document and a 20 000-document
vault maps to the same value when `r` is equal.

**SPEC gap:** §7.2 leaves the squash symbolically defined. This parameterization is
a clarification, not a change of family, and §9 records the one-line SPEC edit that
would remove the ambiguity. If the eval later prefers a fixed midpoint, it becomes a
config value and a decision row — not a silent edit.

### 3.4 Reuse identity: key on the indexed input, not on `ChunkId`

Review finding 2, and it is a real defect: `ChunkId = blake3(heading_path ‖
normalized body text)` **excludes the title** (CH-5, D48), while S-3 indexes
`embed_text` = title + breadcrumb + body. Two consequences, both fatal to a
`ChunkId`-only reuse rule:

- A frontmatter `title:` edit changes tokens and not the `ChunkId` → stale postings.
- Two documents with identical bodies and different titles share a `ChunkId` →
  whichever is indexed first would supply postings for both, even on a clean build.

Resolution, with two separate identities:

| Identity | Formula | Purpose |
|---|---|---|
| `ChunkId` | `blake3(heading_path ‖ 0x00 ‖ normalized_text)` (unchanged, D48) | embedding cache, cross-references, span lookup. **Never a postings key.** |
| `DocMeta.lex_key` | `blake3(tokenizer_fp ‖ 0x00 ‖ chunker_fp ‖ 0x00 ‖ effective_title)` | “the text this document contributes to the index is unchanged” |

Regeneration is per **document**, and reuse is a contract with five conjuncts:

```
reuse(doc) ⟺  old.lex_key == new.lex_key
           ∧  old.content_hash == new.content_hash          (cheap short-circuit)
           ∧  old.tokenizer_fp == new.tokenizer_fp          (also in the manifest)
           ∧  every new ChunkId of the document is present in the old snapshot
           ∧  the document's chunk count is unchanged
```

What is reused is **tokens only**. Freshly rebuilt every run, always: `DocMeta`
(including `rel_path`, `title`, `doc_id`, dates, tags, status), `ChunkMeta` spans,
heading paths, and the document ordering. A reused document keeps no metadata from
the previous generation.

**Postings are per chunk occurrence.** `postings` entries are `(chunk_index, tf)`
where `chunk_index` is the occurrence’s position in `chunks`, never a `ChunkId`.
Identical bodies in two documents with different titles are two occurrences, each
with its own term frequencies and its own length contribution to `avgdl` — so the
clean build and the incremental build agree by construction.

**Mechanism** (no duplicated storage, and review finding 2’s “cache body terms
separately” option without a second cache): rebuild one pass over the decoded
`postings` to reconstruct each old chunk’s `(term, tf)` list (term-major → forward),
then tokenize only non-reused chunks, then re-invert everything. Two linear passes,
**zero re-tokenization** — which is the cost that matters; the passes are ~10 ms at
200k chunks. A build-stat counter (`retokenized_chunks`) makes the claim testable.

**Acceptance (each compares incremental output with a clean rebuild, byte-for-byte
on the snapshot after normalizing `built_at`):**

1. Frontmatter `title:` edited, body untouched → identical to clean rebuild.
2. File renamed so the **derived** title changes (no `title:` key) → identical.
3. Two documents, byte-identical bodies, different titles → both occurrences
   present with correct `df`, `tf`, `avgdl`; identical to clean rebuild.
4. Pure move (`rel_path` changes, title and body unchanged) → identical **and**
   `retokenized_chunks == 0`.
5. Body edited → that document retokenized, others not.
6. Tokenizer fingerprint bump → everything retokenized.

### 3.5 Publication and reader protocol (`.janus/lock`)

Review finding 1: “one command surface” does not mean one writer. Two `janus index`
processes, or two `janus search` calls auto-building a missing index, already
contend in M3. §8.3 assigns `.janus/lock` to exactly this.

**Writer protocol** (every path that writes `.janus/`):

1. `LockFile::new(".janus/lock")?.write()` — exclusive, blocking with a 10 s
   timeout; timeout is `JanusError::LockTimeout` (exit 1, names the file).
2. **Double-checked freshness under the lock**: if another process built a usable
   snapshot while we waited, adopt it (`rebuilt = false, reused = true`) and stop.
   This is what makes two simultaneous auto-builds cost one build.
3. Sweep leftover `.tmp*` files from crashed writers (they are only ever written
   under the lock, so a stale one is garbage).
4. Build the snapshot in memory.
5. `NamedTempFile::new_in(".janus")` → write bytes → `sync_all()`.
6. `persist("index.bin")` — atomic replace.
7. `sync_all()` on `.janus/` (directory), ignored where unsupported (`#[cfg]`).
8. New temp manifest → write → `sync_all()` → `persist("manifest.json")` → dir sync.

**Manifest last, and never a shared temp name.** The old `index.bin.new` was itself
a race (two writers truncating one file); a `NamedTempFile` is unique per writer and
removed on drop if step 5–8 fail, so a failed publish leaves the previous generation
intact — which is invariant I3’s spirit applied to the index.

**Reader protocol** (no lock — readers must never block on a builder):

1. Read `manifest.json`. Absent → “not built” (auto-build path).
2. Reject on `format_version`, `tokenizer_fp`, `chunker_fp`, `corpus_id`, or
   `built_root` mismatch → rebuild path (§3.5 writer).
3. Read `index.bin`, compute `blake3`, compare with `manifest.index_sha`.
4. Match → load and validate; mismatch → **generation changed under us** or
   corruption → re-read the manifest and retry, up to 3 times with 10/50/200 ms
   backoff.
5. Still mismatched → take the lock and rebuild once; if that also fails,
   `JanusError::IndexUnstable { path }`, exit 1, naming the file.

**Generation identity is the pair `(format_version, index_sha)`.** Because
`index.bin` is renamed *before* `manifest.json`, a reader that sees generation *G*
in the manifest is guaranteed that `index.bin` already holds *G*: the only reachable
mismatch is “new index, old manifest”, which the retry resolves. This is why the
order is fixed and why step 8 exists.

**Load-time validation** (a hand-edited `index.bin` must not index out of bounds):
every `StringId` `< strings.len()`, every `(start, end)` `<= postings.len()` and
`start <= end`, every `chunk.doc < docs.len()`, `chunk_range` in range and monotone,
`terms` postings ranges disjoint and ascending, `avgdl >= 0` and finite. Any
violation is a checksum-equivalent corruption: rebuild, never a panic.

**Acceptance (no sleeps, no wall-clock assertions):**

1. **Reader during publication.** A `#[doc(hidden)] pub fn publish_with_hook(dir,
   snapshot, &mut dyn FnMut(Stage))` seam calls back at each of the six writer
   stages; a test runs a reader at every stage and asserts it either loads the
   *previous* generation (sha matches the bytes it read) or retries into the new one.
   No test sleeps to create the race.
2. **Two simultaneous auto-builds.** Two threads call the auto-build path against a
   missing index with the same root; assert exactly one built (`rebuilt` count 1,
   `reused` 1) and both returned identical hits.
3. **Lock timeout.** A holder keeps the lock; the second writer with a 10 ms timeout
   exits 1 with `lock timeout`, writes no temp file, and leaves the prior generation
   loadable.
4. **Failed publish.** Injected failure between index rename and manifest rename →
   the previous generation still loads; the leftover temp is removed by the next
   publish’s sweep.
5. **Corrupt bytes** (flip one byte, truncate, zero the file) → rebuild with one
   stderr line, exit 0 for a read command.
6. **Concurrent read storm.** 8 reader threads during one publish: all either
   succeed or retry; zero errors; every returned hit set equals the pre- or
   post-publication set.

### 3.6 `janus facts`: an inclusion predicate, not a penalty

SPEC §9.2: “Active decisions/specs/principles only.” The `current` intent admits
`draft` (0.7×) and `unknown` (0.8×) per §6.6, so reusing that gate would surface
draft specs as authoritative facts (review finding 3). A weight is not an
inclusion rule.

```
facts(doc) ⟺ doc.type ∈ { decision, spec, principle }        (canonical names, post-alias)
          ∧ doc.declared_status == active
          ∧ (scope filter, if any)
```

No decay, no status factor (factor is 1.0 for every included document), ordering
`type_authority` desc → `date` desc → `chunk_id` asc. `--query` narrows by BM25
(relevance still computed and reported); with no `--query` the result is an
authority-ordered listing, which is what “give me the current facts” means.

**Acceptance:** table-driven over all five statuses × the three types — only
`active` decision/spec/principle appear; `draft`, `unknown`, `deprecated`,
`superseded` are absent even though `search --intent current` returns the `draft` and
`unknown` ones. A CLI test runs both commands on the fixture vault and asserts the
difference, so the two behaviours are pinned against each other.

**Documented limitation:** “active” is the *declared* status in M3. `effective_status`
is scope-relative and needs supersession (M5); when M5 lands, `facts` switches to
`effective_status == active`, which is a behaviour change recorded there, not a
silent one here.

### 3.7 Tokenizer ownership

The first draft’s signature was unimplementable without `unsafe`:
`terms<'a>(&'a self, text: &'a str) -> impl Iterator<Item = &'a str>` would have had
to return slices of a lowercased buffer that no one owns. Review finding: define a
safe owner, without a trait or a self-referential struct.

Ownership rule: **`Tokens` owns the folded bytes; terms borrow from `Tokens`, never
from the input and never from the `Tokenizer`.** `Tokenizer` is stateless (except
its fingerprint) and can be shared; `Tokens` is the per-call allocation and is
reusable through `tokenize_into`, so an index build reuses one buffer for every
chunk instead of one allocation per chunk.

- No `unsafe`, no `PhantomData`, no `Rc<RefCell<…>>`, no lifetime parameter on
  `Tokens`: `folded: String` + `ranges: Vec<Range<u32>>` is enough.
- Borrowing shape is `tokens.iter() -> impl Iterator<Item = &str> + '_`, tied to
  `&Tokens`; callers that need to keep a term (the index) copy it into the
  `StringPool` immediately.
- Query path uses the same `Tokenizer` and the same `Tokens` type. If the two ever
  diverge, every query silently misses; a test asserts a shared table of
  `(text → terms)` for both paths, and a second test asserts that indexing a chunk
  and tokenizing the same text as a query produce identical term vectors.

**Acceptance:** a doc test showing `let tokens = tokenizer.tokenize(&text); let
terms: Vec<&str> = tokens.iter().collect();` compiles without a lifetime annotation
and without `unsafe`; a test that `tokenize_into` reuses the allocation
(`tokens.folded.capacity()` doesn’t grow across calls after warm-up); the
`proptest` that no term is ever a slice outside the folded buffer.

### 3.8 Quality gate: milestone acceptance vs CI report vs timing

Review finding 7: §10 calls retrieval quality a **non-blocking CI report**, while the
first draft put a failing `≥ 0.80` assertion inside `tests/eval_quality.rs`, which
`cargo test --all-targets` would gate on — contradicting SPEC *and* turning a
quality experiment into a flaky build. It also promised a wall-clock assertion,
which §10 and AGENTS.md both forbid.

Three separate things, three separate homes:

| Thing | Where | Blocking? |
|---|---|---|
| Correctness (BM25 values, gates, ordering, scope, dedupe, publication) | unit + integration tests | **Yes** — `cargo test --all-targets` |
| Retrieval quality (`active top-1`, `nDCG@10`) | `tests/eval_quality.rs`, `#[ignore]`’d; run by `scripts/eval.sh`; CI job `eval` with `continue-on-error: true` publishing both numbers | **No** — a report, per §10 |
| Milestone acceptance (`active top-1 ≥ 0.80`) | the manual acceptance step in §7: run `scripts/eval.sh`, read the number, record it in the PR that changes the ranking | **Yes at milestone close, by a human** |
| Performance | opt-in measurement (`scripts/eval.sh --timing`), printed, never asserted; `criterion` benches are M8 | **No** |

`active top-1` definition, since “the eval’s active top-1 rate” is otherwise
undefined:

- **Eligible denominator:** queries in `tests/eval/queries.jsonl` with
  `"intent": "current"` whose expectation names at least one **active** document.
  Queries with `history`/`evolution`/`exhaustive` intent are excluded from this
  metric — they legitimately rank inactive documents, and averaging them in would
  make the number meaningless.
- **Numerator:** eligible queries whose rank-1 hit is one of the expected active
  documents.
- `nDCG@10` is reported over **all** queries with graded expectations, separately.
- Both numbers print with their denominators (`active-top-1 0.84 (27/32)`,
  `nDCG@10 0.71 (41 queries)`), so a change in the corpus cannot silently move the
  denominator.
- **The clock is injected:** eval and retrieval tests run under
  `FixedClock(2025-04-01)`, so decay is deterministic and a ranking cannot change
  because a test ran in a different month.

The §11 “used `janus search` instead of `rg` for a day” criterion is **kept
verbatim** as a manual sign-off item in §7, because it is the only criterion that
tests whether the tool is pleasant, and no proxy measures that.

## 4. Decisions

| # | Question | Default | Where it lands |
|---|---|---|---|
| S-1 | Tokenization rule | Lowercase (Unicode-aware), keep alphanumerics and `_`, everything else separates. Digits kept (`blake3`, `m1-scan-parse` → `m1`, `scan`, `parse`). | `src/tokenize.rs` |
| S-2 | Stemming | **Off**; `Stemmer::None` only. Both sides are stemmed when it is on, so exact identifiers still match themselves; the real costs are precision collisions (`general`/`generate` → `gener`), mangled non-English text, nothing for CJK, and unreadable terms when debugging a ranking. The eval decides, not taste. | `src/tokenize.rs`, §3.8 |
| S-2a | Enabling it later | Config flag (`[search] stemmer = "porter"`), which changes the tokenizer fingerprint ⇒ full re-tokenize ⇒ rebuild. Never enabled silently, never in a patch release. | `src/config.rs`, `src/index/store.rs` |
| S-3 | Indexed text | `Chunk::embed_text(body, title)` — title + breadcrumb + chunk text — then tokenized by S-1. This is why reuse cannot key on `ChunkId` (§3.4). | `src/index/mod.rs` |
| S-4 | BM25 parameters | `k1 = 1.2`, `b = 0.75`, `idf = ln(1 + (N − df + 0.5)/(df + 0.5))` (always positive), `f64`. | `src/index/lexical.rs` |
| S-5 | Normalization | Sigmoid, defined concretely and made scale-free: `lexical = s / (s + s_ref)` with `s_ref = Σ idf(t)` over query terms (§3.3, numeric table). Constant-free because `idf` grows with `ln N`; a fixed midpoint would saturate on a large vault and barely move on the fixture vault. Short-circuit `lexical(0) = 0`. | `src/index/lexical.rs` |
| S-6 | Index location and format | `.janus/index.bin` + `.janus/manifest.json`, `format_version = 1` — M3 is the first writer, so `1` is its number (§6.8). | `src/index/store.rs` |
| S-7 | Cross-process coordination | `.janus/lock`, exclusive for writers, 10 s timeout, **held across the whole publish**; readers never lock (§3.5). Replaces the first draft’s “no lock in M3”, which review finding 1 correctly rejected: one command surface is not one writer. | `src/index/store.rs` |
| S-8 | Staleness | `search`/`facts` auto-build when the index is missing and auto-rebuild when the manifest disagrees with config, root, fingerprints, or `format_version`; one stderr line says which (`stale: tokenizer v1 != v2`). `--no-rebuild` refuses instead (exit 1, nothing written), for callers that want a read to be a read. | `src/index/store.rs`, `src/search/mod.rs` |
| S-9 | Reuse key | Per document, on the **indexed input**: `lex_key = blake3(tokenizer_fp ‖ chunker_fp ‖ effective_title)` plus `content_hash` and the chunk-id set (§3.4). Tokens only are reused; metadata and spans are always rebuilt. Postings are per occurrence, never keyed by `ChunkId`. | `src/index/build.rs` |
| S-10 | Intents | All four, with the explicit per-intent table in §3.2 (inclusion, factor, decay). `exhaustive` is filled in from §7.1 and flagged as a SPEC gap. | `src/score.rs` |
| S-11 | Scope filter | §6.12 rules 3, 4, 9: default union; repeated `--scope` unions; `--scope-only` excludes `global` and shared-only; normalization `ORG:Acme` → `org:acme`. Rules 6–8 (scope-relative windows) wait for M5 with a `todo` in the module docs. | `src/score.rs` |
| S-12 | `janus facts` | Explicit inclusion predicate (§3.6): type ∈ {decision, spec, principle} **and** declared status `active`, no decay, factor 1.0, authority-ordered. Not the `current` gate. | `src/search/facts.rs` |
| S-13 | `Clock` | `src/clock.rs`; trait + `SystemClock` + public `FixedClock`. Nothing else in the crate reads the wall clock. Eval and retrieval tests use `FixedClock(2025-04-01)`. | `src/clock.rs` |
| S-14 | Determinism | No float `==`; total order via `total_cmp` on `(final, date desc, chunk_id)`; no `HashMap` iteration reaches output. | `src/score.rs`, `src/search/mod.rs` |
| S-15 | Eval harness | `tests/eval_quality.rs` is `#[ignore]`’d (not part of `cargo test --all-targets`); `scripts/eval.sh` runs it; CI job `eval` is `continue-on-error: true` and publishes both numbers. `active top-1` denominator = `intent: current` queries expecting an active document (§3.8). | `tests/eval_quality.rs`, `scripts/eval.sh`, `.github/workflows/ci.yml` |
| S-16 | Result contract | One hit per document at its argmax chunk; sort documents; **then** limit (§3.1). No per-document multi-chunk display, no MMR. `--limit` default 10. | `src/search/mod.rs` |
| S-17 | Manifest as a generation | `index_sha` is the generation id; manifest is written last, from a unique temp file, with a directory `fsync`; readers validate bytes against it and retry (§3.5). | `src/index/store.rs` |
| S-18 | Load-time validation | Every id, range and offset read from disk is checked before use; a violation is corruption (rebuild), never a panic (§3.5). | `src/index/store.rs` |

## 5. Phases

Six phases instead of five: review finding 1 moved cross-process publication out of
“persistence” into its own phase, because a lock, a unique-temp publish protocol, a
reader retry loop and their deterministic tests are a reviewable unit that should not
share a PR with `bincode` round-trips. Budgets in §8 are reassessed accordingly.

### Phase 1 — Tokenizer (½ day)

**Goal:** `text → Tokens`, total, allocation-reusing, and the single definition for
documents and queries.

**Files:** `src/tokenize.rs` (new), `src/lib.rs`.

**Build (~150 logic lines):** `Tokens` (owned `folded` + `Vec<Range<u32>>`), and one
`char_indices` scan that lowercases into `folded` with `char::to_lowercase`
(multi-char expansions push both chars), closes a range on any
non-alphanumeric/non-`_` char, drops ranges over `MAX_TERM_BYTES = 64` and counts
them, plus `tokenize_into` for buffer reuse. `Tokenizer::fingerprint()` returns
`TokenizerFingerprint { algorithm: "v1", stemmer: Stemmer::None }` — a `Copy` value
that the manifest stores and the reuse key hashes (§3.4). Known limitation recorded
in the module docs: a CJK run is one term, because a segmenter is not this milestone.

**Tests (~200 lines):** splitting table (`JANUS_WRITE`, `m1-scan-parse`,
`v2.0`-style punctuation, `café`, `İ` → `i̇`), CJK-as-one-term pinned as a limitation,
emoji dropped, empty/whitespace input, 64-byte drop, `tokenize_into` capacity
stability (§3.7), a doc-test proving the borrow shape needs no lifetime annotation,
and a `proptest` that terms are always in-bounds slices of `folded`.

**Handoff (paste):** “Implement Phase 1 of M3 per `docs/m3-breakdown.md` §3.7 and
Phase 1. `Tokens` owns the folded text and lends `&str`; no `unsafe`, no lifetime
parameter, no trait. `Tokenizer::tokenize` takes `&self` and `&str` and returns an
owned `Tokens`; `tokenize_into` reuses it. Documents and queries must share this one
function — a test asserts it. Decisions to the phase log, not to a new journal.”

**Accept:** `cargo test` green; the doc-test compiles as written; a 1 MB body
tokenizes in < 5 ms (debug measurement, not a test assertion).

---

### Phase 2 — Inverted index + BM25 (1–1½ day)

**Goal:** `LexicalIndex::build(chunks) -> LexicalIndex` and
`LexicalIndex::score(&terms) -> (Vec<f64>, f64)` where the second value is `s_ref`
(§3.3), in memory, no I/O.

**Files:** `src/index/mod.rs` (new), `src/index/lexical.rs` (new), `src/lib.rs`.

**Build (~280 logic lines):** intern terms into a `StringPool` during build;
flat `postings: Vec<Posting>` with a `TermMeta.postings` range per term (the layout
the snapshot reuses, so phase 4 is a derive and not a redesign); `df`, `avgdl`, and
an `idf()` accessor computed once at build/load with the always-positive form;
`score_query(&[&str]) -> Vec<f64>` over all chunks plus the `s_ref` sum; the squash
from §3.3.

**Tests (~360 lines):** reviewed for three specific defects.

1. **Exact expected IDF values**, not an idf computed by the code under test:

   | N | df | `idf` (6 dp) |
   |---|---|---|
   | 3 | 1 | 0.980829 |
   | 3 | 2 | 0.470004 |
   | 3 | 3 | 0.133531 |
   | 40 | 1 | 3.308107 |
   | 40 | 10 | 1.362197 |

   Assertions are `(actual − expected).abs() < 1e-6` against these literals, and
   `idf > 0` for `df == N` (the always-positive form’s whole point).

2. **Document TF saturation vs query-term repetition are different tests**, because
   they behave differently and a single “repetition” test would hide the difference:

   | Effect | Expectation | Test |
   |---|---|---|
   | Document `tf` grows (1 → 3 → 10, `dl = avgdl`) | saturating: `1.0000, 1.5714, 1.9643` × `idf`, bounded by `k1 + 1 = 2.2` | `document_tf_saturates_toward_k1_plus_one` |
   | Query repeats a term (`cache` vs `cache cache`) | **linear and exact**: `2 ×` the single-term score for that document, because each occurrence contributes `bm25(t, c)` again | `query_term_repetition_is_linear_unlike_tf` |

3. **Monotonicity properties, with their preconditions stated** — the property is
   false without them, which is why the first draft’s “monotone in term frequency”
   was wrong as stated:

   - Fix `(N, df, avgdl)` and `dl = avgdl`: BM25 is strictly increasing in `tf`,
     bounded above by `idf · (k1 + 1)`.
   - Fix `(N, tf, avgdl)`: BM25 is strictly decreasing in `dl` for `b > 0`.
   - Fix `(N, tf, dl)`: BM25 is strictly decreasing in `avgdl` for `b > 0`.
   - Fix `(N, dl, tf)`: `idf` is strictly decreasing in `df`.
   - **Not** claimed: monotone in `tf` across documents of different length — a long
     document with high `tf` may score below a short one with low `tf`, and that is
     correct.
   - `proptest` generates `(N, df, avgdl, dl)` and varies one axis at a time; a
     second property asserts no `NaN`/`inf` and every returned index in range.

Additional tests: a hand-computed three-document fixture (postings, `df`, `avgdl`,
final BM25 to 6 dp), a term in every document (`idf` small but positive, not zero),
prefix terms not substring-matching, empty query/index, and the §3.3 squash table
(`r` → `lexical`) as literals.

**Handoff (paste):** “Implement Phase 2 of M3 per `docs/m3-breakdown.md` §3.3 and
Phase 2. Flat postings + range table, `f64`, no I/O. Use the always-positive idf, and
assert **literal** expected idf values (0.980829, 0.470004, 0.133531 at N=3). Keep
document-TF saturation and query-term repetition as separate tests — one saturates,
the other is exactly linear — and state the preconditions for every monotonicity
property (fix N, df, avgdl, dl).”

**Accept:** tests green; building over the fixture vault (≈40 docs) < 10 ms debug.

---

### Phase 3 — Scoring: authority, gates, intents, scope (1 day)

**Goal:** the §3.2 contract as a pure function, plus the facts predicate as a
predicate (used by phase 6).

**Files:** `src/score.rs` (new), `src/clock.rs` (new), `src/config.rs` (docs: the
authority/decay tables from M1 are consumed here), `src/lib.rs`.

**Build (~300 logic lines):** `ScoreInput`/`Breakdown`; type authority from config
with the 0.80 fallback and the existing `unknown_type` note; the intent table of
§3.2 as data (a `const` table, so adding a row is one line and a test can iterate
it); decay with the five `1.0` short-circuits; `relevance`; `final`; the scope
matcher (rules 3, 4, 9) as a pure predicate over `DocMeta`; `is_fact(doc)`
(type ∈ set ∧ declared status active, §3.6); the document comparator (§3.1).

**Tests (~420 lines):** one table-driven test iterating every `(intent, status)`
cell of §3.2 and asserting inclusion, factor and decay — the table is the test
input, so a missing cell fails. Explicit numeric assertions: `current` + `draft` is
exactly `0.7 ×` the same document as `active` (`assert!((a/b - 0.7).abs() < 1e-12)`);
`unknown` is `0.8 ×`; `deprecated`/`superseded` absent under `current`, present
under `history`; `history`/`exhaustive` return factor `1.0` and `decay == 1.0` for
every status; `evolution` excludes `draft` and orders oldest→newest. Decay
monotonicity (`age ↑ ⇒ decay ↓`), the five `1.0` cases, future dates, missing dates.
Scope obligations 5, 8, 9 from §6.12.1. Facts predicate table (all 5 statuses × 3
types + one non-fact type). Ties deterministic across runs. `proptest`:
`0 ≤ final ≤ 1.5 · 1.0 · 1.0`. Every test injects `FixedClock`.

**Handoff (paste):** “Implement Phase 3 of M3 per `docs/m3-breakdown.md` §3.2, §3.6
and Phase 3. Read SPEC §6.6, §6.12, §7.1, §7.2 first. `authority` in `Breakdown` is
the **product** of type and status; report the factors separately. Encode the
per-intent table as data and iterate it in the test, so a missing status fails.
`janus facts` is an inclusion predicate (`active` only), not the `current` gate. No
wall clock — `FixedClock` in every test.”

**Accept:** every §7.2 obligation and §6.12.1 obligation 5/8/9 present and green;
clippy clean.

---

### Phase 4 — Snapshot + incremental rebuild (1 day)

**Goal:** the snapshot round-trips and a second build is cheap — **tokens only**,
keyed on the indexed input (§3.4). Publication is phase 5.

**Files:** `src/index/store.rs` (new), `src/index/build.rs` (new),
`src/index/mod.rs`, `Cargo.toml` (`bincode`).

**Build (~300 logic lines):** `Snapshot` ⇄ `LexicalIndex`; `lex_key` computation;
the six acceptance cases of §3.4 as build logic (per-document reuse, forward-index
reconstruction from the old postings in one pass, tokenize the delta, re-invert);
`Manifest` assembly including `index_sha` and both fingerprints; load-time validation
(§3.5) with a `Corrupt { reason }` result that is a rebuild, not a panic;
`BuildStats { retokenized_chunks, reused_documents, rebuilt_documents }` surfaced in
the phase-6 receipt; `--dry-run` support in the API (report, write nothing).

**Tests (~330 lines):** round-trip equality of scores; the six §3.4 acceptances
(incremental == clean rebuild after title-only edit, derived-title rename, identical
bodies/different titles, pure move with `retokenized_chunks == 0`, body edit,
fingerprint bump); validation rejects crafted invalid snapshots (bad `StringId`, bad
postings range, `chunk_range` out of order, non-finite `avgdl`) without panicking;
`format_version` mismatch is a rebuild; a moved vault keeps every `DocId`/`ChunkId`
and only changes `built_root`; `proptest` `Snapshot → bytes → Snapshot` identity.

**Handoff (paste):** “Implement Phase 4 of M3 per `docs/m3-breakdown.md` §3.4, §3.5
(load validation only) and Phase 4. Reuse is keyed on `lex_key` (title + fingerprints
+ content hash + chunk-id set), never on `ChunkId` alone — `ChunkId` excludes the
title. Postings are per chunk occurrence. Every invalid value read from disk is a
rebuild, never a panic. No `--rebuild` publication logic here; that is phase 5.”

**Accept:** the six §3.4 acceptances pass; `.janus/manifest.json` is readable JSON.

---

### Phase 5 — Publication, locking, reader retry (½–¾ day)

**Goal:** §3.5 in code: exclusive lock, unique temp files, manifest last, readers
validating and retrying.

**Files:** `src/index/store.rs` (publication half), `src/error.rs`
(`LockTimeout`, `IndexUnstable`), `Cargo.toml` (`fd-lock`, `tempfile`),
`src/index/build.rs` (the hook seam).

**Build (~200 logic lines):** `LockFile` guard with the 10 s timeout; the six-stage
publish; the double-checked freshness re-test under the lock; stale temp sweep;
`publish_with_hook` (doc-hidden) for deterministic interleaving; the reader’s
read→validate→retry loop with 10/50/200 ms backoff and the one rebuild attempt;
two new error variants with exit code 1 and messages that name `.janus/lock` /
`.janus/index.bin`.

**Tests (~280 lines):** the six §3.5 acceptances, all deterministic: reader at every
publication stage via the hook; two concurrent auto-builds (threads, counter
assertions, no sleeps); lock timeout; injected mid-publish failure leaving the
previous generation loadable; corrupt/truncated/zeroed bytes; 8-reader storm during
publish with zero errors. A test asserts the publish order by recording hook stages
and checking `index.bin` is visible before `manifest.json` is replaced.

**Handoff (paste):** “Implement Phase 5 of M3 per `docs/m3-breakdown.md` §3.5 and
Phase 5. `.janus/lock` is exclusive for writers with a 10 s timeout and held across
the entire publish; readers never lock. Unique temp file per writer, `index.bin`
renamed before `manifest.json`, directory `fsync` after each. Reader validates bytes
against `manifest.index_sha` and retries (10/50/200 ms), then rebuilds once, then
`IndexUnstable`. Tests must interleave through `publish_with_hook`, not `sleep`.”

**Accept:** all six pass; `cargo test` never sleeps; two processes cannot produce a
mixed generation (asserted by the hook test).

---

### Phase 6 — `janus index`, `janus search`, `janus facts`, eval (1–1½ day)

**Goal:** the milestone’s surface, the §3.1 result contract end to end, and the
quality report.

**Files:** `src/index/cmd.rs`, `src/search/mod.rs`, `src/search/facts.rs`,
`src/cli.rs`, `src/main.rs`, `tests/cli_search.rs`, `tests/golden_search.rs`,
`tests/eval/queries.jsonl`, `tests/eval_quality.rs`, `scripts/eval.sh`,
`.github/workflows/ci.yml`.

**Build (~320 logic lines):** `janus index` (build/refresh/`--dry-run`, one stderr
line, `--json` receipt `{api, docs, chunks, terms, rebuilt, reused, retokenized,
taken_ms}`); `janus search` (load-or-auto-build → tokenize → BM25 → score → **group
to documents → sort → limit** → render per §3.1); `janus facts` (§3.6); human table
(`score  authority  status  date  path  breadcrumb`), `--json` per §9.4 with the full
`Breakdown` per hit so a ranking is explainable without reading code; `--limit`
documents, not chunks.

**Tests (~380 lines):** `index → search → search --json` on the fixture vault;
the dominant-multi-chunk-document fixture from §3.1 (`--limit 10` → 10 distinct
documents); `--tags` and `--types` through the CLI **after a reload** (second
process), which is the review-finding-2 metadata case; `--scope`/`--scope-only` end
to end; `--intent history` returns a `superseded` document that `current` omits;
`facts` excludes draft/unknown/deprecated/superseded while `search --intent current`
includes the first two (pinned against each other); `--json` never contains body
text; stale index rebuilds with one stderr line and exit 0; `--no-rebuild` exits 1
and writes nothing; empty corpus; golden `insta` over `search --json` redacting only
the corpus root; determinism across two runs.

**Eval:** `tests/eval/queries.jsonl` ≥30 queries with `intent` and graded
expectations, five of them built from M2’s awkward fixtures (CRLF, BOM, the
`---`-body spec, quoted scalars, anchors). `scripts/eval.sh` runs the ignored test and
prints both numbers with denominators; CI job `eval` is `continue-on-error`, per
§3.8. No timing assertion.

**Handoff (paste):** “Implement Phase 6 of M3 per `docs/m3-breakdown.md` §3.1, §3.6,
§3.8 and Phase 6. One hit per document at its argmax chunk, sort then limit —
`--limit N` returns N *documents*. `--tags`/`--types` filter documents after reload,
from `DocMeta.tags`/`type_name`. `facts` is the active-only predicate. The eval test
is `#[ignore]`’d and CI runs it non-blocking with `FixedClock`: do not put a quality
floor inside `cargo test`. Reuse `src/scan.rs` and `src/chunk/dump.rs` for the report
shape.”

**Accept:** `janus index && janus search cache --json | jq .` works on the fixture
vault; `scripts/eval.sh` prints `active-top-1 0.xx (n/m)`; exit codes match §9.1.

## 6. Cross-cutting requirements

- **Determinism.** No `HashMap` iteration in output; no float `==`; total comparator;
  injected `Clock`; two runs byte-identical.
- **No content without a flag** (§8.5): `search --json` returns spans, paths and
  breadcrumbs, never chunk text.
- **No panics** (§10, D12): no `unwrap`/`expect` in lib code; every value read from
  disk validated; indexing everywhere.
- **Fail open on reads, fail closed on refusal.** Corrupt or stale → rebuild; a
  `--no-rebuild` read refuses; a failed publish leaves the previous generation.
- **Cross-process safety** (§3.5): `.janus/lock` for writers, lock-free readers with
  generation validation and bounded retry.
- **Cross-platform** (§8.7): `rel_path` `/`-separated; `built_root` via `dunce`;
  directory `fsync` `#[cfg]`-guarded; `fd-lock` must build on Windows.
- **Windows stays possible:** `cargo check --target aarch64-pc-windows-msvc
  --all-targets --features blake3/pure` must pass, `fd-lock`/`tempfile` included.

## 7. Definition of done, made checkable

Blocking (CI / `cargo test`):

1. `janus index` then `janus search <term>` returns the right document first, human
   and JSON, on the fixture vault.
2. A second `janus index` re-tokenizes zero chunks; a single-document edit
   re-tokenizes only that document (§3.4 acceptances 1–6, incremental == clean).
3. A moved vault keeps every `DocId`/`ChunkId`; only `built_root` changes, with a
   warning, never an error.
4. Corrupt, truncated, `format_version`-mismatched or hand-edited `index.bin`
   rebuilds with a warning and never panics (§3.5 acceptance 5, §3.5 validation).
5. Two concurrent builders yield one build and two identical result sets; a reader
   interleaved at every publication stage sees a validated generation
   (§3.5 acceptances 1–4, 6).
6. `--limit N` returns N distinct documents; a dominant multi-chunk document appears
   once (§3.1).
7. `janus facts` excludes `draft`/`unknown`/`deprecated`/`superseded` while
   `search --intent current` still includes `draft` and `unknown` (§3.6).
8. Every §7.2 obligation, every §6.12.1 obligation M3 can answer, and every row of
   the §3.2 intent table has a test.
9. Gates: fmt, clippy `-D warnings`, `cargo test --all-targets`, `cargo doc`
   (0 warnings), Windows `cargo check`, `scripts/pr-size.sh`.

Milestone acceptance (manual, recorded in the PR that changes ranking):

10. `scripts/eval.sh` reports **active top-1 ≥ 0.80** over ≥ 30 queries, with the
    denominator printed and eligible-query rule from §3.8 applied.
11. **“I used `janus search` instead of `rg` for a day.”** Signed off in the PR
    description when phase 6 merges; if it fails, the follow-up is a UX change, not a
    lower threshold.

Not gating: retrieval numbers in CI (`continue-on-error`), performance numbers
(printed, never asserted; `criterion` at M8).

## 8. PR plan and size budget

One phase per PR, one branch from `main` after the previous merge. Budgets are
reassessed after review: the review added a phase (publication) and ~80 logic lines
of coordination, and the test budget grows where contracts became acceptance cases.

| PR | Branch | Logic estimate | Tests estimate | Why it splits here |
|---|---|---|---|---|
| P1 | `m3-tokenize` | ~160 | ~200 | The shared definition both sides of a query depend on; owns an explicit ownership contract. |
| P2 | `m3-lexical` | ~280 | ~360 | BM25: dense numerics, literal expected values, separated saturation semantics. |
| P3 | `m3-score` | ~300 | ~420 | Policy layer where a mistake is invisible in the output; table-driven by construction. |
| P4 | `m3-index-store` | ~300 | ~330 | Snapshot format + reuse identity; owns `format_version` and the incremental contract. |
| P5 | `m3-publish` | ~200 | ~280 | Cross-process publication and reader retry — its own PR so review finding 1 gets a focused diff. |
| P6 | `m3-search-cli` | ~320 | ~380 | Command surface + result contract + eval report. If it approaches 450 logic, the eval harness ships as `m3-eval`. |

Total ≈ 1 560 logic lines across six PRs, every one far under the 600 ceiling.

## 9. Review resolutions and remaining SPEC gaps

Seven findings from review, resolved as follows.

| # | Finding (short) | Resolution | Where |
|---|---|---|---|
| 1 | Concurrent publication is unprotected | `.janus/lock` exclusive for writers held across the publish; unique temp files instead of a shared `.new`; index renamed before manifest; generation identity `(format_version, index_sha)`; readers validate and retry with bounded backoff, rebuild once, then `IndexUnstable`; double-checked freshness under the lock; stale temp sweep. New phase 5 with six deterministic acceptance cases (publication hook, no `sleep`). | §3.5, S-7, S-17, P5 |
| 2 | Reuse keyed on `ChunkId` misses the title | Split identities: `ChunkId` = content (unchanged, excludes title), `lex_key` = indexed input (tokenizer fp ‖ chunker fp ‖ effective title). Reuse is per document, five conjuncts, tokens only; metadata/spans always rebuilt; postings per occurrence, never keyed by `ChunkId`; six acceptances incl. identical bodies/different titles and a pure move with `retokenized_chunks == 0`. | §3.4, S-3, S-9, P4 |
| 3 | Facts reuse the `current` gate | Explicit inclusion predicate: type ∈ {decision, spec, principle} ∧ declared status `active`; no factor, no decay; table test over 5 statuses × 3 types plus a CLI test that pins the difference against `search --intent current`. | §3.6, S-12, P3/P6 |
| 4 | Three chunks per document and limit-before-dedupe | One hit per document at the argmax chunk; group → sort → limit; multi-chunk display and MMR removed from the default path; dominant-multi-chunk fixture asserting N distinct documents for `--limit N`. | §3.1, S-16, P6 |
| 5 | `status_factor` computed but not applied; “gate in history” wrong | Full equations with `authority = type_authority · status_factor`; `Breakdown` reports all three; the per-intent table states inclusion, factor and decay, including `history`/`exhaustive` at factor 1.0 and no decay; cells asserted numerically (`0.7×`, `0.8×`). | §3.2, S-10, P3 |
| 6 | `s/(s+4)` silently replaced SPEC’s sigmoid | Kept the sigmoid and defined it scale-free: `lexical = s/(s+s_ref)`, `s_ref = Σ idf(t)`; numeric tables for `r → lexical` and a worked N=40/tf example; rationale is that `idf` grows with `ln N`, so a fixed midpoint is corpus-dependent. | §3.3, S-5, P2 |
| 7 | `≥0.80` assertion inside `cargo test`; wall-clock assertion; §11 criterion replaced | Three homes: correctness gates in `cargo test`; quality as an `#[ignore]`d harness + non-blocking CI job publishing both numbers; milestone acceptance as a manual step that keeps §11’s wording verbatim; timing printed, never asserted; `active top-1` denominator defined (current-intent queries expecting an active document); `FixedClock` in all retrieval/eval tests. | §3.8, S-15, §7, P6 |

Also fixed in this revision, from the same review pass: the tokenizer ownership
contract (§3.7), `--tags` support after reload via `DocMeta.tags` (§2, P4/P6), and
the BM25 test defects — literal expected idf values, document-TF saturation vs
query-term repetition as separate tests, and monotonicity preconditions (§5 P2).

### Remaining SPEC conflicts and gaps

None of these is resolved by editing `SPEC.md` in this PR; each is either a
clarification the plan makes and §15 can ratify later, or a gap the next SPEC edit
should close. They are listed so nothing is silently overridden.

1. **§6.6 omits `exhaustive`.** The status table has `current`/`history`/`evolution`
   columns; §7.1 defines a fourth intent. §3.2 fills it in as “all statuses included,
   factor 1.0, no decay”, which follows §7.1’s wording. **Proposed SPEC edit:** add
   the `exhaustive` column to §6.6’s table.
2. **§7.2 says “sigmoid squash” with no parameters or scaling.** §3.3 defines it and
   makes it query-relative. This is a clarification of a symbolic formula, not a
   change of family, but the SPEC should name the form. **Proposed SPEC edit:** one
   line under §7.2’s formula.
3. **§8.3 declares `.janus/lock` without a scope.** It does not say who takes it,
   whether readers do, or what a reader does mid-publish. §3.5 defines it (writers
   exclusive, readers lock-free + validate/retry, manifest last). **Proposed SPEC
   edit:** two sentences in §8.3, and the rebuild-trigger list gains “or a generation
   mismatch observed by a reader”.
4. **§9.2’s “Active … only” vs §6.6’s `current` gate.** These are not in conflict
   once `facts` has its own predicate (§3.6), but §6.6’s phrase “status gate (before
   scoring)” invites reuse. **Proposed SPEC edit:** one sentence in §9.2 saying
   `facts` uses an active-only inclusion rule, not the `current` gate.
5. **§6.13/§7.2’s `--as-of` vs M3 scope.** Intentional and already documented: M3
   has no validity windows, so the flag is not exposed. No conflict, but M5 must add
   it together with `effective_status`, and `facts` then switches from declared to
   effective status (§3.6) — a behaviour change to record there.
6. **§10’s “no wall-clock time in tests” vs a performance DoD.** The plan prints
   timings and asserts none; the 200 ms warm-search figure lives in the manual
   acceptance checklist, not in a test. No conflict after this revision.

## 10. Implementation log — Phase 1 (tokenizer)

## 11. Implementation log — Phase 2 (BM25)

## 12. Implementation log — Phase 3 (scoring)

## 13. Implementation log — Phase 4 (snapshot + incremental)

## 14. Implementation log — Phase 5 (publication)

## 15. Implementation log — Phase 6 (CLI + eval)

## 16. Handoff to M4

<!-- What M4 inherits: snapshot format and load path, the alpha seam, where
     vectors attach, the fingerprints that force rebuilds, what M3 left out. -->
