# M4C — Retrieval that finds the document you meant

> Branch: `m4c-plan` (this document), then one branch per phase from `main`.
> Spec: §6.6, §7.2, §9.4, §11, §15.
> Evidence: a 2 119-document vault, measured in M4B.
>
> **Done when:** a document whose *title* names the topic outranks documents that merely mention it,
> and a document with metadata the config does not know is not punished for it.

**Words used here.** **title**: the document title, from frontmatter or the file name.
**breadcrumb**: the heading path of a chunk. **relevance**: the blended semantic and lexical score.
**final**: relevance × authority × decay. **boost**: a multiplier above 1.0.

## 0. Scope

**Decision: a document that is *about* a topic must beat a document that *mentions* it.**

Measured on the real vault, with the query `what is our q4 okrs?`:

| fact | value |
|---|---|
| the document the user wanted | `1.martin/planning/q4-2026-plan-draft.md` |
| its mentions of `q4` | 17, across 18 chunks |
| its rank for `q4` | **27** |
| its rank for `q4 plan draft` | **19** |
| its rank for its own file name | **miss, below 5** |
| documents containing `q4` | 152 |
| its type and status | `personal-operating-plan`, `unknown` — both unknown to the config |
| its multiplier from authority × status | **0.64**, against 0.90 for a note |

Three causes:

1. **A title match counts the same as a body mention.** The title is the strongest statement of what a document is about, and the index concatenates it to the body as one more string.
2. **A document's many mentions collapse into one.** A document's score is its best chunk. Seventeen mentions across eighteen sections count as one mention.
3. **Unknown metadata is a penalty.** An unrecognised type is ×0.80, an unrecognised status is ×0.80. Together 0.64, against 0.90 for a plain note.

### In scope

- A title boost at query time: a result whose title contains query terms scores higher.
- Neutral treatment of an unknown type and an unknown status.
- Measurement before and after, on the fixture eval and on the three real queries.
- A decision gate on cause 2. Fix it only if causes 1 and 3 are not enough.

### Not in scope

- Breadcrumb weighting. It needs index-time support to be cheap per query (C-2).
- A change to "a document's score is its best chunk". Deferred behind phase 3's measurement (C-5).
- Stemming, stopwords, synonyms, query expansion.
- Staleness detection, incremental refresh, progress output. Those are M4D.
- Any change to the index format. **No `format_version` bump, no rebuild.**
- A second index or a new file.

## 1. Dependencies

None. No crate. No feature flag.

## 2. Data model

```rust
// src/score.rs — one new reported factor.
pub struct Breakdown {
    /* … unchanged … */
    /// The title multiplier applied to `relevance`; `1.0` when no query term is in the title.
    pub title_boost: f64,
}

// src/config.rs — one new key, `[search] title_boost`.
pub struct SearchConfig {
    /// Weight of the title boost. `0.0` disables it and reproduces the old ranking.
    pub title_boost: f64, // default 1.0, clamped to [0.0, 2.0]
}
```

`Breakdown` gains one field, so `search --json` gains one key. The change is additive. `api` stays `1`.

## 3. Contracts

### 3.1 The title boost

**A result whose title contains query terms is multiplied by a factor between 1.0 and 2.0.**

```
title_hits  = number of distinct query terms present in the document title
title_cov   = title_hits / max(distinct query terms, 1)
title_boost = 1 + w · title_cov        w = [search] title_boost, default 1.0
title_boost = clamp(title_boost, 1.0, 2.0)
relevance   = (α · semantic + (1 − α) · lexical) · title_boost
```

- **`w = 0.0` reproduces the old ranking exactly.** That is the regression switch and the test.
- **A title matching one of two query terms gives ×1.5.** A title matching all terms gives ×2.0.
- **The boost never lowers a score.** It is clamped at 1.0 from below.
- **An empty query has no terms**, so its boost is 1.0. `facts` and the eval's exhaustive pass are unchanged.

**Where the terms come from.** The title is tokenized with the same `Tokenizer` as the index. The query's distinct terms come from the same tokenizer. There is no second tokenizer and no second normalization.

**Cost.** One title tokenization per document per query. At 2 119 documents this is milliseconds. The phase measures it and prints the number.

### 3.2 Where the boost applies, and where it must not

**The boost multiplies `relevance`, before authority and decay.**

| stage | changes? |
|---|---|
| `semantic`, `lexical` | no. They keep their meaning and their reported values |
| `relevance` | yes, multiplied |
| `authority`, `status_factor`, `decay` | no |
| `final` | yes, through `relevance` |
| the total order, the tie-breaks | no |

The identity `relevance == (α·semantic + (1−α)·lexical) · title_boost` must hold in every test and in the golden.

### 3.3 Neutral metadata for words the config does not know

**An unknown type and an unknown status stop being penalties.**

| field | today | after |
|---|---|---|
| unknown type | ×0.80 | **×1.00**, note kept |
| unknown status under `current` | ×0.80 | **×1.00**, note kept |

An unknown type is not evidence of low authority. It is evidence of a vocabulary the config has not seen. A new user's vault is made of those words. The note stays, so the user can still learn that the type is unlisted.

### 3.4 What does not change

- **No index format change.** The boost is computed at query time from `DocMeta.title`, which is already in the snapshot. No rebuild, no `format_version` bump.
- **A document is still scored by its best chunk.** Phase 3 decides whether to revisit that.
- **One hit per document** (§3.1 of M3). The boost cannot add results, only reorder them.

## 4. Decisions

| # | Question | Default | Where |
|---|---|---|---|
| C-1 | Boost at query time or at index time | **Query time.** Index-time duplication of the title would force an 18-minute rebuild and a `format_version` bump for a weight | `search/mod.rs` |
| C-2 | Title only, or title and breadcrumb | **Title only in M4C.** Per-chunk breadcrumbs would need a tokenized heading index to stay cheap per query | `search/mod.rs` |
| C-3 | The formula | `1 + w · hits/terms`, clamped to `[1, 2]`, `w` default 1.0 | `score.rs` |
| C-4 | Is the boost reported | **Yes**, in `Breakdown`. A ranking the user cannot explain is a ranking they will not trust | `score.rs` |
| C-5 | Document-level aboutness | **Deferred.** Fix causes 1 and 3 first, then measure. It is a change to a documented contract | §5 phase 3 |
| C-6 | Unknown type and status | **Neutral (1.0).** Never punish a document for a word the config has not seen | `config.rs`, `score.rs` |
| C-7 | Regression switch | `w = 0.0` reproduces the old ranking. The fixture test proves it | `config.rs` |
| C-8 | Format version | **Unchanged.** No layout change | — |

## 5. Phases

Three phases. One PR each, from `main` after the previous merge. Phase 3 may produce no code.

### Phase 1 — the title boost

**Goal:** a document whose title names the topic outranks documents that mention it.

**Files:** `src/score.rs`, `src/search/mod.rs`, `src/config.rs`, `tests/title_boost.rs` (new),
`tests/golden_search.rs`, `tests/hybrid_search.rs`, `tests/eval_quality.rs` (options).

**Build (~180 logic lines):**

1. `SearchConfig.title_boost` with the default and the clamp, plus a warning outside `[0, 2]`.
2. `tokenize` the document title once per document per query, and collect the distinct query terms once per query.
3. Compute `title_cov` and `title_boost`, pass it in `ScoreInput`, multiply `relevance`.
4. Report `title_boost` in `Breakdown`.
5. Add the identity to the reported fields and to the golden assertion.

**Tests (~250 lines):**

- **The reproduction, written first.** A fixture with one long document whose title names the topic and whose 8 sections mention it once each, plus 10 short notes that mention it once. Without the boost the long document is outside the top 5; with `w = 1.0` it is rank 1. This test fails on `main`.
- The formula table, with literals: hits 0, 1 of 1, 1 of 2, 2 of 2, 1 of 3.
- `w = 0.0` reproduces the pre-change ranking, compared against a golden taken before the change.
- A title with no query term gets exactly 1.0.
- An empty query gets exactly 1.0.
- The blend identity holds within `1e-12` on the hybrid golden.
- The cost: 2 119 title tokenizations per query, printed by the eval, never asserted.

**Handoff (paste):** "Implement phase 1 of M4C per `docs/m4c-breakdown.md` §3.1–§3.3 and §5. Query time only: no index rebuild, no format bump. `w = 0.0` must reproduce today's ranking exactly. Write the reproduction test first and show it failing on `main`. Write comments in Simplified Technical English."

**Accept:** gates green. The reproduction test passes. `w = 0.0` reproduces the old order. No golden moves except the additive field.

### Phase 2 — neutral metadata

**Goal:** a document with an unknown type or status is not punished for it.

**Files:** `src/config.rs`, `src/score.rs`, `src/search/facts.rs` if its note changes, the goldens,
`docs/m4c-breakdown.md` §11, `SPEC.md`.

**Build (~40 logic lines):** unknown type `0.80 → 1.00`; unknown status under `current` `0.80 → 1.00`. Keep both notes. Update the per-intent table's data, not the code that reads it.

**Tests (~120 lines):** the intent table's cells for `unknown` at factor 1.0; a document with unknown type and status ranks as its relevance says; the two notes still appear.

**Accept:** gates green; goldens move only where an unknown-type or unknown-status document is in the results.

### Phase 3 — measure, then decide

**Goal:** the numbers decide whether cause 2 is worth fixing.

**Files:** `docs/m4c-breakdown.md` §12, and code only if the numbers demand it.

**Work:**

1. Run the eval at `w = 0.0` and `w = 1.0`, α = 0 and α = 0.5. Record `active top-1`, `nDCG@10`, and the elapsed time.
2. Run the three real queries against the author's vault. Record the rank of the target document.
3. If the target is still outside the top 5, write the smallest change that scores documents by more than their best chunk, and record it as a decision with the number that justifies it.
4. If it is inside the top 5, record that, and close the question.

**Accept:** the numbers are in §12 with their corpus named. A quality regression is impossible to miss: the fixture eval must not fall below its M4 value (α = 0: 0.9590; α = 0.5: 0.9694).

## 6. Cross-cutting

- **Determinism.** No wall clock. The boost is a pure function of the query and the snapshot.
- **No format change.** No rebuild is required by M4C.
- **Explainable.** `title_boost` is reported with every other factor.
- **Small.** Two code PRs, both under 300 logic lines, plus one measurement.

## 7. Definition of done

1. A document whose title names the topic outranks ten short documents that mention it. The test exists and failed before the change.
2. `w = 0.0` reproduces the pre-change ranking, field for field.
3. `relevance == (α·semantic + (1−α)·lexical) · title_boost` holds within `1e-12`.
4. An unknown type and an unknown status score 1.0, with their notes.
5. The fixture eval does not fall below its M4 value at either α.
6. No `format_version` change, no rebuild, no new dependency.
7. Gates: fmt, clippy, tests, doc, Windows cross-check, `scripts/pr-size.sh`.
8. **Milestone acceptance, by the author:** `search "what is our q4 okrs?"` returns the Q4 plan
   draft in the top 5, and `search "q4-2026-plan-draft"` returns it first.

## 8. PR plan and size budget

| PR | Branch | Logic | Tests | Content |
|---|---|---|---|---|
| P1 | `m4c-title-boost` | ~180 | ~250 | the boost, the reported factor, the reproduction test |
| P2 | `m4c-neutral-metadata` | ~40 | ~120 | unknown type and status at 1.0 |
| P3 | `m4c-measure` | ~0–80 | ~0–80 | the measurement, and a change only if the numbers demand it |

## 9. SPEC corrections

1. **§7.2** — `relevance` gains the title boost. State the formula and the default weight.
2. **§7.2** — a document is still scored by its best chunk; if phase 3 changes it, that edit lands then.
3. **§6.6** — an unknown type is neutral, not 0.80. Replace the "fail-low" sentence with the reason.
4. **§6.6** — an unknown status under `current` is neutral, not 0.80.
5. **§16.1** — the `[search]` block gains `title_boost`.
6. **§9.4** — the `scores` example gains `title_boost`.
7. **§11** — add M4C between M4B and M5.

## 10. Implementation log — Phase 1

_Nothing recorded yet._

## 11. Implementation log — Phase 2

_Nothing recorded yet._

## 12. Implementation log — Phase 3

_Nothing recorded yet._
