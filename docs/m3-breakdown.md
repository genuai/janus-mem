# M3 — Lexical search, daily usable: phased breakdown

> Branch: `m3-tokenize` (phase 1), then one branch per phase from `main`.
> Spec: `SPEC.md` §6.5, §6.6, §6.6.1, §6.12 (rules 1–5, 9), §6.12.1 (obligations 5,
> 8, 9), §7.1 (intents), §7.2 (scoring), §8.3 (index format), §8.3.1 (RAM),
> §9.1, §9.2 (`search`, `get_authoritative_facts`), §9.4 (envelope), §10 (eval,
> property, benchmark rows), §11 (M3), §13, §14.
>
> Milestone statement (SPEC §11): “Hand-written BM25 (inverted index, tokenizer,
> optional stemmer); scoring module with authority, status gates, intents, and the
> scope filter (`--scope`, `--scope-only`, default union); `janus search` and
> `janus facts` with human + JSON output.”
>
> **Done when:** the author uses `janus search` instead of `rg` for a day, and the
> eval set’s “active top-1” rate is ≥80% (lexical only). Restated measurably in §6,
> because “uses it for a day” is a wish, not a gate.
>
> **Learn targets (SPEC §11):** `HashMap`/`BTreeMap` and when each is right,
> `Eq`/`Hash` derives, `sort_by` with a multi-key comparator, `&mut self`
> accumulation, float comparison pitfalls, newtypes over stringly-typed ids.

M2 is merged (`25b995a`). M3 inherits: `Chunk` with byte spans and content-addressed
ids, `Chunk::embed_text` (breadcrumb + text, the string the index should see),
`Document` with resolved type/status/scope/date, `DocNote` for irregularities, the
`scan`/`chunks` pipeline seam, and the `{code, message}` DTO pair. It inherits no
index, no clock abstraction, and no scoring.

## 0. Scope

### In scope
- A Unicode-aware tokenizer producing normalized terms (`src/tokenize.rs`).
- An in-memory inverted index with BM25 (`src/index/lexical.rs`): postings, document
  frequencies, lengths, phrase-free scoring — the ~200 lines SPEC §7.2 predicted.
- A pure scoring module (`src/score.rs`): authority × status factor × decay, the four
  intents, the scope filter, candidate-set scoping, deterministic tie-breaks, with
  `Clock` injected (§7.2 lists exactly which tests must exist).
- `index.bin` + `manifest.json` under `.janus/`, `format_version = 1`, blake3
  checksums, atomic replace, and incremental rebuild by `content_hash` (§8.3).
- `janus index [PATHS...] [--rebuild] [--dry-run]` — build/refresh the index.
- `janus search <QUERY> [--intent ...] [--scope ...] [--scope-only] [--limit N]
  [--types T,..] [--tags T,..] [--decay-half-life DAYS] [--include-superseded]
  [--json]` and `janus facts [--tags ...] [--query Q] [--scope S]... [--json]`.
- The retrieval eval harness: `tests/eval/queries.jsonl`, ≥30 hand-labelled queries,
  reporting nDCG@10 and “active top-1” as a non-blocking CI report (§10).

### Explicitly NOT in scope (do not build)
- Embeddings, `Embedder`, `vectors.f32`, `α`, hybrid scoring (M4). `semantic` is
  fixed at `0.0` and `relevance = lexical`; the scorer keeps the shape so M4 is an
  addition, not a rewrite.
- `mmap`, `madvise`, quantization, `watch`, `criterion` benches (M8) — with one
  exception: phase 5 adds a cheap wall-clock assertion, not a benchmark.
- MCP and `janus rpc` (M6). CLI only.
- Any write to the vault. `janus index` writes `.janus/` only, never a document.
- Supersession, transitive lineage, `--as-of`, validity windows, `effective_status`,
  `doctor`, `lineage`, `moves`, `path_history` (M5). M3 gates on **declared**
  `status` only, and says so in its own docs; `--as-of` is not exposed until M5 rather
  than exposed-and-wrong.
- Multi-corpus queries, `--corpus`, archived weighting (M5/M6).
- The Porter stemmer. It is specified as *optional* (§11); see S-2 for why the
  default is off and what the hook looks like.
- `janus grep` (rejected in §15 open questions: `rg` exists).

## 1. Dependency changes (`Cargo.toml`)

| Crate | Version | Why (one-line justification per §8.2 policy) |
|---|---|---|
| `bincode` | 1.3 (serde-based; verify at implementation time — 2.x drops serde for `Encode`/`Decode`) | `index.bin` is a `bincode` sidecar by decision D13; hand-rolling a binary format for postings would be a second, worse format. |

No new crates for tokenization, stemming (off), stopwords (none), or file locking
(deferred to M7, see S-6). `serde` and `blake3` are already present.

## 2. Data model (write these first — §12.3.3)

```rust
// src/index/mod.rs — the persisted shape (§8.3), lexical-only in M3.
pub const FORMAT_VERSION: u16 = 1;

/// `.janus/manifest.json` — small, human-inspectable, checksums the binary.
pub struct Manifest {
    pub format_version: u16,
    pub schema_version: u16,
    pub corpus_id: String,
    pub built_root: String,          // canonical root, diagnostic + relocation check
    pub built_at: String,            // RFC 3339, UTC
    pub docs: usize,
    pub chunks: usize,
    pub terms: usize,
    pub index_sha: String,           // blake3 of index.bin
    pub chunking: ChunkingFingerprint, // max_chars + overlap: a change rebuilds
    pub embedder: Option<String>,    // None in M3; Some in M4 (§8.3)
}

/// `index.bin`, bincode-serialized. Not `Index` yet: no vectors, no edges, no
/// path_history (§8.3 declares those; M3 fills the ones it can and leaves the
/// rest for M4/M5 rather than writing empty placeholders the reader must trust).
pub struct Snapshot {
    pub format_version: u16,
    pub corpus_id: String,
    pub strings: StringPool,
    pub docs: Vec<DocMeta>,
    pub chunks: Vec<ChunkMeta>,
    pub terms: Vec<TermMeta>,        // term text lives in the pool; this is df
    pub postings: Vec<Posting>,      // (chunk_index, term_freq), grouped by term
}

pub struct DocMeta {
    pub id: DocId,
    pub rel_path: StringId,
    pub title: StringId,
    pub type_name: StringId,
    pub status: u8,                  // Status as a compact code
    pub scope: StringId,
    pub shared_with: Vec<StringId>,
    pub date: Option<NaiveDate>,     // effective_from falls back to date (§7.2)
    pub effective_from: Option<NaiveDate>,
    pub timeless: bool,
    pub content_hash: Blake3Hash,    // freshness + incremental skip
    pub chunk_range: (u32, u32),     // into `chunks`
}

pub struct ChunkMeta {
    pub id: ChunkId,
    pub doc: u32,                    // index into `docs`
    pub span: (u32, u32),
    pub heading_path: Vec<StringId>,
    pub tokens: u32,                 // length norm for BM25
}
```

```rust
// src/tokenize.rs
pub enum Stemmer { None }            // one variant now, a second in M8 if ever

pub struct Tokenizer { /* options from config */ }
impl Tokenizer {
    pub fn terms<'a>(&'a self, text: &'a str) -> impl Iterator<Item = &'a str>;
}
```

```rust
// src/score.rs — pure, clock-injected (§7.2's hard requirement)
pub trait Clock { fn today(&self) -> NaiveDate; }
pub struct FixedClock(pub NaiveDate);
pub struct SystemClock;

pub struct Params {                  // everything score() needs, resolved once
    pub authority: f64,              // AUTHORITY[type]
    pub status_factor: f64,
    pub decay: f64,
    pub alpha: f64,                  // 0.0 in M3; 0.65 in M4
}

pub struct Breakdown {               // what the JSON reports, field for field
    pub semantic: f64,               // always 0.0 in M3
    pub lexical: f64,
    pub relevance: f64,
    pub authority: f64,
    pub decay: f64,
    pub final_score: f64,
}
```

## 3. Decisions to settle (defaults chosen)

| # | Question | Default | Where it lands |
|---|---|---|---|
| S-1 | Tokenization rule | Lowercase (Unicode-aware `char::to_lowercase`), keep `[alphanumeric \| '_']`, everything else is a separator. Digits kept, so `blake3`, `m1-scan-parse` → `m1`, `scan`, `parse`, and `JANUS_WRITE` all match case-insensitively. | `src/tokenize.rs` |
| S-2 | Stemming | **Off**, and the API has no stemmer parameter yet — only the `Stemmer` enum with one variant, so adding `Porter` later changes no call site. The trade is real both ways: stemming buys recall (`cache`/`caching`/`cached` collapse) and both query and document are stemmed, so an exact identifier still matches itself. What it costs is **precision** — unrelated words collide (`general`/`generate` → `gener`), it mangles non-English text (the fixture vault has German), it does nothing for CJK, and it makes the term list unreadable when debugging a ranking. §7.2 says the lexical layer exists for exact terms, so the default is off and the eval decides whether to flip it. | `src/tokenize.rs`, eval row |
| S-2a | If stemming is ever enabled | It is a config flag (`[search] stemmer = "porter"`), not a code change, and it re-tokenizes the index ⇒ fingerprint change ⇒ rebuild. Never enable it silently, and never in a patch release. | `src/config.rs`, `src/index/store.rs` |
| S-3 | What text is indexed | `Chunk::embed_text(body, title)` — breadcrumb **plus** chunk text, lowercased and tokenized. | `src/index/lexical.rs` |
| S-4 | BM25 parameters | `k1 = 1.2`, `b = 0.75`, `idf = ln(1 + (N − df + 0.5)/(df + 0.5))` (the always-positive form), `f64` throughout. | `src/index/lexical.rs` |
| S-5 | Lexical → `[0, 1]` | `lexical = s / (s + 4.0)` — a monotone squash, no `exp`, so a strong exact hit lands near 0.6–0.7 and the authority multiplier still decides between good matches. Revisit if the eval says the signal is flat. | `src/index/lexical.rs` |
| S-6 | Index location and format | `.janus/index.bin` + `.janus/manifest.json`, `format_version = 1`. M3 is the first writer of an on-disk format, so `1` is its number and it is bumped by any later layout change (§6.8). | `src/index/store.rs` |
| S-7 | Concurrency | Write `index.bin.new`, `fsync`, rename, then write `manifest.json` last (§8.3's atomicity rule), so a reader sees old-or-new and never a mix. **No advisory lock in M3**: there is exactly one writer surface (`janus index`, `janus search`'s auto-build) and M7 adds the lock together with the write path, where the second writer actually appears. Documented limitation in `store.rs`. | `src/index/store.rs` |
| S-8 | Staleness | `janus search`/`facts` auto-build when the index is missing, and auto-rebuild when `manifest.json` disagrees with the config, the root, the chunking fingerprint, or `format_version`. Every rebuild prints one line to **stderr** and says why (`stale: format_version 1 != 2`), because a silent 3-second rebuild looks like a hang. `--no-rebuild` refuses instead of building: an agent that wanted a read gets a read. | `src/index/store.rs`, `src/search/mod.rs` |
| S-9 | Incremental rebuild | Skip a document when its `content_hash` is unchanged **and** its chunk span set is unchanged; re-tokenize only chunks whose `ChunkId` is absent from the old snapshot, so a moved-but-unchanged file costs zero tokenization (the lexical analogue of §11-M4's embedding rule). | `src/index/build.rs` |
| S-10 | Intents | All four from §7.1, since all four need only declared `status` and dates: `current` (default), `history`, `evolution`, `exhaustive`. `evolution` orders oldest→newest; `current` applies decay and the status gate. | `src/score.rs` |
| S-11 | Scope filter | §6.12 rules 3, 4, 9: default union, repeated `--scope` unions, `--scope-only` excludes `global` and shared-only matches, normalized comparison. No supersession, so rules 6–8 (scope-relative windows) wait for M5 with a `todo` in the module docs, not a half-answer. | `src/score.rs` |
| S-12 | `janus facts` shape | `search` restricted to `decision \| spec \| principle`, status gate of `current`, no decay, ordered by authority then date desc — SPEC §9.2's `get_authoritative_facts` semantics, so M6's tool is a call, not a second implementation. | `src/search/facts.rs` |
| S-13 | Where `Clock` lives | `src/clock.rs`, one trait, `SystemClock` + `FixedClock` (test-only constructor in lib, not `#[cfg(test)]`, because integration tests need it). Nothing else in the crate reads the wall clock afterwards. | `src/clock.rs` |
| S-14 | Float comparison | No glyph of `==` on scores, anywhere: ties break on `(final, date desc, chunk_id)` with an explicit `total_cmp` comparator. `clippy::float_cmp` stays on. | `src/score.rs` |
| S-15 | Eval harness home | `tests/eval_quality.rs` reading `tests/eval/queries.jsonl`, printing nDCG@10 and active-top-1 to stderr, asserting only a floor (`≥ 0.80` on active top-1) so it gates without being brittle. Not a `janus eval` subcommand: no CLI surface for a developer-only report. | `tests/eval_quality.rs` |
| S-16 | `--limit` default | 10, capped by `mcp.max_chars` later (M6). Chunk-level hits, deduplicated to at most 3 chunks per document in the *display* layer only — the scorer still returns the argmax chunk per §7.2. | `src/search/mod.rs` |

## 4. Phases

### Phase 1 — Tokenizer (½ day)

**Goal:** `text -> terms`, boring and total: no dependency, no panic on any UTF-8,
and a stable definition that the index and the query share (a query is tokenized by
exactly the same code, or the two disagree — the classic lexical-search bug).

**Files:** `src/tokenize.rs` (new), `src/lib.rs`, `src/config.rs` (docs only if a knob
appears; the default plan adds none).

**Build (~120 logic lines):** iterator over `char_indices`, fold case with
`char::to_lowercase` (multi-char expansions like `İ` handled by pushing both),
accumulate into a reused `String` buffer, yield `&str` slices of that buffer through
a small boundary-aware struct. Terms longer than 64 bytes are dropped as a
pathological-input guard, counted in a `TokenStats`.

**Tests (~150 lines, unit):** ASCII and CJK (no word breaks — **decision**: a CJK
run becomes one term in M3, recorded as a known limitation, because a real CJK
segmenter is not in this milestone); `_` kept, `-`/`.` split; emoji dropped;
`JANUS_WRITE` vs `janus_write`; `İ`→`i̇`; empty input; 64-byte truncation; a
`proptest` that the tokenizer never panics and never yields a term that is not a
substring-slice of the folded buffer.

**Handoff (paste):** “Implement Phase 1 of M3 for janus-mem per `docs/m3-breakdown.md`
Phase 1 and S-1/S-2. No dependency. The tokenizer is the shared definition for
documents and queries; a query must be tokenized by the same function. Terms are
borrowed from an internal buffer — no `String` per term. Tests first for the splitting
rules and the property.”

**Accept:** `cargo test` green; a 1 MB document tokenizes in < 5 ms.

---

### Phase 2 — Inverted index + BM25 (1–1½ day)

**Goal:** `LexicalIndex::build(chunks) -> LexicalIndex` and
`LexicalIndex::score(&terms) -> Vec<(chunk_index, f64)>`, in memory, no I/O.

**Files:** `src/index/mod.rs` (new), `src/index/lexical.rs` (new), `src/lib.rs`.

**Build (~250 logic lines):** `HashMap<&str, u32>` term → term id during the build
(borrowed from the token buffer, then interned into a `StringPool` so the snapshot
persists strings once); postings as `Vec<(u32, u32)>` grouped by term with a
`term_offsets: Vec<(u32, u32)>` range table — the flat layout the on-disk format will
reuse, so phase 4 is a `serde` derive and not a redesign. `avgdl` over chunks;
per-term `df`; BM25 accumulation into a `Vec<f32>`-sized array of `f64` scores. Query
terms are scored with the same idf, repeated terms contribute repeatedly (that is
what `k1` is for), and terms absent from the index are skipped without a branch in
the inner loop.

**Tests (~300 lines, unit + property):**
- A hand-computed three-document fixture: term frequencies, idf, `avgdl`, and the
  final BM25 value to 6 decimal places (`assert!((a - b).abs() < 1e-6)`).
- `df` correctness; a term in every document gets idf ≈ 0; a term in none is skipped.
- Longer documents do not automatically outrank shorter ones (`b` works): a fixture
  where the naive sum would invert the order.
- Repeated query terms beat a single occurrence.
- Empty query, empty index, one-document index, a term that is a prefix of another
  (no accidental substring matching).
- `proptest`: arbitrary term lists never panic, never produce `NaN`/`inf`, and every
  returned chunk index is in range; scoring is monotone in term frequency for a fixed
  document set.

**Handoff (paste):** “Implement Phase 2 of M3 per `docs/m3-breakdown.md` Phase 2 and
S-3…S-5. Flat postings with a range table, `f64` internally, no I/O, no `unwrap` in
lib code. Hand-compute at least one BM25 value in a test rather than asserting the
implementation against itself.”

**Accept:** all BM25 tests green; building over the fixture vault (≈40 docs) is
< 10 ms in a debug build.

---

### Phase 3 — Scoring: authority, gates, intents, scope (1 day)

**Goal:** the §7.2 formula, pure: `score(item, query, params, clock) -> Breakdown`.

**Files:** `src/score.rs` (new), `src/clock.rs` (new), `src/config.rs` (docs: the
authority/half-life tables already exist from M1 and must now be *consumed*),
`src/lib.rs`.

**Build (~220 logic lines):** `AUTHORITY[type]` from config with the 0.80 fallback and
the existing `unknown_type` note; `STATUS_FACTOR` from §6.6's table per intent (with
the `draft` 0.7× and `unknown` 0.8× factors in `current`, the gate in `history`, and
`evolution` excluding `draft`); decay `exp(−ln2 · age/half_life)` anchored on
`effective_from` else `date`, `1.0` when `timeless` or `half_life <= 0` or the date is
in the future (clamped at age 0); `relevance = alpha·0 + (1−alpha)·lexical` with
`alpha = 0.0`; `final = relevance · authority · decay`; the scope filter; candidate-set
scoping as `Option<&CandidateSet>` per §7.2; the deterministic comparator.

**Tests (~350 lines, table-driven + property):**
- Every row of §7.2's hard requirement, one test each: decay monotonicity, authority
  ordering, each status gate per intent, α endpoints, future dates, missing dates,
  `timeless` override.
- §6.12.1 obligations 5, 8, 9 (same title different scope survives; `--scope-only`
  excludes global and shared-only; `ORG:Acme` normalizes).
- Ties: equal `final` breaks by date desc then `chunk_id`, and the order is stable
  across runs.
- `proptest`: `0.0 <= final <= 1.5 · 1.0 · 1.0` (authority ceiling × status 1.0 ×
  decay 1.0), and `final >= 0` for any non-negative lexical input.
- No wall clock: every test injects `FixedClock`.

**Handoff (paste):** “Implement Phase 3 of M3 per `docs/m3-breakdown.md` Phase 3 and
S-10…S-14. Read `SPEC.md` §6.6, §6.6.1, §6.12, §7.1, §7.2 first. `score()` takes a
`Clock`; nothing in the crate may read `SystemTime::now()` below `main`. Table-driven
tests are the deliverable, not an afterthought.”

**Accept:** all §7.2 test obligations present and green; `cargo clippy` clean.

---

### Phase 4 — Persistence: `index.bin`, `manifest.json`, rebuild (1–1½ day)

**Goal:** the index survives the process, and a second run is cheap.

**Files:** `src/index/store.rs` (new), `src/index/build.rs` (new), `src/index/mod.rs`.

**Build (~300 logic lines):** `Snapshot` ⇄ `LexicalIndex` conversion (the in-memory
index owns `StringPool`-backed ids; the snapshot owns the pool); `bincode`
serialize/deserialize with a `format_version` check on load (mismatch is a *rebuild*,
never an error, §8.3); `Manifest` written last, carrying `index_sha` = blake3 of the
bytes just written; `built_root` and chunking-fingerprint comparison; atomic write via
`index.bin.new` + `fsync` + `rename`; incremental rebuild driven by `content_hash`
and the chunk-id set (S-9); `--rebuild` and `--dry-run` (report the delta, write
nothing).

**Tests (~300 lines):** round-trip a snapshot through bytes and back with equal
scores; a corrupted `index.bin` is detected by the checksum and triggers a rebuild
with a warning, never a panic; a truncated file likewise; `format_version` bump forces
a rebuild; moving the vault changes `built_root` and *keeps every `DocId` and
`ChunkId`* (the §8.3 warning-not-error rule); touching one document re-tokenizes only
its chunks (assert on a build-stat counter, not on timing); `--dry-run` writes nothing
(byte-compare `.janus/`); an interrupted write (simulated by writing `.new` and not
renaming) is ignored on the next load; `proptest` that `Snapshot → bytes → Snapshot`
is identity for arbitrary pool/term content.

**Handoff (paste):** “Implement Phase 4 of M3 per `docs/m3-breakdown.md` Phase 4 and
S-6…S-9. Read `SPEC.md` §8.3. Atomicity is rename-then-manifest; a bad
checksum/version/root is a rebuild, never an error. `format_version = 1` is M3's
number. No vault writes; `.janus/` only.”

**Accept:** index → second run re-tokenizes zero chunks; a corrupted index recovers
with a warning; `.janus/manifest.json` reads as JSON to a human.

---

### Phase 5 — `janus index`, `janus search`, `janus facts`, eval (1–1½ day)

**Goal:** the milestone's usable surface, plus the number that says whether it works.

**Files:** `src/index/cmd.rs`, `src/search/mod.rs`, `src/search/facts.rs`,
`src/cli.rs`, `src/main.rs`, `tests/cli_search.rs`, `tests/golden_search.rs`,
`tests/eval/queries.jsonl`, `tests/eval_quality.rs`.

**Build (~320 logic lines):** `janus index` (build/refresh/dry-run, one stderr line,
`--json` receipt with `{api: 1, docs, chunks, terms, rebuilt, reused, taken_ms}`);
`janus search` (query → snapshot load or auto-build → tokenize → BM25 → score →
sort → limit → dedupe display to 3 per doc → render); `janus facts` per S-12; human
output as a compact table (`score  authority  status  date  path  breadcrumb  snippet
`) with the breakdown available under `--json`; `--json` per §9.4's envelope with
`"api": 1` and every §7.2 factor reported per hit so a surprising ranking is
diagnosable without reading code.

**Tests (~330 lines):** `assert_cmd` for both commands; the §10 integration row
(`index → search → search --json` on the fixture vault); golden `insta` over
`search --json` with the same one-field redaction as M2's chunks golden (the corpus
root); a query for a term only one fixture document contains returns it first;
`--scope`/`--scope-only` behaviour end to end (§6.12.1 obligations 5, 8, 9 through the
CLI, not just the scorer); `--intent history` returns a `superseded` document that
`current` does not; `--json` never contains body text (§8.5); a stale index rebuilds
with one stderr line and still exits 0; `--no-rebuild` on a stale index exits 1 with a
message and *no* index written; an empty corpus searches successfully with zero hits.

**Eval harness:** `tests/eval/queries.jsonl` — one JSON object per line
(`{"query", "intent", "expect": ["rel_path", ...]}`), ≥30 queries written against the
fixture vault (including at least five that the M2 fixtures make interesting: a
CRLF doc, a BOM doc, the `---`-body spec, the quoted-scalar note, the anchor doc).
`tests/eval_quality.rs` computes nDCG@10 and active-top-1, prints them to stderr, and
asserts the floor in S-15.

**Handoff (paste):** “Implement Phase 5 of M3 per `docs/m3-breakdown.md` Phase 5 and
S-8, S-12, S-15, S-16. Read `src/scan.rs` and `src/chunk/dump.rs` for the established
DTO/exit-code/report shape; reuse it rather than inventing a third one. stdout is
results, stderr is diagnostics. No content without an explicit flag.”

**Accept:** `janus index && janus search cache --json | jq .` works on the fixture
vault; eval prints its two numbers; exit codes match §9.1.

## 5. Cross-cutting requirements

- **Determinism.** No `HashMap` iteration reaching output (intern to a sorted
  `Vec`/`BTreeMap` before rendering); ties broken explicitly; two runs byte-identical.
- **No wall clock below the edge.** `Clock` is injected; `FixedClock` in every test.
- **No content in output without a flag** (§8.5): `search --json` returns spans and
  paths, never chunk text, unless `--text` is added later by M4/M6.
- **Fail open on reads, refuse on writes.** A corrupt index rebuilds; a stale one
  rebuilds; `--no-rebuild` refuses with exit 1.
- **No panics.** No `unwrap()`/`expect()` in lib code; indexing everywhere; the
  loader validates every offset it reads from disk (a hand-edited `index.bin` must not
  be able to index out of bounds).
- **Cross-platform (§8.7).** `rel_path` stays `/`-separated; `built_root` comparison
  goes through `dunce`; no `std::os::*`.
- **Windows stays possible:** `bincode` and `blake3/pure` must still `cargo check`.

## 6. Definition of done, made checkable

1. `janus index` then `janus search <term>` returns the right document first on the
   fixture vault, human and JSON.
2. A second `janus index` re-tokenizes zero chunks; touching one document
   re-tokenizes only that document's chunks.
3. A moved vault keeps every `DocId` and `ChunkId` and only logs a `built_root`
   warning.
4. A corrupt or `format_version`-mismatched `index.bin` rebuilds with a warning and
   never panics.
5. Every §7.2 test obligation and every §6.12.1 obligation that M3 can answer exist
   and pass.
6. The eval harness reports ≥ 0.80 active-top-1 over ≥ 30 queries, and the number is
   committed with the change that moved it.
7. `janus search` on this repo’s own `docs/` completes in < 200 ms warm.
8. Gates: fmt, clippy `-D warnings`, `cargo test --all-targets`, `cargo doc`
   (0 warnings), Windows `cargo check`, `scripts/pr-size.sh`.
9. Decisions recorded in §§8–12 of this file; README status line and SPEC §9.1’s
   `index`/`search`/`facts` rows updated in the last phase.

## 7. PR plan and size budget

One phase per branch from `main` after the previous merge, one PR each.

| PR | Branch | Logic estimate | Tests estimate | Why it splits here |
|---|---|---|---|---|
| P1 | `m3-tokenize` | ~120 | ~150 | The shared definition both sides of a query depend on; nothing else can be tested until it exists. |
| P2 | `m3-lexical` | ~250 | ~300 | BM25 itself: dense numeric code, hand-computed tests, no I/O. |
| P3 | `m3-score` | ~220 | ~350 | The policy layer (authority/status/decay/scope) — the part where being wrong is invisible, so it gets its own review. |
| P4 | `m3-index-store` | ~300 | ~300 | First on-disk format; owns `format_version` and the rebuild rules. |
| P5 | `m3-search-cli` | ~320 | ~330 | Command surface + eval. If it approaches 400 logic, the eval harness ships as `m3-eval` on its own. |

Total ≈ 1 200 logic lines across five PRs, each comfortably under the 600 ceiling.

## 8. Implementation log — Phase 1 (tokenizer)

## 9. Implementation log — Phase 2 (BM25)

## 10. Implementation log — Phase 3 (scoring)

## 11. Implementation log — Phase 4 (persistence)

## 12. Implementation log — Phase 5 (CLI + eval)

## 13. Handoff to M4

<!-- What M4 inherits: index format and load path, the alpha seam, where vectors
     attach, what M3 left out on purpose. -->
