# M5 — The temporal graph

> Branch: `m5-plan` (this document), then one branch per phase from `main`.
> Spec: §6.4, §6.7, §6.9, §6.10, §6.12, §7.1, §7.2, §8.3, §8.9, §9.1, §9.4, §11.
> Evidence: the committed fixture vault, the code as merged at `36fc7e5`, and the handoff
> from `docs/m4d-breakdown.md` §10.
>
> **Done when:** `lineage` renders a correct 5-link chain, `search --as-of` returns the old
> value for a past date and the new one today, the seeded cycle is detected and broken
> deterministically, and a simulated dream cycle — move a linked document unchanged, move
> and edit a linked document with no `id` anywhere, rewrite a third body — breaks no chain,
> writes no vault file, and asks for no confirmation.

**Words used here.** **Link**: a declared `supersedes` or `superseded_by` value.
**Edge**: a link resolved to two `DocId`s. **Successor**: the newer document of an edge.
**Window**: a document's `[valid_from, valid_to)` interval. **Basis**: the previous
generation, used to carry identity forward. **Alias map**: every path ever seen, mapped to
its `DocId`.

## 0. Scope

**Decision: M5 makes time and lineage part of the answer, and it makes identity survive a
move without writing to the vault.**

Today the engine ranks by declared metadata only. It has no graph, no window, no carried
identity, and no command that explains either. Measured against the code:

| M5 item | state today | evidence |
|---|---|---|
| `supersedes` / `superseded_by` reach `Document` | no — parsed into `Frontmatter`, never copied to `Document` | `src/document.rs:28` and `src/document.rs:149` |
| links persisted in the generation | no — `DocMeta` has no link field | `src/index/mod.rs:87` |
| `effective_from` parsed and persisted | yes | `src/document.rs:145`, `src/index/mod.rs:109` |
| `effective_to` persisted | no — parsed only, never reaches `DocMeta` | `src/document.rs:148`, `src/index/mod.rs:105-110` |
| `effective_status` derived | no — the gate reads the declared status | `src/score.rs:28`, `src/score.rs:283` |
| validity windows, chain fallback | no | — |
| `--as-of` | no flag, no option, no parse | `src/cli.rs:113`, `src/search/mod.rs:44` |
| path history, alias map | no | `src/vault/identity.rs:20` explains why rung 2 is skipped: no previous set exists |
| carry-forward identity | no — every scan re-derives | `src/vault/identity.rs:86` |
| `janus lineage` | absent | `src/cli.rs:51-204` (the enum ends at `facts`) |
| `janus doctor` | absent | `src/cli.rs:51-204` |
| `janus ids`, `janus moves`, `janus suggest-links` | absent | `src/cli.rs:51-204` |
| cycle detection | none; the fixture seeds a 3-document cycle | `tests/fixtures/vault/decisions/*.md` |
| cross-scope supersession | no rule; the scope filter is retrieval-only | `src/score.rs` `ScopeFilter`, m3 plan S-11 |

### In scope

- Identity carries forward while the path is stable, and matches across a move by the
  §6.4 ladder. The content-hash rung becomes real, because a previous set now exists.
- A path history per `DocId`, kept in the generation, and an alias map derived from it.
- Tombstones: a vanished document keeps its id and its last path, and stays out of search.
- Edges resolved at build time, closed symmetrically, broken deterministically at a cycle,
  and inert across disjoint scopes.
- Scope-relative validity windows, derived `effective_status`, and `search --as-of`.
- Five commands: `lineage`, `doctor`, `ids`, `moves`, `suggest-links`.
- Quarter dates in names, and the corpus-relative decay for an undated document.
- One new fixture vault for graph cases. The main fixture keeps its goldens.

### Not in scope

- Any write to a vault file. M5 reports and derives only (§9.3, D7).
- MCP and `janus rpc` (M6). The commands are core functions so M6 can call them.
- Workspaces, multi-corpus queries, `--corpus`, `archived_weight`, qualified cross-corpus
  links (M6, §6.11, D42). A qualified link to another corpus is reported, not resolved.
- `doctor --fix`, quarantine, and the CI gate (M8).
- The `deprecated` merge with "retired with no successor" (open question 21, M7).
- Graph expansion in retrieval, entity edges, `EdgeSource::Derived` (§8.9).
- A file watcher, a warm server, or a background reindex (M8).
- `--git-history`, transaction-time queries (post-1.0, D40).

### Why the milestone splits into eight phases

M5 is larger than any earlier milestone. One PR cannot hold it under the 600-line logic
ceiling. The phases below are reviewable units in this order:

```
P1 identity + paths  →  P2 edges  →  P3 windows + as-of  →  P4 lineage
                                                        →  P5 doctor
     P6 moves + hints  ←  needs P1
     P7 suggest-links  →  needs P2
     P8 quarter dates + undated decay  (independent; last)
```

P1 comes first because every later phase resolves a path through the alias map. P6 needs
P1's path history and P6's hints are what make a lost identity visible. P7 needs P2's
graph, so a suggested pair never repeats an existing edge.

## 0a. Review resolutions

One review round, sixteen findings, one rejected sub-claim. Every accepted finding is fixed
in this document, not annotated.

| # | finding | resolution |
|---|---|---|
| R-1 | §3.1 step 3 can derive an id that step 1 already claimed: move `A` to `B` and create a new file at `A` in one build gives two documents one `DocId` | **Accepted.** Step 3 now skips any derived id that steps 1–2 hold and falls through to `rel_path`. Case 1.13 pins it. |
| R-2 | Edge direction contradicted itself: "older is always `from`" against a fixture whose oldest file replaces the newest, and case 4.1 named a pair that is not in the edge set | **Accepted.** `from` is now "the document the declaration replaces". The cycle rule and case 4.1 are recomputed: the ignored edge is `(revisited, redis)`, and the chain is `redis → postgres → revisited`. |
| R-3 | `--as-of` + `--intent` as exit 2 contradicts §7.1, §9.1 and §9.2's MCP shape | **Accepted.** `--as-of` wins and the response carries one override note. Case 8.8 and new case 8.10 pin it. §9 correction 4 states the rule. |
| R-4 | The window gate `valid_from <= today` drops an undated document, which §6.10 rule 9 keeps in `current` | **Accepted.** A missing `valid_from` passes the gate. Case 7.8 pins the fixture's undated document. |
| R-5 | One stored key cannot answer five ladder rungs, and `DocMeta` holds neither a declared id nor `doc_version` | **Accepted.** `IdentityRecord` stores one typed `MatchKey` per reachable rung. Case 1.14 covers two consecutive moves. |
| R-6 | P1 described the wrong write path; `--rebuild` drops the previous generation inside `refresh::plan_from_walk`, and `forced_documents` derives ids statelessly | **Accepted.** The phase now names `build_until_stable` → `plan_from_walk` → `plan_forced`, adds `basis` to all three plus `forced_documents`, and adds `store::previous_snapshot`. |
| R-7 | `tombstone` was a path flag in one place and a document flag in another, and case 1.3 expected a vacated path not to be one | **Accepted.** `tombstone` is true only on the last entry of a retired document. Step 4 clears it on re-entry; case 1.15 pins it. |
| R-8 | `doctor` reported `status_mismatch` for a dateless successor, then said it invents no check | **Accepted.** One behaviour: no check. The window rule and the consequence are stated once. |
| R-9 | "`--include-superseded` cancels the status gate" would admit `deprecated`, which §7.1 forbids | **Accepted.** The flag uses the `include_superseded` row and cancels only the window gate. Case 7.9 pins `deprecated`. |
| R-10 | `facts` on effective status contradicts §9.2's "declared status `active`", and no amendment was listed | **Accepted.** §9 correction 11 amends §9.2 in P3. Applies the window gate too. |
| R-11 | P3 claims a `format_version` bump but persists nothing new | **Accepted.** P3 no longer bumps. M5-19 now reads P1 → 4, P2 → 5, P6 → 6. |
| R-12 | Nothing owns the combined dream-cycle test that DoD rows 4–8 need | **Accepted.** P4 owns `tests/dream_cycle.rs`, which performs all three steps and compares every file hash and mtime before and after each. |
| R-13 | §0's `src/index/mod.rs:118` citation points at nothing useful for either row | **Accepted.** Both rows now cite `src/index/mod.rs:105-110` and `src/document.rs:145-148`. |
| R-14 | The undated-decay rule overrides §6.3's `1.0` without a SPEC amendment | **Accepted.** §9 correction 12 amends §6.3 and §6.10 rule 9. |
| R-15 | `chunks --json` prints the document `id`, so a stateless derivation would disagree with `search` | **Accepted.** `chunks` resolves against the basis too, and the plan cites `src/chunk/dump.rs:329`. |
| R-16 | The undated median uses `date`, but the decay anchor is `effective_from ?? date` | **Accepted.** The median now measures the same anchor the decay formula uses. |
| R-6b | "`previous_generation` returns `None` on a vectors mismatch, so a corrupt `vectors.f32` drops the basis" | **Rejected.** `src/index/store.rs:1464` returns `build::Previous::tokens(snapshot)` in that case, so the snapshot survives. The basis loader is still split out, because reading a basis must not pay to hash the vectors file; that reason is in P1 now, and the wrong one is not. |

**Also found in this round, not raised by the reviewer:** a tombstoned successor has no
`DocMeta` row, so it could not close its predecessor's window and a deleted document would
resurrect the one it replaced. Fixed with `TombstoneMeta` (§2.1), which also gives
`lineage` and `ids` a title for a retired document.

### Round 2 — six findings from the re-check

| # | finding | resolution |
|---|---|---|
| R2-1 | Step 3's derivation ladder included `ContentHash`, so case 1.13 does not reach `rel_path` and case 1.7 would derive content-hash ids | **Accepted.** Step 3 names its derivation ladder and excludes `ContentHash`; the enum comment says match-only. Case 1.13 now also makes the title+date key ambiguous, which is what forces `rel_path`. |
| R2-2 | `Graph::build` never sees a tombstoned endpoint, so a retired successor closes no window; `TombstoneMeta` also lacks scope data for the per-query match | **Accepted.** `Graph::build` takes the tombstone rows and creates a `Node` for each. `TombstoneMeta` gains `scope` and `shared_with`. §3.6 states the match rule for a retired successor. |
| R2-3 | "A root is a document that no edge replaces" names the wrong end, and M5-6 still said "older → newer" | **Accepted.** §2.3 now defines `predecessors` and `successors` by time, `Graph` has `Endpoint`, §3.9 defines a root as a document with no predecessor, and M5-6 drops the chronology. |
| R2-4 | M5-9 kept the wording that R-9 removed | **Accepted.** M5-9 now matches §3.7 and case 7.9. |
| R2-5 | `TombstoneMeta.last_path` is a second authority for a path, `tombstones` is never rebuilt, and M5-1 no longer listed it | **Accepted.** `last_path` is gone; the path comes from `path_history`. `tombstones` is rebuilt from the unclaimed set each build. M5-1 names all three tables. |
| R2-6 | `PathEntry.content_hash` is read by no contract and would go stale | **Accepted.** The field is deleted. The content hash lives in `MatchKey::ContentHash`, refreshed for a live document and frozen for a retired one. |

### Round 3 — four findings from the re-check

| # | finding | resolution |
|---|---|---|
| R3-1 | A retired `lineage` node claims `type` and `valid_to`, which no tombstone field can supply | **Accepted.** §3.9 now says a retired node reports `type: null`, `declared_status: null` and `valid_to: null`, and carries only what the tombstone holds. No field is invented. (R4-1 later set `effective_status` to `null` too.) |
| R3-2 | The claimed-id guard had no case that fires it, and case 1.13 already forced the fall-through by ambiguity | **Accepted.** Case 1.13 now has a different title and date, so the guard fires and the id comes from `title`. New case 1.13b keeps the ambiguity path. M5-21 says "the next key whose derived id is unclaimed". |
| R3-3 | M5-22 and §9 correction 13 still required `last_path` | **Accepted.** Both now say the path stays in `path_history`. |
| R3-4 | The content hash was still in two places with no stated owner, and `MatchKey::RelPath` had no reader | **Accepted.** M5-23 names the owner per state: `DocMeta.content_hash` for a live document, the record's copy for a retired one. `RelPath` leaves the enum; rung 6 stores no key. |

### Round 4 — one finding from the re-check

| # | finding | resolution |
|---|---|---|
| R4-1 | A retired node reported `effective_status: "retired"`, but `EffectiveStatus` has five variants and M5-10 forbids a sixth | **Accepted.** A retired node reports `effective_status: null`. `retired` lives only in `terminal.state`, and the sentence now counts four nulls, not three. |

## 1. Dependencies

None. Every phase uses crates already in `Cargo.toml`. Quarter dates use `chrono`, already
present for the date ladder. `suggest-links` uses the existing vector payload.

## 2. Data model

Write these types first. Every later contract refers to them.

### 2.1 Identity and paths (`src/index/mod.rs`, persisted)

```rust
/// One path a document has occupied. `path_history` is keyed by `DocId`, so a
/// path survives a move that the document itself no longer occupies.
///
/// The three new tables store text, not `StringId`: see M5-24.
pub struct PathEntry {
    /// Vault-relative, `/`-separated (§8.7).
    pub rel_path: String,
    /// The calendar date of the build that first saw this path. Injected, never
    /// the wall clock (§7.2, tests/no_wall_clock.rs).
    pub first_seen: NaiveDate,
    /// The calendar date of the build that last saw this path.
    pub last_seen: NaiveDate,
    /// True on the last entry of a `DocId` that no longer appears in the scan. A
    /// path the document merely left is not a tombstone: the document is still
    /// live, and its new path is the last entry.
    pub tombstone: bool,
}

/// One identity key of one document, with the rung that produces it.
///
/// Every rung is stored, because a second move is matched against a rung that
/// no longer produces the id (case 1.14). `ContentHash` is a **match** key and
/// never a derivation key: as a derivation it would win for every document that
/// is not a byte-identical copy, and make rungs 3–5 unreachable (the reason
/// `src/vault/identity.rs:20-29` gives for skipping it today).
pub enum MatchKey {
    DeclaredId(String),
    ContentHash(Blake3Hash),
    TitleDate { title: String, date: NaiveDate },
    TitleVersion { title: String, version: String },
    Title(String),
}

/// Why a `DocId` has the value it has, and every **match** key it can answer to.
pub struct IdentityRecord {
    /// The rung that produced the id (§6.4).
    pub tier: IdentityTier,
    /// One key per match rung, in ladder order. Refreshed on every build for a
    /// live document, and kept unchanged for a retired one so that it can be
    /// matched again if the document returns. Rung 6 (`rel_path`) stores no key:
    /// the path is the key.
    pub keys: Vec<MatchKey>,
}

// Added to `Snapshot`:
pub path_history: BTreeMap<DocId, Vec<PathEntry>>,
pub identity: BTreeMap<DocId, IdentityRecord>,
pub tombstones: Vec<TombstoneMeta>,
```

A tombstone keeps the facts a graph needs after the document is gone. Its path is the last
entry of its `path_history`; a path is never stored twice.

```rust
/// The last known facts about a document that no longer exists.
pub struct TombstoneMeta {
    pub id: DocId,
    /// The title at that moment, so `lineage` and `ids` can name it.
    pub title: String,
    /// The date at that moment.
    pub date: Option<NaiveDate>,
    /// The valid-time start at that moment (`effective_from ?? date`).
    /// A tombstoned successor still closes its predecessor's window (§3.6).
    pub valid_from: Option<NaiveDate>,
    /// The owning scope at that moment, so the scope rule still applies (§6.12).
    pub scope: String,
    /// The retrieval tags at that moment, for the same reason.
    pub shared_with: Vec<String>,
}
```

`tombstones` is rebuilt from the unclaimed set on every build: a `DocId` that step 1 or
step 2 claims has no row in the new vector. A retired document can therefore never be
reported as retired after it returns (case 1.15).

`path_history` is the only source of paths. The alias map is derived at load time:

```rust
// src/vault/identity.rs, pure. P2 adds it, because P2 is its first reader.
pub fn alias_map(path_history: &BTreeMap<DocId, Vec<PathEntry>>) -> BTreeMap<&str, DocId>;
```

Two sources of one fact is the defect this avoids: the alias map is never stored.

### 2.2 Edges (`src/index/mod.rs`, persisted)

```rust
/// The only edge kind in v1 (§8.9, D35).
pub enum EdgeKind { Supersedes }

/// A declared link, resolved to two document ids.
pub struct Edge {
    /// The document the declaration replaces.
    pub from: DocId,
    /// The document that replaces it.
    pub to: DocId,
    /// Reserved for additive kinds (§8.9).
    pub kind: EdgeKind,
}

/// A declared link that resolved to nothing.
pub struct DanglingLink {
    /// The document that declared the link.
    pub from: DocId,
    /// The text the frontmatter carried, for `doctor` to print. Text, not
    /// `StringId`: it may name nothing in the corpus (M5-29).
    pub target: String,
    /// Why the target did not resolve.
    pub reason: DanglingReason,
}

pub enum DanglingReason {
    /// No current path, historical path or declared id matched.
    NotFound,
    /// The target names another corpus. Cross-corpus links are M6 (§6.11).
    OtherCorpus,
}

// Added to `Snapshot`:
pub edges: Vec<Edge>,
pub dangling_links: Vec<DanglingLink>,
// Added to `DocMeta`:
pub effective_to: Option<NaiveDate>,
```

Both directions are stored. A declaration on either side produces one edge, and the
resolver de-duplicates. `edges` is sorted by `(from, to)` so a build is deterministic.
`from` and `to` are relational, not chronological: the dates need not agree with the
direction. The fixture's cycle proves it, because its oldest file name declares that it
replaces the newest.

### 2.3 The graph at query time (`src/graph.rs`, new module, not persisted)

```rust
/// One document's place in the graph, after closure and cycle breaking.
///
/// `predecessors` are the documents this one **replaced**; `successors` are the
/// documents that **replace** it. The names follow time, not the edge, because
/// every reader asks a temporal question.
pub struct Node {
    pub id: DocId,
    pub predecessors: Vec<DocId>,
    pub successors: Vec<DocId>,
}

/// The facts one endpoint carries, live or retired.
pub struct NodeFacts {
    pub id: DocId,
    pub rel_path: String,
    pub scope: String,
    pub shared_with: Vec<String>,
    /// `effective_from ?? date`, or a tombstone's last valid start.
    pub valid_from: Option<NaiveDate>,
    /// The declared valid-time end, exclusive. A tombstone records none, so
    /// this is `None` for a retired endpoint.
    pub effective_to: Option<NaiveDate>,
    /// The declared status. `None` for a retired endpoint, which has no
    /// current metadata to read (R4-1).
    pub declared_status: Option<Status>,
}

/// A node endpoint: a live document, or a retired one that keeps its place in
/// the graph so it can still close a predecessor's window.
pub enum Endpoint {
    Live(NodeFacts),
    Retired(NodeFacts),
}

/// The resolved graph for one generation.
pub struct Graph {
    nodes: BTreeMap<DocId, Node>,
    endpoints: BTreeMap<DocId, Endpoint>,
    /// Edges the cycle breaker ignored, with both paths, for `doctor`.
    ignored: Vec<(DocId, DocId)>,
    /// Edges stored but inert, for `doctor` (§3.5).
    inert: BTreeSet<(DocId, DocId)>,
}
```

`Graph::build(edges, endpoints, allow_disjoint)` takes the snapshot's edges, one endpoint per
live document **and per tombstone**, and the `[links]` flag, and returns the closed,
cycle-broken graph. A tombstoned edge endpoint gets a `Node` like any other, so
`successors(A)` still names it and §3.6 can close A's window at its `valid_from` (case 6.6).
It is pure and it reads no clock.

`Graph::for_snapshot(snapshot, config)` is the one function that turns a generation into a
graph, and both callers use it: `janus index --json` for the counts and the readers for the
answers (M5-28). The graph owns its endpoint facts, so a caller holds no rows of its own and
the two answers cannot drift.

`Graph::endpoint(id)` returns an `Endpoint`, so the window and status rules read the scope,
`shared_with` and dates from one place whether the document is live or retired.

### 2.4 Windows and status (`src/graph.rs`, pure)

```rust
/// A document's valid-time interval, resolved for one query scope set.
pub struct Window {
    pub valid_from: Option<NaiveDate>,
    /// Exclusive.
    pub valid_to: Option<NaiveDate>,
    /// True when `valid_to <= valid_from`. The window is then empty.
    pub invalid: bool,
}

/// The status a reader must use. Not persisted.
pub enum EffectiveStatus { Active, Draft, Deprecated, Superseded, Unknown }
```

`Window::of(facts, graph, scopes)` and `EffectiveStatus::of(facts, graph, scopes)` take only
the one `NodeFacts` from `Graph::endpoint`, so `NodeFacts` carries the declared status and the
declared end as well (M5-30). `EffectiveStatus::of` returns `Option`: a retired endpoint has
no current metadata, and P4 reports `effective_status: null` for it (R4-1).

`EffectiveStatus` has no `expired` variant. Expiry lives in the window and nowhere else
(§6.10 rule 5). A document declared `active` with `effective_to` in the past is active and
outside its window.

### 2.5 Hints and moves (`src/index/mod.rs`, persisted; P6)

```rust
/// A display-only guess that a new document continues a vanished one (§6.9 case 3).
pub struct Hint {
    /// The vanished document.
    pub id: DocId,
    /// The weighted score from §6.9. Never applied.
    pub confidence: f32,
}

// Added to `DocMeta`:
pub hint: Option<Hint>,
```

### 2.6 Command reports (`src/graph.rs`, `src/doctor.rs`, new modules)

These are the `--json` DTOs. They live in the library, so M6 can reuse them (§9.5). The
plan's contract sections give each shape exactly.

## 3. Contracts

Each contract names its owning phase and its acceptance cases. A contract is live: a phase
that changes one amends this section in the same PR.

### 3.1 Identity continuity — the previous generation is the cache (P1)

**Statement.** A document that keeps its path keeps its `DocId`, whatever happens to its
content. A document that changes path inherits the `DocId` of the one vanished document
whose match key it carries. Nothing is written to the vault.

**Algorithm.** `identity::resolve(documents, basis: Option<&Snapshot>)`:

1. **Carry-forward.** For each document whose `rel_path` is the **live** path of a basis
   `DocId`, adopt that `DocId`. A live path is a `PathEntry` that is not a tombstone and is
   the last entry of its `DocId`. Mark the id claimed. A path a document has left is
   historical, never live, so a document that returns to an old path does not adopt the id
   that used to sit there; step 2 decides that case.
2. **Match.** For each unclaimed document, in `rel_path` byte order, walk the §6.4 ladder
   (declared id, content hash, title+date, title+version, unique title). A rung wins when
   exactly one **unclaimed basis `DocId`** carries that key and that `DocId` is absent from
   the current path set. Inherit that `DocId`.
3. **Derive.** Every remaining document gets `DocId = blake3(match_key)`, using the first
   **derivation** key whose multiplicity is exactly one in the current document set **and
   whose derived id is not already claimed by step 1 or step 2**. The derivation ladder is
   declared id → title+date → title+version → unique title → rel_path. `ContentHash` is not
   on it: as a derivation key it is unique for every document that is not a byte-identical
   copy, so it would win for everything and make rungs 3–5 unreachable (the reason
   `src/vault/identity.rs:20-29` gives today). It stays a match key for moves.

   Multiplicity counts every document, carried or not. The claimed-id guard is not
   optional: a build that moves `A` to `B` and creates a new file at `A` gives the new file
   `A`'s old id in step 1, and `A` would then re-derive the same id from its unchanged title
   and date. The guard makes step 3 skip that key and continue down the ladder, so two
   documents can never share one id.
4. **Record.** Write `identity[DocId] = { tier, keys }` for every live document, refreshing
   every `MatchKey` from the current metadata (M5-25), and append a `PathEntry` to
   `path_history[DocId]` with `first_seen` and `last_seen` set to `today` for a new path, or
   `last_seen` updated for a known path. Claiming a tombstoned `DocId` clears its tombstone
   flag, so a document that returns resurrects its identity. An `IdentityRecord` for a
   retired id is kept unchanged, so a returning document can still match its keys.
5. **Tombstone.** A basis `DocId` that step 1 or step 2 did not claim gets `tombstone = true`
   on its last entry and `last_seen = today`. It gets no `DocMeta` row, and a
   `TombstoneMeta` row built from its last known values. The `tombstones` vector is rebuilt
   from this unclaimed set on every build, so a claimed `DocId` has no row in the new vector.

**Basis absence.** A missing, corrupt or `--rebuild`-disabled basis is not an error. The
resolver runs step 3 only and prints one warning on stderr:
`identity: no previous generation; ids re-derived`. `--rebuild` still supplies the basis:
rebuild means "re-tokenize", never "re-identify".

**One fact, two uses.** The build has two different reasons to read the previous
generation: token/vector reuse, and identity. They are separate arguments. `--rebuild`
passes a basis and no reuse source. A single argument would make `--rebuild` re-identify
every document and break contract 3.1 on the first rebuild.

**Sidecar loss.** Deleting `.janus/` removes the basis. Unmoved documents derive the same
id, because the derivation is a pure function of the document. A document that moved while
the sidecar was absent may churn. This is the documented price (§6.4).

**`scan` and `chunks`.** `scan` and `chunks` resolve against the basis when a generation
validates, so the `id` they print agrees with `search`. Both read the generation without a
lock and without a refresh, and derive statelessly when no generation validates. `chunks`
needs the basis because its `--json` row carries the document `id` (`src/chunk/dump.rs:329`),
and a stateless derivation would print a different id after an in-place title edit.

**`janus ids`.** `janus ids [PATH...] [--json]` prints one row per document: `path`, `id`,
`tier`, `keys` (each reachable rung, for inspection), `inherited` (true when the id came
from the basis), and `previous_path` (the path the id held before this build, or `null`).
With no path it prints every live document, sorted by path. A path that names a tombstone
prints one row with `retired: true` and the last known title.

**Acceptance cases.**

| # | action | expected |
|---|---|---|
| 1.1 | index; edit the body of `notes/flow-mapping.md`; index | same `DocId`; `identity` key unchanged; one `PathEntry` |
| 1.2 | index; change `title:` in `decisions/2025-02-01-cache-revisited.md`; index | same `DocId`; `tier` stays `title_date`; the stored keys are refreshed to the new title, so a later move still matches (M5-25) |
| 1.3 | move `notes/flow-mapping.md` to `notes/flow.md` unchanged; index | same `DocId`; `tier` = `content_hash`; two `PathEntry`s, the first not a tombstone |
| 1.4 | move `decisions/2024-11-02-postgres-cache.md` to `decisions/postgres.md`, edit its body only; index | same `DocId`; `tier` = `title_date`; `path_history` holds both paths, oldest first (the alias map is P2's) |
| 1.5 | move a document and change both title and date; index | new `DocId`; the old id is a tombstone; one `Hint` (P6) |
| 1.6 | copy a document byte-identically to a second path; index | two `DocId`s; the copy falls through to `rel_path`; `duplicate_identity` note on the key |
| 1.7 | delete `.janus/`; index | ids identical to case 1.3's *unmoved* set; the moved document may churn |
| 1.8 | index `--rebuild` twice | ids do not change between runs |
| 1.9 | index with a corrupt `index.bin` | build succeeds, ids re-derived, one warning names the re-derivation |
| 1.10 | `scan --json` after case 1.2 | the same `id` and `id_source` as `search --json` |
| 1.11 | `janus ids` with no argument | one row per live document, sorted by path; `inherited: true` for every id the basis held |
| 1.12 | `janus ids <tombstoned path>` | one row, `retired: true`, with the last known title from `tombstones` and the last path from `path_history` |
| 1.13 | move `A` to `B`, then create a new file at `A` with a different title and date, in one build | two distinct `DocId`s; the new file inherits `A`'s id by step 1; `A`'s title+date key derives that same id, the guard skips it, and `A` takes `blake3(title)` |
| 1.13b | same, but the new file repeats `A`'s title and date | two distinct `DocId`s; the new file inherits `A`'s id; `A`'s title+date key is now ambiguous, so `A` falls to `rel_path`; `doctor` reports one `duplicate_identity` |
| 1.14 | move a document with an unchanged body, then move it again after a body edit | the id survives both moves: the first matches by `content_hash`, the second by `title_date` |
| 1.15 | remove a document, index, restore it at its first path, index | the same `DocId`; the tombstone flag is cleared. A move cannot test this: it tombstones nothing (R-7), because the id follows the document (M5-26) |

### 3.2 The alias map and link resolution (P2)

**Statement.** A declared link resolves in this order, and every step is a lookup, never a
guess: current path → historical path (any `PathEntry`, including a tombstone) → declared
`id` → dangling.

**Text forms.** A link value is a path or a declared id. A leading `corpus:` prefix is a
corpus qualifier. A qualifier equal to `config.corpus_id` is stripped and the path resolves
locally. Any other qualifier yields `DanglingReason::OtherCorpus`. A link text that is
absolute, contains `..`, or escapes the root is `DanglingReason::NotFound` and is never
confined against the filesystem: resolution reads the generation only.

**Acceptance cases.**

| # | input | expected |
|---|---|---|
| 2.1 | main fixture | 3 resolved edges (the cycle), 1 dangling with `NotFound` |
| 2.2 | move `bad/one-sided-new.md` to `bad/newer.md`; re-index | the edge still resolves through the historical path; 0 dangling |
| 2.3 | `supersedes: "fixtures:decisions/2024-12-01-redis-cache.md"` | resolves locally |
| 2.4 | `supersedes: "other:decisions/x.md"` | dangling with `OtherCorpus` |
| 2.5 | `supersedes: "../../etc/passwd"` | dangling with `NotFound`; no file is opened |
| 2.6 | a link to a tombstoned id | resolves; the edge exists; the target has no `DocMeta` row |

### 3.3 Symmetric closure and one edge per pair (P2)

**Statement.** `A.superseded_by = B` and `B.supersedes = A` mean one edge. Traversal does
not care which side declared it. Both sides are stored in `DocMeta` and neither is
authoritative.

De-duplication is by `(from, to)`. An edge is never reversed to fit a direction: `from` is
the document the declaration replaces, whatever its date says.

**Acceptance cases.**

| # | input | expected |
|---|---|---|
| 3.1 | only `A.superseded_by = B` | one edge `A → B`; `A` has no successor declaration of its own |
| 3.2 | both sides declare | exactly one edge |
| 3.3 | two documents declare `supersedes` of the same target | two edges with the same `to` |

### 3.4 Cycles break deterministically (P2)

**Statement.** When the edge set contains a cycle, the reader ignores one edge per cycle and
still answers. The graph reports every ignored edge.

**Rule.** While a cycle exists, among the edges on that cycle ignore the one whose
`(from.rel_path, to.rel_path)` byte pair sorts greatest. Paths, not ids: the rule must be
readable in `doctor` output and hand-derivable in a test. A move can change which edge is
dropped. That is acceptable, because a cycle is a data error in the first place.

The ignored edge stays in `Snapshot::edges` and is excluded from `Graph::nodes`. `doctor`
reports it as an error.

**Acceptance cases.**

| # | input | expected |
|---|---|---|
| 4.1 | main fixture cycle: `redis` is replaced by `postgres`, `revisited` by `redis`, `postgres` by `revisited` | pairs are `(redis, postgres)`, `(revisited, redis)`, `(postgres, revisited)`; the greatest `from` is `revisited`, so `(revisited, redis)` is ignored; the surviving chain is `redis → postgres → revisited` |
| 4.2 | same, `janus index --json` | `cycle_breaks: 1` |
| 4.3 | a 2-document cycle | one edge ignored; both documents keep one predecessor and one successor |
| 4.4 | a self-link `supersedes: <own path>` | treated as a cycle of one; ignored; `doctor` reports it |

### 3.5 Cross-scope links are effective only when scopes intersect (P2)

**Statement.** An edge is effective when `S(A) ∩ S(B) ≠ ∅`, or when `A.scope == "global"`, or
when `B.scope == "global"`. Otherwise the edge is inert: it contributes no status and no
window, and `doctor` reports `cross_scope_supersession` as an error.

`S(d) = {d.scope} ∪ d.shared_with` (§6.12 rule 3). `[links] allow_disjoint_supersession =
true` makes a disjoint edge effective and silences the check. It defaults to `false`.

Inertness is a property of the pair, so it does not depend on the query. Whether an
*effective* successor affects a document **for a given query** is a separate rule (3.6).

**Acceptance cases.**

| # | input | expected |
|---|---|---|
| 5.1 | `org:acme` document supersedes an `org:beta` document | edge stored; `Graph` lists no relation; `doctor` reports one error |
| 5.2 | same pair, plus `shared_with: [shared:cache]` on both | effective; beta's document reports `superseded` for the default query |
| 5.3 | same pair, one side `scope: global` | effective |
| 5.4 | `allow_disjoint_supersession = true` | effective; `doctor` silent |
| 5.5 | the flag set in `janus.toml` | no unknown-key warning |

### 3.6 Validity windows are scope-relative (P3)

**Statement.** For a document `A` and a query scope set `Q`:

```
valid_from(A)    = A.effective_from ?? A.date
valid_to(A, Q)   = min(A.effective_to,
                       min over effective successors B of A that match Q
                           of (B.effective_from ?? B.date))
invalid(A, Q)    = valid_to(A, Q) is Some and <= valid_from(A)
```

`B` matches `Q` when `B.scope == "global"` or `S(B) ∩ Q ≠ ∅`. With no `--scope`, `Q` is
universal and every successor matches.

An explicit `effective_to` always wins over a derived one: the `min` keeps the smaller, so a
declared end that is earlier than the successor wins, and a declared end that is later is
clamped by the successor. Chain derivation is the fallback, never the source (§6.10 rule 2).

`invalid` is not an error for a read. Such a document is absent from `as-of` queries and
present in `history` and `exhaustive` (§6.10 rule 4).

The window depends on the query scope set, so it is resolved for one query. `Graph::windows`
builds the whole map, and the caller builds it **once**, before the row loop (M5-33). A window
per row would walk the successors of every document on every query.

**A dateless successor.** A successor that has no `effective_from` and no `date` closes no
window. The predecessor is then still `Superseded` (§3.7), but its window stays open, so an
`--as-of` query still returns it. That is the honest answer: the index holds no date to
close the window with. `doctor` does not invent a check for it.

**A tombstoned successor.** A successor that left the corpus still closes its predecessor's
window, at the `valid_from` its `TombstoneMeta` recorded. It matches `Q` through the `scope`
and `shared_with` recorded there, so the scope rule applies to a retired document exactly as
it does to a live one (§6.12 rule 7). Without this, deleting an obsolete document would
resurrect the one it replaced.

**Acceptance cases.** The graph fixture chain is A 2020-01-01, B 2020-07-01, C 2021-01-01,
D 2021-07-01, E 2022-01-01, each `effective_from` explicit and each declaring `supersedes`
of the previous document.

| # | input | expected |
|---|---|---|
| 6.1 | `--as-of 2020-03-01` | only A |
| 6.2 | `--as-of 2020-07-01` | only B (exclusive start of B's window) |
| 6.3 | `--as-of 2025-01-01` | only E |
| 6.4 | A with `effective_to: 2020-04-01` | A's window ends 2020-04-01, not 2020-07-01 |
| 6.5 | `bad/invalid-window.md` (`effective_from > effective_to`) | `invalid: true`, absent from as-of, present in history |
| 6.6 | `bad/one-sided-new.md` moved away and tombstoned (P1) | A's window still closes at the tombstone's `valid_from`; no dangling link; the tombstone still appears in `lineage` and `ids` with its last title |
| 6.7 | global G superseded by acme H | `--scope org:beta` leaves G's window open; `--scope org:acme` closes it |
| 6.8 | default query (D) | H closes G's window everywhere |

### 3.7 `effective_status` and the intent gate (P3)

**Statement.** `effective_status = Superseded` when the effective graph gives the document at
least one successor **that matches `Q`**, else the declared status. No other status is
derived (§6.10 rule 6; the `deprecated` merge is M7).

The intent gate uses `effective_status`. The table in `src/score.rs` does not change; only
its input does. Consequences:

- `current` excludes a document that a link supersedes, even when `status: active`.
- `current` also requires the window to contain today: (`valid_from` absent or
  `valid_from <= today`) and (`valid_to` absent or `today < valid_to`). A document with no
  date has no `valid_from`, so it passes: §6.10 rule 9 keeps an undated document in
  `current`, and `tests/eval/queries.jsonl` asserts exactly that.
- `history`, `evolution` and `exhaustive` apply no window gate and no status gate beyond
  their table row.
- `--include-superseded` on `current` uses the table's `include_superseded` row and cancels
  the window gate. That row admits Active, Draft, Unknown and Superseded, and still excludes
  Deprecated (§7.1). A flag that shows an old document but drops it for being old is
  useless.
- `facts` uses `effective_status == active` and the window gate. A document declared active
  but superseded by a link must vanish from `facts`. This amends §9.2; see §9 correction 11.

**Where the gate lives.** Eligibility, not scoring: `search::Filter` owns the status row, the
time gate and both precomputed maps, and `Filter::evaluate` returns the reason it dropped a
row. The note count is therefore over documents, not over results: a document the gate
excludes is never scored, so "results" would be a claim the reader cannot check. One line
each, only when non-zero:

| line | when |
|---|---|
| `N document(s) hidden by the status gate; …` | `current` dropped a document by status |
| `N document(s) hidden by the validity window; …` | today's window dropped it |
| `N result(s) have no validity window` | `--as-of` alone, and it is the only note an `as-of` query adds |

An `--as-of` query adds no window-hidden line: the window **is** the query, so a document
outside it is not hidden, it is simply not the answer (M5-35).

**Acceptance cases.**

| # | input | expected |
|---|---|---|
| 7.1 | `bad/status-lies.md` (`status: active`, superseded by a link) | `effective_status: "superseded"`; absent from `current`; absent from `facts` |
| 7.2 | `bad/quiet-expiry.md` (`status: superseded`, no successor) | `effective_status: "superseded"`; present in `history`; `lineage` shows the terminal state |
| 7.3 | `bad/expired.md` (`status: active`, `effective_to: 2020-01-01`) | `effective_status: "active"`; absent from `current`; present in `history` |
| 7.4 | `--include-superseded` | all of the above appear under `current` |
| 7.5 | the chain, default query | only E |
| 7.6 | a document with no successor and no `effective_to` | present in `current` |
| 7.7 | `--scope org:beta` on a case 6.7 pair whose G is declared `active` | G is current and H is not returned. The fixture's G declares `superseded`, so it is out for `beta` too: the case needs a pair the status gate can admit (M5-36) |
| 7.8 | `notes/undated.md` under `current` | present: a missing `valid_from` passes the window gate |
| 7.9 | `--include-superseded` on `bad/deprecated.md` | absent: the flag does not admit `deprecated` |

### 3.8 `search --as-of` (P3)

**Statement.** `--as-of` selects the valid-time window and nothing else.

- Inclusion is `valid_from <= T < valid_to`. A document with no `valid_from` is excluded and
  counted in `notes` as `N results have no validity window`.
- Decay is `1.0`; a `--decay-half-life` value is accepted and inert. Both properties come
  from one place: the query scores with the `history` row (no decay, every status at factor
  1.0), and the gate turns status off separately (M5-34). The envelope reports no `intent`
  for an `as-of` query, because `--as-of` is a parameter.
- The status gate is off. `effective_status` is still computed and reported for `T`'s scope
  set, so a reader sees the document's standing at `T` without losing it to that standing.
- The scope filter still applies.
- `--as-of` wins over `--intent` when both are given, as §7.1 says. It does not error, and
  the response carries one note: `as-of DATE overrides intent INTENT`. §9.2's MCP `search`
  accepts both parameters, so a legal call shape must exist.
- Accepted forms are `YYYY-MM-DD`, `YYYY-MM` and `YYYY`. The short forms expand to the first
  day of the period. The expanded value is echoed as `as_of` in `--json`.
- An unparseable value is a usage error (exit 2) that names the value.

**Acceptance cases.**

| # | input | expected |
|---|---|---|
| 8.1 | `--as-of 2020-03-01` on the chain, query `cache` | exactly one result, path A, `valid_to: "2020-07-01"` |
| 8.2 | `--as-of 2025-01-01`, query `chain` | exactly one result, path E, `valid_to: null`. Query `cache` would also return the near-identical pair, which is in the window and matches (M5-36) |
| 8.3 | same query, no flag | E only |
| 8.4 | `--as-of 2021`, query `chain` | expands to `2021-01-01`; C only; JSON `as_of: "2021-01-01"` (M5-36) |
| 8.5 | `--as-of 2020-03-01` with a 2019-dated `draft` document | the draft is returned; `decay: 1.0`; `status: "draft"` |
| 8.6 | `--as-of 2019-01-01` | zero results, one note counting documents with no window |
| 8.7 | `--as-of 2025-13-40` | exit 2, message names the value |
| 8.8 | `--as-of 2020-03-01 --intent history` | one result, A; note names the override; behaviour is as-of's |
| 8.10 | `--as-of 2020-03-01 --intent exhaustive --json` | `intent: "current"` is not reported; the envelope reports `as_of` and the override note |
| 8.9 | `--as-of 2020-02-15 --scope org:acme` on case 6.7 | G returned, `valid_to: "2020-03-01"`; for `--scope org:beta` the same date returns G with `valid_to: null`. 2020-03-01 itself is H's first day and G's last day plus one, so the boundary is exclusive (M5-36) |

### 3.9 `janus lineage` (P4)

**Statement.** `janus lineage <PATH|--topic QUERY>` renders the connected chain that contains
one document, root first, with each node's relation to its predecessor and the terminal
state of every leaf. A root is a document that **replaced nothing**: its `predecessors` list
is empty. It reads the generation and the graph. It never writes and never builds; it uses
the same read policy as `search` (refresh allowed, `--no-rebuild` respected).

Exactly one of `<PATH>` or `--topic` is required. `--topic` runs a `current` search and uses
the top result; zero results is `not_found`.

**Node fields.** `path`, `id`, `title`, `type`, `date`, `declared_status`,
`effective_status`, `valid_from`, `valid_to`, `depth`, `self`, `predecessors` (paths),
`successors` (paths).

`depth` is 0 at the root and increases by one per edge. The array is ordered by
`(depth, path)`. A node that the cycle breaker separates is still listed; the ignored edge
appears in `cycle_breaks`. A node with several successors is followed depth-first.

`terminal` is a separate object: `{state, paths}` where `state` is one of `active`, `draft`,
`deprecated`, `quiet_expired`, `unknown`, `retired`. A `retired` node carries `id`, `path`,
`title`, `date` and `valid_from` from its `TombstoneMeta`, and reports `type: null`,
`declared_status: null`, `effective_status: null` and `valid_to: null`. Those four are null
because a retired document has no current metadata to read; nothing is invented. `retired`
is a `terminal.state` and never an `EffectiveStatus`: the enum stays at five variants (M5-10).

`--json` uses the §9.4 envelope with `results` holding the nodes, `api: 1`, and a `notes`
line for a cycle break.

**Acceptance cases.**

| # | input | expected |
|---|---|---|
| 9.1 | `lineage chain/2022-01-01-cache-returns.md` | 5 nodes, depths 0–4, `self` at depth 4, terminal `active` |
| 9.2 | `lineage chain/2020-01-01-adopt-postgres.md` | same 5 nodes, `self` at depth 0 |
| 9.3 | `lineage bad/quiet-expiry.md` | 1 node, terminal `quiet_expired` |
| 9.4 | `lineage decisions/2024-12-01-redis-cache.md` (main fixture) | 3 nodes: `redis` at depth 0, `postgres` at depth 1, `revisited` at depth 2; one `cycle_breaks` entry naming the ignored edge `(revisited, redis)` |
| 9.5 | `lineage nope.md` | exit 1, message names the path |
| 9.6 | `lineage --topic "cache"` | the top `current` result's chain; `topic` echoed |
| 9.7 | `lineage` with neither argument | exit 2 |
| 9.8 | `lineage` with both | exit 2 |
| 9.9 | a moved, tombstoned target | a `retired` node with the last path |

### 3.10 `janus doctor` (P5)

**Statement.** `doctor` aggregates every problem it can see into one machine-readable report.
It always scans the vault. It reads the generation when one validates, and never builds,
refreshes or writes. It exits 1 when it reports at least one error, and 0 otherwise.

```json
{ "api": 1, "ok": false,
  "summary": { "errors": 2, "warnings": 3 },
  "checks": [
    { "code": "dangling_link", "severity": "error",
      "path": "decisions/2025-03-01-broken-link.md",
      "message": "supersedes target does not resolve: decisions/2025-04-01-never-written.md" }
  ] }
```

`checks` is sorted by `(code, path, target)`. `ok` is `errors == 0`. Human output prints one
line per check and the summary last.

**Codes and severities.**

| code | severity | fires when |
|---|---|---|
| `supersession_cycle` | error | the cycle breaker ignored at least one edge |
| `dangling_link` | error | a link resolves to nothing |
| `cross_corpus_link_unsupported` | error | a link names another corpus |
| `cross_scope_supersession` | error | an edge is inert because its two sides' scopes are disjoint |
| `invalid_validity` | error | `valid_to <= valid_from` |
| `duplicate_identity` | error | two documents share a match key (promoted from a note) |
| `one_sided_supersession` | warning | a link exists on one side only |
| `status_mismatch` | warning | declared status differs from `effective_status` |
| `unknown_type` | warning | the document's type is not in the registry |
| `invalid_scope` | warning | the scope value is malformed and fell back to `global` |
| `yaml_unparseable`, `no_closing_fence`, `oversized_block`, `symlink_escape` | warning | the existing document note fired |
| `index_absent` | warning | `.janus/` holds no validated generation |
| `index_stale` | warning | the corpus digest differs from the manifest's |

Codes are public API (§9.5). Adding one is additive. Renaming or removing one is breaking.

A warning never makes the exit code non-zero. This is what lets M8 run `doctor` in CI on a
vault with one unknown type.

**Acceptance cases.**

| # | input | expected |
|---|---|---|
| 10.1 | main fixture | errors ≥ 2, including `dangling_link` and `supersession_cycle`; exit 1 |
| 10.2 | graph fixture, clean subset (the `bad/` directory removed in a temp copy) | exit 0, `ok: true`, warnings for unknown types only |
| 10.3 | graph fixture as committed | one check per seeded defect, one line each; no duplicates |
| 10.4 | a vault with no `.janus/` | exit 0, one `index_absent` warning, no `.janus/` created |
| 10.5 | only an unknown type is wrong | exit 0, one warning |
| 10.6 | two documents with the same title+date | exit 1, `duplicate_identity` names both paths |
| 10.7 | `--json` twice on the same vault | byte-identical output |
| 10.8 | `status-lies.md` | one `status_mismatch` warning, no error |

### 3.11 Continuation hints (P6)

**Statement.** A new document that probably continues a vanished one gets a display-only
hint. Nothing is applied, nothing is written, and no chain changes.

```
confidence = 0.50 · jaccard(chunk_ids)
           + 0.20 · title_match
           + 0.15 · basename_match
           + 0.15 · tag_jaccard
```

- `chunk_ids` are the content-addressed ids of each document's chunks. `jaccard` is
  `|∩| / |∪|`, and `0.0` when the union is empty.
- `title_match` is `1.0` when the two normalised titles are equal, else `0.0`. Normalisation
  is the identity key's: trim and collapse internal whitespace, case as written.
- `basename_match` is `1.0` when the file stems are equal after `strip_leading_date`, else
  `0.0`.
- `tag_jaccard` uses `tags ∪ shared_with` on both sides.
- Candidates are basis `DocId`s that are tombstoned and unclaimed by P1's steps 1 and 2.
- The best candidate wins. Ties break by `DocId` ascending. A result below `0.50` is not
  stored.
- The hint is recomputed on every build from the basis. It never survives a basis it did not
  see.

`janus moves [--since DATE] [--json]` reports two lists:

- `confirmed`: a path change that P1 resolved, with `id`, `from`, `to`, `first_seen`,
  `last_seen`, and `reason` (the `IdentityTier` that matched, or `unknown` for a tombstone).
- `hints`: a `Hint` with both paths, the four components and the confidence.

`--since` keeps rows whose `last_seen` is on or after the date. Reports only: `moves` never
writes and never builds.

**Acceptance cases.**

| # | input | expected |
|---|---|---|
| 11.1 | move a document and edit its body, title and date intact | `confirmed` row with `reason: "title_date"`; no hint |
| 11.2 | move and change title and date; the new document has 10 chunks and the vanished one had 8, all 8 reappearing | `jaccard = 8/10 = 0.8`; stem equal, tags disjoint, titles differ → `0.40 + 0.00 + 0.15 + 0.00 = 0.55`; one hint |
| 11.3 | both documents have 4 chunks, all 4 shared; titles equal, stems differ, tags disjoint | `jaccard = 1.0` → `0.50 + 0.20 + 0.00 + 0.00 = 0.70`; one hint |
| 11.4 | the new document has 2 chunks and both were already known; titles differ, stem equal | `jaccard = 2/10 = 0.2` → `0.10 + 0.15 = 0.25`, below the floor: no hint |
| 11.5 | `--since` before every `last_seen` | every row |
| 11.6 | `--since` after every `last_seen` | no rows |
| 11.7 | run `moves` twice | byte-identical output; vault file hashes unchanged |
| 11.8 | no vault file has an `id` | hints still appear |

### 3.12 `janus suggest-links` (P7)

**Statement.** `suggest-links` lists semantic candidate pairs that no declared link joins.
It reads the generation and the vectors. It never writes, never applies, and never prompts.

- Candidate pairs are scored by cosine between the best chunk of each document pair. Because
  vectors are normalised, cosine is a dot product.
- A pair is reported when its cosine is `>= --threshold` (default `0.85`) and neither
  direction of the pair has an edge, effective or inert.
- Output is one row per pair, sorted by `(cosine desc, path_a asc, path_b asc)`, with
  `path_a < path_b` in byte order.
- Without vectors: exit 0, `degraded: ["no embedder; no semantic candidates"]`, empty list.
- `--threshold` outside `[0, 1]` is a usage error.
- Cost is `O(n²·d)` per pair set. The command scans the document list once and is not on the
  query path. It is not optimised in v1; §13 does not cover it.

**Acceptance cases.**

| # | input | expected |
|---|---|---|
| 12.1 | two near-identical documents with no link | one candidate row, cosine ≥ 0.85 |
| 12.2 | a pair already linked in either direction | never reported |
| 12.3 | `--threshold 0.99` on case 12.1 | empty list |
| 12.4 | `--threshold 1.5` | exit 2 |
| 12.5 | `--no-embeddings` | exit 0, one `degraded` line, empty list |
| 12.6 | run twice | byte-identical; vault hashes unchanged |

### 3.13 Quarter dates and the undated decay (P8)

**Statement, part 1.** A quarter in a file or folder name is a date: the first day of that
quarter, day precision, on the same rung as the ISO shapes. Accepted forms are `YYYY-qN`,
`YYYYQN`, `YYYY.qN`, `YYYY_qN`, and the reverse `qN-YYYY`, `qN_YYYY`, `qN.YYYY`, `qNYYYY`.
`N` is 1–4. `q5`, `q0`, `2026-q04` and a quarter without a year are not quarters; they join
the existing near-miss count.

Two `DateSource` variants are appended: `FilenameQuarter` and `FolderQuarter`. Appending
keeps the bincode variant index of every existing value, so `format_version` does not move.

**Statement, part 2.** An undated document decays by the median age of the dated documents
in the corpus, instead of by `1.0`.

- The median is computed once per query over every document in the generation that has a
  decay anchor, `effective_from ?? date`: `today - anchor` in days, clamped at 0, sorted,
  middle value or the lower middle for an even count. The anchor, not the authored date, so
  the median measures the same age the decay formula uses (§6.10 rule 8).
- It is filter-independent: `--types` and `--scope` do not change it.
- A corpus with no dated document decays an undated document by `1.0` and adds one `notes`
  line.
- The calculation is a new `DecayBasis { today, undated_age_days: Option<f64> }` passed to
  `score`, so `score` stays pure.

**Acceptance cases.**

| # | input | expected |
|---|---|---|
| 13.1 | `notes/2026-q4-plan.md` | date `2026-10-01`, `date_source: "filename_quarter"` |
| 13.2 | `q2-2026.md`, `2026.q3.md`, `2026_q1.md` | 2026-04-01, 2026-07-01, 2026-01-01 |
| 13.3 | `2026-q5.md` | no date; near-miss count rises by one |
| 13.4 | dated documents aged 10, 20, 30 days | undated decay with half-life 180 = `exp(-ln2·20/180) = 0.9259` (4 dp) |
| 13.5 | dated documents aged 10, 30 days | median is the lower middle: 10 → `exp(-ln2·10/180) = 0.9622` |
| 13.6 | no dated document | decay `1.0`; one note |
| 13.7 | `--types decision` on case 13.4 | the same decay value |

### 3.14 What must not change

| invariant | evidence that it holds |
|---|---|
| No command writes a vault file | every new command compares file hashes and mtimes before and after (cases 11.7, 12.6, 10.4) |
| Scoring order | the §7.2 pipeline order is untouched; `effective_status` replaces the gate input only |
| `chunk_id` never includes `doc_id` | `src/id.rs` unchanged; chunk ids identical after a move (case 1.3) |
| The tie-break chain | `src/score.rs` unchanged; `DocId asc` then `rel_path asc` |
| `--json` stays at `api: 1` | every new field is additive |
| `--no-rebuild` writes nothing | the new commands share `store::load`'s policy |
| The main fixture goldens | the main fixture gains no file; graph cases use a new vault |
| No wall clock below `main.rs` | `today` is a `NaiveDate` argument; `tests/no_wall_clock.rs` stays green |

## 4. Decisions

| # | decision | why |
|---|---|---|
| M5-1 | `path_history`, `identity` and `tombstones` are the only persisted identity facts; the alias map is derived at load | One source of truth for paths. A stored alias map can disagree with the history, and a stored last path can disagree with both. |
| M5-2 | The build takes a **basis** and a **reuse source** as separate arguments | `--rebuild` must re-tokenize without re-identifying. One argument would make it churn every id. |
| M5-3 | A missing basis is a warning, never an error | A cache must not fail a build. Deleted `.janus/` and a corrupt pair are normal. |
| M5-4 | Match-key multiplicity counts every document in the current set, carried or not | Otherwise a new document can derive the id of a carried document with the same key. |
| M5-5 | Edges are resolved and closed at build time; windows and status at query time | Resolution depends on paths and ids, which are build inputs. Windows depend on the query scope set, which is not. |
| M5-6 | One `Edge` per `(from, to)` pair, `from` being the document the declaration replaces, whichever side declared it | Traversal must not care which side declared. Two edges for one relation double-count. The direction is relational, not chronological. |
| M5-7 | A cycle drops the edge whose `(from path, to path)` sorts greatest | Deterministic, readable in output, and hand-derivable in a test. |
| M5-8 | An inert cross-scope edge is stored and reported, never deleted | The declaration is the user's. Reporting keeps it visible; deleting would hide a mistake. |
| M5-9 | `--include-superseded` uses the `include_superseded` row and cancels only the window gate | A flag that shows an old document then drops it for being old is useless. The row still excludes `deprecated`, so the flag is not a second status gate (§7.1). |
| M5-10 | `effective_status` has no `expired` variant; the window is the only expiry signal | §6.10 rule 5 defines expiry as a window property. A sixth status would duplicate it. |
| M5-11 | `facts` switches to `effective_status` in P3 | M3 deferred it with a `todo`; leaving it on declared status would make `facts` disagree with `search`. |
| M5-12 | `doctor` scans and never builds | A diagnostic must not mutate the thing it diagnoses. A stale index is a warning, not a reason to write. |
| M5-13 | `doctor` warnings never set a non-zero exit | M8's CI gate must survive one unknown type. |
| M5-14 | `duplicate_identity` is promoted from note to error | It is the one condition that can make an answer wrong rather than merely sorted oddly. |
| M5-15 | Hints are recomputed from the basis on every build | A hint that outlives its evidence is a guess wearing a fact's clothes. |
| M5-16 | Quarter dates append two `DateSource` variants; no format bump | bincode encodes the variant index, so appending keeps every existing value valid. |
| M5-17 | The undated decay median is corpus-wide and filter-independent | A median that moved with `--types` would make one filter change another document's score. |
| M5-18 | Graph cases live in a new fixture vault | Adding five linked documents to the main fixture moves three goldens and `FIXTURE_DOCUMENTS`. A second vault keeps the M1–M4 goldens as evidence that M5 changed nothing. |
| M5-19 | `format_version` bumps once per phase that changes the layout: P1 → 4, P2 → 5, P6 → 6. P3, P4, P5, P7 and P8 add no field and bump nothing | Each version is a complete, honest layout with every field written. A phase that persists nothing must not force a rebuild. A user who upgrades once pays one rebuild, because only the latest version is compared. |
| M5-20 | A qualified link to another corpus is an error, not a silent miss | M6 owns cross-corpus resolution. A silent miss looks like a typo in the user's file. |
| M5-21 | A derived `DocId` is rejected when steps 1–2 already claimed it, and the resolver continues to the next key whose derived id is unclaimed | A move plus a new file at the old path is one build. Path continuity wins (§6.4), so the new file takes the id and the moved document must not borrow it. Two documents under one id is the worst failure this milestone can produce. Case 1.13 is the guard's own test. |
| M5-22 | A retired document keeps a `TombstoneMeta` row with its title, date, `valid_from`, scope and `shared_with`; its path comes from `path_history` | Without it, a deleted successor cannot close its predecessor's window and the predecessor returns from history. The row is small and rebuildable, and it also gives `lineage` a title. |
| M5-23 | `DocMeta.content_hash` is the authority for a live document; the `MatchKey::ContentHash` in its record is a view refreshed in step 4. For a retired document the record's copy is the only one | The etag cannot move (D18) and the move key must survive the document, so one value has two jobs and one owner per state. |
| M5-24 | The three new tables store text, not `StringId` | `alias_map` must return paths as text, and a pool handle would make every reader of the tables depend on the interned pool. The tables are cache-sized, not chunk-sized, so the duplication is small and it removes one coupling. |
| M5-25 | `tier` is the rung that carried or matched the id; the record's keys are refreshed on every build | §3.1 step 4 refreshes the keys, so a later move matches the title the document has now. A path-stable document keeps its tier (case 1.2); a move reports the matching rung (case 1.3), which is what makes the content-hash rung visible. |
| M5-26 | Case 1.15 tests a removal, not a move | A move tombstones nothing (R-7): the id follows the document to its new path. Only a document that leaves the corpus retires, so only a removal can exercise the cleared flag. |
| M5-27 | Scope qualifies every match key, including rungs 1 and 2 | A scope change is a re-key, not a move (§6.12 rule 5), so the id is not reported as `inherited`, and P6 does not report it as a confirmed move. The declared-id and content-hash id *values* are unaffected: only the candidate set is narrowed. §6.4's "keys 1–2 are not namespaced" describes the encoding, not the match. |
| M5-29 | `DanglingLink.target` stores text, not `StringId` | The value may name nothing in the corpus, so there is no pool handle to give it, and `doctor` prints the user's own words. Same reason as M5-24. |
| M5-30 | `NodeFacts` carries `declared_status` and `effective_to`, and `EffectiveStatus::of` returns `Option` | §3.6 and §3.7 name one call shape, `(facts, graph, scopes)`. Without the declared status the derivation has no else-branch, and without the declared end `Window::of` cannot honour an explicit `effective_to`. A retired endpoint has neither value, so the status rule answers `None` for it, which is what P4 renders (R4-1). |
| M5-31 | The query scope set is `score::ScopeFilter`, and the window and status rules match a successor with `ScopeFilter::intersects`, never `matches` | One type owns both rules, but they are not the same predicate: `matches` also applies `--scope-only`, which narrows the *result* set (§6.12 rule 4). A successor that is global still supersedes what it replaced, even when the caller asked to see owned scopes only (§6.12 rule 7). `intersects` is the successor rule, and `matches` delegates to it when `--scope-only` is off. |
| M5-32 | `rank_documents` drops a chunk whose scored `status_factor` is zero, not a chunk whose declared status the row excludes | The effective status is a per-query value that only the `ScoreInput` knows, and the breakdown is the only channel that carries the scored row. The declared status on `ScoreDoc` stays what it is: declared metadata, reported as `status`. |
| M5-33 | The window map is built once per query by `Graph::windows`, and the gate lives in `search::Filter`; `ScoreInput` carries no `window_member` | §5 P3 named both `effective_status` and `window_member` on the score input, and §3.7 moves the gate into eligibility: the same field cannot be a gate the caller applies and an input the scorer reads. The map is built before the row loop, and a thread-local build counter pins the count at one (P3 scope 10). |
| M5-34 | `--as-of` scores with the `history` row and reports no `intent` | Decay `1.0` and "no status gate" are exactly that row on both axes, so one row supplies both and `score` needs no special case. `--as-of` is a parameter and not an intent, so the envelope reports `as_of` and leaves `intent` out; the override note names the intent it replaced. |
| M5-35 | The hidden-count notes count documents, and an `--as-of` query emits only the `no_validity` count | The gate runs before scoring, so "results" would claim a relevance the code never computed. For `--as-of` the window is the query: an out-of-window document is not hidden by a gate, it is not the answer, and a second line would only repeat that. |
| M5-36 | Three acceptance cases were recomputed against the committed fixtures: 7.7, 8.2 and 8.4 (query), and 8.9 (date) | 7.7 asked for a `superseded` document to be `current`; 8.2 and 8.4 used `cache`, which the near-identical pair also matches; 8.9 used 2020-03-01, which is the exclusive end of G's window. The rule under test is unchanged; only the input that can show it moved. |
| M5-28 | `Graph::build` owns its endpoint facts, and `Graph::for_snapshot(snapshot, config)` is the only caller | Two callers need one graph: the `index --json` counts and every read. A borrowed `DocRow` would force each caller to hold rows of its own, and a snapshot-only builder is what keeps the counts and the answers equal. The disjoint flag is a rule parameter, so it is an argument, not a second function. |

## 5. Phases

Each phase is one branch from `main`, one PR, and one reviewable unit. Each phase name ends
with its split seam: the place to split **before** the ceiling, not after.

### Phase 1 — Identity continuity and the path history

**Files.** `src/index/mod.rs`, `src/index/build.rs`, `src/index/store.rs`,
`src/index/cmd.rs`, `src/index/refresh.rs`, `src/vault/identity.rs`, `src/scan.rs`,
`src/cli.rs`, `src/main.rs`, `src/search/mod.rs` (for `ids`), `src/lib.rs`, new
`src/ids.rs`.

**Build.** `Snapshot` gains `path_history`, `identity` and `tombstones`; `FORMAT_VERSION =
4`. `identity::derive_identity` becomes `identity::resolve(documents, basis)`, keeping the
stateless path as the no-basis branch. `IdentityTier::ContentHash` becomes reachable.
`build::plan_forced` gains a `today: NaiveDate` and a basis argument separate from
`previous`.

The basis threads through the **real** write path, which is not `cmd`:

```
cmd::run → store::build_until_stable → refresh::plan_from_walk → build::plan_forced
```

`plan_from_walk` currently sets `previous = None` under `--rebuild` (`src/index/refresh.rs:220`),
so a single argument would drop the basis with the reuse source. `plan_from_walk`,
`build_until_stable` and `forced_documents` each gain a `basis: Option<&Snapshot>`, loaded
once by the caller through a new `store::previous_snapshot(dir)` that reads and decodes
`index.bin` and never reads `vectors.f32` (`previous_generation` hashes the vectors file;
a basis does not need them). `previous_generation` already preserves the snapshot when the
vectors are unusable (`src/index/store.rs:1464`), so a corrupt `vectors.f32` cannot cost the
basis.

`forced_documents` resolves identity with the same basis, so a targeted `janus index PATH`
forces the ids the build will publish. `cmd`'s dry-run branch uses the same resolution.

**Tests.** Unit: the ladder with a basis (cases 1.1–1.12), tombstone creation, alias map
derivation, `--rebuild` stability, the claimed-id guard (1.13), two consecutive moves (1.14),
re-entry (1.15). Integration: `tests/identity_moves.rs` with a real temp vault, using
`janus ids --json`. A test that proves `--rebuild` keeps ids must fail on `main` first.

**Handoff.** P2 uses `alias_map` for link resolution. P3 uses `TombstoneMeta::valid_from` to
close a window at a retired successor. P6 uses `path_history` and `identity`.

**Accept.** Cases 1.1–1.15, plus `cargo test --all-targets` green, no golden moved.

**Split seam.** As delivered, the seam sits between the **resolver** and everything that
calls it: the writer plumbing alone measured 880 logic lines, over the 600 ceiling, because
`src/vault/identity.rs` is a rewrite rather than an addition. P1 shipped as two stacked
PRs — the tables and `identity::resolve` with its unit tests (PR 1), then the basis
threading, the build, the readers and `janus ids` (PR 2). Both are measured against their
own base. The rejected seam — writer versus readers — put 880 lines in one PR.

### Phase 2 — Edges in the generation

**Files.** `src/index/mod.rs`, `src/index/build.rs`, `src/index/store.rs`,
`src/index/cmd.rs`, `src/link.rs` (new), `src/vault/identity.rs` (`alias_map`),
`src/graph.rs` (new), `src/config.rs`, `src/lib.rs`, `janus.toml`,
`tests/fixtures/graph-vault/**`, `tests/common`, `tests/edges.rs`, `tests/graph.rs`.

**Build.** `Snapshot` gains `edges` and `dangling_links`; `DocMeta` gains `effective_to`;
`FORMAT_VERSION = 5`. A new `link::resolve` walks declared values through the alias map,
current paths and declared ids, and emits edges plus dangling rows. Symmetric closure and
de-duplication at build time. Cycle breaking and cross-scope inertness in `Graph::build`.
New `[links] allow_disjoint_supersession` config key, default `false`, unknown-key tolerant.
`janus index --json` gains `edges`, `dangling_links`, `cycle_breaks`, `inert_links`.

**Tests.** Unit: resolution order (cases 2.1–2.6), closure (3.1–3.3), cycle rule (4.1, 4.3,
4.4), scope intersection (5.1–5.5). Integration: the graph fixture builds; the count report
matches; a link follows a move.

**The graph fixture.** A new committed vault at `tests/fixtures/graph-vault/`, with its own
`janus.toml` (`corpus_id = "graph"`, `[embedding] model = "hash-64"`) and no date later than
2022, so no assertion depends on today.

| path | role |
|---|---|
| `chain/2020-01-01-adopt-postgres.md` | A, `effective_from` 2020-01-01 |
| `chain/2020-07-01-move-to-redis.md` | B, supersedes A |
| `chain/2021-01-01-cache-in-front.md` | C, supersedes B |
| `chain/2021-07-01-drop-cache.md` | D, supersedes C |
| `chain/2022-01-01-cache-returns.md` | E, supersedes D; the 5-link chain |
| `scopes/global-claim.md` | `scope` absent, replaced by `acme-claim.md` |
| `scopes/acme-claim.md` | `scope: org:acme`, supersedes `global-claim.md` |
| `scopes/acme-vs-beta.md` | `scope: org:acme`, supersedes a `org:beta` document |
| `scopes/beta-claim.md` | `scope: org:beta` |
| `scopes/shared-old.md` | `org:acme`, `shared_with: [shared:cache]` |
| `scopes/shared-new.md` | `org:beta`, `shared_with: [shared:cache]`, supersedes `shared-old.md` |
| `bad/invalid-window.md` | `effective_from` after `effective_to` |
| `bad/one-sided.md` | declares `superseded_by`; the other side declares nothing |
| `bad/one-sided-new.md` | the target of `one-sided.md`, no declaration of its own |
| `bad/quiet-expiry.md` | `status: superseded`, no successor |
| `bad/status-lies.md` | `status: active`, `type: decision`, superseded by a link |
| `bad/expired.md` | `status: active`, `type: decision`, `effective_to` 2020-01-01 |
| `bad/deprecated.md` | `status: deprecated`, `type: decision` |
| `bad/dangling.md` | `supersedes` a path that does not exist |
| `similar/alpha.md`, `similar/beta.md` | near-identical bodies, no link; the `suggest-links` pair |

The fixture's documents carry explicit `type` and `status` so that a `doctor` warning count
means what the test says it means.

**Handoff.** P3 reads `Graph` and `effective_to`. P5 reports `dangling_links` and `ignored`.
P7 excludes existing pairs.

**Accept.** Cases 2.1–2.6, 3.1–3.3, 4.1–4.4, 5.1–5.5. Main-fixture goldens unchanged
except `janus index --json`, which gains fields.

**Split seam.** Resolution and closure (build side) versus `Graph` (query side).

### Phase 3 — Windows, effective status, and `--as-of`

**Files.** `src/graph.rs`, `src/score.rs`, `src/search/mod.rs`, `src/search/facts.rs`,
`src/cli.rs`, `src/main.rs`, `tests/cli_search.rs`.

**Build.** `Window::of(facts, graph, scopes)` and `EffectiveStatus::of(facts, graph, scopes)`.
`ScoreInput` gains `effective_status`; `IntentPolicy` is consulted with it. `search::Filter`
gains the status row, the time gate and both per-query maps, because the gate moves from a
per-document property to a per-query one: each map is computed once per query, never once per
row. `--as-of` is parsed in the adapter, expanded and echoed; it wins over `--intent` and says
so in one note. `facts` switches to `effective_status` and the window gate. No
`format_version` change: P3 persists nothing that P2 did not.

The phase split at its named seam: the derivation shipped first, the gate and the flag
second. `ScoreInput` does **not** gain `window_member`: the window gate lives in `Filter`, so
an unread flag would be a lie (M5-33).

**Tests.** Table-driven unit tests for window derivation (6.1–6.8) and status derivation
(7.1–7.9). CLI tests for the flag (8.1–8.10). One test proves the superseded-by-link case is
absent from `current` and fails on `main` first. Three search goldens move; each diff is
reviewed field by field and the review says which field moved.

**Handoff.** P4 renders windows and status and owns the end-to-end dream-cycle test. P5
reports `invalid_validity` and `status_mismatch`.

**Accept.** Cases 6.1–6.8, 7.1–7.9, 8.1–8.10, and the eval set keeps its active top-1 rate,
including the undated document at `tests/eval/queries.jsonl:11`.

**Split seam.** Window and status derivation (pure) versus the CLI flag and DTO plumbing.

### Phase 4 — `janus lineage`

**Files.** `src/graph.rs` (traversal), new `src/lineage.rs`, `src/cli.rs`, `src/main.rs`,
`src/lib.rs`, `tests/cli_lineage.rs`, `tests/dream_cycle.rs`.

**Build.** Traversal with an explicit stack, never recursion: depth-first, ordered by
`(depth, path)`. Root discovery walks predecessors. Terminals computed per leaf. `--topic`
runs a `current` search. Tombstoned nodes render from `TombstoneMeta`: `retired`, with the
last title and `valid_from`.

**Tests.** Cases 9.1–9.9 on the graph fixture and the main fixture's cycle.

**Dream cycle.** `tests/dream_cycle.rs` owns the milestone's headline test, because it is
the first phase that can assert a chain end to end. One test, three steps, one temp vault:

1. Move a linked document unchanged; assert the chain still renders with the same ids.
2. Move and edit the body of a linked document that has no `id` anywhere; assert the same.
3. Rewrite a third body in place; assert the same.

Before every step it records `(path, content_hash, mtime)` for every file in the vault, and
after every step it asserts the same list. The test runs the commands with no stdin and no
prompt. This is DoD rows 4–8.

**Handoff.** M6's `get_lineage` reshapes this DTO; the shapes are already additive.

**Accept.** Cases 9.1–9.9 and the dream-cycle test.

**Split seam.** Traversal and shapes versus rendering and `--topic`.

### Phase 5 — `janus doctor`

**Files.** new `src/doctor.rs`, `src/cli.rs`, `src/main.rs`, `src/lib.rs`,
`tests/cli_doctor.rs`.

**Build.** Aggregate document notes, graph findings and index staleness into one report.
Stable order. Exit 1 when `errors > 0`. `index_stale` uses the existing corpus-digest walk.
`index_absent` is a warning. No build, no write, no lock.

**Tests.** Cases 10.1–10.8. One test proves a single unknown type exits 0.

**Handoff.** M8 adds `doctor` to CI and may add `--fix` for cache repairs only.

**Accept.** Cases 10.1–10.8.

**Split seam.** Scan-level checks (notes, duplicate identity) versus graph-level checks
(links, cycles, windows).

### Phase 6 — `janus moves` and continuation hints

**Files.** new `src/moves.rs`, `src/index/build.rs` (hint computation), `src/index/mod.rs`
(`Hint`, `FORMAT_VERSION = 6`), `src/graph.rs` (jaccard helper), `src/cli.rs`, `src/main.rs`,
`tests/cli_moves.rs`.

**Build.** Hint computation from the basis and the new snapshot, using the §6.9 weights.
Confirmed moves derived by pairing `path_history` entries with the current `id_source`.
`--since` filter. Reports only.

**Tests.** Cases 11.1–11.8, with every confidence hand-derived in the test comment.

**Handoff.** M8's freshness report may show hints. M7's explicit `id` write is the way to
assert continuity instead of guessing.

**Accept.** Cases 11.1–11.8.

**Split seam.** Hint computation versus the `moves` command.

### Phase 7 — `janus suggest-links`

**Files.** new `src/links.rs`, `src/cli.rs`, `src/main.rs`, `tests/cli_suggest_links.rs`.

**Build.** Pairwise cosine over per-document best chunks, excluding existing pairs, sorted.
Degraded path without vectors.

**Tests.** Cases 12.1–12.6. The similarity fixture must produce a hand-derivable cosine; use
`HashEmbedder` through `[embedding] model = "hash-64"`, not a real model.

**Handoff.** M6 may expose it as a read tool. Nothing writes.

**Accept.** Cases 12.1–12.6.

**Split seam.** Pair scoring versus the command.

### Phase 8 — Quarter dates and the undated decay

**Files.** `src/vault/resolve.rs`, `src/document.rs` (`DateSource` variants), `src/score.rs`,
`src/search/mod.rs`, `src/search/facts.rs`, `tests/date_ladder.rs`.

**Build.** Quarter parsing on the same separator set, whole-prefix and year-range rules.
Two appended `DateSource` variants. `DecayBasis` threaded through `score` and both commands.

**Tests.** Cases 13.1–13.7, plus one test that `2026-q04` and `2026-q5` stay near-misses.
Golden search snapshots move; the diff is reviewed field by field.

**Handoff.** None. This phase is independent and may be dropped without affecting M5's
done-when.

**Accept.** Cases 13.1–13.7.

**Split seam.** Quarter parsing versus the decay basis.

## 6. Cross-cutting requirements

- **Ste writing.** Every comment, doc string and user-facing message uses short sentences,
  active voice and plain words.
- **No `unwrap`/`expect` in library code.** A resolution failure is a `JanusError` or a
  documented default, never a panic.
- **Determinism.** Every map that reaches output is a `BTreeMap`. Every list is sorted. Two
  runs on one vault are byte-identical, except the elapsed-time field (`taken_ms`).
- **Injected time.** `today` and `--as-of` arrive as `NaiveDate` arguments. No module below
  `main.rs` reads the wall clock.
- **Read policy.** Every new read command inherits `store::load`'s policy: refresh when the
  corpus changed, never wait for a writer, refuse a cold mismatch, honour `--no-rebuild`.
- **Fixture hygiene.** The graph vault is committed, small, and covers every seeded defect
  exactly once.
- **Hygiene in artifacts.** No tool names, harness paths, thread ids or model names in code,
  tests, commit messages or PR bodies.

## 7. Definition of done

Each line is checkable, with the command that proves it.

| # | condition | proof |
|---|---|---|
| 1 | A 5-link chain renders, root first, with a terminal state | `cargo test --test cli_lineage` case 9.1 |
| 2 | `--as-of` returns the old value for a past date and the new one today | cases 8.1 and 8.3 |
| 3 | The seeded cycle is detected and broken deterministically | case 9.4; `janus index --json` reports `cycle_breaks: 1` |
| 4 | Dream cycle (a) moves a linked document unchanged: no chain breaks | `cargo test --test dream_cycle`, step 1 |
| 5 | Dream cycle (b) moves and edits a linked document with no `id`: no chain breaks | `cargo test --test dream_cycle`, step 2 |
| 6 | Dream cycle (c) rewrites a third body: no chain breaks | `cargo test --test dream_cycle`, step 3 |
| 7 | No dream-cycle step writes a vault file | the same test compares every `(path, content_hash, mtime)` before and after each step |
| 8 | No step asks for confirmation | the same test runs the commands with no stdin and asserts exit 0 |
| 9 | A superseded document never appears in `current` | case 7.1 |
| 10 | `doctor` exits 1 on an error and 0 on warnings | cases 10.5 and 10.1 |
| 11 | Two runs of every new command are byte-identical | cases 10.7, 11.7, 12.6 |
| 12 | No golden moved except where a phase declares it | `git diff --stat tests/snapshots` reviewed per phase |
| 13 | All gates pass | `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets`, `cargo doc --no-deps`, the Windows cross-check |
| 14 | No new dependency | `git diff Cargo.toml` is empty |

## 8. PR plan and size budget

| phase | logic ceiling | estimate | test ceiling | estimate | base |
|---|---|---|---|---|---|
| P1 identity + paths | 600 | ~550 | 900 | ~650 | `main` |
| P2 edges | 550 | ~450 | 800 | ~500 | `main` |
| P3 windows + as-of | 500 | ~400 | 750 | ~450 | `main` |
| P4 lineage | 350 | ~280 | 600 | ~300 | `main` |
| P5 doctor | 500 | ~430 | 700 | ~400 | `main` |
| P6 moves + hints | 450 | ~350 | 650 | ~300 | `main` |
| P7 suggest-links | 300 | ~200 | 450 | ~180 | `main` |
| P8 quarter + undated decay | 300 | ~220 | 450 | ~220 | `main` |

Every phase is dispatched from `main` after the previous phase merged. No phase stacks on
another: each is measured against `main`.

If a phase exceeds its ceiling, split at its named seam and re-measure. Never shave a
comment or drop a test to fit.

## 9. SPEC corrections

These amendments ship in the phase that owns them. Each is in the same PR as the code.

1. **§11 M5 done-when.** "from the fixture vault" becomes "from the graph fixture vault".
   The committed main fixture holds a 3-document cycle, not a 5-link chain. (P2 adds the
   vault; P4 asserts the chain.)
2. **§6.10 rule 5.** Add one sentence: `effective_status` has no `expired` variant. A
   document past its `effective_to` keeps its declared status and is outside its window;
   the window is the only expiry signal. (P3.)
3. **§6.7 validity windows.** Replace "earliest successor's `effective_from`" with "earliest
   successor's `effective_from ?? date`", and state that a successor with no date cannot
   close a window. (P3.)
4. **§7.1.** Add: `--as-of` wins over `--intent`, and the response carries one note naming
   the override. Add: `--include-superseded` on `current` uses the `include_superseded` row
   and cancels the validity window; every other intent ignores the flag. (P3.)
5. **§9.1.** `janus doctor` exits 1 when it reports an error and 0 otherwise. A warning
   never changes the exit code. Add the code list as the contract. (P5.)
6. **§6.9 hint formula.** Define `title_match` (equal normalised titles), `basename_match`
   (equal stems after `strip_leading_date`) and `tag_jaccard` (`tags ∪ shared_with`), and
   name the candidate set. (P6.)
7. **§8.3 `Index` sketch.** Replace the sketch with the real split: `Manifest` in
   `manifest.json`, `Snapshot` in `index.bin`. The sketch names fields no type has
   (`built_at: DateTime<Utc>`, `vault_root`), and a reader will trust it. (P1.)
8. **§6.4.** State that the previous generation is the identity cache, that a missing or
   corrupt generation re-derives ids with a warning, and that `--rebuild` keeps the cache.
   (P1.)
9. **§6.9.** State that a link resolves through the alias map before a declared id, and that
   a corpus qualifier naming another corpus is an error until M6. (P2.)
10. **§6.12.** Add the `[links] allow_disjoint_supersession` key to the corpus config table
    in §16.1. (P2.)
11. **§9.2 `authoritative_facts`.** Replace "declared status `active`" with "effective
    status `active`, inside its validity window". The M3 plan already deferred this switch
    to M5, and declared status alone would let a superseded decision return as an
    authoritative fact. (P3.)
12. **§6.3 and §6.10 rule 9.** Replace "its decay factor is `1.0`" with "its decay factor is
    the corpus median, and `1.0` when the corpus has no dated document". The undated
    document keeps its place in `current`; only its decay changes. (P8.)
13. **§6.9 and §6.4 tombstones.** State that a retired document keeps a `TombstoneMeta` row
    with its title, date, `valid_from`, scope and `shared_with`, that its path stays in
    `path_history`, and that the row still closes its predecessor's window and still renders
    in `lineage`. (P1.)
14. **§6.4 namespacing.** Add that the `corpus_id ‖ scope` namespace enters the *encoding*
    of rungs 3–5, and that scope still qualifies the candidate set for every rung, rungs 1
    and 2 included. A scope change is a re-key, not a move (M5-27). (P1.)

## 10. Handoff to M6

- `lineage`, `doctor`, `ids`, `moves` and `suggest-links` are core functions with DTOs in the
  library. M6 wraps them; it does not reimplement them (§9.5).
- `get_lineage` returns the P4 shape. `get_authoritative_facts` uses the P3 status and window
  rules, so `facts` and the MCP tool share one predicate.
- Cross-corpus links, `--corpus` and workspaces remain M6's. `DanglingReason::OtherCorpus`
  is the seam: M6 resolves it instead of reporting it.
- `doctor` gains `--json` consumers and may gain `--fix` for cache repairs only. It never
  repairs a `.md` file (§9.3).

## 11. Implementation logs

Each phase appends a log here when it merges: what deviated, why, and the size numbers.

### Phase 1 — identity continuity and the path history

Delivered in two stacked changes at the resolver seam: the three tables and
`identity::resolve` with its unit tests, then the basis threading, the build, the
readers and `janus ids`. Both are gated and within budget.

- **The seam moved.** The writer as one unit measured 880 logic lines. The 600
  ceiling cannot hold a rewrite of `src/vault/identity.rs` plus its plumbing, so
  the split is resolver versus callers. See §5.
- **`format_version` 4.** The snapshot carries the three tables, so an older
  index is rebuilt once with the usual warning.
- **Deviations.** M5-24 (text, not `StringId`), M5-25 (`tier` and the refreshed
  keys), M5-26 (case 1.15 is a removal), M5-27 (scope qualifies every match key).
  `alias_map` moves to P2, its first reader.
- **A new reader entry point.** `store::validated_snapshot` loads the published
  generation under `Policy::Refuse`, so `scan`, `chunks` and `ids` resolve
  against it without a lock, a build or a write.
- **Evidence.** The main fixture goldens did not move. `--rebuild` after an
  in-place edit keeps the id; a byte-identical move reports `content_hash` and
  keeps the old path in `previous_path`.

### Phase 2 — edges in the generation

Delivered in two stacked changes at the split seam of §5: this one resolves
links and publishes the edges, and the next adds the query-time graph. Each is
measured against `main` and each is inside its ceiling.

- **The seam was used.** The phase as one change measured 733 logic lines and
  847 test lines — over both ceilings — so it was split into build side and
  query side exactly as §5 names. The estimate in §8 (~450/~500) was too low
  for a fixture with this many cases.
- **`format_version` is 5** in this change: the snapshot carries `edges` and
  `dangling_links`, and `DocMeta` carries `effective_to`.
- **Deviations.** M5-29 (a dangling target is text). `src/search/mod.rs` needed
  no change: the graph reads the snapshot's own rows, so `DocRow` gained no
  field.
- **The fixture is committed.** `tests/fixtures/graph-vault/` holds the 5-link
  chain, three scope pairs, eight `bad/` documents and the near-identical pair
  for P7. Its own config sets `corpus_id = "graph"` and `hash-64`, so a build
  needs no model file. No date is later than 2022.
- **Evidence.** On the main fixture the build publishes 3 edges and 1 dangling
  row with `NotFound`. On the graph fixture it publishes 9 and 1. A move and a
  deletion each keep the edge count, and a second build publishes the same
  tables. The M1–M4 goldens did not move; the only test values that changed are
  the two `format_version` literals, which the bump requires.
- **Not included.** The graph, the cycle break, the scope rule and the four
  `--json` counts are the follow-up change.

**Part 2 — the graph at query time.** Measured against part 1, inside both
ceilings.

- **One builder.** `Graph::for_snapshot` is the only caller of `Graph::build`,
  and the receipt's four counts come from its result (M5-28). The graph owns
  its endpoint facts, so `src/search/mod.rs` needed no change: `DocRow` gained
  no link field.
- **The two rules bite.** With the cycle breaker disabled, case 4.1 fails with
  an empty ignored list. With the scope rule disabled, case 5.1 fails with the
  disjoint pair listed as a relation.
- **Evidence.** The main fixture reports 3 edges, 1 dangling with `NotFound`, 1
  cycle break and 0 inert links. The graph fixture reports 9, 1, 0 and 1. The
  flag makes the disjoint pair effective and leaves nothing inert or ignored.
  Two runs of the receipt are identical except `taken_ms`. No golden moved.

### Phase 3 — windows, effective status, and `--as-of`

Delivered in two stacked changes at the split seam of §5: the pure
derivation, then the eligibility gate and the flag.

- **The seam was used.** The phase as one change measured 722 logic and 854
  test lines. Both half-changes are inside the 500/750 budget of §8.
- **No `format_version` change.** P3 persists nothing new (M5-19, R-11).
- **`NodeFacts` grew two fields.** `Window::of` and `EffectiveStatus::of` must
  read the declared end and the declared status from the one endpoint, so the
  struct sketch of §2.3 was amended (M5-30). A retired endpoint carries
  neither, and `EffectiveStatus::of` answers `None` for it, which is what P4
  renders (R4-1).
- **`ScoreInput` dropped `window_member`.** The gate lives in `Filter` (M5-33).
- **Deviations.** M5-31, M5-32, M5-33, M5-34, M5-35, M5-36. SPEC §6.7 and
  §6.10 rule 5 ship with part 1; §7.1 and §9.2 ship with part 2.
- **Evidence.** On the graph fixture, `--as-of 2020-03-01` returns the first
  link and `--as-of 2025-01-01` the last; `bad/status-lies.md` reports
  `effective_status: superseded` and is absent from `current` and `facts`;
  `bad/expired.md` is active and outside its window; the eval set keeps
  `active top-1 = 1.00 (22/22)` including `notes/undated.md`.
- **Three search goldens moved** and nothing else: `effective_status`,
  `valid_from` and `valid_to` are the only added keys. The main fixture's
  seeded cycle points at an older successor, so `redis` reports
  `valid_to < valid_from`; `history` applies no window gate, so no result
  moved.
- **Not included.** `lineage`, `doctor`, `moves`, `suggest-links`, quarter
  dates, the undated decay median, and any write to a vault file.

### Phase 4 — `janus lineage`

_(empty)_

### Phase 5 — `janus doctor`

_(empty)_

### Phase 6 — `janus moves` and continuation hints

_(empty)_

### Phase 7 — `janus suggest-links`

_(empty)_

### Phase 8 — quarter dates and the undated decay

_(empty)_
