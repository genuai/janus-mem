# R2 — Retrieval: the document, not its best chunk

> Branch: `r2-plan` (this document), then one branch per phase from `main`.
> Spec: §7.2 (scoring), §3.1 (grouping), §6.5 (chunks), §11 (M7/M7b done), §15.
> Evidence: the gate of 2026-11-12 on two corpora, and a chunk-level diagnosis of one real
> question that ranked its answer 97th.
>
> **Done when:** the case recorded outside this repository as **the Q3 case** moves its answering
> document from rank **97** into the top three, without losing a grade on the repository packet and
> without dropping a decision, spec or principle out of its own answer's top three.

**Words used here.** **Argmax**: the document's score is its best chunk's score. **Coverage**: how
many chunks of a document hold a query term. **Coordination**: how many distinct query terms a
document matches anywhere. **Canary**: a question whose answer must not move.

## 0. Scope

**Decision: R2 changes how a document's score is aggregated from its chunks. It does not add
embeddings, and it does not change the type weights.**

A real question, recorded outside this repository, puts its answering document at rank 97. The
diagnosis is not what a first reading suggests:

| Claim | Measured |
|---|---|
| the authority gap decides | **no**: 46 of the 96 documents above it are `note` at the same 0.90, and giving it the winner's 1.50 still leaves it below eight of them |
| recency decides | **no**: within a 14-day window it is 48th |
| its text lacks the terms | **no**: 25 occurrences of the query's rare term, against 6 in the document that wins |
| its **best chunk** lacks the terms | **yes**: 1 of 3 query terms, against 2 of 3 for the winner |

The document is spread thin: 32 chunks, 18 holding the rare term, and its best chunk holds two
occurrences in 145 tokens. The document that wins has a 120-token chunk holding two different query
terms. The argmax rewards density and ignores the 25 occurrences elsewhere. The case's paths and
text stay outside this repository, as §7 requires.

| R2 item | state today | evidence |
|---|---|---|
| document score | argmax over chunks | `src/search/mod.rs` (group to one row per document) |
| coordination (distinct terms matched per document) | absent | the scorer sees one chunk at a time |
| a per-chunk diagnostic | absent | `search` reports the argmax chunk only |
| chunk ordinals | `search` and `chunks` disagree | the diagnosis: search ordinal 11 is chunks ordinal 15 by span |
| `scores` breakdown | has `lexical`, `relevance`, `final` | enough to compare two documents, not enough to see why |

### 0.1 In scope

1. A per-document diagnostic: the top *k* chunk scores and the distinct query terms matched anywhere
   in the document, behind `--explain` on `search` and in the JSON.
2. A document aggregation behind `[scoring] aggregate = "max" | "max_plus_top" | "coverage"`, with
   `max` the default, so the release is byte-identical until the calibration picks otherwise.
3. The calibration: the two ten-question packets, the Q3 case, and five canaries whose answers are
   decisions, specs or principles.
4. One default, chosen by a rule fixed before the numbers.
5. SPEC §7.2's "A doc's score is the max over its chunks" sentence, corrected to whatever the
   calibration selects.

### 0.2 Not in scope

| Item | Owner | Why |
|---|---|---|
| Embeddings, semantic ranking | later, if the wording count says so | the packets measure one wording failure in ten; Q3 is a coverage failure, not a synonym failure |
| Type weights, `[authority]` | nobody, for now | measured: the type gap is not the gate at rank 97 |
| Recency as a signal | later | measured: the 14-day window does not fix it, and `--as-of` already exists |
| The chunk ordinal inconsistency | a small PR after R2 | it is a reporting bug, and mixing it into a ranking change makes one review out of two |
| Chunk size or overlap | later | the chunker fingerprint is in the manifest; changing it rebuilds every index |
| `format_version` | unchanged | the snapshot layout does not change; only how it is scored |

### 0.3 Why three phases

| Phase | Unit | Why it is one reviewable change |
|---|---|---|
| R2-1 | the diagnostic | it changes no score; it makes the other two measurable |
| R2-2 | the aggregation, default `max` | a new field, a new function, and a byte-identical default |
| R2-3 | the calibration and the default | the numbers, the rule, and the SPEC correction |

## 1. Dependencies

None. The aggregation is arithmetic over numbers the scorer already computes.

## 2. Data model

### 2.1 The evidence a document carries

```rust
pub struct DocEvidence {
    pub top: Vec<f64>,        // the k best chunk scores, in order, k = 3
    pub coordination: usize,  // distinct query terms matched anywhere in the document
    pub total: usize,         // distinct query terms in the query
}
```

The chunk scores exist where the grouping happens. **Coordination does not**: `LexicalIndex::score_query`
returns scores only (`src/index/lexical.rs:241`), so the per-chunk matched terms are not in the
scorer's output. Phase 1 widens that signature to return the matched terms per chunk, and that file
is in Phase 1's list. Nothing is re-walked; the terms are already in hand where the score is
computed.

### 2.2 The three aggregations

The aggregation is a **factor** in `(0, 1.25]`, applied after the argmax and reported as its own
field. It is not folded into `relevance`, because `relevance = blend · title_boost ∈ [0, 2]` is a
documented identity and folding would break it.

```text
factor_max    = 1
factor_top    = 1 + λ · (top[1] + top[2]) / (2 · top[0])   λ = [scoring] top_weight, default 0.10
factor_cover  = 1 + μ · coordination / total                μ = [scoring] coverage_weight, default 0.15
factory       = clamp(chosen, 1.0, 1.25)
final_doc     = final_argmax_chunk · factor
```

`top[i]` are the chunk's **relevance** scores, before authority and decay, so the factor multiplies
the same quantity in every mode. `top[0] == 0` cannot occur on a returned row (`relevance > 0` is
the admission rule), and the code still guards it: a zero makes the factor 1.0. `total == 0` makes
the coverage factor 1.0 for the same reason.

The clamp at 1.25 keeps the score bound a constant: `final ≤ 3.0 · 1.25 = 3.75`, and §10 corrects
the bound in SPEC §7.2 and the property test's assertion.

`max` is the default and must reproduce today's output byte for byte, including ties. The factor
field is omitted from the JSON when it is 1.0, the way `scores.corpus` already is
(`src/score.rs:257`), so every golden stays.

### 2.3 The diagnostic

`search --explain --json` adds, per row:

```json
"explain": {
  "top": [0.3252, 0.2401, 0.2013],
  "coordination": 2,
  "total": 3,
  "chunks": 32,
  "chunks_matching": 18
}
```

`--explain` is a flag; without it the row is unchanged, so every golden stays. With it, one golden
per packet moves, and that is recorded.

## 3. Contracts

### 3.1 The diagnostic (P1)

**Case 1.1.** `--explain` on a document with one chunk reports `top` of length 1, `coordination`
equal to the distinct query terms the chunk holds, and `chunks_matching` 1.

**Case 1.2.** A document whose query terms appear in three chunks and not in the argmax chunk reports
`coordination` 3 and `top[0]` equal to the argmax score.

**Case 1.3.** Without `--explain`, every row is byte-identical to today. Proved by the existing
goldens.

**Case 1.4.** `chunks_matching` counts chunks with `relevance > 0`, not chunks with any term: a
chunk that matched only a stopword is not a match.

**Case 1.5.** The five canaries and the Q3 case, run with `--explain`, report values that explain
their rank: for the review, `coordination` ≥ 2 and `chunks_matching` ≥ 18.

### 3.2 The aggregation (P2)

**Case 2.1.** With `aggregate = "max"`, both packets produce byte-identical rows to today, and the
three rankings are identical.

**Case 2.2.** For a document with `top = [0.5, 0.4, 0.1]` and `λ = 0.1`,
`max_plus_top = 0.5 + 0.1 · 0.25 = 0.525`, computed by hand.

**Case 2.3.** For a document with `coordination = 2`, `total = 3` and `μ = 0.15`,
`coverage = top[0] · 1.1`, computed by hand.

**Case 2.4.** `scores.aggregate` is a field of `Breakdown`, omitted when it is 1.0, and
`final = relevance · authority · decay · corpus · aggregate` for every row (the M6b-32 identity,
with one more factor). A test asserts the identity on a row where the factor is not 1.0.

**Case 2.5.** A document with zero coordination cannot occur on a returned row (`relevance > 0` is
the admission rule), and a test asserts that the multiplier is never applied to a zero.

**Case 2.6.** An unknown `aggregate` value is a config warning and falls back to `max`, like every
other enumeration in the config.

### 3.3 The calibration (P3)

**Case 3.1.** The rule below is applied to the numbers, and the losing candidates' numbers are
recorded in the phase log.

**Case 3.2.** The chosen default is written into `janus.toml`, the SPEC, and one test that pins the
three rankings it produced.

**Case 3.3.** The Q3 case reaches the top three.

**Case 3.4.** The repository packet loses no grade: top-1 **≥ 4/10** and top-3 **≥ 9/10**. The
private packet is recorded beside it and is not a condition.

**Case 3.5.** Five canaries keep their answer in the top three.

**Case 3.6.** p50 latency on the repository packet stays under **300 ms**, twice today's 145.

### 3.4 What must not change

1. `format_version` 6, `api` 1, no exit code and no error code.
2. No new dependency.
3. A document is still one row, and the returned chunk is still its argmax.
4. Every tie-break still ends in `(final, date desc, chunk_id, doc_id, rel_path)`, and the new factor
   is applied before all of them.
5. A search that returns nothing at `max` still returns nothing under any aggregation: a document
   with no matching chunk has no `top`.

## 4. Decisions

| ID | Decision | Why |
|---|---|---|
| R2-1 | The default stays `max` until the calibration selects otherwise | A release that changes ranking by default must have the numbers first. `max` reproduces today byte for byte, so the release is safe before calibration and the change is one line after. |
| R2-2 | The aggregate factor multiplies `relevance`, before authority and decay | Otherwise the `scores` breakdown stops multiplying to `final`, and the breakdown is the only way a user can see why a document ranked where it did. |
| R2-3 | `max_plus_top` rewards breadth linearly, `coverage` rewards distinct terms | They answer different failures: a document that says one thing in several places, against one that answers the whole question in one place. The calibration says which this corpus needs. |
| R2-4 | The calibration rule is fixed before the numbers, per §7 | A rule edited after the data arrives is worthless. |
| R2-5 | `--explain` is a flag, not a default field | One extra field per row costs every consumer, and the diagnostic is for an investigation, not for an agent's context. |
| R2-6 | The five canaries come from the private packet and are recorded outside this repository | The repository's packet guards top-1 and top-3; the canaries guard the answer that must not move, and their text is private. |
| R2-7 | Chunk ordinals stay inconsistent until a separate PR | A ranking review should not carry a reporting fix, and the fix has its own cause: `search` numbers chunks after grouping, `chunks` numbers them before. |
| R2-9 | The factor is a separate clamped field, not a change to `relevance` | `relevance` has a documented range and a documented identity; folding would break both, and a clamp keeps `final`'s bound a constant. |
| R2-10 | `top[i]` are chunk **relevance** scores, and a zero makes the factor 1.0 | One quantity in every mode, and a guard rather than a division by zero. |
| R2-11 | The repository gate for the Q3 failure is a fixture Phase 1 builds | A gate no reviewer can read is not a gate. The private case stays evidence. |
| R2-8 | No embeddings | The packet measured one wording failure in ten. The trigger for semantic ranking is three, and Q3 is a coverage failure: the terms are present, spread thin. |

## 5. Phases

### Phase 1 — the diagnostic

**Files.** `src/index/lexical.rs` (return the matched terms per chunk), `src/search/mod.rs` (the
grouping, the evidence), `src/score.rs` (the evidence types), `src/api/dto.rs` (`explain`),
`src/cli.rs`, `src/main.rs`,
`src/mcp/tools.rs` (the `explain` parameter), `tests/search_explain.rs` (new),
`tests/golden_search.rs` (one golden with the flag).

**Build.** `DocEvidence`, the `explain` field, the flag on `search` and on the MCP tool.

**Tests.** Cases 1.1 to 1.5.

**Accept.** Without `--explain`, every golden is byte-identical. With it, the Q3 review and the five
canaries report the numbers their ranks imply.

**Split seam.** The index change first (the matched terms per chunk), the evidence and the surfaces
second. If the index change alone passes 400, ship it as its own PR: it changes no score.

### Phase 2 — the aggregation

**Files.** `src/score.rs` (the three functions), `src/search/mod.rs` (apply the factor), `src/config.rs`
(`[scoring] aggregate`, `top_weight`, `coverage_weight`), `src/api/dto.rs` (the breakdown term),
`tests/scoring_aggregate.rs` (new), `tests/golden_search.rs` (unchanged, because `max` is default).

**Build.** The three aggregations, the config, the breakdown term.

**Tests.** Cases 2.1 to 2.6.

**Accept.** `max` is byte-identical; the two others are hand-derived.

**Split seam.** The pure functions first; the config and the plumbing second.

### Phase 3 — the calibration

**Files.** `janus.toml`, `SPEC.md` §7.2, `docs/r2-breakdown.md`, `tests/golden_search.rs` (a
non-`max` default moves those goldens, and this PR owns the move), one test that pins the three
rankings, and the phase log.

**Build.** Run the grid, apply the rule, ship the winner.

**Tests.** Cases 3.1 to 3.6, measured, not asserted.

**Accept.** The Q3 case is in the top three, no packet lost a grade, the canaries hold, latency is
under twice today.

**Split seam.** None. It is one measurement and one line.

## 6. Cross-cutting requirements

- The gates of `AGENTS.md`, unchanged.
- Every test is deterministic: a fixed clock, a fixture corpus, no sleep.
- A ranking change lands with the numbers that justify it in the same PR.
- After each phase merges, rebuild the local install and re-run the three-question test and both
  packets.

## 7. The evaluation packet

Three instruments, all fixed before the work starts:

1. **The repository's ten questions**: `tests/eval/repo-ten.jsonl`, measured on this document's own
   commit: top-1 **4/10**, top-3 **9/10**, p50 6 ms. This is the gate.
2. **The private vault's ten questions**: kept outside this repository. Baseline, graded fresh on
   2026-11-12: top-1 **4/10**, top-3 **7/10**, one wording failure (Q10). **Supplementary evidence,
   never a condition** — the M7 plan's §7 rule, which this plan keeps.
3. **The three real questions** the owner asked, with these expected answers, recorded outside this
   repository except for the first, which is already a public fact: the Q3 case, whose answering
   document is a marketing review, must reach the top three.

### The calibration rule, fixed before the numbers

Try every combination of `aggregate ∈ {max_plus_top, coverage}` and its weight in
`{0.05, 0.10, 0.15, 0.25}`. Keep a combination only if all of these hold:

- **C1** the Q3 answering document is in the top three;
- **C2** the repository packet's top-1 **≥ 4/10** and top-3 **≥ 9/10**, and the private packet's
  top-1 and top-3 no worse than its baseline (supplementary, recorded, not a gate);
- **C3** all five canaries keep their answer in the top three;
- **C4** p50 latency under 300 ms on the repository packet.

Among the combinations that pass, choose **the smallest weight**, and on a tie prefer
`max_plus_top`, because it is a linear function of numbers a reader can see, with no per-term
bookkeeping.

If no combination passes, the plan's answer is `max`, and the milestone's finding is that the
aggregation is not the lever — recorded, with the numbers, and handed to the next plan.

## 8. Definition of done

1. **The Q3 case** moves its answering document into the top three: **rank ≤ 3**, from **97**.
   Supplementary: the case is recorded outside this repository, so no reviewer reads it here. The
   repository gate for the same failure is a fixture that reproduces it: a long document whose rare
   term appears in many chunks and in no single dense one, against a short document with one dense
   chunk. Phase 1 builds that fixture, and case 3.3 asserts it.
2. The repository packet: top-1 **≥ 4/10**, top-3 **≥ 9/10**, no grade worse.
3. The private packet, supplementary: top-1 and top-3 no worse than its baseline, recorded.
4. Five canaries, supplementary: **5 of 5** keep their answer in the top three.
5. p50 on the repository packet: **< 300 ms**, from 6 ms — the aggregation is linear in chunk count,
   so the budget is generous on purpose.
6. `format_version` **6**, `api` **1**, **0** codes added, **0** exit codes moved.
7. Goldens: **0** move at `max`; the calibration's chosen mode updates the goldens it must, one by
   one, each diff read field by field.
8. `cargo doc --no-deps`: **0** warnings.
9. Retrieval claims: **3** measured, one per instrument in §7, each with its number.

## 9. PR plan and size budget

| phase | logic ceiling | estimate | test ceiling | estimate |
|---|---|---|---|---|
| R2-1 diagnostic | 400 | ~350 | 500 | ~350 |
| R2-2 aggregation | 250 | ~180 | 400 | ~280 |
| R2-3 calibration | 150 | ~80 | 250 | ~150 |

Each phase names its split seam and splits before its ceiling. M7's estimates ran low because its
phases created surfaces; R2's phases extend surfaces that exist, so these are upper bounds.

## 10. SPEC corrections

1. **§7.2.** Replace "A doc's score is the max over its chunks; the returned chunk is the argmax"
   with the calibrated rule, and name the config key that selects it.
2. **§7.2.** Add `aggregate` to the arithmetic block as a factor after `decay` and `corpus`, state
   that it is clamped to `[1.0, 1.25]`, and correct the bound from `final ≤ 3.0` to
   `final ≤ 3.75`.
3. **§9.1.** `janus search` gains `--explain`, and the `scores` breakdown gains `aggregate`, omitted
   when neutral, the way `corpus` already is.
4. **§9.2.** `search` gains `explain?` on the MCP tool.
5. **§16.1.** Name `[scoring] aggregate`, `top_weight` and `coverage_weight`.
6. **§11.** Add R2 between M7b and M8, with the done-when above.

## 11. Handoff

- **M8** inherits `watch`, mmap and cross-corpus supersession, and the chunk-ordinal inconsistency
  if it is not fixed first.
- **Semantic ranking** stays parked. The trigger is three wording failures in ten; the packets
  measure one. Recheck after R2 with the same instruments.
- **The three real questions** live outside this repository. Re-run them after every ranking change,
  the way the packets are re-run.

## 12. Implementation logs

Empty. One entry per phase, appended when the phase merges.
