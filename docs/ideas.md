# Ideas list

Parked ideas. **Not decisions, not plans, not commitments.** An entry here means
"someone had this thought and it is worth reviewing later", not "this will be built".

Rules of the list:

- **Settled decisions never live here.** They go in the phase log of the relevant
  `docs/<milestone>-breakdown.md` (with an ID and a one-line *why*), or in `SPEC.md`
  §15 for project-level ones (AGENTS.md).
- **An entry is deleted** when it is promoted (into a milestone plan or a SPEC
  decision) or when it is rejected. A permanent list is a list nobody reads.
- Each entry states the idea, why it is parked, the open questions that would have
  to be answered, and where it would land if adopted.

---

## I-1 — Per-client profiles: limit what each agent can see

**Status:** parked (2026-11-02). Review before M6 (MCP server).

### Idea

Let a janus instance be started with a named profile that pins what that client may
reach, so different agents can share one vault with different visibility:

```
janus mcp --profile hermes        # or: JANUS_PROFILE=hermes janus mcp
```

```toml
[profiles.hermes]
allowed_scopes   = ["org:acme", "shared:finance"]
allowed_corpora  = ["primary"]
read_only        = true            # no write tools, regardless of --allow-write
max_chars        = 4000
default_intent   = "current"
```

Profiles turn a *default* into a *limit*: `--scope` narrows a query that the caller
nevertheless can re-issue unscoped, while a profile **refuses** an out-of-profile
scope in the response. That difference is the whole point, so a profile must be
enforced server-side on every request, not applied once at startup as a default.

### Why parked

It is not needed to make `janus search` good, and it is not in `SPEC.md` — adding it
to the spec before reviewing the shape would create an API to maintain. M6 is the
first milestone with a long-running server and therefore the first place a profile
can actually be enforced.

### Open questions

1. **Enforcement point.** Refuse at the adapter (MCP/RPC returns an error) or in the
   core (a `ScopeSet` carried by every request)? Core, probably: §9.5 says adapters
   must not hold decisions, and `janus rpc` needs the same guarantee.
2. **Is `read_only` a profile field or a separate flag?** `--allow-write` already
   exists (D4). Two switches that can contradict each other is a bug generator;
   probably `read_only = true` wins and `--allow-write` is ignored with a warning.
3. **Path-level limits?** `allowed_paths`/`excluded_paths` globs would let one client
   see `decisions/` but not `journals/`. Cheap to add, easy to get wrong (a rename
   moves a document out of the profile's view).
4. **Where do profiles live?** `janus.toml` (per corpus) vs. a workspace file
   (§6.11 — "one setting, one home", D47). A profile names corpora, so the workspace
   file is the likelier home.
5. **Audit.** Should every request log which profile served it? Cheap, and it turns
   "why couldn't the agent see it" into a one-line answer.
6. **What this is *not*.** It is not an ACL. An agent with shell access to the vault
   reads the files directly; profiles only constrain the janus path. Real isolation is
   separate OS users, separate vault copies, or a restricted service account. This
   must be said in whatever documentation ships, or someone will trust it.

### Where it would land

`SPEC.md` §9 (a per-client section next to §9.3's write gate), one §15 decision row,
and M6's breakdown. Estimated cost is small (~150 logic lines plus tests) if the
enforcement lives in the core `ScopeSet` that M3's scope filter already needs.

---

## I-2 — `doctor` as a machine-readable report, not prose

**Status:** parked (2026-11-01). Review at M5 (when there is something to diagnose)
and M8 (when it runs in CI).

### Idea

`janus doctor` exists as one line in §9.1 (“index health, broken links, duplicates”)
and M5 names two checks (`cross_scope_supersession`, `invalid_validity`), but nothing
defines its *output*: no code list, no severity, no statement of what makes it exit
non-zero. That is the part that matters, because M8 wants `doctor` in CI and an agent
needs to act on a result without parsing English.

The vocabulary should not be invented — `DocNote` already emits stable machine codes
(`DocNote::code()`, `src/document.rs`): `unknown_type`, `unknown_status`,
`invalid_scope`, `yaml_unparseable`, `no_closing_fence`, `duplicate_identity`,
`oversized_block`, `symlink_escape`, … Doctor should **aggregate** those plus the
checks only it can do (link existence and symmetry, cycles, cross-scope supersession,
invalid validity windows, unindexed files, index entries whose document is gone) into
one report:

```json
{ "ok": true,
  "checks": [
    { "code": "one_sided_supersession", "severity": "warning", "path": "…", "message": "…" },
    { "code": "supersession_cycle", "severity": "error", "paths": ["…", "…"] }
  ],
  "summary": { "errors": 0, "warnings": 3 } }
```

Two constraints worth writing down before the checks are built:

- **Codes are the contract.** Adding one is additive; renaming or removing one breaks
  `--json` consumers and CI gates (§6.8, §9.5).
- **Warnings must not make `doctor` exit non-zero**, or the M8 CI gate is unusable on
  a vault with one unknown type. Errors (a cycle, a broken declared link) may. §9.1's
  existing exit codes cover it; no new code is needed.

And it must **not** grow a second `.gitignore` check. `init` already reports that
(`config.rs::warn_if_not_ignored`, §8.6 rule 3); if doctor wants it, it calls the same
predicate instead of owning a copy.

### Low priority: `--fix`, cache-only

Doctor diagnoses; a `--fix` would repair. The only repairs that may ever be automatic
are cache repairs, because the index is rebuildable by definition (§5 rule 1): rebuild
`index.bin`, drop index entries whose document is gone, remove a truncated temp file.
**Never a `.md` file.** A frontmatter “fixer” is a bulk edit of the user's notes and
belongs in M7's explicit, etagged, per-document writes — or nowhere.

If built: `--fix --dry-run` prints the report plus what it would do; `--fix` applies
cache operations only and re-runs the checks; the exit code reflects the post-fix
state. Priority is **low**: `search` already auto-rebuilds on config, format, root and
fingerprint staleness and says why on stderr (M3 S-8), so the remaining failure modes
are checksum corruption and orphaned sidecar files — rare, and `janus index --rebuild`
already covers the first. Review at M8, and only if one is actually observed.

Its own questions:

1. **What does `--fix` do that `janus index --rebuild` does not?** If the answer is
   “nothing”, drop it rather than build it.
2. **Delete orphan files or quarantine them** under `.janus/quarantine/`? Deleting
   derived data is safe in principle; quarantine makes a bug in the check visible
   instead of destructive.
3. **Does a failed repair leave the old index intact?** It must (§8.3 atomic replace).

### Why parked

M5 is the first milestone with an index and links to diagnose, so half the checks have
nothing to run against yet, and M8 is where the CI gate lands. Recording the constraint
now is cheap; specifying the check list now would be guessing.

### Open questions

1. **Severity mapping.** Unknown type is weighted low but tolerated (§6.6.1); an
   unparseable frontmatter block is fail-open on read. Are both warnings? Which
   checks, if any, are ever `error`?
2. **Does doctor require a built index**, or degrade to scan-only checks when
   `.janus/` is missing? Degrading makes "run doctor first" always valid advice.
3. **Overlap with `janus_status`** (§9.2). Status is freshness and counts; doctor is
   problems. Keep them separate rather than making status a one-line doctor.
4. **`--json` shape.** Reuse the §9.4 envelope, or a flat `checks` array? A doctor
   result is not a ranked list, so `intent`/`results` fits badly.
5. **Duplicate identity.** Is the per-document note enough, or does the vault-level
   view need its own section (it is the one note that can make an answer wrong rather
   than merely sorted oddly)?

### Where it would land

`SPEC.md` §9.1 (output shape and the code contract), §10 (a test row: every check
fires on a seeded fixture and stays silent on a clean one), M5 (link/scope checks),
M8 (CI). Design cost only — the aggregation is a grouping pass over data scan and
index already produce.

---

## I-3 — Superseded facts must be absent from `current`, not merely outranked

**Status:** parked (2026-11-01). Review at M5 (supersession exists) and M8 (eval
harness).

### Idea

G1 measures “the top-3 results are the active authoritative docs ≥ 90% of the time”. A
superseded document can occupy one of those three slots and the metric will not notice:
90% is an explicit tolerance. The design already hides superseded docs from the
default path (D8, §6.6's status gate, D41 for `as_of`), but nothing *asserts* it end to
end. M5's done-when checks that `--as-of` returns the old value; it does not check
that `current` refuses it.

Proposal: one extra assertion over paired fixtures, separate from the nDCG and
active-top-1 report.

- **Paired fixture.** A superseded document that is *closer* to the query than its
  successor — lexically and semantically — so ranking alone would keep it, plus the
  successor that supersedes it.
- **The assertion.** For an `intent=current` query, no returned result may carry
  `status ∈ {superseded, deprecated}` or a `valid_to` before `now`. A hard zero, not a
  rate.
- **The counterpart.** The same pair queried with `--as-of` before the supersession
  must return the predecessor and not the successor (M5).
- **Exempt by definition.** `intent=history`, `intent=evolution`, `--include-superseded`.

Report a “stale leak” count next to nDCG for diagnosis; gate on the zero.

### Why parked

M5 is the first milestone where links and validity windows exist at all — M3 gates on
declared status only and says so — so the fixtures cannot be written yet. Recording the
shape now stops the eval from becoming “nDCG plus a wish”.

### Open questions

1. **Zero is only correct per scope set.** D45 allows intersecting cross-scope
   supersession and D46 resolves windows per query scope, so a document can be
   superseded for one org and current for another. Assert on single-scope queries
   only (scope the fixture explicitly), or compute the expected set per scope?
2. **Hard gate or report?** A hard gate on a fixture easier than a real vault gives
   false confidence. How adversarial should the pair be — a rephrased title, a
   near-identical body, both?
3. **Where does the fixture live?** `tests/eval/` (quality) or M5's integration
   fixtures (correctness)? The mechanism is correctness; the query is quality.
   Probably one fixture, asserted twice.
4. **`get_authoritative_facts` too?** It filters on status already, so a leak there is
   a bug rather than a metric — but the same fixture asserts it cheaply.

### Where it would land

`SPEC.md` §10 (a test row), M5 (integration fixtures), M8 (eval report). Cost is
fixtures and query lines; no logic.

---

## I-4 — Client integration recipes, including a first-turn snippet

**Status:** parked (2026-11-01). Review at M6 (server exists) and M8 (docs).

### Idea

M8 already plans MCP config snippets for Claude Desktop and Hermes, and M6's done-when
registers the server in both. Extend that to a small per-client set — Claude Code,
Codex, Cursor, OpenCode, Hermes — each a copy-pasteable config plus one short
**first-turn recipe**: call `get_authoritative_facts`, `search` the task's topic, cite
paths, then answer.

This is the legitimate home for the “auto-inject active principles” instinct. The
instruction belongs in the client's project rules or system prompt, not in the engine:
janus returns ranked, sourced evidence and stops there (§3.1), and it does not own the
agent's context window.

**Constraint:** verify each client's MCP-over-stdio support before shipping a snippet
(§3's verification policy). A config file for a harness that cannot speak the protocol
is worse than no file.

### Why parked

Docs-only, and it cannot be written before M6 has a server to point at. Also small
enough not to need its own milestone.

### Open questions

1. **Supported template or example?** A template implies a support burden and a
   compatibility promise; an `examples/` directory promises nothing.
2. **Where do the files live** — `docs/clients/` (documentation) or `examples/mcp/`
   (copyable artifacts)?
3. **Token budget guidance.** Per-client `max_chars`/`limit` advice, or one default?
4. **Over-retrieval.** A recipe that fires on *every* turn pays latency and context
   for questions that do not need the vault. Scope it to “before answering a question
   about the project's current state”, not “always”.

### Where it would land

`SPEC.md` §16.3 (example MCP configuration) and M8's docs. No code, no test.

---

## I-5 — `handoff` is a document convention, not a command

**Status:** parked (2026-11-01). Review at M8 (docs), and probably close as rejected.

### Idea

An agent-to-agent handoff — Claude Code hands a task to Codex, and the second agent
should know what the first was doing — is the need behind “snapshot the session”.
janus-mem should not build it. Session state is ephemeral, and §3.1 exists to separate
persistence from capture: a command that writes state is janus *generating* memory.

The cheap answer is already a feature: a handoff is an ordinary document. The client
writes it with `write_document` (type `note`, `date` today, `status: active`), tags it
`handoff`, and janus indexes it like every other note. A genre alias makes it read
naturally with no release (§6.6.1):

```toml
[types]
handoff = "note"   # a genre: note authority and decay, plus a `handoff` tag
```

The document earns durability by existing and being committed; the next agent finds it
with `search`/`facts`. Nothing in the engine knows the word “handoff”, which is D6
(generic write primitives, no domain verbs) working as intended.

### Why parked

There is no code to write. It needs one sentence in M8's “how to write docs the engine
understands” guide, and that guide is already planned. If the guide ships without it,
delete this entry.

### Open questions

1. **Alias, or a first-class type?** §6.6.1's rule: a type earns a row only when it
   needs a different authority, decay or lifecycle. A handoff is short-lived; if it
   should decay faster than a note, that is behavioral and justifies
   `[decay.by_type] handoff = 30`. Otherwise alias.
2. **Does the next handoff supersede the previous one?** Probably yes — that is the
   `idea → decision` shape applied to state, and it keeps `intent=current` returning
   only the live handoff. A convention the client applies, not the engine.
3. **Is the tag enough to find it?** A handoff that only says “continue the migration”
   is useless without the paths it refers to. Should the convention require a pointer
   to the work item, or is free text the point?

### Where it would land

M8's docs, possibly a two-line example. No `SPEC.md` change, no code.
