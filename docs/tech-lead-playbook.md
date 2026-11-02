# Tech lead playbook

The generic procedure — roles, dispatch briefs, review checklist, merge protocol, model policy — is
the `milestone-tech-lead` skill. This file is what is **specific to janus-mem**, plus what M4 taught
the next lead. Read it before starting M5.

## Defaults for this repo

| Thing | Value |
|---|---|
| Model for phase agents | the model the lead is running, same reasoning level |
| Branch per phase | `bb/<milestone>-<phase>-<thread-id>`, from `main` after the previous merge |
| One phase | one PR; a split is `P<n>a`/`P<n>b`/`P<n>c`, each measured against its own base |
| Plan file | `docs/<milestone>-breakdown.md`, mirroring the previous milestone's shape |
| Phase log | the plan's `§<n>` implementation log, one `P<n>-<k>` row per decision |
| Decisions that outlive the milestone | `SPEC.md` §15 (`D<n>`), or a contract section of the plan |
| Gates | `AGENTS.md` "Gates"; add `--features embeddings` and the baseline crate where relevant |
| Size budget | `scripts/pr-size.sh`; markdown and `Cargo.lock` are reported, not budgeted |

Phase logs are **history**: never rewrite a row after the fact. Contract sections are **live**: they
must be amended in the same PR as the change. M4 needed a whole review round to catch nine contract
lines that still stated the old α default.

## What M4 taught, with the evidence

1. **Plan review is the cheapest code review there is.** Five rounds, 17 findings, before a line of
   feature code. Three were blockers that would have shipped: a cache key missing an input, a
   set-vs-sequence comparison, and two binaries deleting each other's cache. Budget the plan review
   as real work, and expect it to change the plan.

2. **Verify every claim about existing code.** Three "facts" in the M4 hand-off were wrong: the
   `--no-embeddings` flag did not exist, SPEC still claimed M3 work for M4, and "a rename costs zero
   embedding work" was true only when the document declares a `title:`. Each would have produced a
   phase that did not match the code.

3. **A cache key must enumerate every input to the cached value.** M4 lost two review rounds to this:
   `ChunkId` hashes *normalised* text while the embedder sees raw text (an `autocrlf` checkout would
   reuse stale vectors), and `content_hash` is the whole file including frontmatter (a one-tag edit
   re-embedded everything). Write the inputs down, then hash them. A partial witness is a silent
   staleness bug, and none of the obvious tests catch it.

4. **Estimates were 2–3× low in every phase.** Measured: P1b 463 vs ~320, P1c 279 vs ~230, P2 332 vs
   ~260, P4 465 vs ~320, P5 482 vs ~150. Name the split seam in the plan and tell each agent to split
   *before* the ceiling. P4 correctly declined a split at 465 with the reason stated — that judgement
   is acceptable when it is written down; a silent 700-line PR is not.

5. **Tests must be able to fail, and you must prove it.** M4's best evidence, from P4: the metadata-only
   edit test failed on `main` with `left: 1, right: 0` and passed after the witness change. Anything
   less is a claim. A regression guard that passes before the change should say so in its own comment.

6. **Planned temporary defects need a named reversal.** P1b deliberately dropped `vectors.f32` on a
   degraded publish; P4 was told, in the plan and the brief, to flip that test. Without the naming,
   the stopgap becomes the design.

7. **Boundaries break more often than formulas.** Round 1's fixes were right about the mechanism and
   wrong about the boundary: which input witnesses a change, which branch owns a manifest field, which
   phase owns a signature change. When a fix moves work, recheck the ownership table.

8. **The reviewer is a second opinion, not an oracle.** Round 4 found that a correct sentence ("no CLI
   path produces vectors before P4") made Phase 2 impossible. Trust the evidence, not the confidence.

9. **Stacked PRs: never `--delete-branch` the base while the child is open.** It closed the child and
   GitHub refused to reopen it; the recovery was a new PR from the same head branch. Merge the base,
   retarget the child, then delete both.

10. **A fired pre-declared rule beats a rationalised one.** P5's α sweep broke one top-1 at the planned
    0.65 and rescued none, so the shipped default moved to 0.5 — provisional, one line to revert, with
    the revisit condition written into §9.1 and the corpus caveat in the log. Decide the rule before
    the numbers, then obey it.

11. **Verify reported numbers yourself.** The pr-size totals, the pinned bucket literal (I recomputed
    `blake3("a") % 4` independently) and the α arithmetic (my `0.480` was wrong; `0.65·0.4 + 0.35·1.0
    = 0.610`) all earned their check. Reviewers make arithmetic mistakes in plans too.

12. **Say what the numbers cannot tell you.** The α decision rests on a 19-document parser fixture,
    because no real vault exists in the agent environment. That is written down as an open item, with
    what would revisit it. An honest caveat costs one paragraph; a silent extrapolation costs trust.

## Open items M5 inherits

- The α default (0.5) is measured on the fixture corpus only. An author's-vault sweep may revert it.
- Warm-query latency at 200k chunks is printed at fixture scale, never at reference scale; that
  measurement is M8's.
- The `tantivy` baseline is a one-off gate in `scripts/baseline/`, not a dependency. If it ever says
  "swap", that is a milestone of its own.
- `janus index PATHS` re-tokenizes but reuses rows for an unchanged document. Documented, not changed.
