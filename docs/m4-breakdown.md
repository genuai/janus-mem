# M4 — Embeddings and hybrid index: phased breakdown

> Branch: this document (`bb/m4-tech-lead-embeddings-hybrid-scoring-…`), then one branch per
> phase from `main`.
> Spec: `SPEC.md` §6.4 (chunk ids), §7.2 (scoring, α, cosine, candidate scope, D20 gate),
> §8.2 (crate stack, feature policy), §8.3 (index layout, `vectors`, rebuild triggers),
> §8.3.1 (RAM budget), §8.7 (platform), §9.1 (CLI), §9.4 (envelope), §10 (tests),
> §11-M4, §12.1 (learn targets), §13 (performance), §15 rows D13/D17/D20/D22/D29.
> Inherited contracts: `docs/m3-breakdown.md` §2, §3.1–§3.5, §16.
>
> Milestone statement (SPEC §11): "`Embedder` trait (`fn embed(&self, batch: &[&str]) ->
> Result<Vec<Vec<f32>>>`), `HashEmbedder` for tests, `FastEmbed` behind a feature; flat vector
> storage written to a **separate `vectors.f32` file from day one** (so M8's mmap is a load-path
> change, not a format change); rayon top-k; `Index` metadata via `bincode`; incremental reindex;
> exact-hash move detection (`path_history`); `janus index`."
>
> **Done when (verbatim from §11):** "second index run touches zero embeddings for unchanged docs;
> a moved-but-unchanged file is recognized as a move and costs zero embedding work; queries are
> <10 ms warm at 100k chunks; `--no-embeddings` degrades cleanly." §7 restates each as a checkable
> condition, and three of the four are amended with reasons — see §3.7 and §9.
>
> **Learn targets (SPEC §12.1):** traits and trait objects (`Embedder`, alongside the existing
> `Clock`), mild generics, `Send`/`Sync` and `dyn`, serde in anger, and **Cargo features**
> (`embeddings`). Rayon and mmap are explicitly *not* in this milestone; §9 recommends the two SPEC
> rows that name them be moved, because M4 has no measurement that asks for either.

`main` is at the end of M3 (`3c6d044`). 437 tests across 18 binaries, fmt/clippy/doc/Windows/
size all green. M4 inherits a working lexical engine from which it needs four seams, and no more:

* `score::score` already takes `semantic` and `alpha` and reports both, with `alpha = 0.0` and
  `semantic = 0.0` hard-coded at the one call site in `search::query`.
* `Snapshot.chunks` already carries one row per chunk *occurrence* with `(doc, span,
  heading_path, tokens)`, so a parallel array of vectors attaches by position without touching
  `DocMeta`.
* `Manifest.embedder: Option<String>` exists and is `None`; `store::Expected::check` does not
  compare it yet.
* `store::publish` is a six-stage, hash-validated publication under `.janus/lock`; a second
  payload file has to join that protocol rather than bypass it.
* `Config.embedding` already exists (`enabled`, `model`, `batch_size`) and is unused.
* `tests/eval_quality.rs` already computes `nDCG@10` and `active top-1` (M3 W-9), so M4 upgrades
  the *comparison*, not the metric.

**Corrections to the handoff and to SPEC §11-M4 (read this before the scope section).** Three
claims inherited from the brief are not what the code does, and the difference changes the plan:

1. `--no-embeddings` **is not implemented** in M3. `degraded: ["lexical only; embeddings arrive
   in M4"]` exists in the report (`search/mod.rs`), and `Cargo.toml`/`cli.rs` have no flag. SPEC
   §9.1 lists `janus index [--no-embeddings]`; the binary does not accept it. M4 adds the flag to
   `index`, `search` and `facts`, and the plan's degradation contract (§3.6) is written from
   scratch rather than inherited.
2. `bincode` metadata, incremental reindex, `janus index` and `--dry-run` are M3 work that
   §11-M4 still claims. They are **already done** and are removed from §0's scope.
3. "A moved-but-unchanged file costs zero embedding work" is **not** free, and the brief's
   reasoning is wrong for the most common rename. `ChunkId` excludes the title (D48, CH-5), but
   the embedded text is `Chunk::embed_text` = *title* + breadcrumb + text (S-3), and the title is
   the frontmatter `title:` **or the filename stem**. A rename of a document that declares no
   `title:` therefore changes the embedded text and must re-embed it. §3.7 states the correct
   conditional and proposes the SPEC sentence that separates "recognized as a move" (M5) from
   "costs zero embedding work" (M4). No `path_history`, no file-hash comparison and no move
   detection is built in M4.

**Status: first draft, awaiting review.** No implementation exists. Every phase log in §10–§14 is
empty on purpose.

## 0. Scope

### In scope
- `Embedder` trait (`Send + Sync`, `&dyn` at call sites), `EmbedderId`, `EmbedError`, and a
  deterministic `HashEmbedder` that needs no model (§3.2).
- `vectors.f32`: raw little-endian `f32`, row-major, L2-normalised, **no header**, row *i* = chunk
  *i* of the snapshot; a writer, a reader, and load-time validation (§3.1). `format_version = 2`.
- The two-file generation: `vectors.f32` is published and hash-validated through the existing
  `.janus/lock` protocol, with `vectors_sha`/`vectors_len` in the manifest and a stated recovery
  contract (§3.1).
- Hybrid scoring: `semantic = clamp(cos, 0, 1)`, `relevance = α·semantic + (1−α)·lexical`, α from
  `[embedding] alpha` with default `0.65` and `0.0` when there is no embedder; the admission rule
  changes from `lexical > 0` to `relevance > 0` (§3.3).
- A real embedder (`fastembed`) behind a **non-default** `embeddings` feature, batched, with no
  network access anywhere in the crate or its tests; model files are read from disk only (§3.2).
- Incremental embedding keyed on what is actually embedded — the title is in the key, `ChunkId`
  is not the key (§3.4); `janus index` reports what it embedded and what it reused.
- Clean degradation, made checkable as a matrix (§3.6), plus `--no-embeddings` on all three
  commands that need it.
- The eval upgrade: the α sweep as the calibration instrument (§3.5), and a `tantivy` baseline as
  D20's decision gate — run from `scripts/baseline/`, a standalone crate that the repo's
  dependency graph never sees (§3.8).

### Explicitly NOT in scope (do not build)
- `path_history`, exact-hash move detection, move *recognition*, `janus moves`, continuation
  hints, the alias table (M5, §6.9). §3.7 explains why nothing in M4 needs them.
- `mmap`, `memmap2`, `bytemuck`, quantization (`--quantize f16|i8`), `madvise`, mmap'd page-cache
  accounting, ANN/vector indexes of any kind (M8). M4 reads the whole file with `fs::read`.
- `rayon`, parallel dot products, parallel embedding batches, `criterion` benches (M8; §9 proposes
  moving the two SPEC rows that name rayon).
- Supersession, `effective_status`, `--as-of`, validity windows (M5). α is a parameter in M4; the
  *status* rules next to it are unchanged.
- Multi-corpus queries, `archived_weight`, `--corpus` (M5/M6).
- MCP and `janus rpc` (M6). CLI only.
- Any model download, model cache management, or `janus model` subcommand. D22 says the model is
  "downloaded separately, never bundled"; M4 only *reads* what is already on disk (§3.2, V-13).
- Stemming, stopwords (S-2, unchanged), reranking, cross-encoders, query expansion, chunk
  summarisation, MMR/`--diversity`.
- Any change to the tokenizer, BM25, the status gates, the intents, the result contract (§3.1) or
  the scope filter. M4 adds a term to `relevance` and a file beside `index.bin`.

## 1. Dependency changes (`Cargo.toml`)

| Crate | Version | Why (one-line justification per §8.2 policy) |
|---|---|---|
| `fastembed` | 5.x (verify with `cargo search`; SPEC §8.2 names it, D20) | The only maintained Rust wrapper for the ONNX sentence-transformer SPEC §7.2 names; optional, so a lexical-only build pays nothing for it. `= { optional = true }` behind `embeddings = ["dep:fastembed"]`. |
| *(none)* | — | `rayon` is deferred (V-16). `memmap2`/`bytemuck` are M8. The `tantivy` baseline lives in `scripts/baseline/`, a separate crate, so it is not in this graph at all (§3.8). |

Feature policy, and it is the load-bearing decision of this milestone:

* `embeddings` is **not** a default feature (V-13). Reason: `fastembed` → `ort` → a prebuilt ONNX
  Runtime binary fetched at build time, which cannot be cross-compiled from macOS to
  `aarch64-pc-windows-msvc`. AGENTS.md's Windows gate (`cargo check --target
  aarch64-pc-windows-msvc --all-targets --features blake3/pure`) must stay green, and
  `main`'s default `cargo check --all-targets` must stay network-free.
* `cargo clippy --all-targets -- -D warnings` therefore does not lint the `fastembed` module on
  the default path; the feature job in CI runs clippy and check with `--features embeddings` so it
  is still linted somewhere (§5 P3).
* `fastembed`'s default features include a C-toolchain compression backend via `ort`'s download
  path; do not "helpfully" turn on `download-binaries` in the library. The TOML comment says so.

No new required crate. A lexical-only `janus` gains no dependency, no binary size and no build
time from M4.

## 2. Data model (write these first — §12.3.3)

```rust
// src/index/embed.rs — the trait, its identity, and the deterministic double.
/// What an embedder is, for the manifest and the reuse keys (§8.3, §3.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedderId {
    pub name: String,          // "hash-64", "AllMiniLML6V2"
    pub dim: u32,              // 64, 384
    pub dtype: String,         // "f32" — the only dtype M4 writes
    pub normalisation: String, // "l2" — the only normalisation M4 accepts
}

/// A batch embedder. `Send + Sync` so a later phase can share one across threads
/// without revisiting this signature; `&dyn` at every call site because the
/// choice is runtime (config × feature × model availability), not generic.
pub trait Embedder: Send + Sync {
    fn id(&self) -> &EmbedderId;
    /// One row per input, in order, each of `id().dim` finite values.
    fn embed(&self, batch: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError>;
}

pub struct EmbedError {
    pub embedder: String,
    pub batch: usize,   // which batch index failed, so the message names a place
    pub reason: String,
}

/// Feature-hashed tokenizer terms: deterministic, no model, no download (V-6).
pub struct HashEmbedder { id: EmbedderId, tokenizer: Tokenizer }

/// Resolves config × feature × model availability into an embedder or a reason.
pub enum Resolved { Ready(Box<dyn Embedder>), Degraded(&'static str, String) }
pub fn resolve(config: &Config) -> Resolved;
```

```rust
// src/index/vectors.rs — the separate flat file (D17, §3.1)
pub const VECTORS_FILE: &str = "vectors.f32";

/// Row-major, little-endian f32, L2-normalised, `rows * dim` values, no header.
pub struct Vectors { dim: u32, rows: Vec<f32> }

impl Vectors {
    pub fn new(dim: u32, rows: Vec<f32>) -> Result<Self, Corrupt>; // validates the shape
    pub fn encode(&self) -> Vec<u8>;                                // to_le_bytes
    pub fn decode(bytes: &[u8], dim: u32, rows: usize) -> Result<Self, Corrupt>;
    pub fn row(&self, index: usize) -> Option<&[f32]>;
    pub fn len(&self) -> usize;   // number of rows
    pub fn dim(&self) -> usize;
}

/// Divides by the L2 norm; a zero row stays zero and reports norm 0.0 (V-7).
pub fn normalize(row: &mut [f32]) -> f32;
/// Dot product. On normalised rows this *is* the cosine (§7.2).
pub fn dot(a: &[f32], b: &[f32]) -> f32;
```

```rust
// src/index/store.rs — additions, not a redesign
pub struct Manifest {
    /* … unchanged … */
    pub embedder: Option<EmbedderId>,   // was Option<String>
    pub vectors_sha: Option<String>,    // blake3 of vectors.f32
    pub vectors_len: Option<u64>,       // byte length, checked against rows*dim*4
}

pub struct Publication<'a> {
    /* … unchanged … */
    pub vectors: Option<&'a Vectors>,
}

pub enum Stage {           // three new stages, before the index (V-3)
    VectorsWritten, VectorsRenamed, VectorsDirSynced,
    IndexWritten, IndexRenamed, IndexDirSynced,
    ManifestWritten, ManifestRenamed, ManifestDirSynced,
}

pub struct Load {
    /* … unchanged … */
    pub vectors: Option<Vectors>,   // None when the caller did not want them (§3.1)
}

pub struct LoadOptions<'a> {
    /* … unchanged … */
    pub want_vectors: bool,                      // §3.1: a lexical-only read skips the file
    pub embedder: Option<&'a dyn Embedder>,      // resolved once at the edge, passed down
}
```

```rust
// src/index/build.rs — the vector half of the plan
pub struct BuildPlan {
    /* … unchanged … */
    pub vectors: Option<Vectors>,
}

pub struct BuildStats {
    /* … unchanged … */
    pub embedded_chunks: usize,      // chunks that actually went through `Embedder::embed`
    pub reused_vector_chunks: usize, // rows copied from the previous generation
}
```

`score::ScoreInput` and `Breakdown` are **unchanged**: `semantic` and `alpha` are already the right
shape and the only edit is at the call site. `search::Options` gains `no_embeddings: bool`; the
report gains a top-level `alpha: f64` and `degraded` becomes `Vec<String>` (it now carries a
reason, sometimes with a path).

## 3. Contracts

These are the normative parts of this plan. Each has acceptance cases that fail when violated, and
each exists because the first draft of a similar plan for M3 got it wrong in review.

### 3.1 Two payload files, one generation: the `vectors.f32` format and publication

`vectors.f32` is a **headerless** array of little-endian `f32` values, row-major, row *i* belonging
to snapshot chunk *i*, `rows × dim` values, every row L2-normalised (D17, §7.2 "Vectors are
L2-normalized at index time, so cosine is a dot product").

**No header, deliberately.** §8.3's `VectorSection { offset, len, dim, dtype }` lives in the
manifest, not in the file. A header would put `dim` in two places (it is already in
`EmbedderId`), would force M8's `bytemuck` cast to skip a prefix, and would make "raw
little-endian f32" false. `offset` is `0` in M4 and stays `0` — the field exists for a future
multi-section file, not for a header we have not written.

**The manifest is the only description of the file.** It carries `embedder` (id, dim, dtype,
normalisation), `vectors_sha` and `vectors_len`. A reader never infers a shape from a byte count
alone: `vectors_len` must equal `rows × dim × 4`, and `rows` must equal `snapshot.chunks.len()`.
All three are `Some` together or `None` together; a manifest with two of three is corrupt.

**Both payload files are hash-validated against the manifest, and that is the whole reason the
pair is safe.** This is the one genuinely new correctness risk in M4: with two files, a reader
that checks only `index.bin` can load *new* vectors against an *old* index — a state that looks
perfectly valid and silently returns wrong rankings. Generation identity is therefore
`(format_version, index_sha, vectors_sha)`.

**Writer order and recovery.** `vectors.f32` is renamed first, then `index.bin`, then
`manifest.json`, all under `.janus/lock`, each from its own `NamedTempFile`, each followed by a
directory `fsync`. The order is near-arbitrary once both payloads are hash-checked — the manifest
must be last because it is the commit record, and the largest file goes first so the window in
which the commit record lags its payload is as short as possible. Say that honestly instead of
pretending the order is atomic.

| Failure point | On-disk state | Next read, rebuild allowed | Next read, `--no-rebuild` |
|---|---|---|---|
| before the vectors rename | the previous (index, vectors, manifest) triple | loads | loads |
| **between the vectors rename and the index rename** | **new vectors, old index, old manifest** | `vectors_sha` mismatch → retry → rebuild under the lock | exit 1, names `.janus/vectors.f32`, writes nothing |
| **between the index rename and the manifest rename** | **new vectors + new index, old manifest** | `index_sha` mismatch → retry → rebuild | exit 1, names `.janus/index.bin`, writes nothing |
| after the manifest rename | the new triple | loads | loads |

The middle two rows are the contract: recovery is a **rebuild, not a restore** (§3.5 of M3 stands).
`index.bin` and `vectors.f32` are replaced by rename; the previous bytes are gone. Nothing in M4
adds versioned generations. The reader's guarantee is unchanged in kind — it validates the bytes it
actually read — and is now stated over three files instead of two.

**A lexical-only read does not read `vectors.f32` (V-4).** `LoadOptions.want_vectors` is false for
`search --no-embeddings`, for `facts`, and whenever α will be 0. The file is not opened, hashed or
validated. Consequence, stated so nobody has to discover it: a lexical-only read cannot observe a
corrupt vectors file, and must not pay 300 ms to learn something it will not use. A read that
*does* want vectors validates them, and a mismatch follows the table above.

**`format_version = 2` (V-1).** The manifest gains fields, `vectors.f32` is a new file, and the
meaning of "a generation" changes. Per AGENTS.md that is exactly the case the number exists for;
an M3 index is refused by `Expected::check` and rebuilt with one warning, never misread.

**Load-time validation (§3.5 of M3, extended).** `Vectors::decode` checks, before any row is
used: byte length is a multiple of 4 and equals `rows × dim × 4`; `dim > 0`; every value is finite
(`NaN`/`±inf` is corruption, not a score); every row's norm is `0.0` or within `1e-3` of `1.0`
(a row that is neither normalised nor zero means the writer was not this code). Any violation is a
rebuild, never a panic.

**Memory, stated now so review does not have to ask.** M4 holds the whole array on the heap:
`200k × 384 × 4 ≈ 307 MB` at §8.3.1's reference vault, which exceeds that section's 60–100 MB
steady-state heap budget. This is deliberate and temporary: D17 puts vectors in their own file
precisely so M8's mmap reuses the file unchanged. At the fixture scale the array is ~5 KB. §9
proposes the one-line SPEC amendment that records the exception, rather than letting a future
reader believe the budget is met.

### 3.2 The embedder: identity, determinism, and the absence of a model

**Identity is metadata, not weights (V-20).** `EmbedderId` records name, dim, dtype and
normalisation. It does not hash the model file. Hashing a ~90 MB ONNX file costs ~100 ms per
process, which breaks the 50 ms startup target (§13) to protect against a failure mode that is
already a user action ("I replaced the model file"). Documented limitation: replacing a model file
under the same name requires `janus index --rebuild`; the message in the docs says so. If M8
measures the hash as affordable, it becomes a field and a rebuild trigger — a decision for then.

**`HashEmbedder` is a test double, and its numbers are never quality (V-6).** It tokenizes with the
same `Tokenizer` as the index (S-1), maps each term to a bucket by `blake3(term)`, accumulates
signed counts into `dim = 64`, and L2-normalises. Properties that are asserted: identical text ⇒
identical vector; empty text ⇒ all-zero row; unit norm for non-empty text; determinism across runs
and platforms (blake3 + integer arithmetic only); `cos(a, a) == 1.0` within `1e-6`;
`cos(a, b) > cos(a, c)` for a hand-picked vocabulary where `b` shares a term and `c` shares none.
It is a bag of tokens, so its cosine tracks BM25 almost exactly. That makes it excellent for
plumbing tests and worthless for calibration; **no quality number in this milestone may be produced
with `HashEmbedder`**, and §3.5 says what replaces it.

**No network, ever, in the crate or its tests (§10).** `resolve` reads a model from disk:
`[embedding] model_dir`, else `JANUS_MODEL_DIR` (SPEC §14 already names it), else the
`fastembed` default cache directory. If the files are not there, `resolve` returns `Degraded` with
a reason that names the directory it looked in. It never downloads and never blocks. A test asserts
that a `Config` whose model directory does not exist yields `Degraded`, with no socket, no
`std::env` read below `main`, and no timeout.

**Selection is one function, at the edge.** `main` calls `embed::resolve(&config)` once and passes
`Option<&dyn Embedder>` down. Nothing below `main` reads the environment or the feature flag: the
feature controls whether the `fastembed` module *exists*, not what any function decides at
runtime.

**The writer normalises and validates (V-7).** `Embedder::embed` may return any L2 norm; the
builder normalises every row and checks it against `id().dim`, finiteness, and the norm rule of
§3.1. A violation **fails the build** (exit 1, message names the embedder and the chunk index) and
publishes nothing. Substituting a zero row would be invisible quality loss, which is worse than a
failed index; reads fail open, writes fail closed.

### 3.3 Hybrid scoring: the equations, the admission rule, and what the weight claims

```
semantic   = clamp(dot(q_vec, c_vec), 0.0, 1.0)         rows are L2-normalised, so dot is cosine
lexical    = s / (s + s_ref)                            unchanged (M3 §3.3)
relevance  = α · semantic + (1 − α) · lexical           α ∈ [0, 1], default 0.65, 0.0 without an embedder
authority  = type_authority · status_factor             unchanged
decay      = exp(−ln2 · age_days / half_life)            unchanged
final      = relevance · authority · decay              unchanged
admit(chunk) ⟺ relevance > 0.0
```

**Preconditions, all held by construction and asserted:** the query vector and every chunk row
come from the same `EmbedderId` (guaranteed by `Expected::check`, which rebuilds on a mismatch);
`q_vec.len() == c_vec.len() == dim`; rows are unit-norm or zero; `α ∈ [0,1]`. A zero row scores
`semantic = 0.0`, not `NaN`.

**The admission rule changes, and that is a behaviour change worth naming (V-9).** M3 skips a
chunk when `lexical <= 0`, because a non-matching chunk cannot be relevant. Under hybrid a chunk
with no lexical match but a real semantic match *is* relevant, so the test moves to the quantity
that decides the ranking: `relevance > 0`. Consequence: a document can now enter the result set on
semantic evidence alone, and the notes that say "N results hidden by intent" / "N more documents
matched" keep their meaning because they are computed from the same admitted set. Written this way,
α = 0 is **score-identical** to M3 — `relevance == lexical`, and the skip fires on the same chunks
as before. The regression contract is stated precisely, because "identical" without a scope is
how M2 and M3 got into trouble: with α = 0 the `results` array of `search --json` (ranks, ids,
scores, spans, breadcrumbs, order) is **byte-identical** to the M3 golden under the same
redactions, and the only fields permitted to differ are the new top-level `alpha` and the text of
`degraded`.

**What α = 0.65 claims, as numbers** (both signals in `[0,1]`, so the comparison is meaningful for
once):

| semantic | lexical | relevance at α = 0.65 |
|---|---|---|
| 0.5 | 0.5 | 0.500 |
| 0.0 | 1.0 (perfect lexical, no semantic) | 0.350 |
| 1.0 (perfect semantic, no lexical) | 0.0 | 0.650 |
| 0.4 (typical cosine for unrelated text) | 0.0 | 0.260 |
| 0.4 (typical cosine for unrelated text) | 1.0 | 0.480 |

The third and second rows are the claim: a perfect semantic match with no shared term outranks a
perfect lexical match with no semantic signal by 1.86×. Row 4 is the danger: a *real* sentence
transformer gives ~0.2–0.5 cosine for unrelated pairs, so "no lexical match" is not "no semantic
credit", and the sampled 0.26 is more than a third of a perfect lexical match. Whether that is
good depends entirely on the corpus, which is why §3.5 exists and why the number is a config value
rather than a literal in `score()`.

**α = 0.0 and α = 1.0 are both supported and both tested.** `0.0` is the degradation path and is
score-identical to M3 (above, §7 condition 4). `1.0` exists so the eval can isolate the semantic
signal; it is not a recommended default and nothing sets it automatically.

**Candidate scope is unchanged: every chunk is scored.** §7.2 allows a candidate set and says v1
passes `None`; M4 keeps that. The semantic scan is therefore a full dot product over the array,
`200k × 384` multiply-adds ≈ 77 MFLOP, single-threaded with `chunks_exact(dim)` — measured in P2
and printed, never asserted. A top-k prefilter or an ANN index is an M8 measurement away, not a
guess now (V-16). `search --json` reports the query-wide `alpha` so a reader can recompute
`relevance` from `scores.semantic` and `scores.lexical`; without it the breakdown is not
recomputable, which defeats the point of reporting it.

**Empty query ⇒ no embedder call.** `facts` and the eval's exhaustive pass query with no text; the
query vector is `None`, `semantic = 0` for every chunk, and admission falls back to lexical. This
keeps those commands at M3 cost and removes `embed("")` from the hot path.

### 3.4 Vector reuse: the key must include everything that is embedded

M3 keys token reuse per document on `lex_key = blake3(tokenizer_fp ‖ chunker_fp ‖ title)` plus the
chunk-id set and count (§3.4 of M3, S-9), because `ChunkId` excludes the title while the indexed
text includes it. Embeddings have the same defect plus one of their own, so they get their own key:

| Identity | Formula | Purpose |
|---|---|---|
| `ChunkId` | `blake3(heading_path ‖ 0x00 ‖ normalized_text)` | content identity; **never** an embedding-cache key |
| `lex_key` | `blake3(tokenizer_fp ‖ chunker_fp ‖ title)` | "the tokens this document contributes are unchanged" |
| `vec_key` | `blake3(embedder_id ‖ 0x00 ‖ title)` | "the text this document asks the embedder for is unchanged" |

`vec_key` needs exactly three things and no more: the embedder (a different model is a different
vector), the title (the only input to `embed_text` that `ChunkId` excludes), and — through the
document-level conjuncts below — the chunk set, which carries breadcrumb and text. It deliberately
does **not** include the tokenizer fingerprint: a tokenizer change alters `lex_key` and must
re-tokenize, but it does not change `embed_text`, so re-embedding everything would be pure waste.

Reuse is per document, and it is a contract with the same shape as §3.4 of M3:

```
reuse_vectors(doc) ⟺  old.vec_key == new.vec_key
                   ∧  old.embedder == new.embedder          (also in the manifest check)
                   ∧  every new ChunkId of the document is present in the old snapshot
                   ∧  the document's chunk count is unchanged
                   ∧  the previous generation's vectors validated
```

What is reused is **rows only**, copied by ordinal into the new snapshot positions. Everything
else about a vector row is derived from the snapshot: a row is addressed by chunk position, so a
reused document whose chunks moved position must have its rows copied to the new positions. If the
old generation has no vectors, or its array failed validation, or its row count does not match, the
whole corpus re-embeds — a wrong row order would silently attach one document's vector to another,
which is the worst failure this milestone can produce.

**No cross-document dedupe (V-12).** Two documents that share a `ChunkId` and a title produce
identical rows; M4 still writes two, exactly as postings are per occurrence (§3.4 of M3). Deduping
would need a second key and a second index to undo it, for a saving that only exists in corpora
full of copied documents. Recorded as a known, harmless redundancy.

**Acceptance** (each compares the incremental result with a clean rebuild: identical snapshot after
normalising `built_at`, identical `vectors.f32` bytes, and the named counter):

1. Frontmatter `title:` edited → tokens and vectors both re-keyed for that document only;
   `embedded_chunks == chunks(doc)`.
2. Frontmatter change that touches neither title nor body (tags, status, scope) → **zero**
   re-embedding and zero re-tokenizing.
3. **Body edited** → that document re-embeds; every other document reuses; `embedded_chunks ==
   chunks(doc)`.
4. **Pure rename of a document that declares `title:`** → identical snapshot and identical
   `vectors.f32`; `embedded_chunks == 0` and `retokenized_chunks == 0`.
5. **Pure rename of a document with no `title:`** → the derived title changes, so the document
   re-embeds and re-tokenizes; every other document does neither. (This is the §3.7 correction,
   pinned as a test so the claim cannot drift.)
6. **Embedder id change** (e.g. `hash-64` → a 128-dim double) → manifest check forces a rebuild;
   every chunk re-embeds; every token is reused.
7. **Two documents, identical bodies, different titles** → two rows, and the rows differ.
8. **`--rebuild`** → `embedded_chunks == total chunks`, and the published `vectors.f32` is
   byte-identical to case 7's clean build.
9. **Vectors and index stay in step**: after every incremental build, `vectors.len() ==
   snapshot.chunks.len()`, and for every row *i*, `snapshot.chunks[i].doc` names a document whose
   span/text produced it. Asserted structurally over the whole fixture and by a property test over
   generated edits.

### 3.5 Calibration: how the blend is verified, and what happens if cosine dominates

**This is the real risk, not the plumbing.** `lexical = s/(s+s_ref)` is `[0,1)` with a *defined*
midpoint: 0.5 means "an average match for this query" (M3 §3.3), and it is corpus-size-free.
`semantic = clamp(cos, 0, 1)` is in the same interval but not the same units: cosine says nothing
about relevance, and a real sentence transformer returns ~0.2–0.5 for pairs that share no topic.
Blending them at 0.65 is a claim that the two scales are comparable at all. The plan's job is to
make that claim testable and to pre-declare what happens when it fails.

**Three homes, as in M3 §3.8 — do not put a quality floor inside `cargo test`.**

| Thing | Where | Blocking? |
|---|---|---|
| Plumbing: α moves the score, `semantic` is non-zero and recomputable from the DTO, α=0 is exactly M3, `--no-embeddings` reproduces the lexical ranking | `tests/*`, using `HashEmbedder` | **Yes** |
| The lexical report (`active top-1`, `nDCG@10`, α = 0) | `eval_quality` (ignored) + CI `eval` job, unchanged | **No** |
| The hybrid numbers and the α sweep | `scripts/eval.sh --sweep`, run with `--features embeddings` and a real model on the author's vault and the fixture | **No** |
| The α decision | a human reads the sweep, records the numbers and the chosen default in this file's phase log | **Yes at milestone close, by a human** |
| D20's `tantivy` gate | `scripts/baseline/` (§3.8) | **No** (one decision, one run) |

**What the sweep prints, per α ∈ {0.0, 0.35, 0.5, 0.65, 0.8, 1.0}:**

1. `active-top-1` and `nDCG@10` with their denominators (M3's definitions, unchanged).
2. `rescued` / `broken` against α = 0: queries whose rank-1 hit becomes correct / becomes wrong.
   This is the only honest statement of "the semantic layer helped": not "the number went up", but
   "these queries changed and here is which way".
3. The **semantic distribution** of the returned top-10 per query: min, median, max. If the median
   sits above ~0.6 for queries that the hybrid ranks *wrongly*, cosine is saturating and α is
   amplifying noise; if the spread is under ~0.05, semantic is a near-constant and α is merely
   shrinking the lexical term — the safe failure, and worth knowing which one you have.
4. Wall time per query, printed, never asserted.

**Pre-declared decision rule (so the choice is not made after seeing the numbers):**

* Default ships at **α = 0.65** (SPEC §7.2).
* If `nDCG@10(0.65) < nDCG@10(0.0) − 0.02` on the author's vault, or `broken > rescued`, the
  default α changes in `Config`'s shipped example — **one line, plus a decision row in this file**.
  No code change, no format change, no rebuild.
* If the semantic distribution shows saturation, the candidate fix is a documented, monotone
  deadzone (`semantic = max(0, (cos − floor)/(1 − floor))`) — a *new contract row and a config key*,
  never a silent edit to `score()`. M3's review rejected an undeclared `s/(s+4)` for exactly this
  reason; the same rule applies in the other direction.
* The fixture eval (18 keyword-rich documents, W-10 of M3) is a **smoke test**: it proves the sweep
  runs and the numbers move. The decision is taken on the author's real vault, with the numbers
  pasted into §14.
* `HashEmbedder`'s numbers are never an input to this decision (§3.2).

**What is deliberately not built:** no learned calibration, no temperature, no per-query
normalisation of cosine across the candidate set. Min–max scaling within the result set would make
a document's score depend on which other documents matched, break "the same document/query pair
maps to the same value", and cannot be computed in a single-document test. If calibration is
needed beyond a floor, it is a milestone of its own.

### 3.6 Degradation, as a matrix rather than a sentence

`degraded` is the field SPEC §9.4 puts in every response; the matrix below is what each state
writes into it. `α` is what the scorer receives.

| State | `janus index` | `janus search` | α | `degraded` entry |
|---|---|---|---|---|
| Feature off | lexical generation, **no** `vectors.f32` | lexical | 0.0 | `embeddings not compiled; lexical only` |
| Feature on, `[embedding] enabled = false` | lexical generation | lexical | 0.0 | `embeddings disabled in janus.toml; lexical only` |
| Feature on, model files absent | lexical generation, one stderr line naming the directory | lexical | 0.0 | `model not found in <dir>; lexical only` |
| Feature on, model present | generation with vectors | hybrid | 0.65 | *(empty)* |
| `--no-embeddings` on `index` | lexical generation; an existing `vectors.f32` is **removed** after the manifest rename | — | — | — |
| `--no-embeddings` on `search`/`facts` | — | lexical, `.janus/` untouched | 0.0 | `--no-embeddings: lexical only` |
| Index has no vectors, embeddings available | — | stale → rebuild (one `stale:` line) | 0.65 | *(empty after the rebuild)* |

The last row is a decision (V-21), not an accident. `Expected.embedder` is the id this process
*would* embed with, or `None` when it cannot; a generation built without vectors therefore fails
`Expected::check` for a process that can embed, and S-8's existing rule rebuilds it. The
alternatives are worse: silently serving α = 0 when the user has embeddings is a lie, and treating
`None` as fresh forever means vectors never appear without a manual `--rebuild`. The cost is one
model load and one build, announced on one stderr line. `--no-rebuild` still refuses and writes
nothing.

`--no-embeddings` means two different things and the plan says so in one place (V-14): on `index` it
*is* a statement about the generation ("do not write vectors"), and on `search`/`facts` it is a
statement about the query ("do not use them"), with `.janus/` left byte-identical. A single flag
that silently rewrote the cache on a read would be the worse of the two.

### 3.7 The move-detection correction

SPEC §11-M4's done-when says "a moved-but-unchanged file is recognized as a move and costs zero
embedding work". Three separate things are being conflated, and only one of them belongs to M4:

1. **Zero embedding work for an unchanged *embedded text*.** True today by construction, once
   `vec_key` (§3.4) exists: `ChunkId` is content-addressed (D48) and excludes the title, `DocId`
   survives a rename (D15, and M3 §3.4 acceptance 4 passes), so a rename changes nothing the cache
   keys on…
2. …**except the title, which is in the embedded text.** `Chunk::embed_text` = title + breadcrumb +
   chunk text (S-3), and the effective title is the frontmatter `title:` **or the filename stem**
   (`vault/resolve.rs`). So a rename of a document that declares `title:` costs zero embedding work,
   and a rename of a document that does not **must** re-embed that document, because its vector now
   describes different text. The brief's claim holds only under the first condition.
3. **"Recognized as a move"** — a reported event, a `path_history` entry, a continuation hint, a
   `janus moves` row. That is §6.9 and M5, and it needs the graph M5 is building anyway.

Nothing in M4 builds or needs move detection: no file-hash comparison, no `path_history`, no
`DocId` guesswork. **Proposed SPEC correction** (§9): replace the clause with "a rename that leaves
the embedded text unchanged costs zero embedding work; a rename that changes the derived title
re-embeds only that document; move *recognition* is M5", and move "exact-hash move detection
(`path_history`)" out of §11-M4's list into §11-M5's, where §6.9 already assigns it.

The correction matters beyond bookkeeping: "zero work on a move" is only true because of *what the
cache keys on*, and a future reader who believes the sentence literally would either drop the title
from `embed_text` (changing the ranking, for a caching argument) or add a move detector that saves
one document's embeddings. Both are worse than the sentence.

### 3.8 The `tantivy` baseline: D20's gate, outside the dependency graph

D20: "At M4, compare nDCG@10 against `tantivy` on the eval set; swap only if the hand-rolled
version is >10% worse." The comparison is a **one-off decision**, so it lives in
`scripts/baseline/` — a standalone crate with its own `Cargo.toml`, `[workspace]` empty, and no
membership in the repo's build. Nothing in `cargo test`, `cargo clippy --all-targets` or the
Windows cross-check ever sees `tantivy` (which pulls a C-toolchain compression backend and a large
tree). If the gate ever says "swap", `tantivy` becomes a real dependency with a real justification
and the script is retired; until then it is a measurement, not a dependency.

**Fixed preconditions, so the comparison means something:** same corpus, same chunk texts
(`janus chunks --json --text` feeds both), same 35 queries with the same graded expectations, same
`nDCG@10` and `FixedClock(2025-04-01)`, same document-level grading (a document's score is its max
chunk, M3 §3.1), `tantivy`'s own tokenizer and default BM25 — i.e. **out-of-the-box tantivy against
out-of-the-box janus**, which is the swap decision anybody would actually face. Differences in
tokenization are part of the result, not a bug to normalise away. The script prints both numbers
with denominators and the ratio.

## 4. Decisions

| # | Question | Default | Where it lands |
|---|---|---|---|
| V-1 | Index format version | **`format_version = 2`**: a new payload file, new manifest fields, a new definition of "generation". An M3 index is refused and rebuilt with one warning. | `src/index/mod.rs`, `store.rs` |
| V-2 | `vectors.f32` layout | Headerless, little-endian `f32`, row-major, row *i* = snapshot chunk *i*, `rows × dim` values. Shape lives in the manifest only. | `src/index/vectors.rs` |
| V-3 | Publication of two payloads | Vectors → index → manifest, each from its own temp file under `.janus/lock`, each followed by a dir `fsync`. **Both** payloads are hash-validated; generation identity is `(format_version, index_sha, vectors_sha)`. Recovery is a rebuild (§3.1 table). | `src/index/store.rs` |
| V-4 | Does a lexical-only read validate vectors | **No.** `LoadOptions.want_vectors` is false when α = 0; the file is not opened. Stated, with the consequence. | `src/index/store.rs`, `src/search/mod.rs` |
| V-5 | Trait shape | `pub trait Embedder: Send + Sync` with `id()` and `embed(&[&str]) -> Result<Vec<Vec<f32>>, EmbedError>` (SPEC's signature); `&dyn` everywhere, resolved once in `main`. | `src/index/embed.rs` |
| V-6 | `HashEmbedder` | Signed feature hashing of tokenized terms into 64 dims, L2-normalised, blake3 for determinism. A test double: no quality number may come from it. | `src/index/embed.rs` |
| V-7 | Who normalises and validates vectors | The builder: every row normalised, `dim` checked, finite checked, norm ∈ {0} ∪ [0.999, 1.001]. A bad row **fails the publish**; never a zero-row substitution. | `src/index/build.rs` |
| V-8 | Cosine | `dot` over unit rows, clamped to `[0,1]`; a zero row scores 0.0, never `NaN`. | `src/index/vectors.rs`, `score.rs` |
| V-9 | Admission rule | `relevance > 0`, not `lexical > 0`. This is what lets a semantic-only match enter, and what makes α = 0 exactly M3. | `src/search/mod.rs` |
| V-10 | α | `[embedding] alpha`, default `0.65`, clamped to `[0,1]` with a config warning; `0.0` whenever there is no embedder. `α = 0` is the exact M3 path. | `src/config.rs`, `src/search/mod.rs` |
| V-11 | Vector reuse key | Per document: `vec_key = blake3(embedder_id ‖ title)` + chunk-id set + chunk count + validated previous vectors. **Never `ChunkId` alone** — it excludes the title, which is in the embedded text. | `src/index/build.rs` |
| V-12 | Cross-document vector dedupe | None in M4. One row per chunk occurrence, mirroring postings; a copied document is embedded twice. | `src/index/build.rs` |
| V-13 | The `embeddings` feature | **Not default.** `fastembed` is optional; the Windows cross-check and default `cargo test` never build it. No download, ever; the model is read from disk or the build degrades. | `Cargo.toml`, `src/index/embed.rs` |
| V-14 | `--no-embeddings` | On `index`: publish a lexical generation and remove `vectors.f32`. On `search`/`facts`: α = 0 and **touch nothing** on disk. | `src/cli.rs`, `src/index/cmd.rs`, `src/search/mod.rs` |
| V-15 | `facts --query` under hybrid | **Lexical only** (α = 0). `facts` is a listing of authoritative facts whose order is authority; semantic narrowing would surface every active decision for any query. Recorded as reversible if the eval disagrees. | `src/search/facts.rs` |
| V-16 | Rayon / parallel top-k | **Not in M4.** Scalar `chunks_exact(dim)` fold; the sweep prints the latency. A measurement that needs parallelism is an M8 decision, and SPEC's "rayon top-k" row moves there. | §9 |
| V-17 | Where quality is measured | CI keeps the lexical report; the hybrid number is a local acceptance run with a real model; the α sweep is the instrument; the fixture is a smoke test only. No quality floor in `cargo test`. | §3.5, `scripts/eval.sh` |
| V-18 | The `tantivy` baseline | `scripts/baseline/`, a standalone crate outside the dependency graph, run by hand and by `scripts/eval.sh --baseline`. D20's gate is read from its output. | `scripts/baseline/` |
| V-19 | Report shape | `search --json` gains a top-level `alpha` and `degraded` becomes a list of reasons; `scores.semantic` becomes non-zero. Additive, so `api` stays `1`. | `src/search/mod.rs` |
| V-20 | Embedder identity vs model weights | `EmbedderId` = name/dim/dtype/normalisation. The model file is not hashed (100 ms/process); swapping weights under the same name needs `--rebuild`, documented. | `src/index/embed.rs` |
| V-21 | A vectors-less generation seen by a process that can embed | **Stale** → rebuild with one `stale: embedder … != embedder none` line; `--no-rebuild` refuses. | `src/index/store.rs` |

## 5. Phases

Five phases. Each is one PR, branched from `main` after the previous merge. The seams are chosen so
that no phase needs a model to be reviewable, and so that the two genuinely risky pieces — the
two-file publication (P1) and the blend (P2) — do not share a diff.

**Budget note, from M3's own log.** Both M3's phase 5 and phase 6 measured roughly 2× their written
estimate, and phase 6 had to split into four PRs after the fact (W-1). The estimates below are
therefore stated as upper bounds with a named split seam, and the rule is: **if a phase measures
over ~450 logic lines before tests, split it along the seam in the same session rather than
discovering it at `scripts/pr-size.sh` time.**

### Phase 1 — `Embedder`, `HashEmbedder`, `vectors.f32`, two-file publication

**Goal:** the format and the trait, end to end through the real binary, with no model and no
scoring change. After this phase `janus index` can write a `vectors.f32` and a reader can load and
validate it; nothing uses the vectors yet, so a regression is a storage bug and nothing else.

**Files:** `src/index/embed.rs` (new), `src/index/vectors.rs` (new), `src/index/mod.rs`,
`src/index/store.rs`, `src/index/build.rs` (plumbing only), `src/config.rs` (the `hash` model name
and `model_dir`), `src/lib.rs`, `tests/publish.rs`, `tests/vectors.rs` (new).

**Build (~430 logic lines):**
1. `EmbedderId`, `Embedder`, `EmbedError`, `HashEmbedder` (blake3 bucket + sign, tokenizer terms,
   normalise), `embed::resolve` returning `Ready`/`Degraded` (V-5, V-6, V-13).
2. `vectors.rs`: `Vectors::{new, encode, decode, row, len, dim}`, `normalize`, `dot`, and the
   §3.1 validation set. `to_le_bytes`/`from_le_bytes` over `chunks_exact(4)`; no `unsafe`, no
   `transmute`, no `bytemuck` (M8).
3. `Manifest`: `embedder: Option<EmbedderId>`, `vectors_sha`, `vectors_len`; `format_version = 2`;
   `Expected` compares the embedder id and the manifest's three-way consistency.
4. `store::publish`: three new `Stage`s, `vectors.f32` written and renamed first, manifest last,
   `vectors_sha` from the bytes actually written, and the best-effort removal of a stale
   `vectors.f32` when the new generation has none.
5. `store::load`: `want_vectors` + `embedder` in `LoadOptions`; read, hash, decode and validate
   `vectors.f32` when wanted; the retry/refuse outcomes of V-3 with the file name in the message.
6. `build::plan` carries `Option<Vectors>` through unchanged (produced by an embedder only in
   tests this phase); `Publication.vectors`; `BuildStats` counters default to zero.
7. `main.rs`: resolve the embedder **once**, pass it down; the CLI gains `--no-embeddings` on
   `index` only (V-14, `search`/`facts` in P2).

**Tests (~380 lines):** `Vectors` round-trip and validation (wrong length, non-multiple-of-4,
`NaN`, `inf`, wrong row count, un-normalised row, `dim = 0`); `dot`/`normalize` hand-computed
values including the zero row and `cos(a,a) = 1`; `HashEmbedder` determinism, unit norm,
`cos(a,a)`/ordering properties, empty text; `resolve` degradation for each absent-model case with
no network; publication stage order (vectors before index before manifest) through
`publish_with_hook`; a reader at every stage still sees one validated generation; the four rows of
the §3.1 recovery table, with the post-interruption states **built directly** (never by racing a
publisher); a flipped byte, a truncation, a zeroed file and a missing `vectors.f32` each rebuild
with one cause and exit 0, while `--no-rebuild` refuses and leaves `.janus/` byte-identical;
`format_version = 1` is refused with a warning; `want_vectors = false` never opens the file
(asserted by deleting it and reading successfully).

**Handoff (paste):** "Implement Phase 1 of M4 for janus-mem per `docs/m4-breakdown.md` §3.1, §3.2
and Phase 1. Read `src/index/store.rs` (publication + reader) and `docs/m3-breakdown.md` §3.5
first. `vectors.f32` is headerless LE f32, row-major, row *i* = snapshot chunk *i*, shape in the
manifest; `format_version` becomes 2; **both** payloads are hash-validated because a new-vectors/
old-index pair looks valid and scores wrong. No `unsafe`, no `bytemuck`, no `mmap`, no `rayon`, no
network. A bad vector row fails the publish; a corrupt file rebuilds. Tests build post-crash states
directly and never sleep. Record deviations in the M4 phase log, not a new journal."

**Accept:** `cargo test --all-targets` green; a `janus index` writes a `vectors.f32` whose
`blake3` matches the manifest and whose row count equals `chunks`; `--no-embeddings` writes none
and removes an existing one; the Windows cross-check green with default features.
**Split seam if over budget:** P1a = `embed.rs` + `vectors.rs` + unit tests (pure, no I/O),
P1b = manifest + publication + reader + integration tests.

---

### Phase 2 — Hybrid scoring: α, cosine, the admission rule, the report

**Goal:** the ranking contract. After this phase `janus search` is hybrid when an embedder exists
and **score-identical to M3** when it does not. Table-driven tests, no I/O, no model.

**Files:** `src/score.rs` (cosine use + docs), `src/search/mod.rs` (query vector, α, admission,
report fields), `src/search/facts.rs` (α = 0, V-15), `src/config.rs` (alpha + clamping warning),
`src/cli.rs` (`--no-embeddings` on `search`/`facts`, `--alpha`? no — config only), `src/main.rs`,
`tests/cli_search.rs`, `tests/golden_search.rs`, `tests/eval_quality.rs` (Options field),
`tests/fixtures/vault/janus.toml`.

**Build (~260 logic lines):** query embedding (one call, `None` for an empty query); the semantic
scan (`chunks_exact(dim)` fold, V-16); α resolution (`[embedding] alpha`, clamp + warning, `0.0`
without an embedder); the V-9 admission change; `degraded` as `Vec<String>` and the top-level
`alpha` in the report (V-19); `Options.no_embeddings`; `want_vectors` wiring; the fixture config
gains `[embedding] model = "hash-64"` so the corpus is hermetic and hybrid end to end.

**Tests (~340 lines):** table-driven α endpoint and midpoint values against hand-computed
`relevance` (including the five rows of §3.3); `relevance` recomputed from the JSON DTO for every
hit and compared within `1e-12`; **α = 0 is score-identical to M3** — `search --json
--no-embeddings` has a `results` array byte-identical to the committed snapshot under the same
redactions, with `alpha` and `degraded` as the only permitted differences;
a semantic-only document is admitted at α = 0.65 and is absent at α = 0; the zero-row document
scores `semantic = 0` with no `NaN`; an empty query calls no embedder and leaves `semantic = 0`;
`facts` reports `alpha = 0` and its `--query` narrowing is unchanged; the degradation matrix's rows
are each asserted through the CLI (feature-off is the default build, so this is the common case);
`degraded` is empty with a working embedder; the golden `search --json` is regenerated and its diff
reviewed field by field.

**Handoff (paste):** "Implement Phase 2 of M4 per `docs/m4-breakdown.md` §3.3, §3.6 and Phase 2.
`semantic = clamp(dot,0,1)` over unit rows; `relevance = α·semantic + (1−α)·lexical`; **admission
is `relevance > 0`**, which is what makes α = 0 exactly M3 — assert that with the existing golden,
not with a new one. α comes from `[embedding] alpha` (default 0.65, clamped with a warning) and is
`0.0` without an embedder. `facts --query` stays lexical. Report a top-level `alpha` and make
`degraded` a list of reasons. No wall clock, no sleeps, no quality floor in `cargo test`."

**Accept:** all §3.3 rows green; α = 0 score-identical to M3 (`results` array unchanged); clippy
clean; the golden diff is only the fields P2 intends to move.

---

### Phase 3 — The real embedder behind a feature, batching, and the CI feature matrix

**Goal:** `fastembed` in the crate without letting it into the default build, the Windows gate, or
any test's network path.

**Files:** `src/index/embed.rs` (`fastembed` module + `resolve` branch), `Cargo.toml`,
`.github/workflows/ci.yml`, `AGENTS.md` (the gate line names the feature set), `README.md`
(install with `--features embeddings`).

**Build (~300 logic lines):** `#[cfg(feature = "embeddings")] pub mod fastembed`; `FastEmbed`
holding the model, an `EmbedderId { dim, dtype: "f32", normalisation: "l2" }`, and `embed()`
delegating to the model; model resolution from `[embedding] model_dir` → `JANUS_MODEL_DIR` →
fastembed's cache dir, **read-only, no download** (V-13); batching in `build` driven by
`[embedding] batch_size` with one progress line per build (never per batch); a `CountingEmbedder`
test wrapper that records batch sizes and text lengths; error mapping so a model failure names the
path and the embedder.

**Tests (~180 lines):** `resolve` returns `Degraded` for each absent-model case and never blocks or
downloads; batching splits by `batch_size`, preserves order, and sends every chunk exactly once
(asserted with `CountingEmbedder`, no sleeps); a batch whose row count or dim is wrong fails the
build with a message naming the chunk index; the feature-off build still passes the whole suite.
The `fastembed` module itself has no unit test that runs a model — a test would need weights and
would be slow; its coverage is the shared normalise/validate path and the CI `check`, which is
stated rather than pretended.

**Handoff (paste):** "Implement Phase 3 of M4 per `docs/m4-breakdown.md` §3.2 and Phase 3. The
`embeddings` feature is **not** default: the Windows cross-check and default `cargo test` must
never build `fastembed`. **No download path anywhere** — model files are read from disk or the
build degrades with a reason naming the directory it looked in. Batch by `[embedding]
batch_size`; validate every row (dim, finite, norm) and fail the publish on a violation. CI gets a
feature job that runs `cargo check`/`clippy --all-targets --features embeddings` on Linux; Windows
stays feature-free. Tests use `CountingEmbedder`, never a real model, never the network."

**Accept:** `cargo check --all-targets` (no features) and `cargo check --all-targets --features
embeddings` both green; the Windows cross-check green without the feature; `cargo test
--all-targets` green on both feature sets with no network.

---

### Phase 4 — Incremental embedding, `janus index` integration, and keeping the two files in step

**Goal:** §3.4 as code. After this phase the milestone's first two done-when conditions are
checkable numbers rather than claims.

**Files:** `src/index/build.rs`, `src/index/store.rs` (`rebuild_plan` passes the embedder and the
previous vectors), `src/index/cmd.rs` (receipt, `--dry-run` planning), `src/search/mod.rs`
(`LoadOptions` plumbing), `tests/cli_index.rs`, `tests/incremental.rs` (new).

**Build (~280 logic lines):** `vec_key`; the document-level reuse predicate of §3.4; row copying by
ordinal into the new positions; `embedded_chunks`/`reused_vector_chunks`; `plan` becomes fallible
(the embedder can fail) and every caller updates; `--dry-run` computes the reuse decision and
reports `would_embed` **without calling the embedder**; the receipt gains `dim`, `embedded`,
`reused_vectors`, `vectors_bytes`; `--rebuild` drops the previous vectors as well as the previous
snapshot.

**Tests (~380 lines):** the nine acceptance cases of §3.4, each comparing incremental against a
clean rebuild (snapshot equality after normalising `built_at`, byte equality of `vectors.f32`, and
the named counter); case 5 (derived-title rename re-embeds) is the §3.7 correction pinned as a test,
and case 2 (tags-only edit re-embeds nothing) is the counterweight; the in-step assertion over the
whole fixture plus a property test that generates random document edits and checks
`vectors.len() == chunks.len()` and row-to-chunk correspondence after every incremental build; a
tampered previous generation (wrong row count, un-normalised row) forces a full re-embed rather than
a silently misaligned array; `--dry-run` writes nothing and reports a non-zero `would_embed` after a
body edit.

**Handoff (paste):** "Implement Phase 4 of M4 per `docs/m4-breakdown.md` §3.4 and Phase 4. Reuse is
per document and keyed on `vec_key = blake3(embedder_id ‖ title)` **plus** the chunk-id set and the
chunk count — `ChunkId` alone is wrong because it excludes the title, which is in `embed_text`.
Reused rows are copied by ordinal into the new snapshot positions; a row is addressed by chunk
position, so a misalignment is the worst bug this milestone can produce — assert the invariant after
every build. `--dry-run` never calls the embedder. Deviations go in the phase log."

**Accept:** nine acceptance cases green; a second `janus index` reports `embedded 0`; a
single-document body edit reports `embedded = chunks(that doc)`; the receipt is stable across runs.

---

### Phase 5 — Eval upgrade, the `tantivy` baseline, and docs close-out

**Goal:** the D20 gate, the α sweep, and the prose that stops the next session re-deriving all of
this. Logic-light, measurement-heavy, deliberately last so the numbers describe merged code.

**Files:** `tests/eval_quality.rs` (α sweep hooks), `scripts/eval.sh` (`--sweep`,
`--baseline`), `scripts/baseline/` (new standalone crate: `Cargo.toml`, `src/main.rs`),
`docs/m4-breakdown.md` (phase logs), `SPEC.md` (§9's corrections), `README.md`,
`.github/workflows/ci.yml` (if the eval job needs the feature job's ordering).

**Build (~150 logic lines, rest is tests/scripts/docs):** `eval_quality` gains
`JANUS_EVAL_ALPHA`/`--alpha` handling and the semantic-distribution print; `scripts/eval.sh`
sweeps α, prints `rescued`/`broken`, and delegates `--baseline`; `scripts/baseline` reads
`tests/eval/queries.jsonl`, indexes the same chunk texts in `tantivy`, grades documents the same
way, and prints both `nDCG@10` numbers and their ratio.

**Tests (~260 lines):** the eval reports compile and print both metrics under
`--features embeddings` and without it; the baseline script's grading function is unit-tested
against hand-computed `nDCG@10` values for two tiny result lists (so the comparison's arithmetic is
not itself the thing being trusted); `scripts/eval.sh` prints denominators. Docs: the phase log
carries the sweep table and the chosen α; SPEC gets §9's edits; README says what M4 added and how to
enable it.

**Handoff (paste):** "Implement Phase 5 of M4 per `docs/m4-breakdown.md` §3.5, §3.8 and Phase 5.
No quality threshold goes into `cargo test`: the eval is `#[ignore]`d, CI's job is
`continue-on-error`, and the α decision is a human reading the sweep and writing the numbers into
the phase log. The `tantivy` baseline is a standalone crate under `scripts/baseline/` so the repo's
dependency graph and the Windows check never see it. Update SPEC per §9 and record every deviation
in §10–§14."

**Accept:** `scripts/eval.sh --sweep` prints the table; `scripts/eval.sh --baseline` prints both
`nDCG@10` numbers; the phase logs are filled; README and SPEC match the code.

## 6. Cross-cutting requirements

- **No network, anywhere.** Not in the crate, not in tests, not in CI (§10 of SPEC). Model files are
  read from disk; absence is a stated degradation, never a download.
- **No `unsafe`, no `bytemuck`, no `transmute`.** `f32` bytes are converted with
  `to_le_bytes`/`from_le_bytes` over `chunks_exact(4)`.
- **Determinism.** No float `==`; `HashEmbedder` and the normaliser are deterministic by
  construction (blake3, fixed iteration order); no `HashMap` iteration reaches output; vectors are
  written in snapshot order; two runs produce byte-identical `vectors.f32`. A **real** model is not
  guaranteed bit-identical across CPUs, which is why no golden file may contain a vector or a
  `vectors_sha` value.
- **Fail open on reads, fail closed on writes.** A corrupt/missing/mismatched vectors file rebuilds
  (or refuses under `--no-rebuild`); a bad row from an embedder aborts the publish with the chunk
  index in the message.
- **No content in the index.** Vectors are numbers derived from text that is never stored; `--json`
  still returns spans, not bodies (§8.5). A vector *is* a lossy encoding of the text and the plan
  says so: it stays in `.janus/`, which §8.6's rule 1 already excludes from vault content.
- **Dependencies arrive as arguments.** The `Embedder` is resolved once in `main` and passed down;
  nothing below the boundary reads the environment or the feature flag at runtime.
- **Cross-platform (§8.7).** Plain `fs::read`/`write`; LE bytes written explicitly; no `std::os::*`;
  `rel_path` unchanged; the only platform-sensitive thing is the optional `fastembed` build, which
  is why it is not a default feature.
- **Size.** `vectors.f32` is `rows × dim × 4`; no padding, no header, no per-row alignment. §3.1
  states the heap cost at the reference vault and who fixes it (M8).

## 7. Definition of done, made checkable

Blocking (CI / `cargo test`):

1. **Second index run embeds nothing.** `janus index && janus index` reports
   `embedded_chunks == 0`, `reused_vectors == chunks`, and publishes a `vectors.f32` byte-identical
   to the first run's.
2. **A title-only edit re-keys vectors and nothing else.** Frontmatter `title:` changed → that
   document re-embeds; every other document does not; `vectors.f32` equals a clean rebuild's.
3. **§3.7's conditional is pinned both ways.** A pure rename with a declared `title:` reports
   `embedded_chunks == 0`; a pure rename with a derived title re-embeds exactly that document and
   nothing else.
4. **α = 0 is score-identical to M3.** The `results` array of `search --json --no-embeddings` is
   byte-identical to the M3 golden under the same redactions, with `alpha` and `degraded` as the
   only permitted differences; and `relevance == α·semantic + (1−α)·lexical` holds for every hit
   of every committed golden within `1e-12`.
5. **A semantic-only match is admitted and ranks.** A fixture chunk that shares no query term but
   has a non-zero cosine appears at α = 0.65 and is absent at α = 0.
6. **The two files are one generation.** A new-vectors/old-index pair, a new-index/old-manifest
   pair, a flipped byte, a truncated file, a missing file, a wrong row count, a `NaN` row and an
   un-normalised row each either rebuild with one cause line and exit 0, or refuse under
   `--no-rebuild` while leaving `.janus/` byte-identical. Never a panic, never a silent misalignment.
7. **`format_version = 2`.** An index built by M3 is refused with a warning and rebuilt; an index
   built by M4 is never read as v1.
8. **Vectors and index stay in step** after every incremental build (§3.4 case 9), asserted
   structurally over the fixture and by a property test over generated edits.
9. **Degradation is the matrix of §3.6**, one row per state, asserted through the CLI, including
   `--no-embeddings` on all three commands and the vectors-less-generation rebuild (V-21).
10. **No network, no wall clock, no sleeps.** `tests/no_wall_clock.rs` stays green; no test opens a
    socket or reads a model.
11. **Gates:** `cargo fmt --all --check`; `cargo clippy --all-targets -- -D warnings`; the same two
    with `--features embeddings`; `cargo test --all-targets` on both feature sets; `cargo doc
    --no-deps` (0 warnings, and the `fastembed` module documented or `#[doc(hidden)]`-free);
    the Windows cross-check **without** `embeddings`; `scripts/pr-size.sh` per PR.

Milestone acceptance (manual, recorded in the PR that changes the ranking):

12. `scripts/eval.sh --sweep` prints `active-top-1` and `nDCG@10` for the six α values with
    denominators, on the author's vault **and** the fixture; the chosen default α and the numbers
    are written into §14. The fixture result is labelled a smoke test.
13. `scripts/eval.sh --baseline` prints the hand-rolled and `tantivy` `nDCG@10` with the ratio, and
    the D20 verdict (keep / swap) is written into §14. A "swap" produces a follow-up milestone, not
    work in this one.

Not gating: hybrid retrieval numbers in CI (they need a model and a vault), the latency figures
(printed, never asserted), and the semantic-distribution shape (a diagnostic).

## 8. PR plan and size budget

One phase per PR, each branched from `main` after the previous merge. Logic and test estimates are
upper bounds; the seam column says where to split **in the same session** if the diff comes in over
~450 logic lines.

| PR | Branch | Logic est. | Tests est. | Why it splits here | Split seam |
|---|---|---|---|---|---|
| P1 | `m4-vectors` | ~430 | ~380 | The format plus the two-file publication: the only new correctness risk that is not about ranking. | P1a pure modules / P1b store protocol |
| P2 | `m4-hybrid` | ~260 | ~340 | The ranking contract; a mistake here is invisible in a green suite, so it gets its own diff. | α + admission / report + goldens |
| P3 | `m4-fastembed` | ~300 | ~180 | Build, portability and model-availability risk, isolated from scoring and storage. | `fastembed` module / CI matrix |
| P4 | `m4-incremental-vectors` | ~280 | ~380 | The cache contract and the row-alignment invariant; belongs beside P1's format, not inside it. | `vec_key` + reuse / receipt + `--dry-run` |
| P5 | `m4-eval-baseline` | ~150 | ~260 | Measurements and prose; changes shared eval files and SPEC, so it lands alone. | sweep / baseline script |

Total ≈ 1 420 logic lines across five PRs, every one under the 600 ceiling with room for review
fixes. Compare M3: ~1 580 across six. If the review adds a contract (it will — M2 and M3 both did),
the added work goes to the phase that owns the contract, not to a new phase.

## 9. SPEC corrections and gaps

None of these is an edit in *this* PR: this PR is the plan. Each is either a correction the code
already implies, or a gap the M4 implementation PR should close with one line.

1. **§11-M4's done-when conflates three things.** Replace "a moved-but-unchanged file is recognized
   as a move and costs zero embedding work" with "a rename that leaves the embedded text unchanged
   costs zero embedding work; a rename that changes the derived title re-embeds only that document".
   Move "exact-hash move detection (`path_history`)" to §11-M5 (§6.9 already assigns move handling
   there). Reason: §3.7 — the condition is only true when the document declares a `title:`.
2. **§11-M4's list is partly M3.** "`Index` metadata via `bincode`", "incremental reindex" and
   "`janus index`" shipped in M3 (D13, S-6, S-9). Delete them from M4 and add the two things M4
   actually adds: hybrid scoring and vector reuse.
3. **§11-M4's "<10 ms warm at 100k chunks" is an aspiration, not this milestone's gate** (§13 calls
   single-digit ms aspirational and forbids optimizing before measurement). Restate it as a printed
   measurement at 200k chunks with the two-file read path, and put the target where §13 already puts
   it: p50 ≤ 100 ms.
4. **§8.3 does not describe `vectors.f32`'s bytes.** Add: headerless, little-endian `f32`,
   row-major, row *i* = chunk *i*, `dim`/`len`/`sha` in the manifest, no padding; and that a
   generation is validated over **both** payloads because a new-vectors/old-index pair is otherwise
   indistinguishable from a good one.
5. **§8.3's rebuild-trigger list** gains "an embedder id, dim, dtype or normalisation mismatch" and
   "a `vectors_sha` mismatch observed by a reader".
6. **§8.3.1's heap budget is exceeded by M4's load path** (~307 MB at 200k × 384, §3.1). Add one
   sentence saying the whole array is read onto the heap until M8's mmap, so the table describes
   M8 and not M4. Without it, the next reader concludes the code violates the spec.
7. **§7.2 does not say that α is configurable or what it is when there is no embedder.** Add: α is
   `[embedding] alpha`, default `0.65`, and `0.0` whenever no embedder is available.
8. **§7.2 does not state the admission rule.** Wrap the cosine formula and the clamped range in
   SPEC's own words, then add: "a chunk is a candidate when `relevance > 0`, so a semantic-only
   match can enter the result set" — a real behaviour change from lexical-only that the spec should
   contain.
9. **§7.2's `"lexical": 12.4` in the §9.4 example is raw BM25**, while the code reports the
   normalised `[0,1]` value (M3 S-5). Fix the example; it is the one place where a reader
   implementing against the spec would get the DTO's meaning wrong.
10. **§8.2 lists `fastembed` as "Feature `embeddings`" without saying whether it is default, and
    D22 says the model is downloaded separately.** Add: **non-default** feature, and **no download
    from inside the tool in M4** — the model is read from `JANUS_MODEL_DIR`/`[embedding] model_dir`
    or the build degrades. This is the row that keeps the Windows gate honest.
11. **§11-M4 and §12.1 name `rayon`; M4 does not use it.** Move "rayon top-k" and the "concurrency
    without the GIL / rayon over 200k dot products" learn row to M8, together with the measurement
    that would justify them (V-16). M4's learn target is traits, `dyn`, `Send + Sync` and Cargo
    features.
12. **§9.1 lists `--no-embeddings` on `index` only**, and M3 did not implement it. M4 implements it
    on `index` *and* adds it to `search`/`facts`; §9.1 plus one sentence in §3.6
    (of this plan) is the contract.
13. **§8.3.1's "Document text: 0 B" stays true, but vectors are a derived copy of text.** One
    sentence in §8.5 or §8.6 saying that `.janus/vectors.f32` is derived from document content and
    is covered by the same "never vault content" rule as the rest of `.janus/`, since a vector is a
    lossy encoding of the passage.

Remaining open questions this plan does **not** resolve, listed so they are not silently decided:

14. **Should `--diversity`/MMR ever arrive?** §7.2 keeps it optional and off; §3.1 of M3 keeps one
    hit per document. With vectors available, MMR becomes cheap to implement — and it is still not
    in any milestone. It needs a product decision (does a caller want two passages from one
    document?), not a technical one.
15. **A real-vault eval corpus.** `tests/eval/queries.jsonl` is 35 fixture queries over 18
    keyword-rich documents (M3 W-10). The α decision (§3.5) is taken on the author's vault, which
    means it is not reproducible in CI. If the number ever needs to gate anything, the eval corpus
    becomes a milestone.
16. **Model-file integrity.** V-20 records the cost argument for not hashing the weights. If M8
    finds a cheap partial hash acceptable, it becomes a manifest field and a rebuild trigger.

## 10. Implementation log — Phase 1 (trait, `vectors.f32`, publication)

_Nothing recorded yet. Decisions taken during implementation go here as `V-*` rows with a one-line
why, and any deviation from this plan is amended in the plan text in the same PR._

## 11. Implementation log — Phase 2 (hybrid scoring)

_Nothing recorded yet._

## 12. Implementation log — Phase 3 (real embedder behind a feature)

_Nothing recorded yet._

## 13. Implementation log — Phase 4 (incremental embedding)

_Nothing recorded yet._

## 14. Implementation log — Phase 5 (eval, baseline, docs)

_Nothing recorded yet. The α sweep table, the chosen default, and the D20 verdict belong here._
