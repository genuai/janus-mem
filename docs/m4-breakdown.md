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
2. `bincode` metadata, incremental reindex and `janus index` are M3 work that §11-M4 still claims.
   They are **already done** and are removed from §0's scope. (`--dry-run` is M3 work too, but
   §11-M4 does not claim it; an earlier draft of this correction said it did.)
3. "A moved-but-unchanged file costs zero embedding work" is **not** free, and the brief's
   reasoning is wrong for the most common rename. `ChunkId` excludes the title (D48, CH-5), but
   the embedded text is `Chunk::embed_text` = *title* + breadcrumb + text (S-3), and the title is
   the frontmatter `title:` **or the filename stem**. A rename of a document that declares no
   `title:` therefore changes the embedded text and must re-embed it. §3.7 states the correct
   conditional and proposes the SPEC sentence that separates "recognized as a move" (M5) from
   "costs zero embedding work" (M4). No `path_history`, no file-hash comparison and no move
   detection is built in M4.

**Status: reviewed and closed (five rounds, 17 findings, none rejected). Ready for P1a.** No
implementation exists. Every phase log in §10–§14 is empty on purpose.

**Revision note.** Five rounds of independent review, 17 findings, none rejected. Round 1 found
three blockers, two of them defects in **shipped M3 code** this plan was about to copy. Round 2
found three fixes locally right and globally inconsistent. Round 3 found two of those incomplete —
a title-only change slipped past the drop trigger, and `body_hash` was landing in a phase that
would need a second `format_version` bump. Round 4 found that round 3's solution had made the plan
contradict its own Phase 2: the phase that owns the hybrid golden needs a CLI-produced
`vectors.f32`, which only exists once the embedder is wired into the build. P1b now owns that
wiring; P4 owns reuse and carry-forward. Round 5 confirmed the two claims that mattered
(no golden moves; `HashEmbedder` is deterministic) and closed the door on two stale estimates.
Every finding and its resolution is in §9.0.

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
/// The directories are an argument, so nothing below `main` reads `std::env` (V-25).
pub enum Resolved { Ready(Box<dyn Embedder>), Degraded(&'static str, String) }
pub struct ModelDirs { pub config: Option<PathBuf>, pub env: Option<PathBuf>, pub cache: PathBuf }
pub fn resolve(config: &Config, dirs: &ModelDirs) -> Resolved;

impl EmbedderId {
    /// Length-prefixed, unambiguous encoding for cache keys (V-22).
    pub fn to_bytes(&self) -> Vec<u8>;
}
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

pub struct DocMeta {
    /* … unchanged … */
    /// blake3 of `Document.body` **raw**, never a normalised body (V-26) — the
    /// witness both caches key on. Lands in P1b with the layout, not in P4.
    /// `content_hash` stays, as the whole-file fact M5's move detection wants.
    pub body_hash: Blake3Hash,
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

The table has two "next read" columns because **V-4 makes them different**, and a single column
would teach a rule the code does not follow.

| Failure point | On-disk state | Read that wants vectors | Same read, lexical only (V-4) | `--no-rebuild` (either kind) |
|---|---|---|---|---|
| before the vectors rename | the previous (index, vectors, manifest) triple | loads | loads | loads |
| **between the vectors rename and the index rename** | **new vectors, old index, old manifest** | `vectors_sha` mismatch → retry → rebuild under the lock | **loads**, because the index and manifest it validates agree with each other | exit 1 only on the wanting path, naming `.janus/vectors.f32`; the lexical path still loads |
| **between the index rename and the manifest rename** | **new vectors + new index, old manifest** | `index_sha` mismatch → retry → rebuild | `index_sha` mismatch → retry → rebuild | exit 1, names `.janus/index.bin`, writes nothing |
| after the manifest rename | the new triple | loads | loads | loads |

The middle rows are the contract: recovery is a **rebuild, not a restore** (§3.5 of M3 stands).
`index.bin` and `vectors.f32` are replaced by rename; the previous bytes are gone. Nothing in M4
adds versioned generations. The reader's guarantee is unchanged in kind — it validates the bytes it
actually read — and is now stated over three files instead of two. Row 2's lexical path is not a
hole: it validated a consistent (index, manifest) pair and cannot observe the vectors it declined
to read. DoD 6 asserts both paths.

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

**Why "a second build is byte-identical" holds, and what would break it.** Accumulation is
**integer-valued counts** in an `f32` array, exact below 2²⁴, so summation order cannot change a
bit; the norm is a single pass over `0..dim` with IEEE-correctly-rounded `sqrt` and divide; buckets
and signs come from `blake3(term)`. The invariant to preserve: **if a later change adds weights**
(tf-idf, position decay, sublinear scaling), the counts stop being exact and the byte-identical
claim in P1b's Accept silently becomes false. That is a comment in the code, not a hope.

**No network at runtime, and no download (§10).** `resolve` reads a model from disk. It does not
open a socket, and it does not block. If the files are not there, `resolve` returns `Degraded` with
a reason that names the directory it looked in. A test asserts that a `ModelDirs` whose directory
does not exist yields `Degraded`, with no socket and no timeout. (The `embeddings` *build* may fetch
the ONNX Runtime binary; §6 scopes the rule to runtime, because pretending a feature job that
compiles `ort` never touches the network would be a false claim in a plan that forbids false
claims.)

**Paths are resolved at the edge, like every other dependency.** `resolve(&Config, &ModelDirs)`
takes the directories as an argument; `ModelDirs` is built in `main.rs` from `[embedding]
model_dir`, then `JANUS_MODEL_DIR`, then `fastembed`'s cache directory. A library function that
read `std::env` would break AGENTS.md's "a `Config` rather than `std::env::var` deep in the engine"
and would make the "no `std::env` below `main`" test impossible to write — which is how the first
draft of this section contradicted itself.

**Selection is one function, at the edge.** `main` calls `embed::resolve(&config, &dirs)` once and
passes `Option<&dyn Embedder>` down. Nothing below `main` reads the environment or the feature
flag: the feature controls whether the `fastembed` module *exists*, not what any function decides
at runtime.

**The dispatch table, because "model name → behaviour" was left implicit and two sections then
disagreed about it:**

| `[embedding] model` | Feature off | Feature on, files present | Feature on, files absent |
|---|---|---|---|
| `hash-64` (or any `hash-<n>`) | `Ready(HashEmbedder)` | same | same |
| any other name | `Degraded("not compiled")` | `Ready(FastEmbed)` | `Degraded("model not found in <dir>")` |
| `[embedding] enabled = false` | `Degraded("disabled in janus.toml")` | same | same |

`HashEmbedder` is compiled unconditionally and is reachable by name, because the fixture corpus and
half the integration tests need a deterministic embedder without a model (P2). That makes it
selectable in production, so `resolve` emits a `ConfigWarning`: "`model = hash-64` is a test
double; its vectors are bag-of-words, not semantic". §3.6's matrix is keyed on the **resolved**
embedder, not on the feature, so a missing vectors file is a consequence of `Degraded` (on a first
build, or after a degraded build that changed a document), never of the build flag alone — which is
what the first draft got wrong when it wrote "feature off ⇒ no `vectors.f32`" while also giving the
fixture a working hash embedder, and again when it let a degraded `index` delete a good file.

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

**The admission rule has a cost, and M3's notes change meaning under it.** With a real embedder
nearly every chunk has `relevance > 0`, so:

- `matched` approaches the corpus size, and the note `"N more document(s) matched; raise --limit"`
  becomes a corpus-count line nobody can act on. It stays (one line is not a flood) but its text
  names the basis: `"N more document(s) have relevance > 0; raise --limit to see them"`.
- `src/search/mod.rs` currently allocates one `Breakdown` and two `Vec` entries per admitted chunk.
  Under lexical-only that was one entry per *matching* chunk; under hybrid it is one per chunk,
  ~200k per query at the reference vault. That is a per-candidate cost the 77 MFLOP figure does
  not cover, so **P2 measures the whole `query()` call, not the dot product**, and prints it.
- **Pre-declared fix if the measurement misses p50 ≤ 100 ms** (§13): two passes — pass 1 computes
  `relevance` alone (an `f64` per chunk, no `Breakdown`, no `Vec`) and keeps each document's
  argmax; pass 2 builds a `Breakdown` for the winners only. It is the shape §8.3.1 already
  prescribes ("top-k without a full sort"), it does not change any score, and it is deliberately
  **not** written now, because D29 forbids optimising before the measurement exists.

**Empty query ⇒ no embedder call.** `facts` and the eval's exhaustive pass query with no text; the
query vector is `None`, `semantic = 0` for every chunk, and admission falls back to lexical. This
keeps those commands at M3 cost and removes `embed("")` from the hot path.

### 3.4 Vector reuse: the key must include everything that is embedded

M3 keys token reuse per document on `lex_key = blake3(tokenizer_fp ‖ chunker_fp ‖ title)` plus the
chunk-id set, the chunk count and the file's `content_hash` (§3.4 of M3, S-9), because `ChunkId`
excludes the title while the indexed text includes it. Embeddings have that defect plus two of
their own, so they get their own key (and P4 narrows both witnesses, V-26):

| Identity | Formula | Purpose |
|---|---|---|
| `ChunkId` | `blake3(heading_path ‖ 0x00 ‖ normalized_text)` | content identity, and the **content component** of a cache key — never the whole key |
| `lex_key` | `blake3(tokenizer_fp ‖ chunker_fp ‖ title)` | "the tokens this document contributes are unchanged" |
| `vec_key` | `blake3(embedder_id.to_bytes() ‖ 0x00 ‖ title)` | "the text this document asks the embedder for is unchanged" |

`vec_key` needs the embedder (a different model is a different vector) and the title (the only
input to `embed_text` that `ChunkId` excludes). It deliberately does **not** include the tokenizer
fingerprint: a tokenizer change alters `lex_key` and must re-tokenize, but it does not change
`embed_text`, so re-embedding everything would be pure waste.

**`EmbedderId::to_bytes()` is a function, not a concatenation (V-22).** `EmbedderId` has four
fields, and `"hash" + "-64"` must not hash the same as `"hash-" + "64"`. The encoding
length-prefixes every field with `to_le_bytes`, exactly as `chunk_id` and `lex_key` already do, and
both §3.4 and V-11 cite that one function instead of spelling a formula twice.

**`ChunkId` covers normalised text; `embed_text` contains raw text (V-23).** `chunk_id` hashes
`normalize(text)` — CRLF folded to LF, trailing whitespace stripped, edges trimmed — while
`Chunk::embed_text` appends `Chunk::text(body)`, which is the **raw** span. So an EOL-only or
trailing-whitespace-only rewrite of a file (a `git checkout` under a different `core.autocrlf`,
SPEC §8.7 and CH-9) keeps every `ChunkId` byte-identical and changes every embedded text.

**The witness is `body_hash`, not `content_hash` (V-26).** `DocMeta.content_hash` is blake3 of the
**whole file** (`src/vault/mod.rs`), frontmatter included. Using it as the witness is correct but
far too coarse: adding one tag to a document re-hashes the file and would force a re-embed of text
that did not change — which makes this plan's own acceptance case 2 impossible, and which is
already M3's behaviour for tokens (a tag edit re-tokenizes today). So `DocMeta.body_hash =
blake3(doc.body)` **lands in P1b with the layout** and **both** predicates key on it:

**Hash the raw `Document.body`, never a normalised body.** `Document.body` is the verbatim
Markdown the chunker and the embedder read; a normalised hash would fold CRLF and reopen exactly
the EOL hole V-23 closes. It is one blake3 per document per build over bytes that are already in
memory.

| What changed | `body_hash` | `vec_key` | chunk-id sequence | result |
|---|---|---|---|---|
| one tag, status, scope or date | same | same | same | **reuse** (vectors and tokens) |
| `title:` | same | **differs** | same | re-embed and re-tokenize |
| body text | **differs** | same | differs | re-embed and re-tokenize |
| line endings only | **differs** | same | same | re-embed and re-tokenize (V-23) |
| sections reordered | **differs** | same | **differs** | re-embed and re-tokenize |
| nothing | same | same | same | reuse |

`body_hash` is conservative: a body edit that changes no chunk's embedded text (say, an extra blank
line that only moves a span) still re-embeds. That is the right direction — a needless embed costs
one document, a stale vector costs correctness — and it is one blake3 per document per build.

Reuse is per document, and it is a contract with the same shape as §3.4 of M3 — **the conjuncts are
ordered cheapest first**, and the chunk comparison is a **sequence**, not a set:

```
reuse_vectors(doc) ⟺  old.body_hash == new.body_hash            (cheap; the raw text the embedder sees)
                   ∧  old.vec_key == new.vec_key                 (embedder + title)
                   ∧  old.embedder == new.embedder               (also in the manifest check)
                   ∧  the old chunk-id sequence == the new one    (order, V-24)
                   ∧  the previous generation's vectors validated
                   ∧  the old row range has room for every new row
```

**A set is not enough (V-24), and the fix is defence in depth rather than a bug fix.** The first
draft required only that every new `ChunkId` be *present* in the old snapshot and that the count
match — then copied rows by ordinal. Reordering two sections leaves the set of
`(heading_path, text)` pairs and the count unchanged, so reuse would fire and section A's vector
would be attached to section B's chunk: exactly "the worst failure this milestone can produce".
The comparison is therefore `old_chunks.iter().map(|c| c.id).eq(chunks.iter().map(|c| c.id))`.

**Correction to round 1: M3 is not broken today.** An earlier revision of this section claimed
shipped `build::reusable` reuses the wrong term frequencies after a reorder. It does not: the
`content_hash` conjunct fires first and denies reuse on any byte change, so the set check is
unreachable. The hole is latent, and the sequence form removes the dependence on that accident.
P4 therefore **does not edit M3's predicate** for ordering; it changes M3's witness from
`content_hash` to `body_hash` (V-26) because that is what makes acceptance case 2 true, and a test
that fails before the change and passes after it is the only evidence worth having.

What is reused is **rows only**, copied into the new snapshot positions. Rows are addressed by
chunk position, so a reused document whose chunks moved must have its rows copied to the new
positions; with the sequence conjunct, ordinal *is* that mapping. If the old generation has no
vectors, or its array failed validation, or its row count does not match, the whole corpus
re-embeds — a wrong row order silently attaches one document's vector to another.

**No cross-document dedupe (V-12).** Two documents that share a `ChunkId` and a title produce
identical rows; M4 still writes two, exactly as postings are per occurrence (§3.4 of M3). Deduping
would need a second key and a second index to undo it, for a saving that only exists in corpora
full of copied documents. Recorded as a known, harmless redundancy. Note what follows: "identical
`ChunkId` and title produce identical rows" is **not** a claim this plan makes, because identical
`ChunkId`s with different titles or different line endings do **not** (V-23); see §9's correction
to D24/D48, which phrased the old contract more strongly than the code can honour.

**Acceptance** (each compares the incremental result with a clean rebuild: identical snapshot after
normalising `built_at`, identical `vectors.f32` bytes, and the named counter):

1. Frontmatter `title:` edited → tokens and vectors both re-keyed for that document only;
   `embedded_chunks == chunks(doc)`.
2. **Frontmatter change that touches neither title nor body** (tags, status, scope, date) → **zero**
   re-embedding **and** zero re-tokenizing. This case is the reason for `body_hash` (V-26): with
   `content_hash` as the witness it is impossible, and it is already impossible for tokens in M3
   today, so P4's test must fail on `main` and pass after the witness change.
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
9. **EOL rewrite (V-23).** A CRLF file rewritten with LF, and a file whose trailing spaces are
   stripped, each keep every `ChunkId` and change every embedded text → that document re-embeds
   and **nothing is reused**; a clean rebuild's `vectors.f32` is byte-identical to the incremental
   one. The token half asserts `retokenized_chunks == chunks(doc)` for the same rewrite.
10. **Section reorder (V-24).** Swap two sections of one body (same text, same heading paths, new
    order) → the document does not reuse, and **row *i* still belongs to chunk *i***; the published
    array equals a clean rebuild's byte for byte. Note this case passes on `body_hash` alone, which
    is the honest reason it is a regression guard and not evidence for the sequence conjunct.
11. **Vectors and index stay in step**: after every incremental build, `vectors.len() ==
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
writes into it. `α` is what the scorer receives. The rows are keyed on the **resolved** embedder
(§3.2's dispatch table), never on the build flag alone: `hash-64` resolves even when the feature is
off, and `enabled = false` degrades even when it is on.

| # | State | `janus index` | `janus search` | α | `degraded` entry |
|---|---|---|---|---|---|
| 1 | No embedder resolved (feature off, `enabled = false`, or model absent) | lexical generation | lexical | 0.0 | the resolving reason: `embeddings not compiled; lexical only` / `embeddings disabled in janus.toml; lexical only` / `model not found in <dir>; lexical only` |
| 2 | Embedder resolved | generation with vectors | hybrid | 0.65 | *(empty)* |
| 3 | No embedder on **this build** (feature off, disabled, or `--no-embeddings`) while the previous generation had vectors | **carry the rows forward; drop `vectors.f32` only if a document changed** (below) | — | — | — |
| 4 | `--no-embeddings` on `search`/`facts` | — | lexical; `.janus/` written **only** by an S-8 build, which is published without vectors | 0.0 | `--no-embeddings: lexical only` |
| 5 | Index has no vectors, embedder resolved | — | stale → rebuild (one `stale:` line) | 0.65 | *(empty after the rebuild)* |
| 6 | **Index has vectors, no embedder resolved** | row 3's rule | **loads, never rebuilds** | 0.0 | reason from row 1 |

**V-21 is one-directional, and that is the fix rather than a caveat.** The first draft said
"`Expected.embedder` is the id this process would embed with, or `None` when it cannot", and
evaluated it as plain equality. A feature-off binary reading a vectors-bearing generation would
then compare `None` against `Some(id)`, rebuild without vectors, **delete `vectors.f32`**, and hand
the featured binary a generation it must rebuild and re-embed (SPEC §13: ≤60 s, 307 MB). Two
binaries on one machine would ping-pong the cache forever, and the matrix had no row for the state
that starts it. The rule is therefore an ordering, not an equality:

```
manifest.embedder == Some(id)  ∧  resolved == None      →  acceptable (row 6): load, α = 0, say why
manifest.embedder == None      ∧  resolved == Some(id)  →  stale   (row 5): rebuild once, then stable
manifest.embedder == Some(a)   ∧  resolved == Some(b)   →  stale when a ≠ b (different vectors)
```

A generation that *has* vectors is never stale for a process that cannot embed: the vectors are
strictly more information than that process is reading, and rebuilding would destroy them. A
generation *without* vectors is stale for a process that can embed, because otherwise vectors would
never appear without a manual `--rebuild`; the cost is one model load and one build, announced on
one `stale:` line, and `--no-rebuild` still refuses and writes nothing.

**Row 3: carry vectors forward, drop only what cannot be carried.**
The first draft removed `vectors.f32` unconditionally, which turns one degraded `janus index` into
a full re-embed for the next featured run. Instead the build **re-evaluates the §3.4 predicate with
the manifest's embedder id substituted for the one it does not have**, and the file is removed only
when at least one document fails it.

Substituting the id is the load-bearing part (round-3 fix). A degraded build that carried a row
while stamping a *fresh* `vec_key` would launder staleness: the next featured run would compare
`old.vec_key == new.vec_key`, get a match, and reuse the old row against a snapshot whose title had
changed. A title change is exactly the case §3.7 describes — `chunk_id` excludes the title and the
body is untouched, so `body_hash` and the chunk-id sequence both say "unchanged" while the embedded
text is different. Evaluating the predicate under `Some(kept_id)` covers title, body, order and
embedder in one rule instead of four.

**Two branches, two manifest states, and they must not be confused (round-2 fix).** On
carry-forward the manifest keeps `embedder: Some(<the previous id>)`, `vectors_sha` and
`vectors_len` **describing the file that is still there**. The manifest describes the file, not the
writer's capability — which is exactly the rule row 6 already relies on. Writing `embedder: None`
while keeping the rows would violate §3.1's "all three `Some` together or `None` together", lose
`dim` (the reader must not infer a shape from a byte count), and leave the carried rows
**unreusable**, because P4's predicate compares `old.embedder == new.embedder`: the next featured
run would then hit row 5 and re-embed the corpus, which is the cost row 3 exists to prevent.

On the drop branch the manifest writes all three as `None`, the file is removed, and one stderr
line says `vectors dropped: N document(s) changed and this build cannot embed`. If nothing changed,
the vectors survive a degraded `index` untouched — which is also why row 6 is safe.

**Which phase owns what (round-4 correction).** P1b **wires the embedder into `build::plan` and
embeds every chunk on every build, with no reuse** — about fifteen lines on top of
`Publication.vectors` and `resolve`. That is what makes P1b's own acceptance case true ("`janus
index` writes a `vectors.f32` whose blake3 matches the manifest") and what lets Phase 2's hybrid
golden exist at all; an earlier revision said "no CLI path produces vectors before P4", which
contradicted Phase 2 outright. P4 then adds the three things this plan gives it: the reuse
predicate, the `body_hash` switch that consumes P1b's new field, and **row 3's carry-forward**,
which needs the predicate to be correct (substituting a coarse `content_hash` stand-in in P1b and
widening it later is the throwaway this ordering avoids). Until P4, a degraded publish **drops**
the file and says so; nothing can carry a row that the predicate has not yet been written to
justify. `DocMeta.body_hash` itself is a **P1b** layout field, because a second layout change would
need a second `format_version` bump (V-1, and
AGENTS.md's rule that *any* on-disk layout change bumps it).

`--no-embeddings` means two different things and the plan says so in one place (V-14): on `index` it
*is* a statement about the generation, and on `search`/`facts` it is a statement about the query.
Row 4's honesty note: a **first** `janus search --no-embeddings` has no index, so S-8 builds one
and `.janus/` is written — the row says "touches nothing except an S-8 build" rather than the
first draft's "touch nothing", which was false and which DoD 9 asserted. The alternative (making
`--no-embeddings` on a read imply `--no-rebuild`) couples two unrelated things: a user who asks not
to *use* vectors has not asked not to *build* an index.

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

**Fixed preconditions, so the comparison means something:** same corpus, same chunk texts, same 35
queries with the same graded expectations, same `nDCG@10` and `FixedClock(2025-04-01)`, same
document-level grading (a document's score is its max chunk, M3 §3.1), `tantivy`'s own tokenizer and
default BM25 — i.e. **out-of-the-box tantivy against out-of-the-box janus**, which is the swap
decision anybody would actually face. Differences in tokenization are part of the result, not a bug
to normalise away. The script prints both numbers with denominators and the ratio.

**"Same chunk texts" means the whole text, so the baseline cannot use `--text`.**
`chunk::dump` renders a preview capped at `PREVIEW_CHARS = 120` with whitespace collapsed
(`src/chunk/dump.rs`), so `janus chunks --json --text` would hand `tantivy` a 120-character prefix
while janus scores the whole chunk — and the difference would then be attributed to BM25. The
baseline crate depends on `janus-mem` by path and calls `chunk_body` + `Chunk::embed_text` directly,
so both systems index byte-identical text. That is also why it is a crate and not a shell script:
the chunker is the thing that must not be re-implemented.

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
| V-11 | Vector reuse key | Per document: `body_hash` + `vec_key = blake3(EmbedderId::to_bytes() ‖ 0x00 ‖ title)` **+ the chunk-id sequence + a validated previous generation**. **Never `ChunkId` alone** — it excludes the title and it hashes normalised text while `embed_text` is raw. | `src/index/build.rs` |
| V-12 | Cross-document vector dedupe | None in M4. One row per chunk occurrence, mirroring postings; a copied document is embedded twice. "Same `ChunkId` and title ⇒ identical row" is **not** claimed: different line endings break it (V-23). | `src/index/build.rs` |
| V-13 | The `embeddings` feature | **Not default.** `fastembed` is optional; the Windows cross-check and default `cargo test` never build it. **No runtime download and no socket**; the model is read from disk or the build degrades. The feature job's build may fetch `ort` (§6). | `Cargo.toml`, `src/index/embed.rs` |
| V-14 | `--no-embeddings` | On `index`: publish lexically, **carrying vectors forward** and dropping them only when a document's rows cannot be carried. On `search`/`facts`: α = 0, and `.janus/` written only by an S-8 build. | `src/cli.rs`, `src/index/cmd.rs`, `src/search/mod.rs` |
| V-15 | `facts --query` under hybrid | **Lexical only** (α = 0). `facts` is a listing of authoritative facts whose order is authority; semantic narrowing would surface every active decision for any query. Recorded as reversible if the eval disagrees. | `src/search/facts.rs` |
| V-16 | Rayon / parallel top-k | **Not in M4.** Scalar `chunks_exact(dim)` fold; the sweep prints the latency. A measurement that needs parallelism is an M8 decision, and SPEC's "rayon top-k" row moves there. | §9 |
| V-17 | Where quality is measured | CI keeps the lexical report; the hybrid number is a local acceptance run with a real model; the α sweep is the instrument; the fixture is a smoke test only. No quality floor in `cargo test`. | §3.5, `scripts/eval.sh` |
| V-18 | The `tantivy` baseline | `scripts/baseline/`, a standalone crate outside the dependency graph, run by hand and by `scripts/eval.sh --baseline`. D20's gate is read from its output. | `scripts/baseline/` |
| V-19 | Report shape | `search --json` gains a top-level `alpha` and `degraded` becomes a list of reasons; `scores.semantic` becomes non-zero. Additive, so `api` stays `1`. | `src/search/mod.rs` |
| V-20 | Embedder identity vs model weights | `EmbedderId` = name/dim/dtype/normalisation. The model file is not hashed (100 ms/process); swapping weights under the same name needs `--rebuild`, documented. | `src/index/embed.rs` |
| V-21 | Embedder staleness is an **ordering**, not an equality | `Some(id)` manifest + no embedder resolved → **acceptable**, load at α = 0 with the reason in `degraded` (row 6). `None` manifest + an embedder resolved → stale, rebuild once (row 5). Both `Some` and different → stale. This is what stops a feature-off binary and a featured one from deleting each other's vectors. | `src/index/store.rs` |
| V-22 | Cache-key encoding | `EmbedderId::to_bytes()` length-prefixes every field (as `chunk_id`/`lex_key` already do); `vec_key` cites that one function. A bare concatenation lets `"hash"+"-64"` and `"hash-"+"64"` collide. | `src/index/embed.rs`, `build.rs` |
| V-23 | `ChunkId` is not a proxy for the embedded text | `chunk_id` hashes `normalize(text)`; `embed_text` appends the raw span, and prepends the title. So the predicate carries a raw-text witness and `vec_key` carries the title. An EOL-only rewrite re-embeds. | `src/index/build.rs` |
| V-24 | The chunk comparison is a **sequence** | `old_chunks.iter().map(|c| c.id).eq(new.iter().map(|c| c.id))`. A set plus a count permits a reorder, and rows are copied by position. This is defence in depth, not a bug fix: `body_hash` already denies reuse after a reorder, and M3's set check is likewise unreachable behind `content_hash`. P4 does not edit M3 for ordering. | `src/index/build.rs` |
| V-25 | Model directories are an argument | `resolve(&Config, &ModelDirs)`; `ModelDirs` is built in `main` from `[embedding] model_dir` → `JANUS_MODEL_DIR` → the cache dir. No `std::env` below `main`, which is what makes the no-env test possible. | `src/main.rs`, `src/index/embed.rs` |
| V-26 | The reuse witness is `body_hash`, not `content_hash` | New `DocMeta.body_hash = blake3(doc.body)` — **raw body, never normalised** — landed in P1b with the layout (so one `format_version` covers both changes), consumed by **both** predicates in P4 (vectors and M3's tokens). `content_hash` is the whole file, so a one-tag edit would re-embed and re-tokenize text that did not change — making acceptance case 2 impossible. Conservative by design: a body edit that alters no embedded text still re-embeds. | `src/index/mod.rs` (P1b), `src/index/build.rs` (P4) |

## 5. Phases

Five phases, **six PRs** — phase 1 is P1a + P1b (§8). Each PR is branched from `main` after the
previous merge. The seams are chosen so that no phase needs a model to be reviewable, and so that
the two genuinely risky pieces — the two-file publication (P1b) and the blend (P2) — do not share a
diff.

**Budget note, from M3's own log, with the real factors.** M3's phase 5 measured **597** logic
lines against a ~200 estimate (**3.0×**), and phase 6 measured **1384** against ~320 (**4.3×**) and
had to split into four PRs after the fact (W-1). "Roughly 2×" was the first draft's summary and it
understated the error by half. So: the estimates below are upper bounds with a named split seam,
**P1 is already split into two PRs** rather than hoped to fit, and the rule is: **if a phase
measures over ~450 logic lines before tests, split it along the seam in the same session rather
than discovering it at `scripts/pr-size.sh` time.**

### Phase 1 — `Embedder`, `HashEmbedder`, `vectors.f32`, two-file publication

**Three PRs, not two (P1a, P1b, P1c): P1b measured 680 logic lines and the pre-authorised
writer/reader seam was therefore taken.** The split point is the one §8 named: P1b is the manifest,
the nine publication stages, the degraded drop and the first-time embed wiring; P1c is `Expected`'s
V-21 ordering, the payload read/hash/decode and the recovery tests. Both halves ship in the same
session and the P1c branch is stacked on P1b.

**Two PRs, not one (P1a, P1b), because review round 1 showed the estimate was the M3 mistake
again.** P1 contains everything M3's phase 5 contained — six-stage publication, unique temps,
reader retry, recovery — *plus* a new binary format, a trait, and a manifest change. M3 measured
that protocol at **597** logic lines against a ~200 estimate. An "~430" estimate for strictly more
work is not an upper bound, it is a hope. The split is along the protocol boundary P-9 already
recommended: **P1a is the pure modules** (`embed.rs`, `vectors.rs`, no I/O, unit tests), **P1b is
the store protocol** (manifest, publication stages, reader validation, recovery tests).

**Goal:** the format and the trait, end to end through the real binary, with no model and no
scoring change. After these two PRs `janus index` can write a `vectors.f32` and a reader can load
and validate it; nothing uses the vectors yet, so a regression is a storage bug and nothing else.

**Files:** `src/index/embed.rs` (new), `src/index/vectors.rs` (new), `src/index/mod.rs`,
`src/index/store.rs`, `src/index/build.rs` (plumbing only), `src/config.rs` (the `hash` model name
and `model_dir`), `src/lib.rs`, `src/main.rs` (`ModelDirs`), `tests/publish.rs`,
`tests/vectors.rs` (new).

**Build (~520 logic lines, P1a ~200 + P1b ~320):**
1. **P1a.** `EmbedderId` with `to_bytes()` (V-22), `Embedder`, `EmbedError`, `HashEmbedder`
   (blake3 bucket + sign, tokenizer terms, normalise), `EmbedderId` → `ConfigWarning` for the test
   double (§3.2 table).
2. **P1a.** `vectors.rs`: `Vectors::{new, encode, decode, row, len, dim}`, `normalize`, `dot`, and
   the §3.1 validation set. `to_le_bytes`/`from_le_bytes` over `chunks_exact(4)`; no `unsafe`, no
   `transmute`, no `bytemuck` (M8). `Vectors::is_empty` accompanies `len` (clippy's
   `len_without_is_empty`); an empty payload is a valid zero-row generation.
3. **P1a.** `embed::resolve(&Config, &ModelDirs)` returning `Ready`/`Degraded` for the dispatch
   table's three rows (V-5, V-6, V-13, V-25). **P1a implements the feature-off column only**;
   P3 owns the feature-on arms and their tests. Its one `config.rs` edit is the
   `ConfigWarning::TestDoubleEmbedder` variant, emitted by `resolve` through `tracing` because the
   fixed `Resolved` return type has no warnings channel (P1a-4).
4. **P1b (split).** `Manifest`: `embedder: Option<EmbedderId>`, `vectors_sha`, `vectors_len`;
   `format_version = 2`; `DocMeta.body_hash` lands here too — a layout field belongs with the
   version bump, and putting it in P4 would make two layout changes share one `format_version`
   (V-1, round-3 fix). **`Expected`'s V-21 ordering and the three-way consistency check moved to
   P1c** when the measured diff forced the writer/reader split (§10 P1b).
5. **P1b.** `store::publish`: three new `Stage`s, `vectors.f32` written and renamed first, manifest
   last, `vectors_sha` from the bytes actually written, and a degraded publish **drops** any existing
   file with one stderr line — **after** the manifest rename, so the old commit record never names a
   file that is gone (review fix, P1b-10). The **carry-forward** half of row 3 of §3.6 is P4's, because it needs
   the reuse predicate to be correct.
6. **P1c (split from P1b).** `store::load`: `want_vectors` + `embedder` in `LoadOptions`; read,
   hash, decode and validate `vectors.f32` when wanted; the retry/refuse outcomes of V-3 with the
   file name in the message; row 6 of §3.6 (vectors-bearing generation, no embedder) never rebuilds.
7. **P1b.** `build::plan` **embeds every chunk on every build** through the resolved embedder — no
   reuse, no predicate — so a real CLI build produces `vectors.f32` and Phase 2 has something to
   rank. **`plan` becomes fallible here** (the embedder can fail) and every caller updates in this
   PR; P4 only adds the reuse decision. `Publication.vectors`; `BuildStats` counters default to
   zero; `tests/fixtures/vault/janus.toml` gains `[embedding] model = "hash-64"` (no golden moves,
   because scoring still passes `semantic = 0.0, alpha = 0.0` until P2); `main.rs` builds `ModelDirs`
   and resolves the embedder **once**; the CLI gains `--no-embeddings` on `index` only (V-14,
   `search`/`facts` in P2).

**Tests (~380 lines):** `Vectors` round-trip and validation (wrong length, non-multiple-of-4,
`NaN`, `inf`, wrong row count, un-normalised row, `dim = 0`); `dot`/`normalize` hand-computed
values including the zero row and `cos(a,a) = 1`; `EmbedderId::to_bytes` distinguishes
`("hash","-64")` from `("hash-","64")`; `HashEmbedder` determinism, unit norm, `cos(a,a)`/ordering
properties, empty text; `resolve` for **every row of the feature-off column** of the dispatch table
(the feature-on columns need P3's module, so they are P3's tests), including that `hash-64`
resolves with the feature off and that an absent model directory degrades without a socket;
publication stage order (vectors before index before manifest) through `publish_with_hook`; a
reader at every stage still sees one validated generation; **every row of the §3.1 table in both
"wants vectors" and "lexical only" form**, with the post-interruption states **built directly**
(never by racing a publisher); flipped byte, truncation, zeroed file, missing `vectors.f32`, wrong
row count; `format_version = 1` refused with a warning; `want_vectors = false` never opens the file
(asserted by deleting it and reading successfully); **row 6**: a feature-free read of a
vectors-bearing generation loads, reports `degraded`, and leaves `.janus/` byte-identical; **row 3,
P1b half**: a degraded `index` drops `vectors.f32` and says so in exactly one stderr line, and the
next `format_version = 2` read of that generation succeeds with `embedder: None`. The **carry-forward
half is P4's**, with the predicate that makes it correct.

**Handoff (paste):** "Implement Phase 1 of M4 for janus-mem per `docs/m4-breakdown.md` §3.1, §3.2
and Phase 1. Read `src/index/store.rs` (publication + reader) and `docs/m3-breakdown.md` §3.5
first. `vectors.f32` is headerless LE f32, row-major, row *i* = snapshot chunk *i*, shape in the
manifest; `format_version` becomes 2; **both** payloads are hash-validated because a new-vectors/
old-index pair looks valid and scores wrong. No `unsafe`, no `bytemuck`, no `mmap`, no `rayon`, no
network. A bad vector row fails the publish; a corrupt file rebuilds. Tests build post-crash states
directly and never sleep. Record deviations in the M4 phase log, not a new journal."

**Accept:** `cargo test --all-targets` green; a `janus index` on the fixture (which now names
`hash-64`) writes a `vectors.f32` whose `blake3` matches the manifest and whose row count equals
`chunks`; a second `index` produces the **same bytes**, because P1b re-embeds everything and the
embedder is deterministic; `--no-embeddings` **removes** any existing file with one stderr line
(unconditionally in P1b — carry-forward is P4's); the Windows cross-check green with default
features. Expected churn: `tests/publish.rs` enumerates the index directory, so it now sees a third
file; nothing else moves, because scoring still passes `semantic = 0.0`.

**Split seam, now permanent:** P1a = `embed.rs` + `vectors.rs` + their unit tests (pure, no I/O);
P1b = manifest + publication + reader + integration tests. Both are in §8's table with their own
estimates.

---

### Phase 2 — Hybrid scoring: α, cosine, the admission rule, the report

**Goal:** the ranking contract. After this phase `janus search` is hybrid when an embedder exists
and **score-identical to M3** when it does not. Table-driven tests, no I/O, no model.

**Files:** `src/score.rs` (cosine use + docs), `src/search/mod.rs` (query vector, α, admission,
report fields), `src/search/facts.rs` (α = 0, V-15), `src/config.rs` (alpha + clamping warning),
`src/cli.rs` (`--no-embeddings` on `search`/`facts`; α is config-only, not a flag), `src/main.rs`,
`tests/cli_search.rs`, `tests/golden_search.rs`, `tests/eval_quality.rs` (Options field). The
fixture's `[embedding] model = "hash-64"` arrived in **P1b**, so this phase reads vectors that
already exist rather than creating them.

**Build (~260 logic lines):** query embedding (one call, `None` for an empty query); the semantic
scan (`chunks_exact(dim)` fold, V-16); α resolution (`[embedding] alpha`, clamp + warning, `0.0`
without an embedder); the V-9 admission change; the reworded "more matched" note and the
per-candidate cost measurement (§3.3); `degraded` as `Vec<String>` and the top-level `alpha` in the
report (V-19); `Options.no_embeddings`; `want_vectors` wiring. The fixture already names
`[embedding] model = "hash-64"` from P1b, which is safe because `HashEmbedder` compiles without the
feature and `resolve` warns that it is a test double (§3.2); P2 is the phase that starts **using**
the vectors P1b already writes, so the corpus becomes hybrid end to end here and not before.

**Tests (~340 lines):** table-driven α endpoint and midpoint values against hand-computed
`relevance` (including the five rows of §3.3); `relevance` recomputed from the JSON DTO for every
hit of **the hybrid golden** and compared within `1e-12` (on the α = 0 golden the identity reduces
to `relevance == lexical` and would pass with a broken formula); **α = 0 is score-identical to
M3** — `search --json --no-embeddings` has a `results` array byte-identical to the M3 snapshot
under the same redactions, with `alpha` and `degraded` as the only permitted differences;
a semantic-only document is admitted at α = 0.65 and is absent at α = 0; the zero-row document
scores `semantic = 0` with no `NaN`; an empty query calls no embedder and leaves `semantic = 0`;
`facts` reports `alpha = 0` and its `--query` narrowing is unchanged; every row of §3.6's matrix is
asserted through the CLI, with the rows that need a real model marked `#[ignore]`d and named in P3's
handoff; `degraded` is empty with a working embedder; the reworded "more matched" note is asserted
verbatim.

**Two snapshots, not one.** `tests/golden_search.rs` keeps the M3 file as the α = 0 reference
(`search_no_embeddings`, regenerated once for `alpha`/`degraded` only) and adds
`search_hybrid` for the default intent at α = 0.65 with `semantic > 0`. A single snapshot cannot be
both "unchanged from M3" and "regenerated for hybrid", and the first draft asked it to be both.

**Handoff (paste):** "Implement Phase 2 of M4 per `docs/m4-breakdown.md` §3.3, §3.6 and Phase 2.
`semantic = clamp(dot,0,1)` over unit rows; `relevance = α·semantic + (1−α)·lexical`; **admission
is `relevance > 0`**, which is what makes α = 0 exactly M3 — assert that with the existing golden,
not with a new one. α comes from `[embedding] alpha` (default 0.65, clamped with a warning) and is
`0.0` without an embedder. `facts --query` stays lexical. Report a top-level `alpha` and make
`degraded` a list of reasons. No wall clock, no sleeps, no quality floor in `cargo test`."

**Accept:** all §3.3 rows green; α = 0 score-identical to M3 (`results` array unchanged); both
goldens reviewed; clippy clean; the golden diff is only the fields P2 intends to move.

---

### Phase 3 — The real embedder behind a feature, batching, and the CI feature matrix

**Goal:** `fastembed` in the crate without letting it into the default build, the Windows gate, or
any test's network path.

**Files:** `src/index/embed.rs` (`fastembed` module + `resolve` branch), `Cargo.toml`,
`.github/workflows/ci.yml`, `AGENTS.md` (the gate line names the feature set), `README.md`
(install with `--features embeddings`).

**Build (~300 logic lines):** `#[cfg(feature = "embeddings")] pub mod fastembed`; `FastEmbed`
holding the model, an `EmbedderId { dim, dtype: "f32", normalisation: "l2" }`, and `embed()`
delegating to the model; model resolution **from `ModelDirs`** (V-25), **read-only, no download**
(V-13); batching in `build` driven by `[embedding] batch_size` with one progress line per build
(never per batch); a `CountingEmbedder` test wrapper that records batch sizes and text lengths;
error mapping so a model failure names the path and the embedder.

**How the feature job gets `ort`, stated so "no network" is not a false claim.** `fastembed` →
`ort` fetches a prebuilt ONNX Runtime archive during the **build**. That is a build-time dependency,
not a runtime one, and it is why the feature is non-default. Phase 3 names exactly one job that is
allowed to reach the network (`embeddings`, ubuntu, `cargo check`/`clippy`/`test --features
embeddings`), and `SWATINEM/rust-cache` caches the result so it is not fetched every run. If the
archive cannot be fetched in CI, the fallback is `ort`'s system-library path against a
preinstalled runtime, recorded as a decision; the default path never depends on it.

**Tests (~180 lines):** `resolve` returns `Degraded` for each absent-model case and never blocks or
downloads; batching splits by `batch_size`, preserves order, and sends every chunk exactly once
(asserted with `CountingEmbedder`, no sleeps); a batch whose row count or dim is wrong fails the
build with a message naming the chunk index; the feature-off build still passes the whole suite.
The `fastembed` module itself has no unit test that runs a model — a test would need weights and
would be slow; its coverage is the shared normalise/validate path and the CI `check`, which is
stated rather than pretended.

**Handoff (paste):** "Implement Phase 3 of M4 per `docs/m4-breakdown.md` §3.2 and Phase 3. The
`embeddings` feature is **not** default: the Windows cross-check and default `cargo test` must
never build `fastembed`. **No runtime download and no socket** — model files are read from
`ModelDirs` or the build degrades with a reason naming the directory it looked in; the only
network use is the `embeddings` CI job fetching the `ort` archive at build time. Batch by
`[embedding] batch_size`; validate every row (dim, finite, norm) and fail the publish on a
violation. CI gets the `embeddings` job for `cargo check`/`clippy --all-targets`/`cargo test
--features embeddings` on Linux; Windows stays feature-free. Tests use `CountingEmbedder`, never a
real model."

**Accept:** `cargo check --all-targets` (no features) and `cargo check --all-targets --features
embeddings` both green; the Windows cross-check green without the feature; `cargo test
--all-targets` green on both feature sets, with no test opening a socket.

---

### Phase 4 — Incremental embedding, `janus index` integration, and keeping the two files in step

**Goal:** §3.4 as code. After this phase the milestone's first two done-when conditions are
checkable numbers rather than claims.

**Files:** `src/index/build.rs`, `src/index/store.rs` (`rebuild_plan` passes the embedder and the
previous vectors), `src/index/cmd.rs` (receipt, `--dry-run` planning), `src/search/mod.rs`
(`LoadOptions` plumbing), `tests/cli_index.rs`, `tests/incremental.rs` (new).

**Build (~320 logic lines):** `EmbedderId::to_bytes` feed into `vec_key` (V-22); the switch of both
predicates to the `body_hash` field P1b already lays out (V-26); the reuse predicate of §3.4 —
`body_hash`, `vec_key`, and the **chunk-id sequence** (V-23, V-24); row
copying into the new positions; **row 3's carry-forward branch** of §3.6, i.e. the same predicate
evaluated with the manifest's embedder id substituted when this build cannot embed (a title change
must drop, not carry); `embedded_chunks`/`reused_vector_chunks`; `--dry-run` computes the reuse
decision and reports `would_embed` **without calling the embedder**; the receipt gains `dim`,
`embedded`, `reused_vectors`, `vectors_bytes`; `--rebuild` drops the previous vectors as well as the
previous snapshot.

**Tests (~380 lines):** the **eleven** acceptance cases of §3.4, each comparing incremental against
a clean rebuild (snapshot equality after normalising `built_at`, byte equality of `vectors.f32`, and
the named counter); case 2 (a tags-only edit reuses everything) is the `body_hash` test (V-26) and
must **fail on `main`** before the witness change, for tokens as well as vectors — a test that
passes before the change proves nothing; case 5 (derived-title rename re-embeds) is the §3.7
correction pinned as a test; cases 9–10 (EOL rewrite, section reorder) are the two holes round 1
found, and case 10 is labelled a regression guard because `body_hash` alone would pass it; **row 3's
carry-forward**: a degraded `index` after a pure rename of a document with no declared `title:`
**drops** the file rather than carrying a row whose embedded text changed (the round-3 case), while
a degraded `index` after no change carries it byte-identically and the next featured run reports
`embedded 0`; the in-step assertion over the whole fixture plus a property test that generates random
document edits and checks `vectors.len() == chunks.len()` and row-to-chunk correspondence after every
incremental build; a tampered previous generation (wrong row count, un-normalised row) forces a full
re-embed rather than a silently misaligned array; `--dry-run` writes nothing and reports a non-zero
`would_embed` after a body edit.

**Handoff (paste):** "Implement Phase 4 of M4 per `docs/m4-breakdown.md` §3.4 and Phase 4. Reuse is
per document and keyed on **all three**: `body_hash` = blake3 of the raw body (not `content_hash`,
which is the whole file and would re-embed on a one-tag edit — V-26), `vec_key =
blake3(EmbedderId::to_bytes() ‖ 0x00 ‖ title)` (the title because `ChunkId` excludes it), and the
**chunk-id sequence** (a set plus a count permits a reorder, and rows are copied by position). Switch
M3's `build::reusable` from `content_hash` to the same `body_hash`, so a frontmatter-only edit reuses
tokens too; that test must fail before the change. Do **not** touch M3's set check for ordering —
`body_hash` already denies reuse after a reorder, so the sequence form is defence in depth. A row is
addressed by chunk position, so a misalignment is the worst bug this milestone can produce — assert
the invariant after every build. `--dry-run` never calls the embedder. Deviations go in the phase
log."

**Accept:** eleven acceptance cases green; case 2 fails on `main` and passes after the witness
change; a second `janus index` reports `embedded 0`; a single-document body edit reports `embedded =
chunks(that doc)`; an EOL-only rewrite re-embeds and reuses nothing; a section reorder leaves row *i*
on chunk *i*; the receipt is stable across runs.

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

- **No network at runtime.** No socket and no model download in the crate or its tests (§10 of
  SPEC). Model files are read from `ModelDirs`; absence is a stated degradation. The **one**
  exception is the `embeddings` CI job, whose *build* fetches the `ort` archive; the first draft's
  "not in CI" made that job and this rule mutually exclusive, so the rule now says what it means.
  No other job, and no test, reaches the network.
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
   **A metadata-only edit re-keys nothing** (V-26): changing tags, status, scope or date reuses
   every vector **and** every token, and the test fails on `main` before the witness change.
3. **§3.7's conditional is pinned both ways.** A pure rename with a declared `title:` reports
   `embedded_chunks == 0`; a pure rename with a derived title re-embeds exactly that document and
   nothing else.
4. **α = 0 is score-identical to M3, and the hybrid golden is where the formula is checked.** The
   `results` array of `search --json --no-embeddings` is byte-identical to the M3 snapshot under the
   same redactions, with `alpha` and `degraded` as the only permitted differences; and
   `relevance == α·semantic + (1−α)·lexical` holds within `1e-12` for every hit of the **hybrid**
   golden, where α = 0.65 and `semantic > 0`. Checking the identity only on the α = 0 golden would
   reduce it to `relevance == lexical` and pass with a broken formula.
5. **A semantic-only match is admitted and ranks.** A fixture chunk that shares no query term but
   has a non-zero cosine appears at α = 0.65 and is absent at α = 0.
6. **The two files are one generation**, in **both** read modes. A new-vectors/old-index pair, a
   new-index/old-manifest pair, a flipped byte, a truncated file, a missing file, a wrong row count,
   a `NaN` row and an un-normalised row each either rebuild with one cause line and exit 0, or
   refuse under `--no-rebuild` while leaving `.janus/` byte-identical. A **lexical-only** read of the
   new-vectors/old-index state loads, because the pair it validated agrees with itself (V-4). Never
   a panic, never a silent misalignment.
7. **`format_version = 2`.** An index built by M3 is refused with a warning and rebuilt; an index
   built by M4 is never read as v1.
8. **Vectors and index stay in step** after every incremental build (§3.4 cases 9–10), asserted
   structurally over the fixture and by a property test over generated edits; an EOL-only rewrite
   re-embeds, and a section reorder leaves row *i* on chunk *i*.
9. **Degradation is the matrix of §3.6**, one row per state, asserted through the CLI, including
   `--no-embeddings` on all three commands, the vectors-less-generation rebuild (V-21 row 5), the
   vectors-bearing generation read by a non-embedding binary (row 6, which must **not** rebuild),
   and a degraded `index` that carries vectors forward when nothing changed and **drops** them when a
   document's embedded text changed — including a title-only change (row 3, P4).
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
| P1a | `m4-embed-vectors` | ~200 | ~200 | Pure modules: `EmbedderId`, the trait, `HashEmbedder`, `Vectors` encode/decode/validate. No I/O, so no recovery test and no chance of a storage bug hiding a trait bug. | none needed; if it exceeds ~300, the hashing double ships separately |
| P1b | `m4-two-file-publish` | ~320 | ~380 | The protocol delta **plus the first-time embed wiring**: manifest fields, three new stages, dual-hash validation, V-21's ordering, `build::plan` embedding every chunk with no reuse. **~320, not ~230** (round 2), and **not 230+carry-forward** either (round 4): carry-forward is P4's, and the estimate is re-based on the work that is actually here. | writer (manifest + stages + embed wiring) / reader (validation + recovery table) — **pre-authorised as P1c if the diff measures over 450** |
| P1c | `m4-two-file-read` | ~240 | ~340 | The reader half of the same protocol: V-21's ordering in `Expected`, the three-way manifest check, `want_vectors` + `embedder` in `LoadOptions`, payload read/hash/decode, and the §3.1 recovery table asserted in both read modes. | taken: P1b measured **680 logic / 820 tests** against `main`, over the pre-authorised 450 seam |
| P2 | `m4-hybrid` | ~260 | ~340 | The ranking contract; a mistake here is invisible in a green suite, so it gets its own diff. | α + admission / report + goldens |
| P3 | `m4-fastembed` | ~300 | ~180 | Build, portability and model-availability risk, isolated from scoring and storage. | `fastembed` module / CI matrix |
| P4 | `m4-incremental-vectors` | ~320 | ~380 | The cache contract and the row-alignment invariant: the reuse predicate, the `body_hash` switch that both caches then share (V-26), and row 3's carry-forward, which needs the predicate to exist. | reuse predicate / carry-forward + receipt + `--dry-run` |
| P5 | `m4-eval-baseline` | ~150 | ~260 | Measurements and prose; changes shared eval files and SPEC, so it lands alone. | sweep / baseline script |

Total ≈ 1 550 logic lines across **six** PRs on five phases, every one under the 600 ceiling with
room for review fixes. P1 is two PRs on the strength of M3's measurement, not on the strength of
this estimate. The writer/reader split inside P1b is **not** taken now, for the reason M3's phase 5
gave when the same seam was proposed: the two halves share the `Publication` type and half the
acceptance cases, so two stacked PRs would move the same test harness twice. It is pre-authorised
if the measurement says the diff is over 450 — which is what P-9's "estimate from the protocol's
steps, not its bullets" is for. If the review adds a contract (it will — M2, M3, round 1 and round
2 here all did), the added work goes to the phase that owns the contract, not to a new phase.

## 9. Review round 1, SPEC corrections, and gaps

### 9.0 Review round 1 — resolutions

One independent reviewer (pi, `deepseek-v4.1-flash`, read-only, briefed to verify claims against the
code rather than trust the plan) returned 15 findings and the verdict *needs another round*. Every
finding was re-verified here before being accepted; none was rejected. The three blockers were real
defects in the first draft, and two of them were defects in **shipped M3 code** that M4's predicate
was about to copy.

| # | Finding (short) | Resolution | Where |
|---|---|---|---|
| 1 | `vec_key` misses `content_hash`: `ChunkId` hashes *normalised* text while `embed_text` is *raw*, so an EOL-only rewrite reuses stale vectors | **Accepted, blocker.** A raw-text witness becomes the first conjunct (narrowed from `content_hash` to `body_hash` in round 2, R2-1); new decision V-23; acceptance case 9 (EOL rewrite); `reuse_vectors` ordered cheapest-first. | §3.4, §3.7, V-11/V-23/V-26, P4 |
| 2 | "Copied by ordinal" + a set-based predicate misaligns rows after a section reorder | **Accepted, blocker.** The comparison is the chunk-id **sequence**; acceptance case 10. The same hole exists in M3's `build::reusable` for **tokens** — P4 fixes both. | §3.4, V-24, P4 |
| 3 | V-21 is one-directional, and the missing direction makes a feature-off and a featured binary delete each other's vectors | **Accepted, blocker.** Embedder staleness is an ordering, not an equality; row 6 added to the matrix; degraded `index` carries vectors forward and drops only what it cannot carry. | §3.6, V-14/V-21, P1, DoD 9 |
| 4 | `resolve`'s model dispatch was undefined; `hash-64` + feature-off contradicted "feature off ⇒ no vectors" | **Accepted.** Explicit dispatch table, keyed on the *resolved* embedder; `hash-64` always compiles, is selectable, and warns that it is a test double. | §3.2, §3.6, P2 |
| 5 | The §3.1 recovery table ignored V-4: a lexical-only read of "new vectors, old index" loads | **Accepted.** The table has two "next read" columns; the lexical path loads the consistent pair it validated, and DoD 6 asserts both modes. | §3.1, DoD 6 |
| 6 | DoD 4 asked one golden to be both unchanged and regenerated, so its identity check was vacuous | **Accepted.** Two snapshots: `search_no_embeddings` (the M3 reference) and `search_hybrid` (where the α identity is asserted at α = 0.65 with `semantic > 0`). | §7, P2 |
| 7 | Three of the thirteen SPEC corrections were already in the SPEC; item 2 over-claimed `--dry-run` | **Accepted.** Items 4/5/7 rewritten to name only what is missing, `--dry-run` removed from item 2. | §9.1 |
| 8 | The plan contradicts D24, D48, `m3-breakdown` §3.4 and `m2-breakdown` CH-10 on the embedding cache, and corrects none | **Accepted.** Four corrections added; the honest phrasing is "`ChunkId` is the content component of the key, not the whole key". | §3.4, §9.1 items 14–17 |
| 9 | "No network in CI" contradicts the feature job that fetches `ort` at build time | **Accepted.** The rule is scoped to runtime; the `embeddings` job is the one named exception and its fetch path is stated. | §6, P3 |
| 10 | The baseline cannot get the same chunk texts: `--text` truncates at 120 characters | **Accepted.** The baseline crate calls `chunk_body`/`embed_text` through the library instead of parsing a preview. | §3.8, P5 |
| 11 | `relevance > 0` admits nearly every chunk, so M3's "more matched" note becomes a corpus count and the per-candidate cost is unaccounted | **Accepted.** The note names its basis; the per-candidate cost is named and measured; the two-pass fix is pre-declared, not implemented (D29). | §3.3, P2 |
| 12 | The budget rationale said "roughly 2×" when M3 measured 3.0× and 4.3×; P1 did not survive its own data | **Accepted.** Real factors stated; P1 is now two PRs (P1a/P1b) in §8, estimated from the protocol's steps. | §5, §8 |
| 13 | `embed::resolve` read `JANUS_MODEL_DIR` below `main`, contradicting the plan's own no-env rule | **Accepted.** `ModelDirs` is built in `main` and passed in (V-25). | §3.2, §6, V-25 |
| 14 | "touch nothing on disk" is false when the index is absent and S-8 auto-builds | **Accepted.** The row says "touches nothing except an S-8 build", and the alternative (implying `--no-rebuild`) is rejected with a reason. | §3.6, V-14 |
| 15 | `vec_key` has no canonical byte encoding, and two spellings disagreed | **Accepted.** `EmbedderId::to_bytes()` with length-prefixed fields, cited from both places (V-22). | §3.4, V-11/V-22 |

Nothing was rejected. That is not a sign the review was perfect; it is a sign the first draft was
long enough to hide three alignment bugs behind prose that read confidently — which is the same
failure M3's review round found, one milestone later.

#### Review round 2 — verification of the resolutions

The same reviewer checked the amended contracts. Verdict: *still open* — three resolutions were
correct, three had introduced a new problem, and one claim of mine was simply false. All accepted.

| # | Round-2 finding | Resolution |
|---|---|---|
| R2-1 | `content_hash` is the **whole file**, so a one-tag edit re-embeds and re-tokenizes — which makes acceptance case 2 impossible, and it is already impossible for tokens in M3 | **Accepted, new issue.** The witness becomes `DocMeta.body_hash = blake3(body)` for **both** predicates (V-26), so case 2 becomes true and its test fails on `main` before the change. `content_hash` stays as the whole-file fact M5 will want. |
| R2-2 | The sequence conjunct is correct, but the claim that shipped M3 code reuses wrong term frequencies is **false** — `content_hash` gates the set check first | **Accepted, my claim was wrong.** V-24 is reworded: the sequence form is defence in depth and P4 does **not** edit M3's ordering check. A reorder test would have passed before the change, which is the standard this plan holds tests to. |
| R2-3 | Row 3's carry-forward writes `embedder: None` while keeping the rows, contradicting §3.1's three-way consistency, losing `dim`, and making the carried rows unreusable | **Accepted, new issue.** Carry-forward writes `embedder: Some(previous id)` with the kept file's `sha`/`len` — the manifest describes the file, not the writer's capability. `None` belongs to the drop branch only. |
| R2-4 | §9.1 item 5 adds nothing that SPEC §8.3 does not already say | **Accepted.** Item 5 withdrawn and marked as such, because deleting it silently would hide the mistake. |
| R2-5 | §9.1 item 4 still restates the SPEC's checksum rule; item 7 misses §16.1 | **Accepted.** Item 4 keeps only the reason and the headerless/row-*i* facts; item 7 adds the key to §16.1's `[embedding]` block. |
| R2-6 | P1a's test list promises the feature-on dispatch rows, which need P3 | **Accepted.** Scoped to the feature-off column; the other two columns are P3's tests. |
| R2-7 | P1b at ~230 keeps everything M3 measured at 597 and adds more — the same error one PR later | **Accepted in part.** Estimate raised to ~320 with the basis stated; the writer/reader split is pre-authorised as P1c but **not** taken now, for M3 phase 5's own reason (the halves share `Publication` and half the acceptance cases). Stated as a judgement, not hidden. |
| R2-8 | Two acceptance cases were numbered 9 | **Accepted.** Renumbered 9–11. |

The lesson is worth recording: round 1's fixes were locally correct and globally inconsistent. A
narrower witness is only narrower if it still covers the input, and a manifest field is only
consistent if it describes the same thing on both branches.

#### Review round 3 — verification of round 2's fixes

Verdict again *still open*: two of round 2's fixes were incomplete, and two new cross-phase
problems surfaced. All four accepted.

| # | Round-3 finding | Resolution |
|---|---|---|
| R3-1 | `body_hash` is the right witness for tokens, but nothing says to hash the **raw** body | **Accepted.** V-26 and §3.4 now say "raw `Document.body`, never a normalised body — a normalised hash would reopen the EOL hole". |
| R3-2 | Row 3's drop trigger is incomplete: a **title-only** change changes neither `body_hash` nor the chunk-id sequence, so a degraded `index` would carry a row and stamp a fresh `vec_key`, laundering the staleness | **Accepted, new issue.** The degraded build now re-evaluates the §3.4 predicate with the **manifest's** embedder id substituted, which covers title, body, order and embedder in one rule. The title case is added to P4's tests. |
| R3-3 | P1b's carry-forward needs a per-document "changed" test, but the predicate and `body_hash` arrive in P4 | **Accepted, new issue.** `DocMeta.body_hash` moves into **P1b** (it is a layout field); the carry-forward branch moves into **P4**, together with the predicate, and P1b simply drops vectors on a degraded publish. (The sentence this resolution originally gave as the reason — "nothing can produce vectors before P4" — was wrong, and R4-2 corrects it: P1b wires the embedder, so it *does* produce vectors and its drop branch is reachable and tested.) |
| R3-4 | `format_version = 2` would cover two different layouts if `body_hash` landed in P4 | **Accepted, same fix as R3-3.** One version bump, one layout. |

#### Review round 4 — verification of round 3's fixes

Verdict once more *still open*. Two of round 3's edits were right and two left the document
self-contradictory, and one of the contradictions reached back into Phase 2.

| # | Round-4 finding | Resolution |
|---|---|---|
| R4-1 | The `Some(kept_id)` substitution closes the laundering path in both directions | **Fixed, no action.** Verified: title in `vec_key`, body and order in `body_hash`/sequence, embedder in the substitution, `dim` from the kept manifest id. |
| R4-2 | "No CLI path produces vectors before P4" is **false for Phase 2**, which needs a CLI-produced `vectors.f32` for the hybrid golden and DoD 5 | **Accepted, new issue.** P1b now wires the embedder into `build::plan` and embeds every chunk with no reuse (~15 lines). That makes P1b's own acceptance case true and gives P4 exactly the work it claims. |
| R4-3 | §5's P1b build/accept text was stale: "~430 (P1a 200 + P1b 230)" against §8's ~320, and an Accept clause requiring carry-forward | **Accepted.** Re-based to ~520 together; `--no-embeddings` removes unconditionally in P1b; the fixture's `hash-64` config moves into P1b (no golden moves, because scoring still passes `semantic = 0.0`). |
| R4-4 | DoD 9's row-3 wording | **Fixed, no action.** |
| R4-5 | §8's P1b row still listed carry-forward, and the ~320 was justified with that work inside it | **Accepted.** The row names the embed wiring instead, and P4 rises to ~320 for the carry-forward it now owns. |

#### Review round 5 — verdict: closed

Five findings, all confirmations except two of the same mechanical class:

| # | Round-5 finding | Resolution |
|---|---|---|
| R5-1 | The fixture's `hash-64` move does not move a golden | **Confirmed.** `search::query` still passes `semantic = 0.0, alpha = 0.0`, so the snapshot is untouched; the only churn is `tests/publish.rs`'s `dir_snapshot`, which now sees a third file, plus the `Publication` literal and `clean_plan`'s call site. |
| R5-2 | `HashEmbedder` determinism holds | **Confirmed, with one condition recorded:** the counts must stay integer-valued, or "the second build is byte-identical" silently stops holding. Written into §3.2 as a code comment requirement. |
| R5-3 | §5's P4 header said ~280 against §8's ~320 — the identical staleness R4-3 fixed for P1b | **Accepted.** Re-based to ~320. |
| R5-4 | P4's build list claimed `plan` becomes fallible, which P1b now owns | **Accepted.** Moved into P1b item 7, which is where the signature change happens. |

The verdict is **closed**. Nothing is rejected across five rounds; 17 findings in total.

That is fifteen findings across three rounds on a plan whose first draft called itself done. The
pattern is consistent: each fix was right about the mechanism and wrong about the boundary — which
input, which branch, which phase. Round 4 added the sixth, and round 5 closed it. Naming boundaries
as carefully as formulas is the thing this review loop produced.

### 9.1 SPEC corrections and gaps

None of these is an edit in *this* PR: this PR is the plan. Each is either a correction the code
already implies, or a gap the M4 implementation PR should close with one line.

1. **§11-M4's done-when conflates three things.** Replace "a moved-but-unchanged file is recognized
   as a move and costs zero embedding work" with "a rename that leaves the embedded text unchanged
   costs zero embedding work; a rename that changes the derived title re-embeds only that document".
   Move "exact-hash move detection (`path_history`)" to §11-M5 (§6.9 already assigns move handling
   there). Reason: §3.7 — the condition is only true when the document declares a `title:`.
2. **§11-M4's list is partly M3.** "`Index` metadata via `bincode`", "incremental reindex" and
   "`janus index`" shipped in M3 (D13, S-6, S-9). Delete them and add the two things M4 actually
   adds: hybrid scoring and vector reuse.
3. **§11-M4's "<10 ms warm at 100k chunks" is an aspiration, not this milestone's gate** (§13 calls
   single-digit ms aspirational and forbids optimizing before measurement). Restate it as a printed
   measurement at 200k chunks with the two-file read path, and put the target where §13 already puts
   it: p50 ≤ 100 ms.
4. **§8.3's `vectors.f32` comment is true and incomplete.** It already says "raw little-endian f32,
   row-major, L2-normalized, dim known", and §8.3 already says both files are checksummed and that a
   generation mismatch is a rebuild trigger. What is genuinely missing: **headerless**, **row *i* =
   snapshot chunk *i***, `vectors_sha`/`vectors_len` as manifest fields, no padding, and the *reason*
   both payloads must be validated together (a new-vectors/old-index pair is otherwise
   indistinguishable from a good one). Keep the reason, drop the rule the SPEC already states.
5. ~~§8.3's rebuild triggers~~ — **withdrawn after round 2.** SPEC §8.3 already lists
   `embedder`/dim/dtype mismatch, "a generation mismatch observed by a reader", and `EmbedderId`
   already names normalization. There was nothing left to add; round 1's version of this item
   restated the SPEC.
6. **§8.3.1's heap budget is exceeded by M4's load path** (~307 MB at 200k × 384, §3.1). Add one
   sentence saying the whole array is read onto the heap until M8's mmap, so the table describes
   M8 and not M4. Without it, the next reader concludes the code violates the spec.
7. **§7.2 gives α = 0.65 and "0.0 if no embedder" but never says α is configurable.** Add: α is
   `[embedding] alpha`; the default is the number §7.2 already states; it is clamped to `[0,1]` with
   a config warning. **Also add the key to §16.1's `[embedding]` block**, which currently lists only
   `enabled`/`model`/`batch_size` — otherwise the key lives in §7.2 prose and nowhere in the config
   reference.
8. **§7.2 does not state the admission rule.** Add after the formula block: "a chunk is a candidate
   when `relevance > 0`, so a semantic-only match can enter the result set" — a real behaviour
   change from lexical-only that the spec should contain.
9. **§7.2's `"lexical": 12.4` in the §9.4 example is raw BM25**, while the code reports the
   normalised `[0,1]` value (M3 S-5). Fix the example; it is the one place where a reader
   implementing against the spec would get the DTO's meaning wrong.
10. **§8.2 lists `fastembed` as "Feature `embeddings`" without saying whether it is default, and
    D22 says the model is downloaded separately.** Add: **non-default** feature; **no runtime
    download** — the model is read from `JANUS_MODEL_DIR`/`[embedding] model_dir` or the build
    degrades. This is the row that keeps the Windows gate honest.
11. **§11-M4 and §12.1 name `rayon`; M4 does not use it.** Move "rayon top-k" and the "concurrency
    without the GIL / rayon over 200k dot products" learn row to M8, together with the measurement
    that would justify them (V-16). M4's learn target is traits, `dyn`, `Send + Sync` and Cargo
    features.
12. **§9.1 lists `--no-embeddings` on `index` only**, and M3 did not implement it. M4 implements it
    on `index` *and* adds it to `search`/`facts`; §9.1 plus §3.6 of this plan is the contract.
13. **§8.3.1's "Document text: 0 B" stays true, but vectors are a derived copy of text.** One
    sentence in §8.5 or §8.6 saying that `.janus/vectors.f32` is derived from document content and
    is covered by the same "never vault content" rule as the rest of `.janus/`, since a vector is a
    lossy encoding of the passage.
14. **D24 over-states what the cache can do.** It says "embedding cache reuse across edits, moves,
    and copies becomes automatic and identity-free". True of the *content*; not sufficient: the
    embedded text also contains the effective title and the raw line endings, so the cache key is
    `body_hash` + `vec_key` + the chunk-id sequence — `ChunkId` is the content component, not the
    key. **Proposed SPEC edit:** one clause in D24's rationale.
15. **D48's rationale cites a property M4 has to qualify.** "Content-addressing by text is what
    makes the embedding cache free across edits, moves, renames and copies" — free across *content*
    edits, and free across renames only when the title does not change with them (§3.7). Input that
    is not normalised (CRLF) is not free either. **Proposed SPEC edit:** replace "free" with "free
    whenever the embedded text is unchanged", and point at §11-M4's corrected sentence.
16. **`docs/m3-breakdown.md` §3.4 lists `ChunkId`'s purpose as "embedding cache, cross-references,
    span lookup".** Two of three hold. The plan must not leave a merged document asserting that M4
    keys its cache on `ChunkId` alone. **Proposed edit in the M4 phase-5 docs commit:** change it to
    "content identity, the content component of the embedding-cache key, cross-references, span
    lookup".
17. **`docs/m2-breakdown.md` CH-10 says "the M4 embedding cache dedupes by id".** V-12 says it
    does not dedupe at all in M4, and V-23 says two identical ids need not produce identical rows.
    **Proposed edit in the M4 phase-5 docs commit:** "…and the M4 vector table keeps one row per
    occurrence, so a repeated id is harmless there too".

Remaining open questions this plan does **not** resolve, listed so they are not silently decided:

18. **Should `--diversity`/MMR ever arrive?** §7.2 keeps it optional and off; §3.1 of M3 keeps one
    hit per document. With vectors available, MMR becomes cheap to implement — and it is still not
    in any milestone. It needs a product decision (does a caller want two passages from one
    document?), not a technical one.
19. **A real-vault eval corpus.** `tests/eval/queries.jsonl` is 35 fixture queries over 18
    keyword-rich documents (M3 W-10). The α decision (§3.5) is taken on the author's vault, which
    means it is not reproducible in CI. If the number ever needs to gate anything, the eval corpus
    becomes a milestone.
20. **Model-file integrity.** V-20 records the cost argument for not hashing the weights. If M8
    finds a cheap partial hash acceptable, it becomes a manifest field and a rebuild trigger.

## 10. Implementation log — Phase 1 (trait, `vectors.f32`, publication)

### P1a — the embedder trait and the payload format

Branch: `bb/m4-p1a-embedder-trait-vectors-f32-thr_za9nny6u2i`. Scope is `src/index/embed.rs` and
`src/index/vectors.rs` (new), their unit tests, one public-API round-trip in `tests/vectors.rs`,
the module declarations in `src/index/mod.rs`, and the one `ConfigWarning` variant in
`src/config.rs` noted below. No store, build, search, CLI, manifest, feature or dependency change:
P1b owns the publication protocol and P3 owns the `fastembed` module.

| ID | decision | why |
|---|---|---|
| P1a-1 | `EmbedderId::to_bytes` writes, in declaration order, a little-endian `u64` length followed by each field's bytes: `name`, `dim` (`u32` LE), `dtype`, `normalisation`. | V-22 requires an unambiguous length-prefixed encoding. `u64` matches `chunk_id`'s existing text prefix; prefixing `dim` too means a future field cannot shift an old decode. |
| P1a-2 | `HashEmbedder` takes the bucket from the first 8 digest bytes as a little-endian `u64` modulo `dim`, and the sign from bit 0 of digest byte 8. | The bucket and sign each depend only on `blake3(term)`, so iteration order cannot affect the row; integer counts plus one index-order norm pass keep the bytes deterministic. |
| P1a-3 | `HashEmbedder::new(dim)` returns `Option<Self>` and refuses `dim == 0`; `resolve` reports `hash-0` as `"invalid model"`. | §3.1 requires `dim > 0`; a zero-dimension id cannot describe a valid `vectors.f32` generation, so refusal is safer than an embedder every writer must reject. |
| P1a-4 | Added `ConfigWarning::TestDoubleEmbedder` in `src/config.rs`; `EmbedderId::config_warning` constructs it and `resolve` logs it with `tracing::warn!`. | §3.2 requires the `hash-64` warning, but no existing variant means it, and the fixed `Resolved` signature cannot return warnings. This is the only `config.rs` edit in P1a, narrower than the combined P1 file list expected. |
| P1a-5 | `resolve` implements the dispatch table's feature-off column. A non-`hash-<n>` model degrades to `"not compiled"`, and the detail names the configured model directories. | P3 owns the `fastembed` module, so fabricating feature-on behaviour now would need a fake feature or a test-only branch. Naming the directories keeps the degradation diagnostic useful without opening them. |
| P1a-6 | A zero-row `Vectors` is valid, and `is_empty` exists beside `len`. | An empty corpus has an empty payload rather than a missing one, and clippy's `len_without_is_empty` is part of the `-D warnings` gate. |
| P1a-7 | `Vectors::decode` keeps `chunks_exact(4)` with a documented `clippy::chunks_exact_to_as_chunks` allow. | The phase text names `chunks_exact(4)`, and the constant width is checked by the alignment rule immediately above; the allow records the plan reference instead of silently changing the mechanism. |
| P1a-8 | `normalize` returns a non-finite norm without dividing and leaves the row untouched. | It keeps the helper total for an unchecked caller; the read path rejects non-finite values first, so no writer can publish such a row. |
| P1a-9 | Review fix: pin `HashEmbedder::new(4)` over `"a b"` to a literal row instead of versioning the `hash-<n>` name in §3.2. | A literal makes any bucket or sign change fail loudly, and the `HashEmbedder` docs now say such a change must also change the name so P4's reuse key cannot accept old vectors. Keeping `hash-<n>` stable avoids a config/fixture rename in P1b. |
| P1a-10 | Review fix: every `hash-`-prefixed model name that does not parse as a positive `u32` degrades to `"invalid model"`, not `"not compiled"`. | The `embeddings` feature cannot fix a typo or an overflowing dimension, so the reason must name the configuration error. |
| P1a-11 | Review fix: `dot` debug-asserts equal widths and keeps the release `zip`. | P2's `dot(q_vec, row)` with vectors from two models is a bug; truncating it would produce a plausible wrong score. Both widths come from one `EmbedderId`, so debug builds can assert it at no release cost. |

### P1b — the two-file generation, writer half

Branch: `bb/m4-p1b-publish-vectors-f32-thr_q2fyy3ptxm`, stacked P1c on top. Scope is the manifest
fields and `format_version = 2`, `DocMeta::body_hash`, the nine publication stages, the degraded
drop, `build::plan` embedding every chunk, `main`'s `ModelDirs`/`resolve` seam, `--no-embeddings` on
`index`, the fixture's `hash-64`, and the writer-side tests. The reader half is P1c.

**The 680-logic measurement forced the split.** `scripts/pr-size.sh` against `main` reported 680
logic / 820 tests over the whole phase. That is past both the 450 seam and the 600 hard ceiling, so
the diff was split at the seam §8 pre-authorised rather than shaved: comments and tests were not
trimmed to fit, and no test was dropped. P1b measures the numbers in its own PR; P1c is measured
against P1b (`BASE=origin/<p1b branch>`).

| ID | decision | why |
|---|---|---|
| P1b-1 | `Publication.vectors` is `Option<&VectorSet>`, where `VectorSet` is `{ EmbedderId, Vectors }`, not the plan's bare `Option<&Vectors>`. `VectorSet::new` refuses rows whose width is not `id.dim`. | `Vectors` deliberately carries no identity, but the manifest records both the id and the shape, so a writer must hold them together. A pair of loose fields could disagree, and the width check makes "the manifest describes the payload" true by construction. |
| P1b-2 | `Manifest::new` takes `Payloads { index_sha, index_len, vectors: Option<VectorArtifact> }` instead of five loose fields. | The three manifest vector fields are one fact (§3.1). One `Option<VectorArtifact>` cannot be built half-populated, so the writer never emits the corrupt state P1c's reader rejects. |
| P1b-3 | The `vectors::Corrupt` → `store::Corrupt` bridge is a `From` impl carrying the enum's `Display` text; `Corrupt::new` is widened to `pub(crate)` for `build::plan`. | One corruption vocabulary, and the enum's specific message ("vectors byte length 40 does not match 2 rows × 4 dimensions × 4 bytes") survives. The alternative accessor would have duplicated the formatting. |
| P1b-4 | `build::plan`/`plan_forced` return `Result<_, Corrupt>`, embed in batches of `[embedding] batch_size` (clamped to ≥ 1), normalise every row and reject a wrong row count, a wrong width or a non-finite value with the embedder name and the chunk index. Nothing is published. | V-7 and §3.1: a substituted zero row is invisible quality loss, so the build must fail closed. `EmbedError` already names the batch, so the batch loop keeps that diagnostic meaningful. |
| P1b-5 | The degraded drop announces itself with `tracing::warn!`, one line, only when a payload was actually removed. | `publish` has no stderr and both callers (the `index` command and a read's rebuild) must announce it; this is the channel `resolve`'s test-double warning already uses. The consequence is recorded: the line is visible only where the binary installs a subscriber, so the test asserts it through the CLI with tracing on. |
| P1b-6 | `[embedding] model_dir` lands here, not in P1a: `EmbeddingConfig.model_dir: Option<PathBuf>`; the cache path is computed in `main` from `XDG_CACHE_HOME`, `LOCALAPPDATA`, `HOME/.cache`, else the temp dir, without a new dependency. | P1a did not add it, and V-25's precedence needs the config value. Computing the cache in `main` keeps `std::env` out of the library. |
| P1b-7 | A degraded (no-embedder) build that changes nothing still drops `vectors.f32`, unconditionally. | P1b has no reuse predicate, so it cannot know which rows survive; §3.6 row 3's carry-forward is P4's, with the predicate that makes it correct. Unconditional and announced beats silent staleness. |
| P1b-8 | `BuildStats` gains `embedded_chunks` and `reused_vector_chunks`; the latter is always zero here. The `index` receipt is unchanged. | The counters are P4's receipt fields and cost nothing now; the JSON receipt stays P4's, so no `--json` consumer sees an empty field. |
| P1b-9 | The P1b reader (unchanged M3 `Expected`/`load`) does not validate the payload and never asks for one, so a read's auto-build is lexical. | It is the writer half by design; P1c adds `want_vectors`/`embedder` and the validation. `search`/`facts` therefore pass no embedder until P2, which is what keeps the α = 0 path byte-identical. |
| P1b-10 | **Review fix.** The degraded drop runs after the manifest rename and its directory sync, and reports itself through a ninth stage, `Stage::VectorsDropped`, which fires last and only when a file was actually removed. | Dropping with the other payload work left the old manifest naming a file that was already gone: a concurrent reader with `want_vectors = true` saw `is missing`, retried, then rebuilt a state it should never have observed — and a publish that failed after the drop had destroyed a good payload for nothing. After the commit record changes, the new manifest says `embedder: None`, so no reader opens the file, and an orphan from a crash between the two steps is unreachable and overwritten by the next vectors-bearing publish. `a_degraded_publish_drops_the_payload_after_the_commit_record` asserts the payload outlives every pre-commit stage and fails on the old order. |
| P1b-11 | **Review fix.** `janus index` prints one `embeddings unavailable (<reason>): <detail>; lexical only` line when resolution degrades, and nothing when `--no-embeddings` was passed. `resolve_embedder` returns `Resolved` so each caller decides; only the `index` arm announces. | §3.6 row 1 promises one line for a lexical build, and the `index` receipt has no `degraded` field, so without this line a wrong `model_dir` or a missing feature produced a silent lexical generation. The flag is an explicit choice, so it stays quiet. A read still says nothing here: P2's `degraded` field is its channel. |

## 11. Implementation log — Phase 2 (hybrid scoring)

_Nothing recorded yet._

## 12. Implementation log — Phase 3 (real embedder behind a feature)

_Nothing recorded yet._

## 13. Implementation log — Phase 4 (incremental embedding)

_Nothing recorded yet._

## 14. Implementation log — Phase 5 (eval, baseline, docs)

_Nothing recorded yet. The α sweep table, the chosen default, and the D20 verdict belong here._
