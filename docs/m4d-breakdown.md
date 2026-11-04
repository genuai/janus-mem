# M4D — The index keeps itself current

> Branch: `m4d-plan` (this document), then one branch per phase from `main`.
> Spec: §8.3 (publication, rebuild triggers), §9.1, §9.2, §13, §14, §15, §16.1.
> Evidence: a 2 119-document corpus, measured across M4, M4B and M4C.
>
> **Done when:** an edit is visible on the next `search` without the user running `janus index`,
> and a build that takes minutes prints its progress.

**Words used here.** **stale**: the corpus changed after the index was built.
**refresh**: an incremental build that reuses unchanged work. **cold build**: a build with nothing
to reuse. **witness**: the filesystem fact that tells a read the index is old.

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

- A staleness check on read: exact, one walk, and reported in one line.
- Incremental refresh on read when the corpus changed, including embedding only the changed chunks.
- A read never rebuilds for a format, embedder or corpus identity mismatch. That is a command, and
  the refusal names it.
- A writer that closes its own lost-update window: an edit made during a build is in the index when
  the command returns.
- Progress and an estimate during any embedding work, on stderr, behind a terminal gate.
- A note when a read declines to refresh, naming the reason.

### Not in scope

- A file watcher, a daemon, or `janus watch` (M8).
- Resuming a killed cold build. Refresh makes it rare; it stays impossible (P-2).
- Quantisation, a smaller model, a faster runtime. Cold-build throughput is M8's.
- Quarter dates and the undated-decay rule. Both are agreed and handed to M5 (§10).
- Any change to the index layout, `format_version`, or the scoring rules.
- `janus doctor` freshness reporting. `doctor` does not exist yet; §14 keeps its promise for later.

## 0a. Resolutions from the plan review

Ten findings. Every one is resolved in this document, and the sections below are the resolution.

| # | finding | resolution |
|---|---|---|
| R-1 | A document edited *during* a build stays invisible: `index.bin`'s mtime is the end of the build | **Fixed.** §3.2: re-walk after publish and compare the corpus signature with the walk that produced the plan; rebuild while it differs. No stored field, no clock read, no format change. Phase 1. |
| R-2 | "Index absent → refuse" reverses six tests and two contracts, and a corrupt pair still rebuilds cold | **Fixed.** §3.3: a read builds when the index is absent or the pair is corrupt. It refuses only for a cold mismatch. `built_root` refreshes, not refuses, because SPEC §8.3 says a moved vault "is never an error". |
| R-3 | A stale read blocks 2.6 s and can exit 1 on the 10 s lock | **Fixed.** §3.4: load the validated generation first, then refresh under a non-blocking lock. Busy → `refresh: skipped, index busy`, exit 0, old generation. |
| R-4 | The 10 % cost budget cannot be met, and D-5a cannot see an in-place edit | **Fixed.** §3.1 and D-5: no budget. One walk per read, shared with the refresh. D-5a is deleted, with the reason recorded. |
| R-5 | The 500-chunk floor is under the real fixture (1 533 chunks) and breaks `a_clean_build_writes_no_stderr` | **Fixed.** §3.6: no chunk floor. Progress prints only when stderr is a terminal, or with `--progress=always`. A captured stderr is silent. |
| R-6 | The progress seam is wired to `build::plan`, which `janus index` does not call | **Fixed.** §3.6: the callback lives in `plan_inner`/`embed_all` and is threaded through `plan`, `plan_forced` and `plan_forced_dry_run`. Phase 3. |
| R-7 | Strict `>` gives no rule for equal timestamps, and a newer index is never stale | **Fixed.** §3.1: `>` on read, and the blind spots are named with the escape hatch. The writer's window is closed by §3.2, not by the comparison. |
| R-8 | Phase 2's acceptance cannot be measured as written | **Fixed.** §5 P2: a byte comparison of `.janus/index.bin` and `.janus/vectors.f32` against a `--rebuild` copy made before the refresh. |
| R-9 | §9 duplicates D50 and omits the reader protocol, §9.1, §9.2 and the exit code | **Fixed.** §9 amends D50 and lists every SPEC change, with the new error code. |
| R-10 | "5 % of the total" defines no total | **Fixed.** §3.6: the total is the chunks *this build embeds*. A full reuse fires the callback zero times and prints no summary. |

## 1. Dependencies

None. No crate. No feature flag.

## 2. Data model

```rust
// src/index/refresh.rs — the witness and the writer's convergence loop.
pub struct Witness {
    /// The newest document mtime seen by the walk.
    pub newest: SystemTime,
    /// Documents whose mtime is newer than `index.bin`.
    pub changed: usize,
}

/// The corpus as one walk saw it. Two signatures are equal when every admitted
/// file has the same path, size and mtime.
pub struct Signature { /* private */ }

// src/index/build.rs — one callback, the seam the tests use.
pub struct EmbedProgress<'a> {
    pub on_embedded: &'a mut dyn FnMut(u64 done, u64 total),
}

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
```

`Witness` is computed per read and never persisted. `EmbedProgress` is a seam: the CLI prints, the
tests count. Nothing on disk changes shape.

## 3. Contracts

### 3.1 The witness: how a read knows the index is old

**The index file's own modification time is the build time.** `publish` renames a temp file into
place, so `index.bin`'s mtime is when that generation was written.

```
stale ⟺ max(document mtime) > mtime(index.bin)        (strict >)
```

- **No manifest field, no format change, no rebuild.** A stored timestamp would need one of the three.
- **One walk gives the mtimes.** `DiscoveredFile` already carries `mtime` (`src/vault/mod.rs:68`), so
  the check adds no `stat` per file. The same walk feeds the refresh when the read decides to build.
- **The count is reported, not the list.** `N document(s) changed since the index was built`.
- **The walk is the existing one**, with the same excludes, so the check agrees with the build.
- **Cost, measured:** 8.5 ms over 2 119 files (best of 7, release, warm cache). There is no budget
  line. The number goes in P1's PR body, and it is not asserted by a test.
- **mtime is valid here and invalid as a document date.** M4B removed it from the date ladder because
  a copy rewrites it. A *cache* is the opposite case: the question is whether the bytes changed, and
  mtime is the cheapest witness. D50 gains this half (§9).

**Named blind spots.** The witness is exact for a normal edit. It is blind to:

- a content change that restores the old mtime (`cp -a`, a backup restore, a zip that keeps times);
- a filesystem with coarse timestamps (FAT: 2 s) and a network mount that caches attributes;
- a restored `.janus/` whose `index.bin` is newer than every document.

`janus index --rebuild` is the escape hatch for all three. The `search` docs say so.

### 3.2 The writer closes its own window

**An edit made during a build must be in the index when the build returns.**

`publish` writes `index.bin` at the *end* of the build. A file saved at minute 5 of an 18-minute
build is therefore older than `index.bin`, and the witness at §3.1 calls it fresh. The edit would be
invisible until someone touched the file again.

The writer closes the window without a stored field and without reading the clock:

```
walk W0 → plan → publish → walk W1
while signature(W1) != signature(W0):
    plan against W1 → publish → walk W2 ; W0, W1 := W1, W2
stop after 3 passes, or when two consecutive signatures are equal
```

- `signature` is the sorted list of `(rel_path, size, mtime)` over the walked files.
- **The second pass is cheap.** It reuses every token and every row, so it costs 2.6 s on the real
  corpus, not 18.3 minutes.
- **Both writers use it**: `janus index` and a read-triggered build. One function, one rule.
- **Bound.** After 3 passes the corpus is still moving. Publish the last plan and print
  `refresh: corpus changed during the build; some edits may need another run`. A vault under a
  continuous writer cannot converge, and pretending otherwise is worse than one line.
- **Why not a stored build-start time.** The manifest is JSON without `deny_unknown_fields`, so an
  optional field would work. The signature comparison needs no field, no format bump, and no clock
  below `main.rs`, and it is testable with the existing fixture.

### 3.3 What a read does about it

**A read refreshes when the corpus changed. It builds when the index is absent or the pair is
corrupt. It refuses only when a full build is the only cure.**

| state | `search` / `facts`, `refresh = "auto"` | `--no-rebuild` |
|---|---|---|
| fresh | read; no lock, no line | read |
| corpus changed | **refresh, then read**. `refresh: N document(s) changed` | refuse; write nothing |
| index absent | **build once, then read**. `refresh: no index yet` | `no index yet: run janus index` (exit 1) |
| pair corrupt, retries exhausted | **rebuild once, then read**. `refresh: index repaired` | refuse; write nothing (exit 1) |
| `format_version`, `tokenizer`, `chunker`, `corpus_id` or `embedder` mismatch | **refuse**: `index needs a full build: run janus index` (exit 1) | refuse (`IndexUnstable`, exit 1; unchanged) |
| `built_root` mismatch | **refresh, then read**. `refresh: N document(s) changed` | refuse |

**Which mismatches refuse.** A refusal is correct only when an incremental build cannot cure the
state. `Expected::check` (`src/index/store.rs:758`) compares six fields. Five of them change the
identity of every chunk or every row, so the cure is a cold build:

| field | why a read may not build it |
|---|---|
| `format_version` | the layout changed; the previous bytes may not decode |
| `tokenizer` | every token changes; nothing is reusable |
| `chunker` | every chunk boundary changes; nothing is reusable |
| `corpus_id` | every `DocId` changes; tokens and rows are both unreusable |
| `embedder` | the rows have a different width or meaning; all of them must be regenerated |

`built_root` is the exception. It is diagnostic only (`src/index/store.rs:414`), every `DocId` is
unchanged, and SPEC §8.3 states that a moved vault "is never an error". So it refreshes.

**`Expected::check` returns a typed `Mismatch`, not a `String`.** The caller needs the field to
decide: refuse, refresh, or accept. The message text is unchanged, so the existing tests keep their
strings.

**Why an absent index still builds.** It is the first use, there is nothing to reuse, and the corpus
is empty of history. Refusing would deadlock an MCP-only agent: SPEC §9.2 gives `search` no
`--no-rebuild` and offers no index tool. The build is no longer silent, so the user sees why it is
slow (§3.6).

**`--no-rebuild` keeps today's promise.** A read that refuses writes nothing, byte for byte, and
keeps M3's `IndexUnstable` message naming the file. The new `index needs a full build` error belongs
to the default path only, because it names the command the user must run.

**`[index] refresh = "manual"` disables only the corpus-changed row.** It prints
`refresh: skipped, refresh = manual` and answers from the validated generation. An absent index and
a corrupt pair still build, because manual means "do not refresh", not "do not work".

### 3.4 The refresh never waits for a busy writer

**A read that can answer must not block, and must not fail on a lock.**

1. Load the validated generation, with no lock, exactly as today (M3 §3.5).
2. Walk the corpus and compute the witness (§3.1).
3. Fresh → answer. Done.
4. Stale → try the writer lock **once, non-blocking**.
5. Busy → print `refresh: skipped, index busy`, answer from the generation loaded in step 1, exit 0.
6. Acquired → re-read the generation under the lock. If another writer refreshed it, adopt it and
   skip the build. Otherwise build, then answer from the new plan.

- **Why non-blocking.** A validated read takes no lock today, so `search` works while `janus index`
  runs. A 10 s wait (`LOCK_TIMEOUT`, `src/index/store.rs:45`) that can exit 1 (`LockTimeout`) would
  turn a working read into a failure. The refresh is a convenience; it must never be a hazard.
- **`Lock::try_acquire(dir) -> Result<Option<Lock>, JanusError>`** is new. `WouldBlock` → `Ok(None)`.
  `Lock::acquire` keeps its blocking behaviour for the recovery path (§3.3 rows 3 and 4), so
  `a_held_lock_times_out_without_writing` and the two-thread auto-build test stay true.
- **The recovery path still blocks.** A read with an absent index or a corrupt pair takes
  `Lock::acquire(dir, lock_timeout)` and may return `LockTimeout`. That is today's protocol, and a
  corrupt pair has no generation to answer from.

### 3.5 One line per read, one meaning

`stale:` is retired. One word, `refresh:`, means this read ran a build.

| line | when |
|---|---|
| `refresh: N document(s) changed` | a corpus refresh published a generation |
| `refresh: no index yet` | the first build |
| `refresh: index repaired` | a rejected pair was rebuilt |
| `refresh: skipped, index busy` | another writer holds the lock (§3.4) |
| `refresh: skipped, refresh = manual` | `[index] refresh = "manual"` |
| `refresh: corpus changed during the build; some edits may need another run` | the 3-pass bound |
| `index needs a full build: run janus index` | a cold mismatch; exit 1 |

Rules:

- **At most one line.** A read that refreshes prints the `refresh:` line and nothing else. A read
  that adopts a valid generation prints nothing. `Load.stale` keeps its cause for `--json` and tests,
  but it is not printed.
- **stderr, never stdout.** stdout is the results stream (§8.5, §9.5).
- **The line names no timing.** `janus index --json` already reports `taken_ms`. A duration on a
  read line is untestable and adds nothing.

### 3.6 Progress and the estimate

**Every embedding batch reports its position, on stderr, when a human is watching.**

```
embedded 8192/27234 chunks (30%)  ~12 min left
```

- **stderr**, never stdout.
- **The gate is the terminal, not a chunk count.** `--progress <auto|always|never>`, default `auto`:
  print only when stderr is a terminal (`std::io::IsTerminal`). A captured stderr — tests, CI, a
  pipe — is silent, so `a_clean_build_writes_no_stderr` keeps its meaning on any machine.
  `--progress=always` forces the lines; `--progress=never` suppresses them.
- **The total is the chunks this build embeds.** A refresh that embeds 13 chunks reports `x/13`. A
  run that reuses every row embeds nothing, fires the callback zero times, and prints no summary.
- **At most one line per 5 % of that total.** `step = done × 20 / total`; print when `step` changes
  and `step >= 1`. At most 20 lines.
- **The estimate** uses the observed rate and the elapsed time in the printer, which lives at the
  edge (`src/index/cmd.rs`). It is rounded to a whole minute. No test asserts it (D-8).
- **The summary** is one line: `embedded N chunks`. It prints under the same gate.

**The seam is the library, not the printer.** `EmbedProgress` is threaded into `embed_all`
(`src/index/build.rs:588`), the only caller of `Embedder::embed`, and it fires once per embedded
chunk. Tests use a counting embedder and assert the values, never the text.

### 3.7 What must not change

- **No format change.** No `format_version` bump, no index file added.
- **The refresh reuses everything M3 and M4 built**: tokens by `lex_key`, rows by `body_hash` and the
  chunk-id sequence. A refresh is not a smaller cold build.
- **The scoring path is untouched.** No weight, no threshold, no tie-break.
- **A validated read still takes no lock.**
- **`--no-rebuild` writes nothing**, and `--dry-run` never refreshes.

## 4. Decisions

| # | question | decision | where |
|---|---|---|---|
| D-1 | How a read detects staleness | `max(document mtime) > mtime(index.bin)`, computed from the walk's mtimes. No stored field, no format change | `refresh.rs` |
| D-2 | What a stale read does | Incremental refresh under a non-blocking lock, one `refresh:` line | `search/mod.rs` |
| D-3 | May a read run a cold build | **No**, for the five identity-changing mismatches. It refuses and names `janus index` | `store.rs` |
| D-3a | Does a `built_root` mismatch refuse | **No.** It refreshes: every `DocId` is unchanged, and SPEC §8.3 says a moved vault is never an error | `store.rs` |
| D-4 | Is the refresh configurable | Yes: `[index] refresh = "auto" \| "manual"`, default `auto`. `manual` disables the corpus-changed refresh only | `config.rs` |
| D-5 | The cost budget for the witness | **None.** One walk per read, shared with the refresh; 8.5 ms measured over 2 119 files. The number is reported, never asserted | `refresh.rs` |
| D-5a | ~~Directory-mtime prefilter~~ | **Deleted.** A directory's mtime does not change on an in-place edit, so the prefilter misses the single case this milestone exists for | — |
| D-6 | Progress cadence | One line per 5 % of the chunks this build embeds, on stderr, only when stderr is a terminal or `--progress=always` | `build.rs`, `cmd.rs` |
| D-7 | The progress seam | A callback in `plan_inner`/`embed_all`, threaded through all three plan entry points | `build.rs` |
| D-8 | Is the estimate asserted | **No.** Printed, never compared | — |
| D-9 | Does refresh change `--dry-run` | **No.** A dry run never refreshes; it reports what a run would do | `cmd.rs` |
| D-10 | Read order | Load the validated generation first, then decide about the refresh. A read that can answer never waits | `search/mod.rs` |
| D-11 | A busy writer during a refresh | Skip the refresh, print `refresh: skipped, index busy`, answer from the loaded generation, exit 0 | `store.rs` |
| D-12 | How many lines a read prints | At most one, and `refresh:` is the only word | `search/mod.rs` |
| D-13 | The writer's lost-update window | Re-walk after publish; rebuild while the corpus signature differs; stop after 3 passes | `refresh.rs` |

## 5. Phases

Three phases. One PR each, from `main` after the previous merge.

### Phase 1 — the witness, the writer's window, and the note

**Goal:** a read can say whether the index is old, at a known cost, and no build loses an edit made
while it runs.

**Files:** `src/index/refresh.rs` (new), `src/index/mod.rs` (module declaration),
`src/index/store.rs` (the rebuild path calls the loop), `src/index/cmd.rs` (the command calls the
loop), `src/config.rs` (`[index] refresh`), `src/search/mod.rs` (the note), `tests/staleness.rs`
(new), `tests/cli_index.rs` if a count changes.

**Build (~220 logic lines):**

1. `refresh::witness(index_mtime, files) -> Witness`: the newest mtime and the number changed.
2. `refresh::Signature`: `(rel_path, size, mtime)` per admitted file, comparable for equality.
3. `refresh::build_until_stable(...)`: walk → plan → publish → re-walk → compare, bounded at 3
   passes (D-13). It replaces the bodies of `store::rebuild_plan` (`src/index/store.rs:1151`) and the
   plan half of `cmd::run` (`src/index/cmd.rs:65`), so both writers obey one rule.
4. `[index] refresh = "auto" | "manual"`: parse, default `auto`, warn on an unknown value and use
   `auto`.
5. A read computes the witness and prints `refresh: N document(s) changed` when stale. In this
   phase it does not refresh.
6. `Load` gains `stale: bool` and `changed: usize`, so a caller can see the witness. Additive;
   `api` stays `1`.

**Tests (~260 lines):**

- A fresh vault is not stale. One edited document makes it stale, and the count is 1.
- An added file, a removed file and a renamed file each make it stale.
- A file touched to an older time is **not** stale. That is the documented blind spot, pinned.
- A file saved after the build's walk is rebuilt before the command returns: a test embedder writes
  a document on its first `embed` call, then the receipt reports two passes and the new text is in
  the index.
- A file removed during the build is removed from the index.
- `refresh = "manual"` prints `refresh: skipped, refresh = manual` and writes nothing.
- **The cost, measured and printed**: the test prints the witness duration over the committed
  fixture. It never asserts a duration.

**Handoff (paste):** "Implement phase 1 of M4D per `docs/m4d-breakdown.md` §3.1, §3.2, §3.5 and §5.
The witness is `max(document mtime) > mtime(index.bin)`, computed from the walk's own
`DiscoveredFile.mtime`. The writer re-walks after publish and rebuilds while the corpus signature
differs, because `index.bin`'s mtime is the end of the build. No stored field, no format change, no
clock read below `main.rs`. A read reports staleness and does not act on it yet. Module docs must
state why mtime is valid for a cache and invalid as a document date, and name the blind spots. Print
the witness duration; never assert one. Write comments in Simplified Technical English."

**Accept:** gates green. The fixture is not stale after an index. An edit makes it stale with the
right count. An edit made during a build is in the index when the command returns. The printed
duration over the fixture is in the PR body.

### Phase 2 — refresh on read

**Goal:** an edit is visible on the next search, and a read never waits for a writer.

**Split seam:** if the logic passes 450 lines, split before the ceiling into **P2a** (the refresh
writer: `refresh.rs`, `store.rs`, `error.rs` — the typed `Mismatch`, `try_acquire`, the refresh
entry point) and **P2b** (the read policy: `search/mod.rs`, `search/facts.rs`, `config.rs` wiring,
the lines). P2b is stacked on P2a with `BASE=origin/m4d-refresh-a scripts/pr-size.sh`.

**Files:** `src/index/refresh.rs`, `src/index/store.rs`, `src/index/error.rs`,
`src/search/mod.rs`, `src/search/facts.rs`, `tests/staleness.rs`, `tests/cli_search.rs`,
`tests/cli_index.rs`, `tests/publish.rs`.

**Build (~300 logic lines):**

1. `Mismatch` and `MismatchField` replace `Expected::check`'s `Result<(), String>`, with
   `needs_full_build()` giving D-3/D-3a. The message text is unchanged.
2. `JanusError::IndexNeedsFullBuild { reason }`: exit 1, message
   `index needs a full build: run janus index` plus the reason. Additive; `api` stays `1`.
3. `Lock::try_acquire(dir) -> Result<Option<Lock>, JanusError>`.
4. `refresh::on_read(...)`: the six-step order of §3.4, returning either an adopted `Load` or a new
   plan. It takes the walk it was given, so a stale read pays one walk for the witness plus one for
   the post-publish comparison.
5. `prepare` (`src/search/mod.rs:329`) calls it. `facts` inherits it, because both use `prepare`.
6. `--no-rebuild` maps to the refusal rows. `[index] refresh = "manual"` skips step 4.

**Tests (~420 lines):**

- Edit one document, then `search`: the new text is found, and the refreshed `.janus/index.bin` and
  `.janus/vectors.f32` are **byte-identical** to a `--rebuild` copy taken before the refresh (R-8).
- An edit that changes only `status` (never the title or the body) costs no embedding work: after
  the edit, `janus index --json` reports `embedded: 0` and reuses the row for every chunk.
- A format or embedder mismatch refuses with one line naming `janus index`, and `.janus/` is
  byte-identical afterwards.
- A changed `corpus_id` refuses; a moved root refreshes.
- An absent index builds and prints `refresh: no index yet`; a corrupt pair rebuilds and prints
  `refresh: index repaired`.
- With the writer lock held, a stale read prints `refresh: skipped, index busy`, exits 0, and
  answers from the old generation. No sleep: hold the lock, run the read, release.
- `--no-rebuild` and `refresh = "manual"` each print their line and write nothing.
- `stale:` does not appear in any stderr.

**Tests that change, and why.** These pin behaviour this phase deliberately reverses. Each change
is listed in the PR body with its reason.

| test | change |
|---|---|
| `tests/cli_search.rs:147 a_stale_index_rebuilds_with_exactly_one_stderr_line` | the `corpus_id` change now refuses; rename to `a_changed_corpus_id_refuses_with_one_line` |
| `tests/cli_index.rs:357 a_format_version_2_manifest_is_refused_with_a_warning_and_rebuilt` | rename to `…_is_refused_with_one_line_and_not_rebuilt`; assert exit 1 and a byte-identical `.janus/` |
| `tests/publish.rs:929 a_vectors_less_generation_is_stale_for_a_process_that_can_embed` | rename to `…_refuses_to_build_from_a_read`; assert `IndexNeedsFullBuild` and no write |
| `tests/cli_search.rs:175 search_without_an_index_builds_one` | add the `refresh: no index yet` assertion |
| `tests/publish.rs:310 an_interrupted_publish_is_recovered_by_a_rebuild_and_refused_without_one` | keep: a corrupt pair still rebuilds |

**Handoff (paste):** "Implement phase 2 of M4D per `docs/m4d-breakdown.md` §3.3, §3.4, §3.5 and §5.
Order matters: load the validated generation first, then walk, then try the lock once without
blocking. A busy lock is a skip, not an error. Refuse only the five identity-changing mismatches;
`built_root` refreshes. Retire the `stale:` word; `refresh:` is the only line a read prints. Change
the five tests listed in §5 and say so in the PR body. Write comments in Simplified Technical
English."

**Accept:** gates green; an edit is visible on the next search; the refreshed payloads are
byte-identical to a clean build; no cold build is reachable from a read; a held lock does not fail
a read.

### Phase 3 — progress and the estimate

**Goal:** a long build says what it is doing.

**Files:** `src/index/build.rs`, `src/index/cmd.rs`, `src/main.rs`, `tests/progress.rs` (new).

**Build (~180 logic lines):**

1. `EmbedProgress` threaded through `plan`, `plan_forced`, `plan_forced_dry_run` and `plan_inner`
   into `embed_all`. A dry run passes a no-op.
2. The callback fires once per embedded chunk with `(done, total)`, where `total` is the number of
   texts this build will embed.
3. `--progress <auto|always|never>` on `janus index`; `auto` checks `std::io::IsTerminal` on stderr.
4. The printer: one line per 5 % step, the rate from the elapsed time, the estimate in whole
   minutes. The summary line is `embedded N chunks`. Both obey the gate.

**Tests (~200 lines):**

- A counting embedder over the 1 533-chunk fixture reports `done` strictly increasing, the last
  value equal to `total`, and at most 20 boundaries.
- The same build with stderr captured prints nothing.
- `--progress=always` prints, and every line starts with `embedded `.
- A build that embeds nothing fires the callback zero times and prints no summary.
- stdout is unchanged: `--json` parses with and without progress.
- The estimate is never asserted (D-8).

**Handoff (paste):** "Implement phase 3 of M4D per `docs/m4d-breakdown.md` §3.6, §3.3 and §5. The
callback goes in `embed_all` and is threaded through all three plan entry points, because
`janus index` calls `plan_forced`, not `plan`. Progress prints only when stderr is a terminal, or
with `--progress=always`. There is no chunk-count floor. The total is the chunks this build embeds.
Never assert the estimate or a duration. Update `tests/cli_index.rs` only if a line count changes,
and say so in the PR body. Write comments in Simplified Technical English."

**Accept:** a build over the real corpus prints progress. The author runs it and reports the lines.

## 6. Cross-cutting

- **Determinism.** The witness reads the filesystem, so it is absent from every pure function. No
  wall clock is added below `main.rs`: the writer compares corpus signatures, not clock times, and
  the estimate uses elapsed time in the CLI only.
- **Reads fail open, writes fail closed.** A stale read either refreshes or says why not. It never
  returns an old answer in silence, and it never fails because another writer is busy.
- **No format change.** No `format_version` bump, no index file added, no rebuild for an existing
  vault.
- **The lock is reused**, not reinvented. Only the *try* variant is new.

## 7. Definition of done

1. Editing one document makes the next `search` see the edit, print exactly one
   `refresh: 1 document(s) changed` line, and leave `.janus/index.bin` and `.janus/vectors.f32`
   byte-identical to a `--rebuild` copy of the same vault.
2. A document edited *during* a build is in the index when the command returns.
3. A read builds when the index is absent or the pair is corrupt. It refuses only for
   `format_version`, `tokenizer`, `chunker`, `corpus_id` or `embedder`, with one line naming
   `janus index`. A `built_root` mismatch refreshes.
4. A held writer lock makes a stale read print `refresh: skipped, index busy`, answer from the old
   generation, and exit 0.
5. `--no-rebuild`, `refresh = "manual"` and every refusal write nothing: `.janus/` byte-identical.
6. The witness costs one walk per read; the measured number over 2 119 files is in P1's PR body.
7. With `--progress=always`, a build prints at most one line per 5 % of the chunks it embeds. With a
   captured stderr it prints nothing, and stdout is unchanged.
8. No `format_version` change, no rebuild for an existing vault, no new dependency.
9. Gates: fmt, clippy, tests, doc, Windows cross-check, `scripts/pr-size.sh`.
10. **Milestone acceptance, by the author:** edit one document, run `search`, and see the edit
    without running `janus index`.

## 8. PR plan and size budget

| PR | Branch | Logic | Tests | Content |
|---|---|---|---|---|
| P1 | `m4d-staleness` | ~220 | ~260 | the witness, the writer's loop, the config key, the note |
| P2 | `m4d-refresh` | ~300 | ~420 | the refresh, the non-blocking lock, the typed refusals, the read order |
| P3 | `m4d-progress` | ~180 | ~200 | the callback, the terminal gate, the estimate |

If P2 passes 450 logic lines, split it at the named seam into P2a and P2b (§5). Never shave a
comment or drop a test to fit the ceiling.

## 9. SPEC corrections

1. **§8.3, rebuild triggers** — a corpus change since `index.bin` is a *refresh* trigger, not a
   rebuild trigger. A format, tokenizer, chunker, `corpus_id` or embedder mismatch is cured only by
   `janus index`. A `built_root` mismatch is never an error (unchanged).
2. **§8.3, reader protocol** — replace "if no generation validates they take the writer lock,
   re-check freshness, rebuild once, and then refuse". A reader validates without a lock. When the
   corpus changed it tries the writer lock once, without blocking, and on a busy lock answers from
   the generation it validated. When the index is absent or the pair is corrupt it takes the writer
   lock and rebuilds once. When the mismatch is cold it refuses. `--no-rebuild` never writes.
3. **§9.1** — `janus search` and `janus facts` refresh by default; `--no-rebuild` refuses; the new
   error `index needs a full build` exits 1.
4. **§9.2** — MCP reads inherit the refresh and the refusal. They have no `--no-rebuild`, so the
   refusal line must name `janus index`.
5. **§9.5** — add the error code `index needs a full build` (additive; `api` stays `1`).
6. **§13** — the row "Incremental reindex (1 changed doc) ≤ 150 ms" is replaced by a measured row
   on a stated corpus, release build, with the document count in the row. Add the witness cost
   (8.5 ms over 2 119 files) so the next reader does not re-measure it.
7. **§15, D50** — extend the row, do not add another. D50 already says mtime is never a date. Add:
   mtime **is** valid as a cache witness, because a cache asks only whether the bytes changed.
8. **§16.1** — the `[index]` block gains `refresh = "auto" | "manual"`.

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

_Nothing recorded yet._

## 12. Implementation log — Phase 2

_Nothing recorded yet._

## 13. Implementation log — Phase 3

_Nothing recorded yet._
