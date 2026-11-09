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
- `role` is `primary` or `archived`; exactly one primary in v1. More than one is a config
  error, not a warning, because "primary" is what a default query means.
- `read_only` defaults to `true` for `archived` and `false` for `primary`.
- `index_dir` falls back to `<root>/.janus`, which is what a corpus already uses today.
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

**Files.** `src/read.rs`, `src/status.rs`, `src/api/dto.rs`, `src/engine.rs`,
`tests/workspace_read.rs` (new), `tests/cli_status.rs`.

**Build.** `corpus?` on `read_document`, the `corpora` block on `status`, and the same
parameter over rpc and MCP.

**Tests.** Cases 3.1–3.5.

**Accept.** The five tools answer identically over the three surfaces, with the M6
normalizations.

### Phase 4 — The archive becomes a role

**Files.** `docs/corpus-config.md`, `SPEC.md`, `docs/m6b-breakdown.md`, and the private
corpus's `janus.toml` (git-ignored, edited in that project).

**Build.** Reverse the exclude line in the workspace config, declare the archive, re-measure
the ten questions, and update the guide so `exclude` is no longer the recommended answer for
a retired tree.

**Tests.** Cases 4.1–4.3, measured, not asserted in the repository.

**Accept.** The archive is absent from a default query and present under `--corpus all`.

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

## 10. SPEC corrections

1. **§9.2.** Add `corpus?` to `read_document` and to `janus_status`'s response, and state
   that a workspace's `id` and a corpus's declared `corpus_id` are two names reported
   separately when they differ. (P3.)
2. **§9.4.** Add `corpus` to the score breakdown and state its default of `1.0`. (P2.)
3. **§6.11.** State that exactly one corpus is primary in v1, that naming an archived corpus
   does not promote it, and that a cross-corpus link stays inert until M7. (P1, P2.)
4. **§16.2.** Name the `workspace.toml` keys this milestone actually reads, and drop the
   example's implication that `promote` is available. (P4.)

## 11. Handoff

- M7 inherits `allow_cross_corpus_links` and `promote`: both are write-side, and both now have
  a workspace to write into.
- The gate's recommendation to split `_system` doctrine from scaffolding is corpus config, not
  code; it stays in the private corpus's file.

## 12. Implementation logs

_(empty)_
