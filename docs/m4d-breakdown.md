# M4D — The index keeps itself current

> Branch: `m4d-plan` (this document), then one branch per phase from `main`.
> Spec: §8.3 (publication, rebuild triggers), §9.1, §13 (index targets), §11, §15.
> Evidence: a 2 119-document corpus, measured across M4, M4B and M4C.
>
> **Done when:** an edit is visible on the next search without the user running `janus index`, and a
> build that takes minutes prints its progress.

**Words used here.** **stale**: the corpus changed after the index was built.
**refresh**: an incremental build that reuses unchanged work.
**cold build**: a build with nothing to reuse.

## 0. Scope

**Decision: a read must not return an old index silently, and a long build must not be silent.**

Measured on the real corpus:

| fact | value |
|---|---|
| documents | 2 119 |
| chunks | 27 234 |
| a re-index with nothing changed | **2.6 s** (walk, parse, reuse every token and row) |
| a cold lexical build | 3 s |
| a cold build with embeddings | **18.3 min** at 24.8 chunks/s |
| a first `search` after that build, when the generation had no vectors | **re-embedded everything, unasked, for 18 minutes** |

Three problems:

1. **A read cannot tell that the index is old.** Only `janus index` notices an edit. A search returns
   the previous answer and says nothing.
2. **A read can trigger an expensive build.** A generation without vectors is stale for a process that
   can embed, so the first `search` rebuilt for 18 minutes. Nothing warned first.
3. **A build is silent.** No progress, no estimate. Ten minutes is indistinguishable from a hang, and
   the author killed a healthy build at 10:01 because of it.

### In scope

- A staleness check on read: cheap, exact, and reported in one line.
- Incremental refresh on read when the corpus changed, including embedding only the changed chunks.
- Never a cold build on a read. That is a command, and it says so.
- Progress and an estimate during any embedding work, on stderr.
- A note when a read refuses to refresh, naming what is stale.

### Not in scope

- A file watcher, a daemon, or `janus watch` (M8).
- Resuming a killed cold build. Refresh makes it rare; it stays impossible (P-2).
- Quantisation, a smaller model, a faster runtime. Cold-build throughput is M8's.
- Quarter dates and the undated-decay rule. Both are agreed and handed to M5 (§10).
- Any change to the index layout, `format_version`, or the scoring rules.

## 1. Dependencies

None. No crate. No feature flag.

## 2. Data model

```rust
// src/index/store.rs — the staleness check's result.
pub struct Staleness {
    /// Documents whose modification time is newer than the index file.
    pub changed: usize,
    /// True when the newest document is newer than `index.bin`.
    pub stale: bool,
}

// src/index/build.rs — one callback, the seam the tests use.
pub struct EmbedProgress<'a> {
    pub on_chunk: &'a mut dyn FnMut(u64 done, u64 total),
}
```

`Staleness` is computed per read and never persisted. `EmbedProgress` is a seam: the CLI prints, the
tests count. Nothing on disk changes shape.

## 3. Contracts

### 3.1 How staleness is detected, without storing anything

**The index file's own modification time is the build time.** `publish` renames a temp file into place,
so `index.bin`'s mtime is when that generation was written. A document mtime is comparable to it
directly.

```
stale ⟺ max(document mtime over the walked corpus) > mtime(index.bin)
```

- **No manifest field, no format change, no rebuild.** A stored timestamp would need one of the three.
- **The walk is the existing one**, with the same excludes, so the check agrees with the build.
- **The count is reported, not the list.** `N document(s) changed since the index was built`.
- **mtime is valid here and invalid as a document date.** M4B removed it from the date ladder because a
  copy rewrites it, and a ranking must not depend on that. A *cache* is the opposite case: the whole
  question is whether the bytes on disk changed, and mtime is the cheapest witness. The distinction is
  stated in the module docs so nobody applies M4B's rule to the wrong place.

A file whose mtime is older than the index but whose content changed (a restored backup, a file copied
with `cp -a`) is invisible to this check. That is the accepted cost of not hashing 2 119 files on every
query. `janus index --rebuild` is the escape hatch, and the docs say so.

### 3.2 What a read does about it

**A stale read refreshes incrementally. It never runs a cold build.**

| state | `search` / `facts` | `--no-rebuild` |
|---|---|---|
| fresh | read | read |
| stale, same embedder, same format | **refresh, then read**. One line on stderr: `refresh: N document(s) changed in X ms` | refuse with one line naming the count, write nothing |
| stale because of a format or embedder change | **refuse to build**. One line: `index needs a full build: run janus index` | same |
| index absent | **refuse**. One line: `no index yet: run janus index` | same |

**Why a read may not run a cold build.** A cold build with embeddings took 18 minutes on the real
corpus. Making a read do that turns `search` into a surprise batch job. An incremental refresh costs
2.6 s there, which is a read's own order of magnitude, so it is allowed.

**The refresh takes the writer lock** (§3.5 of M3) and re-checks freshness under it, so two agents
searching at once cost one refresh. The existing protocol is reused, not duplicated.

**One line, one meaning.** `refresh:` means a read built a generation. `stale:` keeps its M3 meaning:
a read *adopted* an existing generation after rejecting a broken pair.

### 3.3 Progress and the estimate

**Every embedding batch reports its position, on stderr, at a bounded rate.**

```
embedded 8192/27234 chunks (30%)  ~12.4 min left
```

- **stderr**, never stdout: stdout is the results stream (§8.5, §9.5).
- **At most one line per 5 % of the total**, so a small corpus prints nothing. A 24-document fixture
  must stay silent.
- **The estimate is derived from the observed rate**, and it is printed as a rounded minute. It is a
  diagnostic, not a promise, and no test asserts its value.
- **The final line is a summary**, so a pipe that shows only the last line still says what happened.

**Testable without a clock.** `EmbedProgress` is a callback. A counting embedder asserts how many times
it fired, at which values, and that it fired at all. No sleep, no timing assertion.

### 3.4 What must not change

- **No format change.** No `format_version` bump, no new file.
- **The refresh reuses everything M3 and M4 built**: tokens by `lex_key`, rows by `body_hash` and the
  chunk-id sequence. A refresh is not a smaller cold build.
- **The scoring path is untouched.** No weight, no threshold, no tie-break.
- **`--no-rebuild` keeps its promise**: a read that refuses writes nothing.

## 4. Decisions

| # | Question | Default | Where |
|---|---|---|---|
| D-1 | How staleness is detected | `max(document mtime) > mtime(index.bin)`. No stored field, no format change | `store.rs` |
| D-2 | What a stale read does | Incremental refresh, announced in one line | `search/mod.rs` |
| D-3 | May a read run a cold build | **No.** Format or embedder changes refuse and name the command | `store.rs` |
| D-4 | Is the staleness check configurable | Yes: `[index] refresh = "auto" \| "manual"`, default `auto`. `manual` prints the note and does nothing | `config.rs` |
| D-5 | The cost budget for the check | It must stay under 10 % of a warm query. Phase 1 measures it; if it does not, the fallback is a directory-mtime prefilter (D-5a) | `store.rs` |
| D-5a | Fallback if the walk is too slow | Compare the corpus directories' mtimes first, which catches add, remove and rename; a full walk only when that says nothing changed | `store.rs` |
| D-6 | Progress cadence | One line per 5 % of the total chunks; silent below 500 chunks | `build.rs`, `cmd.rs` |
| D-7 | The progress seam | A callback in `EmbedProgress`, tested with a counting embedder | `build.rs` |
| D-8 | Is the estimate asserted | **No.** Printed, never compared | — |
| D-9 | Does refresh change `--dry-run` | **No.** A dry run never refreshes; it reports what a run would do | `cmd.rs` |

## 5. Phases

Three phases. One PR each, from `main` after the previous merge.

### Phase 1 — detect staleness, and measure it

**Goal:** a read can say whether the index is old, at a cost that does not spoil the read.

**Files:** `src/index/store.rs` (or a new `src/index/stale.rs`), `src/config.rs` (`[index] refresh`),
`src/search/mod.rs` (the note), `src/search/facts.rs`, `tests/staleness.rs` (new),
`tests/golden_scan.rs` if a fixture changes.

**Build (~150 logic lines):**

1. `Staleness { changed, stale }` from the existing walk and one `stat` on `index.bin`.
2. `[index] refresh = "auto" | "manual"`: parse, default `auto`, warn on an unknown value and use `auto`.
3. A read computes `Staleness` and prints one line when stale. In this phase it does **not** refresh.
4. `Outcome` gains `stale: bool` and `changed: usize`, so a caller can see it. Additive; `api` stays `1`.

**Tests (~200 lines):**

- A fresh vault is not stale. One edited document makes it stale, and the count is 1.
- An added file, a removed file and a renamed file each make it stale.
- A file touched to an older time is **not** stale. That is the documented blind spot, pinned as a test.
- `refresh = "manual"` prints the note and changes nothing.
- **The cost, measured and printed**: the eval or a bench-shaped test prints the check's duration over
  the committed fixture. It never asserts a duration. The real-corpus number is the author's to report
  (D-5).

**Handoff (paste):** "Implement phase 1 of M4D per `docs/m4d-breakdown.md` §3.1 and §5. Staleness is
`max(document mtime) > mtime(index.bin)`, computed with the existing walk, with no stored field and no
format change. A read reports it and does not act on it yet. Module docs must state why mtime is valid
for a cache and invalid as a document date. Print the check's duration; never assert one."

**Accept:** gates green. The fixture is not stale after an index. An edit makes it stale with the right
count. The printed duration over the fixture is in the PR body.

### Phase 2 — refresh on read

**Goal:** an edit is visible on the next search.

**Files:** `src/index/store.rs` (the refresh path), `src/search/mod.rs`, `src/search/facts.rs`,
`src/index/cmd.rs`, `tests/staleness.rs`.

**Build (~200 logic lines):**

1. A stale read with `refresh = auto` takes the writer lock, re-checks freshness under it, and runs the
   existing `build::plan` with the previous generation. One line: `refresh: N document(s) changed in X ms`.
2. A format or embedder mismatch refuses with one line naming `janus index`. Reuse the existing
   `Expected::check` reason; do not invent a second vocabulary.
3. `--no-rebuild` keeps refusing and writes nothing.
4. An absent index refuses; it does not build one.
5. `facts` follows the same rule, because it reads the same generation.

**Tests (~300 lines):**

- Edit one document, then `search`: the new text is found, and the refresh changed no other document's
  tokens or rows (compare with a clean build).
- An edit that only touches frontmatter costs no embedding work (`embedded 0`).
- Two searches at once behind a stale index cost one refresh: one reports `refresh:`, one reports
  nothing. No sleeps; use a barrier.
- A version-2 index is refused with one line, and nothing is written.
- `refresh = "manual"`, `--no-rebuild` and an absent index each print their line and write nothing. The
  directory is byte-identical afterwards.
- A refresh never runs a cold build: with the embedder id changed in the manifest, the read refuses and
  the message names the command.

**Accept:** gates green; an edit is visible on the next search; no cold build is reachable from a read.

### Phase 3 — progress and the estimate

**Goal:** a long build says what it is doing.

**Files:** `src/index/build.rs`, `src/index/cmd.rs`, `src/main.rs` if the printer lives at the edge,
`tests/progress.rs` (new).

**Build (~150 logic lines):**

1. `EmbedProgress` threaded from `index::cmd` into `build::plan` so the embedder loop can report.
2. The printer: one line per 5 % of the total chunks, silent below 500 chunks. Rate from the elapsed
   time and the chunks done; the estimate is rounded to a minute.
3. A summary line at the end, on stderr: `embedded N chunks in X`.

**Tests (~150 lines):**

- A counting embedder over a fixture with more than 500 chunks makes the callback fire at the expected
  boundaries. Assert the values, not the text.
- A fixture below 500 chunks prints nothing on stderr.
- stdout is unchanged: `--json` parses with and without progress.
- The estimate is never asserted; a test would only pin a formatting accident.

**Accept:** a build over the real corpus prints progress. The author runs it and reports the lines.

## 6. Cross-cutting

- **Determinism.** The staleness check reads the filesystem, so it is absent from every pure function.
  No wall clock is added; the estimate uses elapsed time in the CLI only, never in a library path.
- **Reads fail open, writes fail closed.** A stale read either refreshes or says why not. It never
  returns an old answer in silence.
- **No format change.** No `format_version` bump, no new file, no rebuild for an existing vault.
- **The lock is reused**, not reinvented.

## 7. Definition of done

1. Editing one document makes the next `search` see the edit, with one `refresh:` line, and no other
   document retokenized or re-embedded.
2. The staleness check costs under 10 % of a warm query on the real corpus, or D-5a's prefilter ships
   with the number that forced it.
3. A read never runs a cold build. A format or embedder mismatch refuses with one line naming
   `janus index`.
4. `--no-rebuild`, `refresh = "manual"` and an absent index each write nothing, byte for byte.
5. A build over 500 chunks prints progress at most once per 5 %, on stderr, and stdout is unchanged.
6. No `format_version` change, no rebuild for an existing vault, no new dependency.
7. Gates: fmt, clippy, tests, doc, Windows cross-check, `scripts/pr-size.sh`.
8. **Milestone acceptance, by the author:** edit one document, run `search`, and see the edit without
   running `janus index`.

## 8. PR plan and size budget

| PR | Branch | Logic | Tests | Content |
|---|---|---|---|---|
| P1 | `m4d-staleness` | ~150 | ~200 | the check, the config key, the note, the measurement |
| P2 | `m4d-refresh` | ~200 | ~300 | refresh on read, the refusals, the lock reuse |
| P3 | `m4d-progress` | ~150 | ~150 | the callback, the cadence, the estimate |

## 9. SPEC corrections

1. **§8.3** — the rebuild-trigger list gains "the corpus changed since `index.bin`", and states that a
   read refreshes incrementally but never builds cold.
2. **§9.1** — `janus search` and `janus facts` refresh by default; `--no-rebuild` refuses.
3. **§16.1** — the `[index]` block gains `refresh = "auto" | "manual"`.
4. **§13** — a measured row: incremental refresh on the 2 119-document corpus is 2.6 s, not the 150 ms
   the table claims. The claim was never measured; this milestone measures it.
5. **§15** — one row: mtime is valid for cache invalidation and invalid as a document date.

## 10. Handoff to M5

Two decisions are agreed and belong to the temporal milestone, not to M4D:

1. **Quarter dates in names.** `q4-2026`, `q2_2026`, `2026-q4` and `2026.q4` parse to the first day of
   that quarter, using the same separator set as the ISO shapes. A new precision variant, the same
   ladder, the same whole-prefix and year-range rules.
2. **An undated document must stop being advantageous.** Today it decays at 1.0, so a document is
   rewarded for missing a date. The agreed rule: an undated document decays by the **median age of the
   dated documents in the corpus**. That is a guess, but a corpus-relative one, and it is measurable.
   It needs a decision row, a test, and a note when the corpus has no dated documents at all.

## 11. Implementation log — Phase 1

_Nothing recorded yet._

## 12. Implementation log — Phase 2

_Nothing recorded yet._

## 13. Implementation log — Phase 3

_Nothing recorded yet._
