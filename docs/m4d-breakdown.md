# M4D — The index keeps itself current

> Branch: `m4d-plan` (this document), then one branch per phase from `main`.
> Spec: §8.3 (publication, rebuild triggers), §9.1, §9.2, §13, §14, §15, §16.1.
> Evidence: a 2 119-document corpus, measured across M4, M4B and M4C.
>
> **Done when:** an edit is visible on the next `search` without the user running `janus index`,
> and a build that takes minutes prints its progress.

**Words used here.** **stale**: the corpus changed after the index was built.
**refresh**: an incremental build that reuses unchanged work. **cold build**: a build with nothing
to reuse. **signature**: the sorted `(path, size, mtime)` of every admitted file.

## 0. Scope

**Decision: a read must not return an old index in silence, and a long build must not be silent.**

Measured on the real corpus:

| fact | value |
|---|---|
| documents | 2 119 |
| chunks | 27 234 |
| a re-index with nothing changed | **2.6 s** (walk, parse, reuse every token and row) |
| a cold lexical build | 3 s |
| a cold build with embeddings | **18.3 min** at 24.8 chunks/s |
| a first `search` after that build, when the generation had no vectors | **re-embedded everything, unasked, for 18 minutes** |
| the walk over 2 119 files | **8.5 ms** (best of 7, release, warm cache) |

Three problems:

1. **A read cannot tell that the index is old.** Only `janus index` notices an edit. A search
   returns the previous answer and says nothing.
2. **A read can trigger an expensive build.** A generation without vectors is stale for a process
   that can embed, so the first `search` rebuilt for 18 minutes. Nothing warned first.
3. **A build is silent.** No progress, no estimate. Ten minutes is indistinguishable from a hang,
   and the author killed a healthy build at 10:01 because of it.

### In scope

- A staleness check on read: exact for add, remove, rename and edit, at the cost of one walk.
- Incremental refresh on read when the corpus changed, including embedding only the changed chunks.
- A read never rebuilds for a format, embedder or corpus identity mismatch. That is a command, and
  the refusal names it.
- A writer that closes its own lost-update window: an edit made during a build is in the index when
  the command returns.
- Progress and an estimate during any embedding work, on stderr, behind a terminal gate. That
  includes a build a read triggers.
- A note when a read declines to refresh, naming the reason.

### Not in scope

- A file watcher, a daemon, or `janus watch` (M8).
- Resuming a killed cold build. Refresh makes it rare; it stays impossible (P-2).
- Sub-second staleness detection. The witness runs once per read, not per file.
- Quantisation, a smaller model, a faster runtime. Cold-build throughput is M8's.
- Quarter dates and the undated-decay rule. Both are handed to M5 (§10).
- Any change to the index payload layout, `format_version`, or the scoring rules.
- `janus doctor` freshness reporting. `doctor` does not exist yet; §14 keeps its promise for later.

## 0a. Review resolutions

### Round 1 — ten findings from the design review

| # | finding | resolution |
|---|---|---|
| R-1 | An edit during a build stays invisible: `index.bin`'s mtime is the end of the build | **Fixed.** §3.2: re-walk after publish and rebuild while the signature differs. No clock read, no format change. |
| R-2 | "Index absent → refuse" reverses six tests and two contracts; a corrupt pair still rebuilds cold | **Fixed.** §3.3: a read builds when the index is absent or the pair is corrupt, and refuses only a cold mismatch. |
| R-3 | A stale read blocks 2.6 s and can exit 1 on the 10 s lock | **Partly fixed.** §3.4: it never waits for another writer and never exits 1 on a busy lock. It still pays its own refresh; that is the milestone's point. |
| R-4 | The 10 % cost budget cannot be met, and D-5a cannot see an in-place edit | **Fixed.** D-5: no budget, one walk per read. D-5a is deleted with its reason. |
| R-5 | The 500-chunk floor is under the real fixture and breaks `a_clean_build_writes_no_stderr` | **Fixed.** §3.6: no chunk floor. Progress is gated on the terminal. |
| R-6 | The progress seam is wired to `build::plan`, which `janus index` does not call | **Fixed.** §3.6: the sink is threaded through all three plan entry points. |
| R-7 | Strict `>` has no rule for equal timestamps | **Superseded** by R-11: the witness is a stored digest, so no comparison of two clocks remains. |
| R-8 | Phase 2's acceptance cannot be measured as written | **Fixed.** §5 P2: a byte comparison against a `--rebuild` copy taken before the refresh. |
| R-9 | §9 duplicates D50 and omits the reader protocol, §9.1, §9.2 and the exit code | **Fixed.** §9 amends D50 and lists every change. |
| R-10 | "5 % of the total" defines no total | **Fixed.** §3.6: the total is the chunks this build embeds. |

### Round 2 — ten findings from the plan review

| # | finding | resolution |
|---|---|---|
| R-11 | `max mtime` cannot see a deletion, and a rename keeps its mtime, so two P1 tests cannot pass | **Accepted.** §3.1: the manifest stores a digest of the corpus signature (`corpus_sig`), and a read compares the digest. This catches add, remove, rename and edit. Payload layout unchanged. |
| R-12 | The `built_root` "refresh" row is unreachable: `Expected::check` rejects it before a generation is returned | **Accepted.** §3.3 and §3.4: `Expected::check` returns three outcomes, and `built_root` yields a usable generation plus a refresh request. |
| R-13 | Three more tests break under D-3 and `tests/hybrid_search.rs` is missing from P2 | **Accepted.** §5 P2 adds the three rows and the file. One of the three (`--no-rebuild`) keeps `IndexUnstable`: see R-18. |
| R-14 | `Load.stale` cannot be both `Option<String>` and `bool` | **Fixed** before this review round: P1 no longer touches `Load`. The witness reaches the caller through `Outcome` instead. |
| R-15 | `refresh:` carries two meanings, and the P1 `manual` test cannot fail | **Accepted.** §3.5: `refresh:` means "this process ran a build". P1 prints no line; it reports the witness in `--json`. The `manual` test moves to P2. |
| R-16 | One `build_until_stable` would self-time-out inside `store::load`, and the receipt has no owner | **Accepted.** §5 P1: `cmd` acquires the lock and passes it; the receipt describes the last pass. |
| R-17 | §3.4 step 6 states no adoption criterion | **Accepted.** §3.4: adopt only when the re-read manifest's `corpus_sig` equals the digest the read computed. |
| R-18 | `--no-rebuild` on a `corpus_id` mismatch now says `index needs a full build` | **Rejected.** `--no-rebuild` keeps M3's `IndexUnstable` message; only the default path uses the new error. `tests/cli_search.rs:198` does not change. |
| R-19 | A read-triggered refresh prints no progress | **Accepted.** §3.6: the sink reaches `store::load`, so an absent-index build on a fresh vault is not silent. |
| R-20 | §9 leaves `schema_version` unresolved, and a DoD item is not measurable | **Accepted.** §9 deletes `schema_version` from the SPEC trigger list, because the code never compares it. The witness item is marked a person check with the command named. |

### Round 3 — five findings from the re-check

| # | finding | resolution |
|---|---|---|
| R-21 | §3.4 drops `Check::Refresh` when the digest is fresh, so a moved vault is never repaired; and `built_root` is compared before `embedder`, so a moved vault with a vectors-less generation re-embeds everything | **Accepted.** §3.4 step 3 splits into `Check::Ok` and `Check::Refresh`. §3.3 orders the five cold fields before `built_root`. New line: `refresh: manifest repaired`. |
| R-22 | P1's `build_until_stable` drops `forced` and `rebuild`, and takes a sink that P3 has not built yet | **Accepted.** §5 P1: the signature carries `forced` and `rebuild`, not `sink`. P3 extends it. |
| R-23 | `--no-rebuild` on a fresh-but-changed corpus is a new refusal with no message, and `IndexUnstable` would be false for it | **Accepted.** `--no-rebuild` keeps reading. It prints `refresh: skipped, --no-rebuild` and writes nothing, so the answer is never silent and no error is invented. |
| R-24 | The P1 pass count and the witness DoD cannot be measured | **Accepted.** The receipt gains `passes`. The witness DoD names its command, and the walk count is stated as two on a stale read. |
| R-25 | The fallback has no empty-corpus rule, keeps `>`, and an older binary erases `corpus_sig` | **Accepted.** §3.1 states all three limits and pins the first two. D-1a says a pre-M4D build drops the field and one `janus index` restores it. |

## 1. Dependencies

None. No crate. No feature flag.

## 2. Data model

```rust
// src/index/refresh.rs — the corpus signature. Pure: it reads no file.
pub struct Signature {
    /// Sorted (rel_path, size, mtime) of every admitted file.
    /* private */
}

impl Signature {
    /// Sorts the walked files and encodes them canonically.
    pub fn of(files: &[DiscoveredFile]) -> Self;
    /// The digest the manifest stores and a read recomputes.
    pub fn digest(&self) -> String;
}

/// The read's answer: the corpus signature differs from the stored one.
pub struct Witness {
    pub stale: bool,
}

// src/index/build.rs — one callback, the seam the tests use.
pub type EmbedSink<'a> = &'a dyn Fn(u64 done, u64 total);

// src/index/store.rs — why a manifest did not agree with this process.
pub struct Mismatch {
    pub field: MismatchField,
    pub found: String,
    pub wanted: String,
}

pub enum MismatchField {
    FormatVersion,
    Tokenizer,
    Chunker,
    CorpusId,
    BuiltRoot,
    Embedder,
}

/// The three outcomes of validation.
pub enum Check {
    /// The manifest describes this process and this corpus.
    Ok,
    /// The generation is usable, but the manifest must be rewritten.
    Refresh(Mismatch),
    /// Only a cold build can cure this.
    Refuse(Mismatch),
}
```

`Witness` is computed per read and never persisted. `EmbedSink` is a seam: the CLI prints, the
tests count. The manifest gains one optional field; the payloads do not change shape.

## 3. Contracts

### 3.1 The witness: how a read knows the index is old

**The manifest stores a digest of the corpus it was built from. A read re-walks the corpus, computes
the same digest, and compares.**

```
signature = sorted (rel_path, size, mtime) over every admitted file
stale     ⟺ digest(signature) ≠ manifest.corpus_sig
```

- **This is the only rule that sees every change.** A deletion lowers no maximum. A rename keeps the
  file's mtime on Unix. Both change the signature.
- **One walk gives the signature.** `DiscoveredFile` already carries `size` and `mtime`
  (`src/vault/mod.rs:64,68`), so the check adds no `stat` per file. The same walk feeds the refresh.
- **The digest is canonical.** Encode each entry as its length-prefixed `/`-separated path, its size
  as `u64`, and its mtime as `(secs, nanos)` since the Unix epoch. Sort by path, then hash with
  blake3. A pre-epoch or unrepresentable time gets a distinct tag and never panics.
- **The answer is one boolean, not a list or a count.** A digest cannot say *which* file changed,
  and a second stored list would cost more than the walk it saves. The line says the corpus changed
  and names the command that fixes a corpus it cannot see (`--rebuild`).
- **The cost is one walk**: 8.5 ms over 2 119 files (best of 7, release, warm cache), plus a hash of
  about 85 KB. A fresh read pays that once. A stale read pays it once for the witness and once more
  after the publish, and reuses the first walk for the plan. There is no budget line. P1's PR body
  records the number; no test asserts it.
- **`corpus_sig` is additive, not a format change.** `Manifest` is JSON without
  `deny_unknown_fields`, so an older binary ignores the field. A new binary reading an older manifest
  gets `None` and falls back to the rule below. `format_version` guards the bincode payload, not the
  manifest's field set, so it does not move.
- **Fallback for an index built before M4D.** With no `corpus_sig`, a read uses
  `max(document mtime) > mtime(index.bin)`, with two extra rules: a corpus with **no documents** while
  the index names some is stale, and the comparison is strict `>`, so an edit in the same timestamp
  tick as the publish is missed. That rule cannot see a deletion or a rename. The next `janus index`
  writes the digest. The limit is named in the `search` docs and pinned by a test.
- **An older binary erases the witness.** A build by a pre-M4D binary serializes only its own
  manifest fields, so `corpus_sig` disappears and the next read falls back. One `janus index` with
  the current binary restores it. D-1a records this.
- **Named blind spot.** A content change that keeps the path, size and mtime (`cp -a`, a backup
  restore) is invisible. `janus index --rebuild` is the escape hatch. The `search` docs say so.
- **mtime is valid here and invalid as a document date.** M4B removed it from the date ladder because
  a copy rewrites it. A cache asks only whether the bytes changed. D50 gains this half (§9).

### 3.2 The writer closes its own window

**An edit made during a build must be in the index when the build returns.**

The manifest's `corpus_sig` describes the walk that produced the plan. A file saved at minute 5 of an
18-minute build is not in that walk, so the published digest would say "fresh" and miss the edit.

The writer closes the window with no stored clock and no second version of the payload:

```
walk W0 → plan → publish(digest(W0)) → walk W1
while signature(W1) != signature(W0):
    plan against W1 → publish(digest(W1)) → walk W2 ; W0, W1 := W1, W2
stop after 3 passes, or when two consecutive signatures are equal
```

- **The second pass is cheap.** It reuses every token and every row, so it costs 2.6 s on the real
  corpus, not 18.3 minutes.
- **Both writers use it**: `janus index` and a read-triggered build. One function.
- **One lock, one holder.** `store::build_until_stable` takes the walk and the `&Lock` its caller already holds and
  publishes with `publish_locked`. `cmd::run` acquires the lock itself instead of calling
  `store::publish`, which would try to take it a second time and time out.
- **The receipt describes the last pass.** `index_sha`, `taken_ms` and the stats come from the pass
  that finished.
- **Bound.** After 3 passes the corpus is still moving. Publish the last plan and print
  `refresh: incomplete, corpus changed during the build`. A vault under a continuous writer cannot
  converge, and pretending otherwise is worse than one line.

### 3.3 What a read does about it

**A read refreshes when the corpus changed. It builds when the index is absent or the pair is
corrupt. It refuses only when a full build is the only cure.**

| state | `search` / `facts`, `refresh = "auto"` | `--no-rebuild` |
|---|---|---|
| fresh | read; no lock, no line | read |
| corpus changed | **refresh, then read**. `refresh: corpus changed` | read; `refresh: skipped, --no-rebuild`; write nothing |
| index absent | **build once, then read**. `refresh: no index yet` | `no index yet: run janus index` (exit 1) |
| pair corrupt, retries exhausted | **rebuild once, then read**. `refresh: index repaired` | refuse; write nothing (exit 1) |
| `format_version`, `tokenizer`, `chunker`, `corpus_id` or `embedder` mismatch | **refuse**: `index needs a full build: run janus index` (exit 1) | refuse (`IndexUnstable`, exit 1; unchanged) |
| `built_root` mismatch | **load the generation, repair the manifest, then read**. `refresh: manifest repaired` | read; `refresh: skipped, --no-rebuild`; write nothing (D-24) |

**Which mismatches refuse.** A refusal is correct only when an incremental build cannot cure the
state. `Expected::check` (`src/index/store.rs:758`) compares six fields. Five of them change the
identity of every chunk or every row, so the cure is a cold build:

| field | why a read may not build it |
|---|---|
| `format_version` | the layout changed; the previous bytes may not decode |
| `tokenizer` | every token changes; nothing is reusable |
| `chunker` | every chunk boundary changes; nothing is reusable |
| `corpus_id` | every `DocId` changes; tokens and rows are both unreusable |
| `embedder` | the rows have a different width or meaning; all must be regenerated |

`built_root` is the exception. It is diagnostic only (`src/index/store.rs:414`), every `DocId` is
unchanged, and SPEC §8.3 states that a moved vault "is never an error". So the generation loads, and
the read refreshes to rewrite the manifest. The two facts travel together: `Check::Refresh` carries
the mismatch, and `Generation` carries it to the caller.

**`Expected::check` returns `Check`, not `Result<(), String>`.** The caller needs the field to decide.
The message text is unchanged, so the tests keep their strings. **The five cold fields are compared
before `built_root`.** Otherwise a moved vault with a vectors-less generation reports `Refresh`,
and the refresh embeds every chunk: the 18-minute build that §0 problem 2 forbids.

**Why an absent index still builds.** It is the first use, there is nothing to reuse, and refusing
would deadlock an MCP-only agent: SPEC §9.2 gives `search` no `--no-rebuild` and offers no index tool.
The build is not silent, so the user sees why it is slow (§3.6).

**`--no-rebuild` never writes, and never surprises.** When a generation validates it reads it, even
when the corpus changed, and says `refresh: skipped, --no-rebuild`. When no generation validates it
refuses and keeps M3's `IndexUnstable` message naming the file. The new
`index needs a full build` error belongs to the default path only, because it names the command the
user must run.

**`[index] refresh = "manual"` disables only the corpus-changed row.** It prints
`refresh: skipped, refresh = manual` and answers from the validated generation. An absent index, a
corrupt pair and a `built_root` mismatch still build or refresh, because manual means "do not refresh
because the corpus changed", not "do not work".

### 3.4 The refresh never waits for a busy writer

**A read that can answer must not block on another process, and must not fail on a lock.**

1. Load the validated generation, with no lock, exactly as today (M3 §3.5). A `built_root` mismatch
   yields a generation plus `Check::Refresh`.
2. Walk the corpus and compute the witness (§3.1).
3. Fresh and `Check::Ok` → answer. Done.
4. Fresh and `Check::Refresh` → the manifest needs the current root. Try the writer lock once,
   non-blocking; on success run the refresh, which republishes the same snapshot with the current
   root, and print `refresh: manifest repaired`. On a busy lock print
   `refresh: skipped, index busy` and answer from step 1.
5. Stale → try the writer lock **once, non-blocking**.
6. Busy → print `refresh: skipped, index busy`, answer from the generation loaded in step 1, exit 0.
7. Acquired → re-read the manifest under the lock. **Adopt only when its `corpus_sig` equals the
   digest computed in step 2**; otherwise build. Then answer from the new plan.

- **Why non-blocking.** A validated read takes no lock today, so `search` works while `janus index`
  runs. A 10 s wait (`LOCK_TIMEOUT`, `src/index/store.rs:45`) that can exit 1 (`LockTimeout`) would
  turn a working read into a failure. The refresh is a convenience; it must never be a hazard.
- **Adoption has a criterion.** A writer that published a plan made before the edit also validates.
  Adopting it unconditionally would lose the edit. The digest comparison is the criterion.
- **The read still pays its own refresh.** On the real corpus that is 2.6 s, once per change, and
  two walks: one for the witness and one after the publish. The alternative is answering from an old
  index, which is the failure this milestone removes.
- **`Lock::try_acquire(dir) -> Result<Option<Lock>, JanusError>`** is new. `WouldBlock` → `Ok(None)`.
  `Lock::acquire` keeps its blocking behaviour for recovery (rows 3 and 4 of §3.3), so
  `a_held_lock_times_out_without_writing` and the two-thread auto-build test stay true.
- **The recovery path still blocks.** A read with an absent index or a corrupt pair takes
  `Lock::acquire(dir, lock_timeout)` and may return `LockTimeout`. That is today's protocol, and a
  corrupt pair has no generation to answer from.

### 3.5 One line per read, one meaning

`refresh:` means **this process wrote a generation, or repaired the manifest that names one**.
Nothing else uses the word.

| line | when |
|---|---|
| `refresh: corpus changed` | a corpus refresh published a generation |
| `refresh: no index yet` | the first build |
| `refresh: index repaired` | a rejected pair was rebuilt |
| `refresh: skipped, index busy` | another writer holds the lock (§3.4) |
| `refresh: skipped, refresh = manual` | `[index] refresh = "manual"` |
| `refresh: skipped, --no-rebuild` | the corpus changed and the caller refused a write |
| `refresh: manifest repaired` | the root moved and the manifest was rewritten |
| `refresh: incomplete, corpus changed during the build` | the 3-pass bound (§3.2) |
| `index needs a full build: run janus index` | a cold mismatch; exit 1 |

Rules:

- **At most one line per read.** A read that refreshes prints the `refresh:` line and nothing else.
  A read that adopts a valid generation prints one skip line when it declined to refresh
  (`index busy`, `refresh = manual`, `--no-rebuild`) and nothing otherwise.
- **`stale:` is retired** when P2 replaces `prepare`'s line. Until then it keeps its M3 meaning.
- **P1 prints no line.** It reports the witness in `--json` as `index: { "stale": bool }`, which is
  additive and keeps `api` at `1`. P2 adds the stderr line.
- **stderr, never stdout.** stdout is the results stream (§8.5, §9.5).
- **The line names no timing.** `janus index --json` already reports `taken_ms`. A duration on a read
  line is untestable and adds nothing.

### 3.6 Progress and the estimate

**Every embedding batch reports its position, on stderr, when a human is watching.**

```
embedded 8192/27234 chunks (30%)  ~12 min left
```

- **stderr**, never stdout.
- **The gate is decided once, at the edge.** `--progress <auto|always|never>`, default `auto`.
  `main.rs` resolves `auto` against `std::io::stderr().is_terminal()` and passes a plain `bool` down.
  A captured stderr — tests, CI, a pipe — is silent, so `a_clean_build_writes_no_stderr` keeps its
  meaning on any machine. No test depends on a terminal.
- **The total is the chunks this build embeds.** A refresh that embeds 13 chunks reports `x/13`. A
  run that reuses every row embeds nothing, fires the sink zero times, and prints no summary.
- **At most one line per 5 % of that total.** `step = done × 20 / total`; print when `step` changes
  and `step >= 1`. At most 20 lines.
- **The estimate** uses the observed rate and the elapsed time in the printer, which is one module,
  `src/progress.rs` (D-26). Both callers build it: `index::cmd` for the command and `search::prepare`
  for a read. It is rounded to a whole minute. No test asserts it (D-8).
- **The summary** is one line: `embedded N chunks`. It prints under the same gate.
- **Both writers report.** `janus index` passes the sink to `plan_forced`, through
  `build_until_stable`. A read passes it through `LoadOptions` to `store::load`, so the first build
  on a new vault is not silent for 18 minutes.

**The seam is the library, not the printer.** `EmbedSink` is threaded into `embed_all`
(`src/index/build.rs`, `embed_all`), the only caller of `Embedder::embed`, and it fires once per
embedded chunk. Tests use a counting embedder and assert the values, never the text.

### 3.7 What must not change

- **No payload change.** No `format_version` bump, no index file added, no rebuild for an existing
  vault. The manifest gains one optional field.
- **The refresh reuses everything M3 and M4 built**: tokens by `lex_key`, rows by `body_hash` and the
  chunk-id sequence. A refresh is not a smaller cold build.
- **The scoring path is untouched.** No weight, no threshold, no tie-break.
- **A validated read still takes no lock.**
- **`--no-rebuild` writes nothing**, and `--dry-run` never refreshes.

## 4. Decisions

| # | question | decision | where |
|---|---|---|---|
| D-1 | How a read detects staleness | A stored blake3 digest of the sorted `(path, size, mtime)` signature, recomputed on read. The max-mtime rule is the fallback for an older manifest | `refresh.rs`, `store.rs` |
| D-1a | Is `corpus_sig` a format change | **No.** The manifest is JSON without `deny_unknown_fields`; an old binary ignores the field, and a new binary defaults it to `None`. A build by a pre-M4D binary drops the field, and one `janus index` restores it. `format_version` guards the payload | `store.rs` |
| D-1b | Which changes the witness sees | Add, remove, rename and edit. A content change that keeps path, size and mtime is invisible; `--rebuild` is the escape hatch | `refresh.rs` |
| D-2 | What a stale read does | Incremental refresh under a non-blocking lock, one `refresh:` line | `search/mod.rs` |
| D-3 | May a read run a cold build | **No**, for the five identity-changing mismatches. It refuses and names `janus index` | `store.rs` |
| D-3a | Does a `built_root` mismatch refuse | **No.** The generation loads and the read refreshes: every `DocId` is unchanged, and SPEC §8.3 says a moved vault is never an error | `store.rs` |
| D-4 | Is the refresh configurable | Yes: `[index] refresh = "auto" \| "manual"`, default `auto`. `manual` disables the corpus-changed refresh only | `config.rs` |
| D-5 | The cost budget for the witness | **None.** One walk for a fresh read, two for a stale one; the first is reused by the plan. 8.5 ms measured, reported, never asserted | `refresh.rs` |
| D-5a | ~~Directory-mtime prefilter~~ | **Deleted.** A directory's mtime does not change on an in-place edit | — |
| D-6 | Progress cadence | One line per 5 % of the chunks this build embeds, on stderr, only when the edge resolved the gate to `true` | `build.rs`, `cmd.rs` |
| D-7 | The progress seam | A sink in `plan_inner`/`embed_all`, threaded through all three plan entry points and `LoadOptions` | `build.rs` |
| D-8 | Is the estimate asserted | **No.** Printed, never compared | — |
| D-9 | Does refresh change `--dry-run` | **No.** A dry run never refreshes; it reports what a run would do | `cmd.rs` |
| D-10 | Read order | Load the validated generation first, then decide about the refresh. A read that can answer never waits | `search/mod.rs` |
| D-11 | A busy writer during a refresh | Skip the refresh, print `refresh: skipped, index busy`, answer from the loaded generation, exit 0 | `store.rs` |
| D-12 | How many lines a read prints | At most one, and `refresh:` is the only word | `search/mod.rs` |
| D-13 | The writer's lost-update window | Re-walk after publish; rebuild while the signature differs; stop after 3 passes | `store.rs` |
| D-14 | Where the lock lives in the writer loop | The caller acquires it; `build_until_stable` takes `&Lock` and publishes with `publish_locked` | `store.rs`, `cmd.rs` |
| D-15 | What `passes` reports on `--dry-run` | **`0`.** No pass ran: a dry run plans once and writes nothing. A receipt that claimed one pass would describe a build that did not happen | `cmd.rs` |
| D-16 | Who prints the incomplete line | **`build_until_stable` itself.** Both writers reach this one function, and a caller-side flag would let one of them forget the line | `store.rs` |
| D-17 | Already-resolved documents in the writer | **Resolved only when they are needed.** `PATHS` resolves the walk to find the `DocId`s it forces; a plain write resolves once, inside the loop. A second resolve of the same walk would read every file twice | `cmd.rs`, `refresh.rs` |
| D-18 | Does P1 split at the 450-line seam | **No.** P1 lands at 559 logic lines and 686 test lines: under the 600 ceiling. The seam would put the digest in one PR and the writer that stores it in the next, so the first PR would have no behaviour a test can see | — |
| D-19 | Where the corrupt vector-triple check lives | **`read_generation`.** `Expected::check` returns `Check` for the six manifest fields only, so a manifest holding two of three vector facts stays a rebuild, not a refusal. Moving it into `check` would have made a corrupt manifest look like a cold mismatch | `store.rs` |
| D-20 | How `Mismatch` reproduces two different embedder texts | **One sentinel `found` value.** The rows read `index has no vectors and this build embeds with …` and `embedder a != b`; `Mismatch { field, found, wanted }` plus a sentinel renders both, and M3's wording stays exact | `store.rs` |
| D-21 | How a read reports what it did | **A typed `Decision`, not a string.** `refresh::on_read` returns the generation, a `Decision` and the `stale` fact; `Decision::line()` owns the seven texts, so a caller cannot invent an eighth (§3.5, D-12) | `refresh.rs` |
| D-22 | What a read prints when another writer already published | **Nothing.** It adopts the newer generation and answers. `refresh:` means this process wrote a generation, and this one did not (§3.5). The adoption branch runs only when a writer publishes between the read's load and its lock | `refresh.rs` |
| D-23 | How a read tells a first build from a repair | **The manifest's presence before the load.** `on_read` stats `manifest.json` first: absent means `refresh: no index yet`, present means `refresh: index repaired`. `store::load` keeps one `Stale` reason and gains no new field | `refresh.rs` |
| D-24 | Does a moved root print a line under `--no-rebuild` | **Yes.** A repair the caller declined is a skipped refresh, so it prints `refresh: skipped, --no-rebuild` (§3.5). It writes nothing, and the generation still answers | `refresh.rs` |
| D-25 | What a manifest repair republishes | **The current plan.** It runs `build_until_stable` with the read's walk, so one pass writes the current root and the current digest through the one writer loop (§3.2). Republishing without planning would need a second write path | `refresh.rs` |
| D-26 | Where the printer lives | **One module, `src/progress.rs`.** §3.6 first named the two callers as the home; one shared `Printer` is what keeps `janus index` and a read from drifting apart. It prints one newline-terminated line per 5 % step and never rewrites a line, so a log file keeps every step | `progress.rs` |
| D-27 | How a caller says it has nothing to report | **A no-op sink, `build::no_progress`.** `plan`, `plan_forced`, `plan_forced_dry_run` and `plan_from_walk` take a plain `EmbedSink`; `LoadOptions` and `build_until_stable` take an `Option` and substitute the no-op. The gate stays a `bool` in the printer, so no library path asks the terminal | `build.rs`, `store.rs` |

## 5. Phases

Three phases. One PR each, from `main` after the previous merge.

### Phase 1 — the witness, the writer's window, and the reported fact

**Goal:** a read can say whether the index is old, at a known cost, and no build loses an edit made
while it runs.

**Files:** `src/index/refresh.rs` (new), `src/index/mod.rs` (module declaration),
`src/index/store.rs` (the manifest field and the loop), `src/index/cmd.rs` (the lock and the loop),
`src/config.rs`, `src/search/mod.rs` (the witness in `Outcome` and `--json`),
`tests/staleness.rs` (new), `tests/cli_index.rs`, `tests/publish.rs`, `tests/cli_search.rs` if a
JSON key changes.

**Build (~260 logic lines):**

1. `Signature` and its canonical digest (§3.1), including the pre-epoch tag.
2. `Witness { stale, changed }`, from the stored digest and the fresh walk, with the max-mtime
   fallback when `corpus_sig` is `None`.
3. `Manifest.corpus_sig: Option<String>` with `#[serde(default)]`, carried by `Publication`.
4. `store::build_until_stable(dir, root, config, embedder, built_at, first_walk, forced, rebuild,
   &Lock)`: plan from `first_walk` → publish → re-walk → compare, bounded at 3 passes (D-13, D-14).
   `first_walk` is the walk the caller already has, so a stale read does not walk twice for nothing.
   `forced` and `rebuild` keep
   `janus index PATHS` and `--rebuild` working exactly as today. It replaces the bodies of
   `store::rebuild_plan` (`src/index/store.rs:1151`) and the plan half of `cmd::run`
   (`src/index/cmd.rs:65`). `cmd` acquires the lock itself. P3 adds the sink parameter.
5. `refresh::plan_from_walk`: resolve, derive identity, plan. The pure half, shared by both writers.
6. `[index] refresh = "auto" | "manual"`: parse, default `auto`, warn on an unknown value and use
   `auto`.
7. `prepare` (`src/search/mod.rs:329`) computes the witness and reports it in `--json` as
   `index: { "stale": bool }`. It prints no line in this phase.

**Tests (~300 lines):**

- A fresh vault is not stale. One edited document makes it stale.
- An added file, a removed file and a renamed file each make it stale. The rename keeps its mtime, so
  the digest must be the thing that sees it.
- A file touched to an older time makes it stale, because mtime is part of the signature.
- An older manifest with no `corpus_sig` falls back to max-mtime; a deletion is invisible there, and
  that limit is pinned.
- The fallback treats an empty corpus over a non-empty index as stale, and an edit in the same
  timestamp tick as the publish as fresh. Both are pinned.
- A file saved after the build's walk is rebuilt before the command returns: a test embedder writes a
  document on its first `embed` call, then the receipt reports two passes and the new text is in the
  index.
- A file removed during the build is removed from the index.
- The receipt's `index_sha` and `taken_ms` are the last pass's, and `passes` counts how many ran
  (`--json`).
- The digest is stable across two walks of an unchanged corpus, and differs when one byte of one
  file changes.

**Handoff (paste):** "Implement phase 1 of M4D per `docs/m4d-breakdown.md` §3.1, §3.2, §3.5 and §5.
The witness is a stored blake3 digest of the sorted `(rel_path, size, mtime)` signature, written into
the manifest as an optional `corpus_sig`; a read recomputes it from one walk. An older manifest with
no digest falls back to `max(document mtime) > mtime(index.bin)`, and that limit is named. The writer
re-walks after publish and rebuilds while the signature differs, because the digest describes the
walk, not the end of the build. The caller acquires the lock and passes `&Lock`. No clock read below
`main.rs`. A read reports the witness in `--json` only; it prints no stderr line in this phase. Write
comments in Simplified Technical English."

**Accept:** gates green. The fixture is fresh after an index. An edit, an add, a remove and a rename
each make it stale. An edit made during a build is in the index when the command returns. The witness
duration over the fixture is in the PR body.

### Phase 2 — refresh on read

**Goal:** an edit is visible on the next search, and a read never waits for a writer.

**Split seam:** if the logic passes 450 lines, split before the ceiling into **P2a** (the refresh
writer: `refresh.rs`, `store.rs`, `error.rs` — `Check`, `Mismatch`, `try_acquire`, the refresh entry
point) and **P2b** (the read policy: `search/mod.rs`, `search/facts.rs`, `config.rs` wiring, the
lines). P2b is stacked on P2a with `BASE=origin/m4d-refresh-a scripts/pr-size.sh`.

**Files:** `src/index/refresh.rs`, `src/index/store.rs`, `src/error.rs`, `src/search/mod.rs`,
`src/search/facts.rs`, `tests/staleness.rs`, `tests/cli_search.rs`, `tests/cli_index.rs`,
`tests/publish.rs`, `tests/hybrid_search.rs`.

**Build (~320 logic lines):**

1. `Check` and `Mismatch` replace `Expected::check`'s `Result<(), String>`, with `needs_full_build()`
   giving D-3/D-3a. `built_root` yields `Check::Refresh`, and `read_generation` returns the
   generation. The message text is unchanged.
2. `JanusError::IndexNeedsFullBuild { reason }`: exit 1, message
   `index needs a full build: run janus index` plus the reason. Additive; `api` stays `1`.
3. `Lock::try_acquire(dir) -> Result<Option<Lock>, JanusError>`.
4. `refresh::on_read(...)`: the seven-step order of §3.4, returning an adopted generation or a new
   plan. It takes the walk it was given, so a stale read pays one walk for the witness plus one for
   the post-publish comparison.
5. `prepare` (`src/search/mod.rs:329`) calls it and prints the one `refresh:` line. `facts` inherits
   it, because both use `prepare`.
6. `--no-rebuild` maps to the refusal rows. `[index] refresh = "manual"` skips step 4.

**Tests (~460 lines):**

- Edit one document, then `search`: the new text is found, and the refreshed `.janus/index.bin` and
  `.janus/vectors.f32` are **byte-identical** to a `--rebuild` copy taken before the refresh (R-8).
- A deleted document stops appearing, and a renamed one appears at its new path.
- An edit that changes only `status` (never the title or the body) costs no embedding work: after
  the edit, `janus index --json` reports `embedded: 0` and reuses the row for every chunk.
- A format or embedder mismatch refuses with one line naming `janus index`, and `.janus/` is
  byte-identical afterwards.
- A changed `corpus_id` refuses; a `built_root` mismatch refreshes and loads, and a busy lock there
  prints `refresh: skipped, index busy` and still answers.
- An absent index builds and prints `refresh: no index yet`; a corrupt pair rebuilds and prints
  `refresh: index repaired`.
- With the writer lock held, a stale read prints `refresh: skipped, index busy`, exits 0, and
  answers from the old generation. No sleep: hold the lock, run the read, release.
- `--no-rebuild` on a changed corpus prints `refresh: skipped, --no-rebuild`, exits 0, answers from
  the generation, and writes nothing. `refresh = "manual"` prints its line and writes nothing.
- `stale:` does not appear in any stderr.
- Every one of the five cold fields refuses, and the check is a unit test of `Expected::check`
  because `tokenizer` cannot be expressed as a readable manifest: the deserializer rejects an
  unknown fingerprint (I-1). The unit test also pins the order: a cold field wins over `built_root`.
- A held lock on a moved vault prints `refresh: skipped, index busy` and answers too.

**Tests that change, and why.** These pin behaviour this phase deliberately reverses. Each change is
listed in the PR body with its reason.

| test | change |
|---|---|
| `tests/cli_search.rs:147 a_stale_index_rebuilds_with_exactly_one_stderr_line` | a `corpus_id` change now refuses; rename to `a_changed_corpus_id_refuses_with_one_line` |
| `tests/cli_index.rs:357 a_format_version_2_manifest_is_refused_with_a_warning_and_rebuilt` | rename to `…_is_refused_with_one_line_and_not_rebuilt`; assert exit 1 and a byte-identical `.janus/` |
| `tests/publish.rs:893 a_format_version_2_manifest_is_stale_and_the_rebuild_writes_version_3` | assert `IndexNeedsFullBuild`; move the "rebuild writes version 3" coverage to a `janus index` test |
| `tests/publish.rs:929 a_vectors_less_generation_is_stale_for_a_process_that_can_embed` | rename to `…_refuses_to_build_from_a_read`; assert `IndexNeedsFullBuild` and no write |
| `tests/hybrid_search.rs:150 a_no_embeddings_rebuild_carries_unchanged_rows_and_drops_changed_ones` | re-point at `janus index --no-embeddings`; a read no longer rebuilds a `corpus_id` mismatch |
| `tests/cli_search.rs:175 search_without_an_index_builds_one` | add the `refresh: no index yet` assertion |
| `tests/cli_search.rs:193 an_edited_document_makes_search_json_report_stale` | the plan missed this one: a read now refreshes, so `index.stale` is `false` and one `refresh: corpus changed` line appears. Renamed to `an_edited_document_is_refreshed_and_reported_fresh` |
| `tests/cli_search.rs:217 an_edited_document_makes_facts_json_report_stale` | the same fact through `facts`, which inherits `prepare`. Renamed to `an_edited_document_is_refreshed_before_facts_answer` |

**Unchanged:** `tests/cli_search.rs:198 no_rebuild_on_a_stale_index_refuses_without_writing` keeps
`IndexUnstable`, and `tests/publish.rs:310` keeps its corrupt-pair rebuild (R-18).

**Handoff (paste):** "Implement phase 2 of M4D per `docs/m4d-breakdown.md` §3.3, §3.4, §3.5 and §5.
Order matters: load the validated generation first, then walk, then try the lock once without
blocking. A busy lock is a skip, not an error. Refuse only the five identity-changing mismatches;
`built_root` loads and refreshes, so `Expected::check` must return a generation for it. Retire the
`stale:` word; `refresh:` is the only line a read prints and it means a build ran. Change the six
tests listed in §5 and say so in the PR body. Write comments in Simplified Technical English."

**Accept:** gates green; an edit is visible on the next search; the refreshed payloads are
byte-identical to a clean build; no cold build is reachable from a read; a held lock does not fail a
read.

### Phase 3 — progress and the estimate

**Goal:** a long build says what it is doing, whether `janus index` or a read started it.

**Files:** `src/index/build.rs`, `src/index/store.rs` (the sink through `LoadOptions`),
`src/index/refresh.rs` (`plan_from_walk` carries the sink), `src/index/cmd.rs`, `src/search/mod.rs`,
`src/search/facts.rs`, `src/cli.rs` (the flag), `src/main.rs` (the gate), `src/progress.rs` (the
printer, new), `src/lib.rs` (the module), `tests/progress.rs` (new).

**Build (~200 logic lines):**

1. `EmbedSink` threaded through `plan`, `plan_forced`, `plan_forced_dry_run`, `plan_from_walk` and
   `plan_inner` into `embed_all`. A dry run passes a no-op.
2. The sink fires once per embedded chunk with `(done, total)`, where `total` is the number of texts
   this build will embed.
3. The sink also reaches `LoadOptions` and `store::load`, so a read-triggered build reports.
4. `--progress <auto|always|never>` on `janus index`, `search` and `facts`. `main.rs` resolves
   `auto` once and passes a `bool`.
5. The printer, in `src/progress.rs` (D-26): one line per 5 % step, the rate from the elapsed time,
   the estimate in whole minutes. The summary line is `embedded N chunks`. Both obey the gate.

**Tests (~220 lines):**

- A counting embedder over the 1 533-chunk fixture reports `done` strictly increasing, the last
  value equal to `total`, one report per embedded chunk, and at most 20 boundaries.
- The same build with the gate off prints nothing.
- `--progress=always` prints, and every line starts with `embedded `.
- A build that embeds nothing fires the sink zero times and prints no summary.
- A read that builds an absent index reports progress with `--progress=always`.
- stdout is unchanged: `--json` parses with and without progress.
- The estimate is never asserted (D-8).

**Handoff (paste):** "Implement phase 3 of M4D per `docs/m4d-breakdown.md` §3.6, §3.3 and §5. The
sink goes in `embed_all` and is threaded through all three plan entry points, because `janus index`
calls `plan_forced`, not `plan`, and a read calls `store::load`. The gate is resolved once in
`main.rs` from `--progress` and `stderr().is_terminal()`; the printer receives a plain `bool`. There
is no chunk-count floor. The total is the chunks this build embeds. Never assert the estimate or a
duration. Write comments in Simplified Technical English."

**Accept:** a build over the real corpus prints progress. The author runs it and reports the lines.

## 6. Cross-cutting

- **Determinism.** The witness reads the filesystem, so it is absent from every pure function. No
  wall clock is added below `main.rs`: the writer compares corpus signatures, and the estimate uses
  elapsed time in the printer only.
- **Reads fail open, writes fail closed.** A stale read either refreshes or says why not. It never
  returns an old answer in silence, and it never fails because another writer is busy.
- **No payload change.** No `format_version` bump, no index file added, no rebuild for an existing
  vault. The manifest gains one optional field.
- **The lock is reused**, not reinvented. Only the *try* variant is new.

## 7. Definition of done

1. Editing one document makes the next `search` see the edit, print exactly one
   `refresh: corpus changed` line, and leave `.janus/index.bin` and `.janus/vectors.f32`
   byte-identical to a `--rebuild` copy of the same vault.
2. A document added, removed or renamed makes the next `search` see it, with a `refresh:` line.
3. A document edited *during* a build is in the index when the command returns.
4. A read builds when the index is absent or the pair is corrupt. It refuses only for
   `format_version`, `tokenizer`, `chunker`, `corpus_id` or `embedder`, with one line naming
   `janus index`. A `built_root` mismatch loads and refreshes.
5. A held writer lock makes a stale read print `refresh: skipped, index busy`, answer from the old
   generation, and exit 0.
6. `--no-rebuild`, `refresh = "manual"` and every refusal write nothing: `.janus/` byte-identical.
7. **Person check.** A fresh read costs one walk; a stale read costs two, because the witness walk
   feeds the plan and only the post-publish comparison walks again. P1's PR body records
   `search --json`'s `taken_ms` over the fixture before and after, and over the real corpus.
8. With `--progress=always`, a build prints at most one line per 5 % of the chunks it embeds, and a
   read-triggered build does the same. With the gate off it prints nothing, and stdout is unchanged.
9. No `format_version` change, no rebuild for an existing vault, no new dependency.
10. Gates: fmt, clippy, tests, doc, Windows cross-check, `scripts/pr-size.sh`.
11. **Milestone acceptance, by the author:** edit one document, run `search`, and see the edit
    without running `janus index`.

## 8. PR plan and size budget

| PR | Branch | Logic | Tests | Content |
|---|---|---|---|---|
| P1 | `m4d-staleness` | ~260 | ~300 | the digest, the writer's loop, the config key, the reported witness |
| P2 | `m4d-refresh` | ~320 | ~460 | the refresh, the non-blocking lock, the typed refusals, the read order |
| P3 | `m4d-progress` | ~200 | ~220 | the sink, the terminal gate, the estimate |

If P2 passes 450 logic lines, split it at the named seam into P2a and P2b (§5). Never shave a
comment or drop a test to fit the ceiling.

## 9. SPEC corrections

1. **§8.3, rebuild triggers** — a corpus change since the index is a *refresh* trigger, not a rebuild
   trigger. A format, tokenizer, chunker, `corpus_id` or embedder mismatch is cured only by
   `janus index`. A `built_root` mismatch is never an error (unchanged).
2. **§8.3, rebuild triggers** — `schema_version` is removed from the list. The code never compares it
   (`Expected` at `src/index/store.rs:725`); the manifest field is diagnostic in v1.
3. **§8.3, reader protocol** — replace "if no generation validates they take the writer lock,
   re-check freshness, rebuild once, and then refuse". A reader validates without a lock. When the
   corpus changed it tries the writer lock once, without blocking, and on a busy lock answers from the
   generation it validated. When the index is absent or the pair is corrupt it takes the writer lock
   and rebuilds once. When the mismatch is cold it refuses. `--no-rebuild` never writes.
4. **§8.3, the index layout** — `manifest.json` gains an optional `corpus_sig` (blake3 of the corpus
   signature). It is additive: an older reader ignores it, and a missing value falls back to the
   mtime rule. `format_version` does not move.
5. **§9.1** — `janus search` and `janus facts` refresh by default; `--no-rebuild` refuses; the new
   error `index needs a full build` exits 1.
6. **§9.2** — MCP reads inherit the refresh and the refusal. They have no `--no-rebuild`, so the
   refusal line must name `janus index`.
7. **§9.5** — add the error code `index needs a full build` (additive; `api` stays `1`), and the
   `passes` field of the `janus index --json` receipt (additive).
8. **§13** — replace the "Incremental reindex (1 changed doc) ≤ 150 ms" row with a measured row on a
   stated corpus, release build, with the document count in the row. Add the witness cost (8.5 ms
   over 2 119 files) so the next reader does not re-measure it.
9. **§15, D50** — extend the row, do not add another. D50 already says mtime is never a date. Add:
   mtime **is** valid as a cache witness, because a cache asks only whether the bytes changed.
10. **§16.1** — the `[index]` block gains `refresh = "auto" | "manual"`.

## 10. Handoff to M5

Two decisions are agreed and belong to the temporal milestone, not to M4D:

1. **Quarter dates in names.** `q4-2026`, `q2_2026`, `2026-q4` and `2026.q4` parse to the first day
   of that quarter, using the same separator set as the ISO shapes. A new precision variant, the
   same ladder, the same whole-prefix and year-range rules.
2. **An undated document must stop being advantageous.** Today it decays at 1.0, so a document is
   rewarded for missing a date. The agreed rule: an undated document decays by the **median age of
   the dated documents in the corpus**. That is a guess, but a corpus-relative one, and it is
   measurable. It needs a decision row, a test, and a note when the corpus has no dated documents.

## 11. Implementation log — Phase 1

- **The signature is a pure function.** `refresh::Signature` sorts the walked
  `(rel_path, size, mtime)` and encodes it as length-prefixed fields, so a path
  cannot shift a field boundary. The mtime encodes as `(tag, secs, nanos)`: a
  pre-epoch time carries its own tag and its distance from the epoch, so it never
  panics and never collapses into a post-epoch value of the same magnitude. No
  `as_secs()`-only encoding: it would hide a change inside one second.
- **The witness reports one boolean.** `Witness::of` compares the digest the
  manifest stores, or falls back to `max mtime > mtime(index.bin)` when the
  manifest has no digest. `Witness { changed }` from §5 was dropped: a boolean
  answer is all a caller uses, and §2 says as much.
- **The writer closes its own window.** `store::build_until_stable` plans from the
  walk the caller made, publishes the digest of that walk, re-walks, and repeats
  while the digest differs, bounded at `MAX_PASSES = 3`. `janus index` and a
  read-triggered rebuild both use it, and both hold one lock for every pass.
- **The command owns the lock.** `cmd::run` acquires it and passes `&Lock`, so
  `publish_locked` never tries to take it twice. `--dry-run` takes no lock.
- **`prepare` walks once for the witness and prints nothing.** The walk is the
  price of an exact answer; P2 reuses it for the refresh.
- **Deviations.** `build_until_stable` keeps the nine named arguments and carries
  `#[allow(clippy::too_many_arguments)]`, with a comment saying why, as
  `build::reusable` already does. `first_walk` is taken by value, so the loop owns
  the walk and `WalkResult` needs no `Clone`.
- **Not in this phase.** No refresh on read, no `refresh:` line, no `try_acquire`,
  no `Check`/`Mismatch`, no progress sink or flag, no `Expected::check` change, no
  payload or `format_version` change, no new dependency.
- **Size.** 559 logic lines and 686 test lines. The 450-line split seam was not
  used, for the reason in D-18.

## 12. Implementation log — Phase 2

### Phase 2a — the mutations a read needs

- **The size forced the split the plan named.** The phase reached 683 logic
  lines, over the 600 ceiling, so it ships as P2a (writer and validation) and
  P2b (read policy and lines), stacked. P2a is 597 logic lines, measured with
  `BASE=origin/main scripts/pr-size.sh`. The number is high because a deletion
  counts: 73 of those lines are the old `Expected::check` body.
- **`Check` is the only validation answer.** `Expected::check` returns
  `Check { Ok, Refresh, Refuse }` and compares `format_version`, `tokenizer`,
  `chunker`, `corpus_id` and `embedder` before `built_root` (D-3a). A unit test
  pins both facts: every cold field refuses with M3's message text, and a cold
  field wins over `built_root`. That ordering is the whole point: the reverse
  sends a read to an 18-minute re-embed.
- **The corrupt vector triple stayed a rebuild.** It moved out of `check` into
  `read_generation` (D-19), so a manifest holding two of three vector facts is
  still repaired by a build, as M3 did.
- **`Mismatch` carries the two embedder wordings through one sentinel** (D-20).
  The message text is unchanged, so every existing assertion kept its string.
- **`Lock::try_acquire` is one attempt.** `acquire` keeps its timeout for the
  absent-index and corrupt-pair rows. `open_lock_file` is shared, so both open
  the file the same way.
- **`on_read` runs the seven steps of §3.4.** It loads the generation first,
  walks, declines or refreshes, and returns a typed `Decision` (D-21). It adopts
  a writer's generation only when the manifest's `corpus_sig` equals the digest
  this read computed; the predicate is `adoptable`, and a unit test fails if the
  comparison is inverted or the pre-M4D `None` is accepted (D-22).
- **A first build is told from a repair by the manifest's presence** (D-23),
  read before the load. `store::load` gains one field, `refresh_request`, and
  keeps its one `Stale` reason.
- **Size.** 597 logic lines and 444 test lines.

### Phase 2b — the read policy and the lines

- **`prepare` is now three steps: walk, `on_read`, one line.** The walk happens
  before the read, so the witness walk is the refresh's plan input (§3.1). The
  `stale:` print is gone; `Load.stale` stays a field and is no longer read by
  the command.
- **`config.refresh` is read, not re-resolved.** `on_read` takes `Refresh` and
  declines the changed-corpus row for `Manual`; every other row still works
  (D-4).
- **A declined repair prints a line** (D-24). The §3.3 table's `--no-rebuild`
  cell for `built_root` is amended to match.
- **The plan's test list was wrong, twice, and is amended here** (§5 P2). Two
  tests that assert `index.stale` after an edit reverse under this phase, and
  the `tokenizer` row of `Expected::check` cannot be reached through a readable
  manifest, because the deserializer rejects an unknown fingerprint (I-1). It
  is pinned by a unit test instead, together with the ordering rule.
- **`facts` needed no code change.** It shares `prepare`, so it inherits the
  refresh and the one line.
- **Size.** 86 logic lines and 318 test lines against `m4d-refresh-a`, measured
  with `BASE=m4d-refresh-a scripts/pr-size.sh`. The two phases together are 683
  logic lines, which is why the seam was used.

## 13. Implementation log — Phase 3

- **The sink is one callback in `build.rs`.** `EmbedSink` fires inside `embed_all`,
  after a row is validated and normalised, so a broken row fails the build
  before it can be reported as progress. `done` counts chunks, `total` is the
  number of texts this build embeds, and a build that reuses every row calls
  nothing.
- **`no_progress` is the quiet sink.** `plan`, `plan_forced`, `plan_forced_dry_run`
  and `plan_from_walk` take a plain `EmbedSink`, because every caller can pass a
  function item. `LoadOptions` and `build_until_stable` take an `Option` and
  substitute the no-op in one place (D-27).
- **The printer is one module** (D-26). `index::cmd` and `search::prepare` each
  build a `Printer` and hand its `report` to the build as the sink. The gate is a
  `bool` in the printer; nothing below `main.rs` asks the terminal.
- **The fixtures confirm the numbers.** Over the 1 533-chunk fixture with
  `--progress=always`, a cold build prints **20 progress lines** and **one
  summary**, `embedded 1533 chunks`. The first line reads
  `embedded 77/1533 chunks (5%)  ~0 min left`. A captured stderr prints nothing.
- **The last chunk prints two lines.** Its step is 20, so the 100 % step line
  prints, and then the summary. Both are under the same gate, and the cap of 20
  progress lines still holds.
- **Deviations.** §3.6 first named `cmd.rs` and `search/mod.rs` as the printer's
  home; the printer is one module instead, and §3.6 is amended. §5 P3's file list
  gains `src/progress.rs`, `src/lib.rs` and `src/index/refresh.rs`, because
  `plan_from_walk` is the call between the two writers and the sink must reach
  it.
- **Not in this phase.** No change to the `refresh:` lines, `on_read`, `Check`,
  the refusal set, `Signature`, `Witness`, the writer loop's algorithm, scoring,
  the payload, the chunker or `format_version`. No new crate, no feature flag.
- **Size.** 241 logic lines and 287 test lines, 528 in total, measured with
  `scripts/pr-size.sh` against `origin/main` at `3096001`. The 400-line split
  seam was not used.
