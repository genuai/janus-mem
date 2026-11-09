# M6b — Corpora and workspaces (read side)

> Branch: `m6b-plan` (this document), then one branch per phase from `main`.
> Spec: §6.11, §6.4 (identity namespacing), §9.1, §9.2, §9.4, §11 (M6), §16.2.
> Evidence: the code at `405133a`, the M6 plan's deferral (§0.3), and the two dogfood gate
> runs of 2026-11-08.
>
> **Done when:** a two-corpus workspace answers a default query from the primary corpus only,
> `--corpus all` returns both with each row naming its corpus and the archived weight in the
> breakdown, and the private workspace's retired archive stops answering "what is true now"
> without an exclude line.

**Words used here.** **Workspace**: a file that lists corpora and federation policy. **Corpus**:
one directory tree with its own `janus.toml` and its own index. **Primary**: the corpus a
default query reads. **Archived**: a corpus that is hard-excluded unless a query names it.
**Merge**: combining per-corpus ranked lists into one.

## 0. Scope

**Decision: M6b makes the read path federated. It adds no write, and it does not resolve
cross-corpus links.**

Both dogfood corpora produced the same defect: retired documents answered questions about
the present. Each time the fix was an `exclude` line, which is the wrong instrument — it
makes the archive unsearchable instead of subordinate.

| M6b item | state today | evidence |
|---|---|---|
| `workspace.toml` parsing | absent | `grep -rn workspace src/` returns prose only |
| `--workspace` resolution | absent | `src/cli.rs` has no flag |
| `janus corpora` | absent | 18 commands, none of them `corpora` |
| `--corpus primary\|all\|<id>` | absent | `search::Options` has no corpus field |
| a corpus on each result | absent; only the report carries `corpus_id` | `src/search/mod.rs:1249` |
| archived role and weight | absent | no config key, no score factor |
| per-corpus index | exists and is already isolated | `[index] index_dir`, `src/config.rs:152` |
| cross-corpus link resolution | deferred, and stays deferred | `DanglingReason::OtherCorpus`, `src/link.rs:135` |

### 0.1 In scope

1. `workspace.toml`: corpora with `id`, `root`, `role`, `read_only`, `index_dir`, and the
   `[search]` policy keys.
2. Resolution order per §6.11: `--workspace`, then `JANUS_WORKSPACE`, then the platform
   config directory, then single-corpus mode.
3. `janus corpora [--json]`: each corpus, its role, its root, and index freshness.
4. `--corpus` on `search` and `facts`, and the `corpus` parameter on the same tools over
   rpc and MCP.
5. Hard exclusion of an archived corpus unless the query names it; `archived_weight`
   applied and shown when it is included.
6. One merged ranking across corpora, exact: per-corpus top-K merged by `final`.
7. Every result row carries `corpus`.
8. `read_document` and `janus_status` in a workspace.

### 0.2 Not in scope

| Item | Owner | Why |
|---|---|---|
| `janus promote`, and every write | M7 | it patches a file, and it needs `allow_cross_corpus_links` to mean something |
| Resolving a qualified cross-corpus link | M7 | a cross-corpus edge changes windows and status across corpora; that is a write-side fact |
| `[scope_groups]` | later | §6.12 calls it sugar, and it widens result sets silently |
| HTTP transport, watch mode | M8 | unchanged |
| Semantic ranking | R2 | its trigger did not fire |

### 0.3 Why four phases

| Phase | Unit | Why it is one reviewable change |
|---|---|---|
| M6b-1 | the file, the resolution, `janus corpora` | parse and report; no query changes |
| M6b-2 | the federated query | the merge, the exclusion, the weight, the `corpus` field |
| M6b-3 | the other tools and surfaces | `read_document`, `janus_status`, rpc and MCP parity |
| M6b-4 | docs, corrections, and the gate | the archive becomes a role on the real corpus |

## 1. Dependencies

None.

## 2. Data model

### 2.1 `workspace.toml`

```toml
[[corpora]]
id = "main"
root = "/Users/me/vault"
role = "primary"

[[corpora]]
id = "context"
root = "/Users/me/archive"
role = "archived"
read_only = true
index_dir = "/Users/me/.cache/janus/context"

[search]
allow_cross_corpus_links = false
archived_weight = 0.8
```

Rules taken from §6.11, which is the authority:

- `id` is required, unique, and matches `[a-z0-9][a-z0-9._-]*`. It is declared, never derived.
- `root` is resolved to an absolute path once, at load, with `dunce::canonicalize`.
  A value that is not absolute resolves against the workspace file's own directory
  (M6b-8), so a workspace can be committed beside its corpora.
- `role` is `primary` or `archived`; exactly one primary in v1. More than one is a config
  error, not a warning, because "primary" is what a default query means.
- `read_only` defaults to `true` for `archived` and `false` for `primary`.
- `index_dir` falls back to `<root>/.janus`, which is what a corpus already uses today.
  It is the index location of a corpus inside a workspace; the corpus config's own
  `index_dir` applies in single-corpus mode (M6b-11). A leading `~` is not expanded.
- An unknown key is tolerated, as in `janus.toml`: a newer janus may write it.
- A missing `root`, a duplicate `id`, or two primaries are errors that name the file and the
  entry. Everything else degrades with a warning, because a broken federated read must still
  read what it can.

### 2.2 The corpus set (`src/workspace.rs`, new)

```rust
pub struct Workspace {
    corpora: Vec<Corpus>,     // primary first, then archived in file order
    search: SearchPolicy,     // archived_weight, allow_cross_corpus_links
}

pub struct Corpus {
    id: String,
    root: PathBuf,
    role: Role,               // Primary | Archived
    read_only: bool,
    index_dir: PathBuf,
}
```

No index data is shared and nothing is merged on disk. Each corpus keeps its own generation,
which is what makes the merge a pure query-time step (§6.11: "query-time merging is exact").

### 2.3 The query sum

`--corpus` accepts `primary` (default), `all`, or an `id`. It resolves to a set of corpora:

| selection | corpora read |
|---|---|
| `primary` | the primary only |
| `all` | every corpus, archived included |
| `<id>` | that one |

A name that matches no corpus is a usage error listing the ids: a silent fallback to primary
would answer a different question than the one asked.

### 2.4 Score fields

`Breakdown` gains `corpus`, the factor applied to `final` for the row's corpus: `1.0` for a
primary corpus, `archived_weight` for an archived one that the query included. It is `1.0`
for every row in single-corpus mode, and it appears in the human table only when it is not.

## 3. Contracts

### 3.1 Resolution and single-corpus compatibility (P1)

**Statement.** With no workspace, every command behaves exactly as it does today. With one,
the corpora it names are read; nothing else changes.

**Case 1.1.** `--workspace PATH` wins over `JANUS_WORKSPACE`, which wins over the platform
config directory. A test sets all three and asserts the first wins; then two, and so on.

**Case 1.2.** With no workspace anywhere, `janus search` output is byte-identical to
`main`'s for every golden query. No golden moves in M6b.

**Case 1.3.** A workspace file with two primaries exits `2` and names both entries. A
duplicate `id` does the same. A missing `root` names the entry and the file.

**Case 1.4.** An archived corpus with no `read_only` is read-only by default; one that
declares `read_only = false` is not, and the difference is reported by `janus corpora`.

**Case 1.5.** `janus corpora --json` reports each corpus's id, role, root, whether its
index is present, and whether it is stale. It builds nothing and takes no lock.

### 3.2 The federated query (P2)

**Statement.** A query reads a set of corpora, ranks each, and merges the ranked lists by the
query's own order key. The merge is exact: it cannot reorder two documents that one corpus
would have ordered, and it cannot drop a document that belongs in the global answer.

**How many rows each corpus returns.** Every corpus answers with its own top-`N`, where `N` is
the query limit. Merging those lists and cutting to `N` is then exact: a document in the global
top-`N` is necessarily inside its own corpus's top-`N`, so asking a corpus for fewer rows would
be the only way to lose one. `limit N` still returns at most `N` rows in total (case 2.6).

**The order key is the tool's, not one key for all tools.** `search` merges by `final`.
`facts` has two orders of its own: the fact list is ordered by authority, and the fallback list
by `final` plus its own `(date, path)` tie-break. The merge follows the list's order, because
§7.1's authority order is the reason `facts` exists; merging a fact list by `final` would
reorder it and break that promise. `archived_weight` therefore multiplies `final` for every
archived row, appears in the breakdown, and leaves an authority-ordered list in place.

**Case 2.1.** With a primary and an archived corpus, a default query returns rows from the
primary only, and prints one note naming how many archived corpora were excluded.

**Case 2.2.** `--corpus all` returns rows from both. Every row carries `corpus`. Each
archived row's `scores.corpus` equals `archived_weight`, and its `final` equals the primary
score times that weight, within the file's float tolerance.

**Case 2.3.** `--corpus context` (an archived id) returns that corpus's rows, weighted
`archived_weight` — an explicit name does not promote a corpus to primary.

**Case 2.4.** The merge is exact in both directions. A property test over generated scores
asserts two things: (a) the merged order equals sorting the **full** union of both corpora's
documents by the query's key, ties broken by corpus order then `path`; and (b) no document is
missing, which is the part a comparison of two already-truncated lists cannot see. The test
generates scores where the global top-`N` is spread across both corpora, so a per-corpus
truncation below `N` fails it.

**Case 2.5.** `--corpus nope` exits `2`, and the message lists the ids.

**Case 2.6.** `limit N` returns at most N rows across all corpora, not N per corpus.

**Case 2.7.** `facts` applies the same selection, the same weight and the same `corpus` field,
through one shared predicate, not a second copy. Its **order** stays authority-first: a
two-corpus fixture whose archived row has the higher `authority` but the lower `final` keeps
that row where authority puts it, and reports `scores.corpus` = `archived_weight` beside it.
The fallback order (a corpus with no fact types) merges by `final` like `search`.

### 3.3 The other tools (P3)

**Case 3.1.** `read_document` takes an optional `corpus`. Absent it, the primary is read. A
path that exists in two corpora reads the named one, and the answer names the corpus it
came from.

**Case 3.2.** `read_document` with an unknown corpus id is `usage`, listing the ids.

**Case 3.3.** `janus_status` in a workspace reports a `corpora` array with each id, role,
presence and staleness, plus the workspace path. In single-corpus mode the shape does not
change.

**Case 3.4.** The five tools over rpc and MCP accept the same `corpus` parameter and return
the same answers as the CLI, after the M6 normalizations.

**Case 3.5.** A write tool name is still refused, and `writes_enabled` is still `false`,
whatever `role` or `read_only` says.

### 3.4 The archive becomes a role (P4)

**Case 4.1.** On a copy of the private workspace, with `6.archive/` removed from the exclude
list and declared as an archived corpus, the ten gate questions answer at least as many
top-1 as the exclude-line configuration does today, and the archive appears in **none** of
the top three.

**Case 4.2.** `--corpus all` on that copy returns the archived documents again, so nothing
was lost.

**Case 4.3.** The counts are recorded in the phase log and in the gate record; the questions
are not committed, because they describe personal material.

### 3.5 What must not change

- Single-corpus mode: every golden, every human table, every exit code, `format_version` 6.
- `api` 1. The `corpus` parameter and the `corpus` score field are additive.
- `corpus_id` stays the declared identity in `janus.toml`; a workspace's `id` is a separate
  name for the same corpus, and the report carries both when they differ.

## 4. Decisions

| ID | Decision | Why |
|---|---|---|
| M6b-1 | One `Engine` per corpus; the workspace is a list of them | The engine already holds a root, a config and a load cache. A second load path would be a second cache bug. |
| M6b-2 | Each corpus answers its own top-`N`; the merge then cuts to `N`, by the tool's own order key | A document in the global top-`N` is always inside its corpus's top-`N`, so this is exact. `facts` merges by authority because that order is its reason to exist; a weight that reordered it would break §7.1. |
| M6b-3 | `--corpus <id>` on an archived corpus still applies `archived_weight` | Naming a corpus is a scope choice, not a claim that its facts are current. |
| M6b-4 | Exactly one primary; two is an error | "Primary" is what an unqualified query means. Two would make the default ambiguous. |
| M6b-5 | Cross-corpus links and `promote` stay in M7 | A cross-corpus edge changes windows and status across corpora. It is a write-side fact. |
| M6b-6 | `read_document` gains `corpus?`, and a qualified `corpus:path` is accepted | A path alone is ambiguous once two corpora exist; the qualifier already exists in link syntax. |
| M6b-7 | A missing index in any corpus is built on first read, as today | A workspace whose second corpus was never indexed must not fail the first query. |
| M6b-8 | A relative `root` resolves against the workspace file's directory | A workspace can be committed beside its corpora, and a committed fixture needs no machine path. An absolute root is used as given. |
| M6b-9 | An `id` that breaks the pattern is an error, not a warning | `--corpus <id>` must be able to name every corpus. A name the command line cannot pass is a corpus no query can select. |
| M6b-10 | A value of the wrong type is an error naming the entry; an unknown key is a warning | A key a newer janus writes is forward compatibility. A value that cannot be read is not. |
| M6b-11 | A workspace's `index_dir` is the index location for a corpus in that workspace | §6.11 gives the workspace that key. The corpus config's own value applies in single-corpus mode, where no workspace exists. |
| M6b-12 | A read surface that holds one corpus refuses `all` and a named id | One `Engine` holds one corpus and one load cache. Answering a corpora question from the primary would answer a different question than the one asked; the multi-corpus engine arrives with the other read surfaces (§2.3). |
| M6b-13 | `primary` and `all` are reserved words in `--corpus` | §2.3 defines both. The workspace parser does not reserve them, so a corpus declared with either name cannot be selected by name, and the resolver reads the keyword first. |
| M6b-14 | A named corpus root wins over the platform workspace file, and naming both a root and a workspace is a usage error | A root is a specific subject and the platform file is a default. One query cannot read a root and a corpora list at once, so the contradiction is refused instead of resolved silently. |
| M6b-15 | At an equal order value the merge keeps corpus order, then the corpus's own order | §3.2 says the merge cannot reorder two documents one corpus already ordered. `facts` orders a fact list by authority, date, chunk id and id before the path, so re-sorting ties by path would reorder a same-corpus pair. The merge is a stable sort over per-corpus ranked lists, and case 2.4 proves it against the full union. |
| M6b-16 | A neutral `scores.corpus` of `1.0` is omitted from JSON, and the human table shows the column only when a row carries a weight | §2.4 asks for the field and for it to appear only when it is not neutral. Omitting the neutral value is what keeps every existing golden byte-identical. |
| M6b-17 | A merged `facts` list uses the authority order whenever any selected corpus listed facts | Authority is why the command exists (§7.1). A corpus with no fact type falls back for itself; its rows still merge by their own authority value. |
| M6b-18 | A merged report takes its head fields from the first selected corpus and lists every selected id in `corpora`; a human row prints its corpus as `id:rel_path` | The query, the intent and the date are the same in every part. `alpha` is each corpus's own setting, so the head reports the first and every row keeps the score its own corpus gave it. The qualified path is the form a cross-corpus link already uses (M6b-6). |
| M6b-19 | One fleet per surface: every corpus the workspace names, with one `Engine` per corpus built on first use | A `corpus` parameter must mean one thing on the command line, over rpc and over MCP, and the rpc loop and the MCP server must answer it too. Building an engine on first use keeps a query that reads the primary from resolving a model for a corpus it left out. This replaces M6b-12's blanket refusal: the refusal now applies only where no workspace is in play. |
| M6b-20 | `janus_status` takes no `corpus`; `--workspace` is its flag, and `corpora` reports every corpus | The array already answers the per-corpus question, so a selection parameter would add a second rule for the same answer. In single-corpus mode both new fields are absent, so the shape does not change. |
| M6b-21 | A read names its corpus in JSON only; the human form stays the document text | `janus read` is a text stream, and `janus read | head` is a documented use. A header line would break the one thing the command promises, so the field that names the corpus lives in the envelope. |
| M6b-22 | A `corpus:path` qualifier and a `corpus` parameter that name different corpora is a usage error | One call cannot read two corpora. Resolving the contradiction silently would answer a question nobody asked, which is the failure M6b-14 refuses for a root and a workspace. |
| M6b-23 | `lineage` keeps no `corpus` parameter, and a qualified path is refused | A supersession chain is one corpus's graph. A cross-corpus chain is a write-side fact, because the edge changes windows and status across corpora, so it stays in M7. |
| M6b-24 | `--workspace` is a global flag, and a command that works on one corpus refuses it with `usage` | One flag in one position, beside `--vault`. The parser then accepts it everywhere, so the refusal is what stops a command answering about one corpus while looking like it read the workspace. The guard names the commands that read a workspace, so a command added later refuses the flag until someone writes it to read one. |
| M6b-25 | A retired tree inside the primary's root keeps one exclude line in the primary; the role is what keeps it out of answers | The walk is not workspace-aware: the primary walks every directory under its root, so a nested archived corpus is indexed twice unless the primary excludes it. The guide states this. Deferred: a loader warning for a corpus root inside another corpus root. |

## 5. Phases

### Phase 1 — The workspace file, resolution, and `janus corpora`

**Files.** `src/workspace.rs` (new), `src/cli.rs`, `src/main.rs`, `src/lib.rs`,
`tests/cli_corpora.rs` (new), `tests/fixtures/workspace/` (two small corpora).

**Build.** Parse and validate `workspace.toml`; resolve the path in the documented order;
`janus corpora` reads each manifest and reports.

**Tests.** Cases 1.1–1.5.

**Accept.** No query changes. A malformed workspace names the file and the entry.

**Split seam.** Parse and validate first; the listing command second.

### Phase 2 — The federated query

**Files.** `src/search/mod.rs`, `src/search/facts.rs`, `src/api/dto.rs`, `src/cli.rs`,
`src/main.rs`, `src/worker.rs` or `src/search/federate.rs` (new, if the merge earns its own
module), `tests/workspace_query.rs` (new).

**Build.** Corpus selection, per-corpus rank, merge, exclusion note, weight, `corpus` field.

**Tests.** Cases 2.1–2.7.

**Accept.** Single-corpus goldens unchanged. The merge property holds over generated scores.

**Split seam.** The primary-only path first (no merge), then `all` and a named corpus.

### Phase 3 — The other tools

**Files.** `src/read.rs` (nothing: the answer names its corpus in the engine),
`src/status.rs`, `src/api/dto.rs`, `src/engine.rs` (the fleet), `src/main.rs`,
`src/cli.rs`, `src/rpc.rs`, `src/mcp/mod.rs`, `src/mcp/tools.rs`,
`src/search/federate.rs`, `src/workspace.rs`, `tests/workspace_read.rs` (new),
`tests/fixtures/workspace-read/` (new).

**Build.** `corpus?` on `read_document`, the `corpora` block on `status`, and the
same parameter over rpc and MCP through one fleet.

**Tests.** Cases 3.1–3.5.

**Accept.** The five tools answer identically over the three surfaces, with the M6
normalizations.

**Split seam.** Part 1 is `read_document` and `janus read`; part 2 is `status`
and the rpc/MCP parity. Measured, and not taken: see §12.

**Amendment.** The original file list named `src/read.rs` and `tests/cli_status.rs`
and no fleet. A read names its corpus where the fleet resolved it, so `read.rs`
is untouched; the fleet lives in `engine.rs` because that is the type every
adapter already calls; and the cases live in one file beside their fixture.
`tests/cli_status.rs` keeps its own cases unchanged, which is the evidence that
single-corpus output did not move.

### Phase 4 — The archive becomes a role

**Files.** `docs/corpus-config.md`, `SPEC.md`, `docs/m6b-breakdown.md`, and the private
corpus's `janus.toml` (git-ignored, edited in that project).

**Build.** Reverse the exclude line in the workspace config, declare the archive, re-measure
the ten questions, and update the guide so `exclude` is no longer the recommended answer for
a retired tree.

**Tests.** Cases 4.1–4.3, measured, not asserted in the repository.

**Accept.** The archive is absent from a default query and present under `--corpus all`.

**Amendment.** The phase also moves `--workspace` into the global `Cli` and adds the guard
that refuses it, because seven command arms carried a copy of the flag and the guide could
not state one placement while the code had two. That work touches `src/cli.rs`,
`src/main.rs` and `tests/cli_workspace_flag.rs`, which the original file list did not name,
and it adds cases 4.4 and 4.5. `tests/corpus_config_docs.rs` also changes: it parses the
guide's new workspace block with the real workspace parser, so the guide cannot drift from
the code that reads it (M6b-24, M6b-25).

## 6. Cross-cutting requirements

- The gates of `AGENTS.md`, unchanged.
- Every new test is deterministic: a fixed clock, no port, no sleep.
- The merge is pure. No corpus may influence another's score except through `archived_weight`.
- After each phase merges, rebuild the local install and re-run the ten questions of this
  repository.

## 7. The evaluation packet

The repository's ten questions are `tests/eval/repo-ten.jsonl`, committed in R1. The private
corpus's ten questions stay out of the repository, by rule; its counts are recorded in the
phase log and the gate record.

## 8. Definition of done

1. A default query reads the primary corpus only: **0 of 10** gate answers come from the
   archived corpus, down from 3 top-three appearances.
2. `--corpus all` returns archived rows, each carrying `corpus` and `scores.corpus` =
   `archived_weight`.
3. The merge is order-equal to the union-by-`final` sort, proved by a property test.
4. Single-corpus mode is byte-identical: **0** goldens move.
5. `janus corpora` reports each corpus's role and freshness without building or locking.
6. `format_version` 6, `api` 1, no exit code moved.

## 9. PR plan and size budget

| phase | logic ceiling | estimate | test ceiling | estimate |
|---|---|---|---|---|
| M6b-1 workspace + `corpora` | 400 | ~300 | 600 | ~350 |
| M6b-2 federated query | 600 | ~500 | 900 | ~600 |
| M6b-3 other tools | 400 | ~300 | 600 | ~350 |
| M6b-4 archive role + gate | 200 | ~100 | 300 | ~100 |

Estimates run two to three times low in this repository. Each phase names its split seam.

**Delivered, M6b-2.** 936 logic against a ceiling of 600, and 818 tests against
900. The ceiling counts every inserted and deleted line of a budgeted file,
comments included; the phase's non-test Rust is 580 lines, its comments 252, and
the rewritten `search` and `facts` command arms carry 106 deleted lines. The
split seam was measured and not taken: part 1 (selection, note, primary-only
execution) needs the same workspace resolution, the same two command arms and
the same report fields, which is 400 to 450 lines on its own, and part 2 would
rewrite those arms to add `all`, the weight and the merge. The split would
therefore move plumbing into a first review and make the second review read it
again, so the phase ships as one change under the `large-pr` label. The numbers
and the whole argument are in §12.

**Delivered, M6b-3.** 839 logic against a ceiling of 400, and 535 tests against
600. The estimate was 300 and the delivery is 2.8× it. The split seam was
measured and not taken: part 1 alone (the fleet, `read_document` and
`janus read`) is about 450, because `search` and `facts` must read through the
same fleet or the branch would hold two ways to open a workspace; part 2
(`status` and the rpc/MCP parity) is about 200. Neither half fits 400 either, so
the split would buy a second review and a rewritten `main.rs` without meeting the
phase ceiling. The phase ships as one change under the `large-pr` label, and §12
states the arithmetic.

## 10. SPEC corrections

1. **§9.2.** Add `corpus?` to `read_document` and to `janus_status`'s response, and state
   that a workspace's `id` and a corpus's declared `corpus_id` are two names reported
   separately when they differ. (P3.) **Delivered in M6b-3.**
2. **§9.4.** Add `corpus` to the score breakdown and state its default of `1.0`. (P2.)
   **Delivered in M6b-2**, and restated in M6b-4 so the neutral `1.0` is named as absent from
   the JSON and from the table.
3. **§6.11.** State that exactly one corpus is primary in v1, that naming an archived corpus
   does not promote it, and that a cross-corpus link stays inert until M7. (P1, P2.)
   **Delivered in M6b-4.**
4. **§16.2.** Name the `workspace.toml` keys this milestone actually reads, and drop the
   example's implication that `promote` is available. (P4.) **Delivered in M6b-4.**
5. **§9.1.** Add `--workspace` to the seven commands that take it, and state that every other
   command refuses it. (P4.) **Delivered in M6b-4.**

## 11. Handoff

- M7 inherits `allow_cross_corpus_links` and `promote`: both are write-side, and both now have
  a workspace to write into.
- The gate's recommendation to split `_system` doctrine from scaffolding is corpus config, not
  code; it stays in the private corpus's file.

## 12. Implementation logs

### M6b-1 — The workspace file, resolution, and `janus corpora`

**State.** Complete. Branch `m6b-phase1-workspace`, one pull request.

**Files.** `src/workspace.rs` (new: the file's shape, its validation, the
resolution, and the listing command), `src/error.rs` and `src/api/error.rs` (one
new failure), `src/cli.rs` (`corpora`), `src/main.rs` (resolution at the edge),
`src/lib.rs`, `tests/cli_corpora.rs` (new), `tests/cli_status.rs` (the renderer
list gains the new module), `tests/fixtures/workspace/` (two corpora and the file
that names them).

**What runs.** `janus corpora [--json]` reads one workspace and reports each
corpus's id, role, root, read-only flag, index directory, config presence, index
presence and staleness. It reads one manifest per corpus and walks only for the
digest. It builds nothing and takes no lock.

**Resolution.** `--workspace PATH`, then `JANUS_WORKSPACE`, then
`<platform config dir>/janus/workspace.toml`, then nothing. `main` reads the
variable and the platform directory once and passes both as values, so
`workspace.rs` reads no environment and two runs in one process cannot disagree.
A named path that is not a file is an error; the platform file is used only when
it exists.

**Cases.**

| Case | Evidence |
|---|---|
| 1.1 flag, then env, then config dir | `case_1_1_the_flag_beats_the_env_and_the_env_beats_the_config_dir` |
| 1.2 no workspace changes nothing | `case_1_2_no_workspace_changes_nothing_about_a_single_corpus_report`, plus the untouched goldens |
| 1.3 two primaries, duplicate id, missing root | three `case_1_3_…` tests; each asserts the file and the entry appear in the message |
| 1.3 unknown key, and an unusable id | `case_1_3_an_unknown_key_is_tolerated_with_a_warning`, `case_1_3_an_id_the_command_line_cannot_name_is_an_error` |
| 1.4 archived is read-only by default | `case_1_4_an_archived_corpus_is_read_only_unless_it_says_otherwise` |
| 1.5 presence and staleness without a build | `case_1_5_corpora_reports_freshness_without_building_or_locking` |

**Single-corpus evidence.** `tests/golden_search.rs` and `tests/golden_scan.rs`
run unchanged and no snapshot moved. In this phase only `corpora` loads a
workspace, so every other command keeps its output byte for byte: `search` gains
no field and prints no workspace diagnostic.

**Gates.** fmt, clippy with `-D warnings`, test (902 passed, 2 ignored), doc
(zero warnings), the Windows cross-check. Size: logic 583, tests 533.

**Size against the plan.** The phase budget was 400 logic, the estimate 300, and
the delivered phase is 583. The split seam was not taken: part 1 alone (the file,
validation and resolution with their tests) is about 450, so neither half fits
under 400 without dropping validation this plan asks for. The numbers stay under
the repository ceiling of 600. The next phase should read 400 as a floor, not a
target.

**Not included.** `--corpus`, the merge, the archived exclusion and the `corpus`
field (M6b-2); `read_document` and `janus_status` in a workspace (M6b-3);
`promote`, cross-corpus links and every write (M7); `[scope_groups]`; and a
`--workspace` flag on commands that read no workspace yet.

### M6b-2 — The federated query

**State.** Complete. Branch `m6b-phase2-federated`, one pull request.

**Files.** `src/search/federate.rs` (new: the selection, the weight, the merge),
`src/search/mod.rs` (the `corpora` head field, the per-row `corpus`, the human
table), `src/search/facts.rs` (the answer and the order it ran in),
`src/score.rs` (`scores.corpus` and the one place a corpus changes a score),
`src/main.rs` (workspace resolution for a query, one config and one embedder per
corpus), `src/cli.rs` (`--corpus` and `--workspace` on `search` and `facts`),
`src/api/dto.rs` and `src/mcp/tools.rs` (the `corpus` parameter),
`src/engine.rs` (the parameter, and the refusal of a selection it cannot serve),
`tests/workspace_query.rs` (new) and `tests/fixtures/workspace-query/` (new: two
corpora, one archived, with a weight the cases can state exactly).

**What runs.** A query resolves a set of corpora, and every corpus answers with
its own top-`N` through the same ranking function a single-corpus query uses.
The merge is one stable sort over those ranked lists, keyed by the tool's own
order value: `final` for `search` and for the `facts` fallback list,
`type_authority` for a `facts` fact list. `archived_weight` multiplies `final`
once, at the merge, and is reported as `scores.corpus`.

**The merge contract, in one paragraph.** A document in the global top-`N` is
necessarily inside its own corpus's top-`N`, because that corpus's list is
ordered by the same key and holds at least `N` rows. So merging the per-corpus
lists and cutting to `N` is exact: it cannot drop a document that belongs in the
answer and it cannot reorder two documents one corpus already ordered. At an
equal value the merge keeps corpus order across corpora and the corpus's own
order inside one corpus, so no second tie-break exists anywhere (M6b-15).

**Selection.** `primary` (default) reads the primary corpus and prints one note
naming how many archived corpora it excluded. `all` reads every corpus and
prints no such note. An id reads that corpus, and an archived corpus keeps its
weight: naming a corpus is a scope choice, not a claim that its facts are
current (M6b-3). An unknown id is a usage error listing the ids, the two
keywords and, when there are archived corpora, what the keywords do.

**Cases.**

| Case | Evidence |
|---|---|
| 2.1 default is the primary, with one note | `case_2_1_a_default_query_reads_the_primary_and_names_what_it_excluded` |
| 2.2 `all` returns both, weighted and named | `case_2_2_all_returns_both_and_each_archived_row_carries_the_weight` |
| 2.3 a named archived corpus keeps its weight | `case_2_3_a_named_archived_corpus_is_read_alone_and_keeps_its_weight` |
| 2.4 the merge is exact against the full union | `case_2_4_the_merge_is_exact_against_the_full_union` (property, 256 cases), `case_2_4_a_per_corpus_fetch_below_the_limit_loses_a_document`, `case_2_4_an_equal_value_keeps_corpus_order_then_the_corpus_s_own_order` |
| 2.5 an unknown corpus is a usage error listing the ids | `case_2_5_an_unknown_corpus_exits_two_and_lists_the_ids` (CLI, exit 2) and `case_2_5_an_unknown_corpus_is_a_usage_error_that_lists_the_ids` |
| 2.6 the limit is a total, not a limit per corpus | `case_2_6_the_limit_is_a_total_across_corpora_not_a_limit_per_corpus` |
| 2.7 `facts` keeps authority and reports the weight | `case_2_7_facts_keeps_the_authority_order_and_reports_the_weight`, plus `case_2_7_the_fact_list_keeps_authority_when_a_corpus_falls_back` |

**The weight, stated as arithmetic.** `final = relevance · authority · decay ·
corpus`, evaluated in that order. The `search` case reads every factor from the
row it checks, so the assertion is exact; the unit test states the literals
(relevance 0.6, authority 1.5, decay 0.8 → `final` 0.72, and 0.576 at weight
0.8) and asserts that a neutral factor is absent from the JSON.

**Why the formula is not checked against a second run.** Two runs of the same
query can differ in the last bit: the query that builds an index reads the
lexical values it just computed, and the next one reads the published rows. The
difference is one unit in the last place and it predates this phase, so the
cases compare one report against itself instead of two reports against each
other.

**The committed fixture.** `tests/fixtures/workspace/` now keeps one archived
note that is still true beside the superseded decision, so a default
`--corpus all` query returns a row from each corpus with the archived weight
beside its score. The retired decision stays superseded, which proves that the
status gate still applies: the role says when a corpus is read, and the status
says whether a document is true now (case 2.2).

**Single-corpus evidence.** With no workspace, `search` and `facts` take the
same path they took before this phase: no `corpora` head field, no `corpus` on a
row, no factor in the breakdown, no prefix in the table. `no_workspace_keeps_the_single_corpus_report_shape`
pins the shape, and no golden moved.

**Resolution for a query.** `--workspace`, then `JANUS_WORKSPACE`, then the
platform file — except that a corpus root named by `--vault` or `JANUS_VAULT`
wins over the platform file, and naming a root and a workspace together is a
usage error (M6b-14). Without that rule, a machine with a platform workspace
would federate every query inside every vault, including the golden fixtures.

**Gates.** fmt, clippy with `-D warnings`, test (923 passed, 2 ignored), doc
(zero warnings), the Windows cross-check. Size: logic 936, tests 818.

**Size against the plan, and the split seam.** The phase budget was 600 logic,
the estimate 500, and the delivered phase is 936 by the script. The script
counts every inserted and deleted line of a budgeted file, comments included:
the non-test Rust is 580 lines, its comments 252, and the diff deletes 106 lines
while rewriting the `search` and `facts` command arms. So the code a reviewer
must hold is at the ceiling while the mechanical number is 1.6× it.

The split seam was measured, not skipped. Part 1 was defined as the selection,
the note and a primary-only execution: it needs the same workspace resolution,
the same two rewritten command arms, the same report fields and the same engine
parameter, which is 400 to 450 lines on its own. Part 2 would then rewrite those
command arms to add `all`, the weight and the merge. The seam therefore moves
plumbing into a first review and rewrites it in a second, and the merge — the
milestone's core contract — arrives in a diff that also changes the code its
reviewer just read. The phase ships as one change with the overrun stated here
and in the pull request, and the next phase reads 600 as a floor: the estimate
was 500 and the delivery is 1.6× it, which is the same error the previous phase
made.

**Not included.** A workspace-aware engine, so a `corpus` parameter that needs
one is refused rather than answered from the primary (M6b-12, M6b-3);
`read_document` and `janus_status` in a workspace (M6b-3); the remaining SPEC
corrections of §10 (the §9.4 `corpus` wording landed with this phase); `promote`,
cross-corpus link resolution and every write (M7); `[scope_groups]`.

### M6b-3 — The other tools

**State.** Complete. Branch `m6b-phase3-tools`, one pull request.

**Files.** `src/engine.rs` (the fleet: every corpus a workspace names, one engine
per corpus built on first use, and the one dispatch every surface calls),
`src/main.rs` (one resolution helper, and the read, status, rpc and MCP commands
wired through it), `src/status.rs` (`workspace` and `corpora`),
`src/workspace.rs` (the corpus rows owned, so two surfaces print one shape),
`src/search/federate.rs` (one answer, rendered or encoded),
`src/cli.rs` (`--workspace` and `--corpus` on `read`, `--workspace` on `status`,
`rpc` and `mcp`), `src/rpc.rs` and `src/mcp/mod.rs` and `src/mcp/tools.rs` (the
fleet, and `corpus` in the read schema), `src/api/dto.rs` (`ReadParams.corpus`),
`src/link.rs` (the qualifier splitter, shared), `SPEC.md` §9.2 (correction 1),
`tests/workspace_read.rs` (new), `tests/fixtures/workspace-read/` (new: two
corpora holding one document at one path with different bytes).

`src/read.rs` is untouched. A read names its corpus where the fleet resolved it,
which is the one place that knows the name, so the read path keeps its rule that
it returns text and nothing else.

**What runs.** `read_document` takes `corpus?`: absent, the primary corpus is
read. A path may carry the same name as a `corpus:path` qualifier, which is the
form a cross-corpus link already uses, and the qualifier is stripped before the
path is resolved. The answer carries `corpus`, and that field is absent in
single-corpus mode. `janus read` gains `--workspace` and `--corpus`, so the
command line can read a document from an archived corpus.

`janus_status` in a workspace reports `workspace` and one row per corpus: `id`,
`role`, `root`, `read_only`, `index_dir`, `config_present`, `index_present`,
`index_stale`. The rows are the ones `janus corpora` prints, read by one function,
so the two commands cannot disagree. Nothing is built and no lock is taken.

**One dispatch, three surfaces.** `Fleet` is the type every adapter calls: the
command line, the rpc loop and the MCP server. It holds one `Engine` per corpus,
built on first use, so a query that reads the primary does not resolve an
embedding model for an archived corpus it left out. A selection that names no
corpus is the same usage failure with the same list of ids on all three surfaces,
because the plan owns the message (M6b-19).

The five tools over rpc and MCP now answer a `corpus` parameter, including
`all` and a named id, which the previous phase refused: the refusal remains only
where no workspace is in play, where one engine really does hold one corpus
(M6b-12). `lineage` keeps no `corpus` parameter, and a qualified path is refused,
because a chain is one corpus's graph (M6b-23). `status` takes no `corpus`
parameter; its `corpora` array answers the per-corpus question, and `--workspace`
is the flag (M6b-20).

**Cases.**

| Case | Evidence |
|---|---|
| 3.1 a read names its corpus | `case_3_1_a_read_without_a_corpus_reads_the_primary_and_names_it`, `case_3_1_the_named_corpus_answers_and_a_qualified_path_answers_the_same` |
| 3.2 an unknown id is `usage`, listing the ids | `case_3_2_an_unknown_corpus_is_a_usage_failure_that_lists_the_ids` (the parameter and the qualifier, exit 2), `case_3_2_a_single_corpus_read_names_its_own_corpus_and_no_other` |
| 3.3 a workspace status lists the corpora | `case_3_3_a_workspace_status_lists_each_corpus_with_its_role_and_freshness`, `case_3_3_a_single_corpus_status_keeps_its_shape` |
| 3.4 the three surfaces agree | `case_3_4_the_five_tools_answer_the_corpus_parameter_alike` |
| 3.5 a write is still refused | `case_3_5_a_write_tool_is_refused_whatever_the_corpus_says` |

**The two bodies, stated.** The fixture holds `notes/shared.md` in both corpora.
The primary's body is `"\n# Cache plan\n\nThe primary corpus keeps one cache in
front of the database.\n"` and the archived body is `"\n# Cache plan\n\nThe archived
corpus describes the retired cache fleet.\n"`. Each case asserts one of them
literally, so a read that fell back to the primary cannot pass.

**Single-corpus evidence.** With no workspace, `status` gains no field: a case
pins the exact nine keys, and `tests/cli_status.rs` runs unchanged. `read`
answers the bytes it always did. The claim was also checked against a build of
`main` on a copy of the committed fixture: `status --json`, `status`, `read
--json`, `read`, `search --json` with and without embeddings, `facts --json`,
`lineage --json`, and a two-request rpc session produced byte-identical stdout,
with `taken_ms` masked in the search report because it is a duration. The
workspace fields are absent rather than null, so no golden moved.

**Gates.** fmt, clippy with `-D warnings`, test (931 passed, 2 ignored), doc
(zero warnings), the Windows cross-check. Size: logic 839, tests 535.

**Size against the plan, and the split seam.** The phase budget was 400 logic,
the estimate 300, and the delivered phase is 839 by the script. The script counts
every inserted and deleted line of a budgeted file, comments included: the
source diff adds 596 lines and deletes 209, of which 148 added and 20 deleted are
comments. The deleted lines are the old `Scope` type, the old `call_tool` body
and the two command arms the fleet replaces, so a reader holds about 430 new
lines of code and about 150 lines of prose that explain them.

The seam was measured, not skipped. Part 1 was defined as the fleet plus
`read_document` and `janus read`: it is about 450 lines, because `search` and
`facts` must read through the same fleet or the branch would hold two ways to
open a workspace, which the scope of this phase forbids. Part 2 is `status` and the
rpc/MCP parity: about 200 lines. Neither half fits the phase ceiling of 400, so
the split would buy a second review and a rewritten `src/main.rs` without meeting
the budget it was made for. The phase therefore ships as one change with the
overrun stated here and in the pull request, under the `large-pr` label. The
lesson for M6b-4 and for M7 is the one this milestone has now recorded twice: an
estimate in this repository is a floor, and a phase that touches six surfaces
should be planned as two from the start, not split at review time.

**Not included.** `promote`, cross-corpus link resolution and every write (M7);
a cross-corpus chain for `lineage` (M6b-23); the ranking path of `search`
(M6b-2); the archive-as-a-role gate and the remaining SPEC corrections of §10
(M6b-4); `[scope_groups]`.
