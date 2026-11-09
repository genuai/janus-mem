# R1 — Retrieval on the real corpus

> Branch: `r1-plan` (this document), then one branch per phase from `main`.
> Evidence: the dogfood gate of 2026-11-08 against two corpora — this
> repository (55 documents) and a private workspace (2,320 documents).
> Spec: §6.3, §6.10, §9.1, §9.4, §11 (M6 done), plus `AGENTS.md`.
>
> **Done when:** this repository answers at least 7 of its own 10 graded questions at top-1,
> the workspace answers at least 5 of 10, and a `scan` report reconciles: admitted + skipped
> entries equal the entries the walk saw, with pruned directories counted separately.

**Words used here.** **Gate**: the dogfood measurement of five numbers and ten graded
questions. **Undated**: a document whose date resolves to nothing (`DateSource::None`).
**Skip**: an entry the walk admitted to inspection and then refused. **Prune**: a
directory the walk never descends into.

## 0. Scope

**Decision: R1 fixes what the gate measured — the corpus instrument and one scoring
rule — and adds no new surface.**

The gate found a fast engine (p50 10 ms on 55 documents, 109 ms on 2,320) with clean
incremental behaviour (0 retokenized, 0 re-embedded on a quiet corpus) and poor answer
quality. Every cause was measurable, and none was a ranking algorithm.

| finding | number | evidence |
|---|---|---|
| An undated document loses 37% before relevance | decay 0.632 against 0.99 for a dated one | workspace gate, case 3: lexical 0.501 lost to 0.380 on the multiplier alone |
| Most documents declare no metadata | 13.8% typed, 7.3% frontmatter-dated, 34% neither | workspace `scan --json` |
| Doctrine outranks the personal record | research 1.10, journal 1.00 | workspace gate, cases 6 and 7 |
| Retired documents are served as current | `6.archive/` admitted: 226 docs, type `spec`, timeless | workspace gate, 3 top-three appearances |
| The skip report cannot be reconciled | 12 hidden entries cover 3,706 files | workspace gate |
| This repository's own corpus answered itself poorly | top-1 1/10 before PR #87, 5/10 after | PR #87 |

The last row is the important one: a config-only change moved top-1 from 1/10 to 5/10.
**The corpus is an input to the score, and it was wrong.** R1 treats it that way.

### 0.1 In scope

1. `scan` reports what it counted: entries, files, and pruned directories, with one
   reconcilable invariant.
2. The undated decay rule is re-decided on measurement, under a rule written here
   before the numbers arrive.
3. `scan` and `status` report metadata quality: typed, dated, undated, and by which
   source.
4. A short product document: how to configure a real vault, written from the gate.

### 0.2 Not in scope

| Item | Owner | Why |
|---|---|---|
| Semantic ranking (`embeddings`) | R2, if the measurement asks | 1 of 20 gate questions failed on wording; the rest on ranking |
| Archived corpus roles, `--corpus`, workspaces | M6b | the archive needs a role and a filter, not a config line |
| Writes, etags, supersede | M7 | no measured need yet; W4 stays unanswered until then |
| Any change to BM25, chunking, identity or windows | nobody in R1 | no gate case pointed at them |
| A date from mtime | nobody | SPEC §6.3 forbids inventing a date |

### 0.3 Why this is three phases and not one

| Phase | Unit | Why it is one reviewable change |
|---|---|---|
| R1-1 | skip accounting | one report shape, one invariant, fixtures |
| R1-2 | the undated rule | one scoring input, measured on two corpora |
| R1-3 | metadata reporting and the config guide | additive report fields and one document |

## 1. Dependencies

None. R1 adds no crate.

## 2. Data model

No persisted change. `format_version` stays 6. `Snapshot` and `Manifest` are untouched.

The `scan --json` report gains fields (additive, `api` stays 1):

```json
"walk": { "files": 55, "directories_pruned": 7 },
"skipped": [
  { "reason": "hidden", "entries": 6, "pruned_directories": 6 },
  { "reason": "not_markdown", "entries": 53, "pruned_directories": 0 }
],
"metadata": { "typed": 28, "typed_non_default": 28, "dated": 39, "undated": 16,
              "by_source": { "frontmatter": 33, "filename": 6, "absent": 16 } }
```

Today `skipped` is a map of reason to a number with no unit. The new shape states the
unit and adds the pruned count, because a skip count without a unit is what made the
gate report irreconcilable. All seven rules are always present, in
`SkipReason::ORDER`, whatever their count.

## 3. Contracts

### 3.1 Skip accounting reconciles (P1)

**Statement.** Every entry the walk yields is either admitted or skipped for one named
reason. Nothing is counted twice, and nothing disappears.

**Case 1.1.** A fixture with one hidden directory holding three documents reports
`walk.files` equal to the admitted count, `directories_pruned` 1, and a `hidden` entry
count of 1. The three files inside are not counted as entries.

**Case 1.2.** The invariant holds on the main fixture: `walk.files + sum(entries)`
equals the number of paths the walk yielded, and `walk.files == docs.len() +
read_skips.len()`. Measured: 20 + 7 = 27, with 1 pruned directory.

**Limit, stated not fixed.** A path hidden by `.gitignore` is never yielded by the
walk, so it is outside the invariant. `ignored_by_git` counts the markdown files the
ignore rules removed; a gitignored directory is not enumerated, so its documents are
not in that count either (R1-8).

**Case 1.3.** The human table prints the invariant in two lines: what was read, and
what was pruned with the reason. A user can reconcile it with `find` by hand once the
prune rule is stated.

**Case 1.4.** `not_markdown` counts files, not extensions, and says so in the field
comment. A directory with 3 `.txt` files adds 3.

### 3.2 The undated decay rule (P2)

**The decision rule, written before the numbers.** Run the ten questions of each corpus
against two settings: `median` (today) and `none` (decay 1.0 for an undated document).
Choose `none` when it gives at least as many top-1 answers on both corpora and loses no
top-three answer on either. Otherwise keep `median`. Ties go to `none`, because it
invents no date.

**Statement.** A document with no date is not degraded for having no date. The answer
says how many results were undated and what rule applied.

**Case 2.1.** An undated document scores with decay 1.0 under the chosen rule, and its
`decay` in the score breakdown equals the value the note names.

**Case 2.2.** `search` prints one note naming the count of undated results, only when at
least one returned result is undated. A dated-only answer prints no such note.

**Case 2.3.** A `history` or `--as-of` query prints no undated note, because decay is
off.

**Case 2.4.** The chosen rule is recorded in the plan's phase log with both corpora's
A/B/C tables, and the losing rule is named with its numbers.

**Case 2.5.** No ranking golden moves silently. If a golden moves, the diff is one
`decay` and one `relevance`/`final` value per affected row, and it is reviewed field by
field.

### 3.3 Metadata quality is visible (P3)

**Case 3.1.** `scan --json` reports typed, dated, undated and the per-source counts, and
the four numbers sum correctly: `dated + undated == docs.len()`.

**Case 3.2.** `janus status` reports the same undated count and its share, so an agent
can ask "is this corpus metadata-poor?" without a second command.

**Case 3.3.** The human `scan` table prints one line: `metadata: 28 typed, 39 dated, 16
undated (29%)`.

### 3.4 The config guide (P3)

**Case 3.4.** `docs/corpus-config.md` states, with the gate's numbers as evidence:
exclude synthetic and retired trees; type frontmatter-less documents by path; and check
whether a corpus's authority order matches what it wants to be true. Each rule names the
finding it prevents.

**Case 3.5.** Every configuration key the guide shows exists in `janus.toml` parsing, and
a test parses the guide's example block as a config.

### 3.5 What must not change

- `format_version` 6, `api` 1, no exit code.
- Chunking, identity, windows, supersession, scope matching.
- The five tool names and the envelope.
- `scan`'s human output keeps its table; new lines are additive.

## 4. Decisions

| ID | Decision | Why |
|---|---|---|
| R1-1 | Skip counts gain a unit and a pruned-directory count; the walk is not repeated to count files inside a pruned tree | A second walk to satisfy a report doubles the cost of the one command whose job is to be quick. Stating the prune rule makes the number reconcilable by hand. |
| R1-2 | The undated rule is re-decided by a pre-written rule, and the loser is recorded with its numbers | The gate showed a 37% multiplier deciding three of ten answers. A rule chosen after the data is worthless, so it is written in §3.2 first. |
| R1-3 | Metadata quality is reported, not repaired | The tool never writes to a vault outside an explicit write command (§9.3). A report is the most it may do before M7. |
| R1-4 | No mtime date source | SPEC §6.3 rejects inventing a date. A copied file is not a new decision. |
| R1-5 | Retrieval accuracy is the milestone's acceptance number, not a test count | The gate exists because green tests and bad answers are the failure mode this milestone answers. |
| R1-6 | The invariant is `walk.files + sum(entries) = paths the walk yielded`, and `walk.files = docs + read skips` | The formula in §3.1 before this phase added file skips to the admitted set and forgot the read skips, so it could never hold. |
| R1-7 | The pruned-directory count is derived from the per-entry skip list, not stored beside it | Two copies of one number drift apart; one source makes a total and its rows agree by construction. |
| R1-8 | A `.gitignore`d tree stays outside the accounting; only its markdown files are counted | The walk never yields those paths, and R1-1 changes no walk rule. Named, so the next gate does not read the gap as a counting bug. |

## 5. Phases

### Phase 1 — Skip accounting

**Files.** `src/scan.rs`, `src/vault/mod.rs` (the walk result), `tests/cli_scan.rs`,
`tests/fixture_scan.rs`, `tests/golden_scan.rs` and `tests/snapshots/golden_scan__scan.snap`.

**Build.** The walk result carries `directories_pruned`. Each skip reason carries an
entry count and a pruned count. The human table gains two lines.

**Tests.** Cases 1.1–1.4.

**Accept.** The golden snapshot moves by the declared shape change only, reviewed field
by field. The invariant holds on the main fixture.

**Split seam.** Part 1 is the walk and the report; part 2 is the human table and the
golden.

### Phase 2 — The undated rule

**Files.** `src/score.rs`, `src/search/mod.rs`, `docs/m6-breakdown.md` (a correction row
if the rule moves), `tests/golden_search.rs` and the snapshots if a golden moves,
`tests/hybrid_search.rs`, `tests/cli_search.rs`.

**Build.** Measure both settings on both corpora with the ten graded questions, obey
§3.2, then implement the winner and its note.

**Tests.** Cases 2.1–2.5.

**Accept.** The A/B/C tables for both rules are in the phase log, the rule was applied
as written, and every moved golden row shows one decay value.

**Split seam.** Measurement first (a report, no code); the implementation second.

### Phase 3 — Metadata quality and the config guide

**Files.** `src/scan.rs`, `src/status.rs`, `src/api/dto.rs`, `docs/corpus-config.md`
(new), `tests/cli_scan.rs`, `tests/cli_status.rs`, `tests/corpus_config_docs.rs` (new).

**Build.** The metadata block in `scan` and `status`; the guide, with its example parsed
by a test.

**Tests.** Cases 3.1–3.5.

**Accept.** The four metadata numbers sum. The guide's example is a valid config.

## 6. Cross-cutting requirements

- The gates of `AGENTS.md`, unchanged: fmt, clippy `-D warnings`, test, doc, Windows
  cross-check.
- Every expected value hand-derived; no test reads the wall clock or sleeps.
- Comments in Simplified Technical English.
- After each phase merges, rebuild the local install (§AGENTS "Dogfooding install") and
  re-run the ten questions of this repository, recording the A/B/C line in the log.

## 7. Definition of done

1. This repository answers **at least 7 of 10** at top-1, measured with the recorded ten
   questions, up from 5/10.
2. The workspace answers **at least 5 of 10** at top-1, measured by a thread in that
   project and reported as counts only, up from 2/10.
3. `scan` reconciles: `walk.files == admitted + file-skips`, with pruned directories
   counted separately and named.
4. The undated rule is the one the pre-written rule selected, and the loser's numbers are
   in the log.
5. `format_version` 6, `api` 1, no exit code moved.
6. `cargo doc` has zero warnings.

## 8. PR plan and size budget

| phase | logic ceiling | estimate | test ceiling | estimate |
|---|---|---|---|---|
| R1-1 skip accounting | 300 | ~220 | 450 | ~250 |
| R1-2 undated rule | 250 | ~150 | 400 | ~200 |
| R1-3 metadata + guide | 250 | ~180 | 350 | ~150 |

Estimates run low by two to three times in this repository. Each phase names its split
seam; split before the ceiling.

## 9. SPEC corrections

1. **§9.1 `janus scan`.** State that skip counts are entries, that a pruned directory is
   one entry with its file count unknown by design, and that `walk.files` plus the skip
   entries reconcile. (P1.)
2. **§6.3, the §7 scoring rule, and §11 M4B.** The undated rule is stated three times:
   `SPEC.md:252` (§6.3), `SPEC.md:686` (the `age_days` bullet in §7), and the M4B
   done-when at `SPEC.md:1310`. Replace all three with the rule the measurement
   selected, and keep the reason beside it. The M4B line is a milestone record, so it
   keeps its place and gains one clause naming §6.3 as the rule that supersedes it: a
   reader must not trust a rule R1 replaced. (P2.)
3. **§9.2 `janus_status`.** Add the metadata block: typed, dated, undated. (P3.)

## 10. Handoff

- M6b inherits the archive role, which makes `6.archive/**` a corpus question instead of
  an exclude line.
- M7 inherits nothing from R1 except a better-measured corpus, which is the point.
- The next gate run reuses the same ten questions per corpus, recorded outside the
  repository, so the numbers stay comparable.

## 11. Implementation logs

### Phase 1 — Skip accounting

**Built.** `WalkResult::pruned(reason)` and `pruned_total()` count the refused
directories, derived from the per-entry skip list (R1-7). `scan --json` reports
`walk: {files, directories_pruned}` and `skipped` as a list of
`{reason, entries, pruned_directories}`, always seven rows in `SkipReason::ORDER`.
One helper, `walk_totals`, feeds both outputs. The human output replaces the old
`skipped:` line with two:

```
read:    20 file(s) admitted, 7 path(s) skipped: 1 always_excluded, 3 hidden, 2 config_exclude, 1 not_markdown
pruned:  1 directory (1 always_excluded); one entry each, contents never visited and not counted
```

**Invariant, measured on the committed fixture.** 20 admitted + 7 skipped = 27
paths the walk yielded; 20 = 19 documents + 1 read skip; 1 pruned directory. The
case 1.1 fixture measures 1 admitted, 1 pruned hidden directory, 1 `.txt`, and
`find . -name '*.md'` counts the 4 documents, 3 of which the walk never visited.

**Golden diff, reviewed field by field.** `skipped` moves from a map of seven
counts to a list of seven rows, and `walk` is added. The values are unchanged:
`always_excluded` 1, `hidden` 3, `config_exclude` 2, `not_markdown` 1, and
`symlinked_directory`, `symlink_escape`, `non_utf8_path` at 0. The new totals are
`walk.files` 20 and `walk.directories_pruned` 1. No document row, no `read_skips`
row, and no other field moved.

**Decisions.** R1-6 (the true invariant), R1-7 (derived counts), R1-8 (a
`.gitignore`d tree stays outside the accounting). SPEC §9.1 now states the unit,
the two rules and the prune rule.

**Size.** logic 199 / 300, tests 322 / 450.

**Not done here.** `api` stays 1, as §3.5 requires, although `skipped` changed
type. Every consumer in this tree is updated in the same commit. A gitignored
directory still produces no skip entry; R1-8 names the limit rather than changing
a walk rule. The dogfood re-run follows the merge (§6), after the local binary is
rebuilt from the merged commit.
