# M4C — Retrieval that finds the document you meant

> Branch: `m4c-plan` (this document), then one branch per phase from `main`.
> Spec: §6.6, §7.2, §9.4, §11, §15, §16.1.
> Evidence: a corpus of 2 119 documents, counted by `janus scan`, measured in M4B.
>
> **Done when:** a document whose *title* names the topic outranks documents that merely mention
> it, and a document with metadata the config does not know is not punished for it.

**Words used here.** **title**: the document title, from frontmatter or the file stem.
**relevance**: the blended semantic and lexical score. **final**: relevance × authority × decay.
**idf**: inverse document frequency, the index's measure of how rare a term is.
**boost**: a multiplier of 1.0 or more.

## 0. Scope

**Decision: a document that is *about* a topic must beat a document that *mentions* it.**

Measured on the real corpus, with the query `what is our q4 okrs?`:

| fact | value |
|---|---|
| the document the user wanted | `1.martin/planning/q4-2026-plan-draft.md` |
| documents `janus scan` reports | 2 119 |
| its mentions of `q4` | 17, across 18 chunks |
| its rank for `q4` | **27** |
| its rank for its own file name | **miss, below 5** |
| documents containing `q4` | 152 |
| its type and status | `personal-operating-plan`, `unknown`, both unknown to the config |
| its multiplier from authority × status | **0.64**, against 0.90 for a note |

Three causes:

1. **A title match counts the same as a body mention.** The index concatenates the title to the body as one more string.
2. **A document's many mentions collapse into one.** A document is scored by its best chunk, so 17 mentions across 18 sections count as one.
3. **Unknown metadata is a penalty.** Unknown type ×0.80, unknown status ×0.80, so 0.64 against 0.90.

### In scope

- A title boost at query time, weighted by how rare the matched query terms are.
- Neutral treatment of an unknown type and an unknown status.
- Measurement before and after, with a knob to turn the boost off.
- A decision gate on cause 2. Fix it only if causes 1 and 3 are not enough.

### Not in scope

- Breadcrumb weighting. It needs index-time support to stay cheap per query (C-2).
- Changing "a document's score is its best chunk". Deferred behind phase 3's measurement (C-5).
- Stemming, stopwords, synonyms, query expansion. The idf weighting in §3.1 makes a stoplist unnecessary.
- Staleness detection, incremental refresh, progress output. Those are M4D.
- Any change to the index format. **No `format_version` bump, no rebuild.**
- A second index, a new file, a new dependency.

## 1. Dependencies

None.

## 2. Data model

```rust
// src/score.rs — one new reported factor.
pub struct Breakdown {
    /* … unchanged … */
    /// The title multiplier applied to `relevance`. `1.0` when no rare query term is in the title.
    pub title_boost: f64,
}

// src/config.rs — a new corpus table, beside `[embedding]` and `[chunking]`.
pub struct ScoringConfig {
    /// Weight of the title boost. `0.0` disables it and reproduces the old ranking.
    pub title_boost: f64, // default 1.0, clamped to [0.0, 1.0]
}
```

`Breakdown` gains one field, so **both** `search --json` and `facts --json` gain one key: each reports
the whole `Breakdown`. The change is additive; `api` stays `1`.

The corpus table is `[scoring]`, not `[search]`. A `[search]` table already exists in the *workspace*
config (§16.2) and holds federation policy; two tables with one name and two owners is a trap.

## 3. Contracts

### 3.1 The title boost, weighted by rarity

**A result whose title contains rare query terms is multiplied by a factor between 1.0 and 2.0.**

Read the query terms and the title terms with the one tokenizer. Take idf from the index that the
query already uses.

```
Q      = distinct query terms
T      = distinct title terms
V      = terms present in the index          (a term outside V has no idf)
hits   = Σ idf(t) for t in Q ∩ T ∩ V
total  = Σ idf(t) for t in Q ∩ V
title_cov   = if total > 0 then hits / total else 0
title_boost = clamp(1 + w · title_cov, 1.0, 2.0)      w = [scoring] title_boost, default 1.0
relevance   = (α · semantic + (1 − α) · lexical) · title_boost
```

**Why idf, and not a count.** A count divides by every query term, and a question carries noise:
`what is our q4 okrs` has five terms, so one title match is `1/5 = 0.2`, a boost of only 1.2. Rare
terms carry the meaning, and the index already knows which ones are rare. The same query with
plausible idfs (`what` 0.3, `is` 0.2, `our` 0.4, `q4` 2.6, `okrs` 2.9) gives:

| title contains | hits / total | title_cov | boost at w = 1.0 |
|---|---|---|---|
| `q4` | 2.6 / 6.4 | 0.406 | **1.41** |
| `q4`, `okrs` | 5.5 / 6.4 | 0.859 | 1.86 |
| `what` only | 0.3 / 6.4 | 0.047 | 1.05 |
| nothing from Q ∩ V | 0 | 0.000 | **1.00** |

Only a title that carries **every** query term reaches 2.00. This row read `6.4 / 6.4` in the first
draft, which is wrong: `total` is the sum over all of `Q ∩ V`, not over the title's terms (P1-3).

The fourth row is why no stoplist is needed: a common word in a title buys almost nothing.

**The weight `w` lives in `[0, 1]`.** At `w = 1.0` the largest possible boost is ×2.0, on a title that
carries every rare query term. The clamp keeps a caller from inventing ×5. A configured value outside
`[0, 1]` is **tested before it is clamped**, so the warning fires; the value used is the clamped one.

**`w = 0.0` reproduces the old ranking.** Multiplying by 1.0 is exact in IEEE 754 and constant for
every chunk of one document, so the argmax and the comparator see identical inputs.

**A date prefix in the title is stripped, at query time only.** A file named
`2024-12-01-redis-cache.md` has the title `2024-12-01-redis-cache`, which tokenizes to `2024`, `12`,
`01`, `redis`, `cache`. Without the strip, a query containing `2024` doubles every dated document in
the vault.

The strip happens **inside the boost path**, on the title text, immediately before tokenizing it. It is
never applied to the stored title. The stored title feeds `DocId` (rungs 3–5), `lex_key`, `vec_key`,
the embedded text and the reported `title`; stripping it there would move identities, invalidate every
reuse key, force the rebuild this milestone forbids, and move the goldens. A frontmatter title that
starts with a date is stripped too: the rule is about the token, not about its source.

The parser already exists in `src/vault/resolve.rs`. M4C adds one pure helper beside it,
`pub(crate) fn strip_leading_date(title: &str) -> &str`, and changes no stored value.

**Cost and site.** `query()` tokenizes each document title **once per query**, before the chunk loop,
and keeps a `Vec<f64>` beside `docs`. The chunk loop indexes it by `doc_index`. Do not tokenize inside
the loop: the loop runs per chunk, and the reference corpus is 200 000 chunks per §13.

### 3.2 Where the boost applies, and where it must not

**The boost multiplies `relevance`, before authority and decay.**

| stage | changes? |
|---|---|
| `semantic`, `lexical` | no. They keep their meaning and their reported values |
| `relevance` | yes, multiplied. The bound becomes `≤ 2.0` |
| `authority`, `status_factor`, `decay` | no |
| `final` | yes, through `relevance`. The bound becomes `≤ 3.0` |
| the total order, the tie-breaks, one-hit-per-document | no |

`facts` passes `title_boost = 1.0`. It orders by `type_authority`, so a boost cannot change its order,
and a factor that cannot change an order must not inflate a reported score.

### 3.3 Neutral metadata for words the config does not know

**An unknown type and an unknown status stop being penalties.**

| field | today | after |
|---|---|---|
| unknown type | ×0.80 | **×1.00** |
| unknown status under `current` | ×0.80 | **×1.00** |

Both notes stay. The type note prints `UNKNOWN_TYPE_AUTHORITY`, so its text follows the code. The
status note states that an unknown status is treated as active for inclusion, because `document.rs`
cannot read the table cell in `score.rs` without a module cycle. An unknown type is not evidence of
low authority; it is evidence of a vocabulary the config has not seen.

The intent table has **two** `current` rows, one with and one without `--include-superseded`. Both
change, or the flag keeps punishing an unknown status.

### 3.4 What does not change

- **No index format change.** The boost is computed at query time from `DocMeta.title`, which the
  snapshot already holds. No rebuild, no `format_version` bump.
- **A document is still scored by its best chunk.** Phase 3 decides whether to revisit that.
- **One hit per document.** The boost reorders; it never adds a result.

## 4. Decisions

| # | Question | Default | Where |
|---|---|---|---|
| C-1 | Query time or index time | **Query time.** Index-time duplication needs an 18-minute rebuild and a format bump for a weight | `search/mod.rs` |
| C-2 | Title only, or title and breadcrumb | **Title only.** Per-chunk breadcrumbs need a tokenized heading index to stay cheap | `search/mod.rs` |
| C-3 | The weight | `Σ idf` over the matched rare terms ÷ `Σ idf` over the query, `1 + w·cov`, clamp `[1,2]`, `w` default 1.0 in `[0,1]`. The idf lookup happens in `search/mod.rs`, because `score()` is pure and has no index | `search/mod.rs`, `score.rs` |
| C-4 | Is the boost reported | **Yes**, in `Breakdown`, and the bounds in the property test and SPEC move with it | `score.rs` |
| C-5 | Document-level aboutness | **Deferred** to phase 3's measurement | §5 phase 3 |
| C-6 | Unknown type and status | **Neutral (1.0)**, both `current` rows, notes keep working | `config.rs`, `score.rs` |
| C-7 | Regression switch | `w = 0.0`. A stored snapshot proves the old ranking returns | `config.rs` |
| C-8 | Format version | **Unchanged** | — |
| C-9 | Config table name | `[scoring]` in the corpus config. `[search]` belongs to the workspace config | `config.rs` |
| C-10 | A date in the title | Stripped **in the boost path only**, from the title text. A date is not a topic, and the stored title is identity | `resolve.rs` (helper), `search/mod.rs` |

## 5. Phases

Three phases. One PR each, from `main` after the previous merge. Phase 3 may produce no code.

### Phase 1 — the title boost

**Goal:** a document whose title names the topic outranks documents that mention it.

**Files:** `src/score.rs` (the factor, the bounds), `src/search/mod.rs` (precompute, report),
`src/search/facts.rs` (`title_boost: 1.0`, E0063 without it), `src/config.rs` (`[scoring]`),
`src/vault/resolve.rs` (one pure helper, `strip_leading_date`), `janus.toml` (`[scoring]`), `tests/title_boost.rs` (new),
`tests/golden_search.rs` (both identity assertions and the module comment), `tests/hybrid_search.rs`
(the identity at `:239`), `tests/eval_quality.rs` (`JANUS_EVAL_TITLE_BOOST`),
`tests/snapshots/` (a **new** pre-change snapshot; phase 1 regenerates the existing one),
`SPEC.md` (§7.2 formula and bound, §9.4 example, §16.1 table; P1-6).

**Build (~220 logic lines):**

1. `ScoringConfig.title_boost`: default 1.0, warn on a configured value outside `[0,1]`, then clamp.
2. `pub(crate) fn strip_leading_date(&str) -> &str` in `vault::resolve`, beside the date parser, and
   use it in the boost path. No stored value changes. Add `[scoring] title_boost = 1.0` to the
   `janus.toml` template so a fresh corpus shows the knob.
3. In `query()`: tokenize the query once; for each document strip its date prefix, tokenize the title
   once, and compute a `Vec<f64>` of boosts beside `docs`. Index it by `doc_index` in the chunk loop.
4. Multiply `relevance`; report `title_boost`.
5. Update the bounds: `relevance ≤ 2.0`, `final ≤ 3.0` in the property test's generator and assertions.
6. Update the three identity assertions named above.

**Tests (~280 lines):**

- **The reproduction, written first, in the real query shape.** Ten short notes that mention `q4` and
  `okrs` once, plus one long document titled `Q4 plan draft` whose eight sections mention `q4` once
  each. Query **`what is our q4 okrs`** — the acceptance query, with `okrs` spelled out and the
  question mark gone. Five terms; `q4` alone is in the title. Without the boost the long document is
  outside the top 5; with `w = 1.0` it is rank 1. **This test fails on `main`.**
  The expected boost is a literal with its arithmetic in the test comment: **five** `df` counts, five
  idfs, `hits`, `total`, `cov`, and the resulting boost. A literal copied from the plan's worked table
  would pin an idf distribution the test corpus does not have.

  **All five query terms must be in the test corpus' vocabulary.** A term missing from the index is
  excluded from `total`, so a corpus that lacks `our` gives a four-term denominator and a boost that is
  too high: a favourable case enshrined as the test. Write `our`, `what`, `is`, `q4` and `okrs` into the
  test documents, and assert the literal is **below 1.5**, so a saturated corpus cannot pass.
- The formula table above, with literals for all four rows.
- `w = 0.0` reproduces the stored pre-change snapshot: same order, same `final`, every pre-existing
  field identical. The new key is the only difference.
- A title with no rare query term scores exactly 1.0. An empty query scores exactly 1.0.
- A file-stem title `2024-12-01-redis-cache` gives boost 1.0 for the query `2024`.
- The blend identity `relevance == (α·semantic + (1−α)·lexical) · title_boost` within `1e-12`.
- The eval prints the number of title tokenizations it performed. Fixture: **19**, one per scored
  document. The first draft said 24, which is the number of Markdown files in the committed tree:
  config and the walk exclude 6 of them, one file is generated, and one generated file is not
  valid UTF-8 (P1-4).

**Handoff (paste):** "Implement phase 1 of M4C per `docs/m4c-breakdown.md` §3.1–§3.3 and §5. Query time
only: no rebuild, no format bump. The boost is idf-weighted, computed once per document per query in
`query()`, and indexed by `doc_index`. Update the two identity assertions and the property test's
bounds; the goldens move. `w = 0` must reproduce the stored pre-change snapshot. Write the
reproduction test first and show it failing on `main`. Comments in Simplified Technical English."

**Accept:** gates green. The reproduction test passes. `w = 0` reproduces the old ranking and scores.
The goldens move only in `scores.relevance`, `scores.final`, and the new `scores.title_boost`, and the
top-3 order does not change.

### Phase 2 — neutral metadata

**Goal:** a document with an unknown type or status is not punished for it.

**Files:** `src/config.rs` (`UNKNOWN_TYPE_AUTHORITY`, the doc comment), `src/score.rs` (both `current`
rows), `src/document.rs` (the note prints the configured factor, not a literal), `src/vault/resolve.rs`
(its `0.80` assertion), `janus.toml` (the comment on line 56), the goldens, `SPEC.md`.

**Build (~40 logic lines):** unknown type `0.80 → 1.00`; unknown status `0.80 → 1.00` in **both**
`current` rows. The type note keeps its number and reads `UNKNOWN_TYPE_AUTHORITY`, which lives in
`config.rs` and is importable. The status note **drops** its number: the factor is a table cell in
`score.rs`, and `document.rs` cannot import `score.rs` without a module cycle. Its new text says the
status is treated as active for inclusion.

**Tests (~120 lines):** the intent table's `unknown` cells at 1.0 for both rows; a document with an
unknown type and status ranks as its relevance says; both notes still appear and agree with the code.

**SPEC:** §6.3, §6.6 (two tables), §6.6.1, §9.2 — the four places that state 0.80.

**Accept:** gates green. Goldens move only for documents whose type or status is unknown.

### Phase 3 — measure, then decide

**Goal:** the numbers decide whether cause 2 is worth fixing.

**Files:** `docs/m4c-breakdown.md` §12, and code only if the numbers demand it.

**Work:**

1. Run the fixture eval at `w ∈ {0.0, 1.0}` and `α ∈ {0, 0.5}`, driven by the two env knobs. Record
   `active top-1`, `nDCG@10`, and elapsed time.
2. **Author-only, outside the PR gate:** run the three real queries against the 2 119-document corpus
   and record the rank of the target document.
3. If the target is still outside the top 5, write the smallest change that scores a document by more
   than its best chunk, and record the number that justifies it.
4. If it is inside the top 5, record that and close the question.

**Accept:** the numbers are in §12 with the corpus named. The fixture eval must not fall below its M4
value (α = 0: 0.9590; α = 0.5: 0.9694). A reviewer can re-run the fixture half; the real-corpus half is
the author's, as in M3 and M4B.

## 6. Cross-cutting

- **Determinism.** No wall clock. The boost is a pure function of the query and the snapshot.
- **No format change, no rebuild.**
- **Explainable.** `title_boost` is reported beside the other factors.
- **Small.** Two code PRs, both under 300 logic lines, plus one measurement.

## 7. Definition of done

1. A document whose title names the topic outranks ten short documents that mention it. The test
   exists and failed before the change.
2. `w = 0.0` reproduces the pre-change ranking and every pre-existing field.
3. `relevance == (α·semantic + (1−α)·lexical) · title_boost` within `1e-12`, and the property test
   bounds are `relevance ≤ 2.0`, `final ≤ 3.0`.
4. An unknown type and an unknown status score 1.0 in both `current` rows, with their notes.
5. The fixture eval shows the title boost helping against the same code with the boost off:
   `w = 1` must not be worse than `w = 0` at either α. The M4 numbers are not the baseline, because
   phase 2 changed the metadata factors (§12).
6. No `format_version` change, no rebuild, no new dependency.
7. Gates: fmt, clippy, tests, doc, Windows cross-check, `scripts/pr-size.sh`.
8. **Milestone acceptance, by the author, outside the PR gate:** `search "what is our q4 okrs?"`
   returns the Q4 plan draft in the top 5, and `search "q4-2026-plan-draft"` returns it first.

## 8. PR plan and size budget

| PR | Branch | Logic | Tests | Content |
|---|---|---|---|---|
| P1 | `m4c-title-boost` | ~220 | ~280 | the idf-weighted boost, the reported factor, the bounds, the reproduction test |
| P2 | `m4c-neutral-metadata` | ~40 | ~120 | unknown type and status at 1.0, both rows, the notes, four SPEC places |
| P3 | `m4c-measure` | ~0–80 | ~0–80 | the sweep knob, the measurement, a change only if the numbers demand it |

## 9. SPEC corrections

1. **§7.2** — `relevance` gains the title boost. State the idf-weighted formula and the default weight.
2. **§7.2** — the hard bound becomes `final ≤ 1.5 · 1.0 · 1.0 · 2.0`. The property test asserts it.
3. **§7.2** — a document is still scored by its best chunk; if phase 3 changes it, that lands then.
4. **§6.6** — an unknown type is neutral (1.0), not 0.80. Replace the "fail-low" sentence with the reason.
5. **§6.6** — an unknown status under `current` is neutral (1.0), not 0.80. Both rows.
6. **§6.3, §6.6.1, §9.2** — the four other places that state 0.80.
7. **§16.1** — the corpus config gains a `[scoring]` block with `title_boost`.
8. **§9.4** — the `scores` example gains `title_boost`.
9. **§11** — add M4C between M4B and M5.

## 10. Implementation log — Phase 1

**Status: implemented.** Logic 176 lines, tests 758 lines, 934 total (`scripts/pr-size.sh`). The
test count includes the new 127-line pre-boost snapshot; the budget excludes Markdown.

| # | Decision | Why | Where |
|---|---|---|---|
| P1-1 | The pre-boost ranking is a **new** snapshot file, `search_pre_title_boost` | The default-path goldens must move (`relevance`, `final`, `title_boost`), so one file cannot hold both. The file was generated from the build before the change | `tests/golden_search.rs` |
| P1-2 | `w = 0.0` is proven byte-for-byte | The weight-zero report equals the stored pre-change snapshot after the new key is removed. Multiplying by `1.0` is exact, so the argmax and the comparator see the same inputs | `tests/golden_search.rs` |
| P1-3 | The plan's §3.1 table row `q4, okrs` was arithmetically wrong | `6.4 / 6.4` was printed; the value is `5.5 / 6.4 = 0.859`, boost 1.86. The table is corrected above; the unit test uses the correct literals | §3.1, `src/search/mod.rs` |
| P1-4 | The eval's title-tokenization count is **19** on the fixture, not 24 | 24 is the Markdown file count. Config and the walk exclude 6, one generated file is added, one generated file is unreadable. The count is asserted, not just printed | `tests/eval_quality.rs`, `tests/title_boost.rs` |
| P1-5 | The reproduction corpus needs 20 filler notes | Without them `q4` is in every chunk, its idf collapses to 0.03, and a boost bounded at 2.0 cannot move a rank. The filler gives the realistic idf balance: `df(q4) = 18` of 38 chunks | `tests/title_boost.rs` |
| P1-6 | The SPEC corrections that the formula forces land here: §7.2, §9.4, §16.1 | The phase-1 file list omitted `SPEC.md`, but the formula, the bound, the reported key and the config table are all wrong without them. §11's milestone entry is not phase-1 work | `SPEC.md` |
| P1-7 | The eval knob is `JANUS_EVAL_TITLE_BOOST`, default 1.0 | Phase 3 needs to measure `w ∈ {0.0, 1.0}`. Like alpha, it is read at the test edge and set on the config the scorer already reads | `tests/eval_quality.rs` |
| P1-8 | `Outcome.title_tokenizations` reports the count | The eval must print a measured number, not infer it from `selected.len()`. The counter increments where the tokenizer runs | `src/search/mod.rs` |

**Evidence.**

| Claim | Evidence |
|---|---|
| The reproduction failed before the change | `assert_eq!(left: Some("notes/mention-08.md"), right: Some("plans/q4-plan-draft.md"))`; the plan was 11th |
| The reproduction passes after | `the_document_about_q4_outranks_the_documents_that_mention_it` |
| The boost literal is unsaturated | `1.43130972481946550`, below 1.5 |
| `w = 0` reproduces the old ranking | The weight-zero report equals the pre-change snapshot byte for byte |
| The blend identity holds | `relevance == (α·semantic + (1−α)·lexical) · title_boost` in both goldens, within `1e-12` |
| A year query ignores a title date | `a_year_query_does_not_match_a_leading_date_in_the_title`, boost exactly 1.0 |
| An empty query boosts nothing | `an_empty_query_or_a_term_outside_the_index_boosts_nothing` |
| Cost is per document | 19 tokenizations over 1 533 chunks on the fixture |
| The eval does not regress at α = 0 | `nDCG@10 0.9590`, `active top-1 1.00 (22/22)`, as in M4 |
| No format change | `format_version` untouched; no rebuild is required |

## 11. Implementation log — Phase 2

**Status: implemented.** Logic 38 lines, tests 191 lines, 229 total (`scripts/pr-size.sh`).

| # | Decision | Why | Where |
|---|---|---|---|
| P2-1 | The status note states the inclusion rule, not a factor | The factor is a table cell in `score.rs`, and `document.rs` cannot import it without a module cycle. §3.3 said both notes print the factor; that holds for the type note only | §3.3, `src/document.rs` |
| P2-2 | The type note formats `UNKNOWN_TYPE_AUTHORITY` with two decimals | `1.00` keeps the old note's shape and still follows the constant if it changes | `src/document.rs` |
| P2-3 | The note text moves only the scan and chunks goldens | No search golden result carries the fixture's unknown-metadata document, so the ranking goldens are unchanged | `tests/snapshots/` |
| P2-4 | The doc comments that stated `0.8` move with the table: `is_fact` and the facts module | A stale number in a doc comment contradicts the code it describes | `src/score.rs`, `src/search/facts.rs` |

**Evidence.**

| Claim | Evidence |
|---|---|
| The reproduction failed before the change | `assert_eq!(unknown.scores.status_factor, 1.0)` saw `0.8`; the status note still said `using the fallback factor 0.8` |
| The reproduction passes after | `an_unknown_type_and_status_rank_as_their_relevance_says` |
| Unknown type and status are 1.0 in both `current` rows | The 25-cell table test; both `current` rows changed |
| The unknown document outranks a plain note | The two documents have equal relevance and decay; `final` ratio is `1.00 / 0.90 = 1.111…` |
| Both notes still appear and agree with the code | `both_unknown_metadata_notes_state_the_rule_the_code_uses`, plus the scan and chunks goldens |
| No ranking golden moved | Only the two note messages changed in `golden_scan` and `golden_chunks`; both search goldens are unchanged |
| No format change | `format_version` untouched; no rebuild is required |

## 12. Implementation log — Phase 3

Branch: `m4c-p3-measure`. No production code. The measurement, and one amendment to DoD 5.

### The fixture eval, both knobs

Real model, `FixedClock(2025-04-01)`, 19 documents, 1 533 chunks, `JANUS_EVAL_TITLE_BOOST` and
`JANUS_EVAL_ALPHA` set at the test edge.

| weight `w` | α | active top-1 | nDCG@10 | 1 533-chunk sweep |
|---|---|---|---|---|
| 0 | 0.0 | 0.95 (21/22) | 0.9484 | 287 ms |
| 0 | 0.5 | 0.95 (21/22) | 0.9461 | 552 ms |
| **1** | **0.0** | **1.00 (22/22)** | **0.9590** | 284 ms |
| **1** | **0.5** | **1.00 (22/22)** | **0.9566** | 557 ms |

**Like for like, the title boost helps.** With the same code and only the knob changed, `w = 1` is
better at both α: top-1 `21/22 → 22/22`, nDCG@10 `+0.0106`. That is the clean comparison, and it is
what DoD 5 now asks for.

**The M4 numbers are not the baseline.** M4 recorded α = 0: 0.9590 and α = 0.5: 0.9694. Phase 2 changed
the metadata factors, so `w = 0` now scores 0.9484 and 0.9461: the unknown-type and unknown-status
documents in the fixture (`speculative`, `frozen`) stop being penalised. That is a −0.0106 and −0.0233
change from a rule we chose on purpose.

Net against M4 at the shipped default (`w = 1`): α = 0 is unchanged at 0.9590, and α = 0.5 is 0.9566,
2.8 points lower, with top-1 still 22/22 in both. The fixture has 24 documents, so one document moving
shifts nDCG by several points; the drop is reordering at ranks 2–10, not a lost answer.

### Decisions taken

| # | Decision | Why |
|---|---|---|
| P3-1 | DoD 5 compares `w = 1` against `w = 0` on the same code, not against M4 | Phase 2 changed the metadata factors on purpose. Comparing across it would measure two changes as one |
| P3-2 | The fixture's `speculative` type and `frozen` status stay unknown to the config | Declaring them in the fixture would tune the corpus to the metric. The real question is whether the author's vault ranks better, and that is measured on the vault |
| P3-3 | No document-level aboutness change in M4C | The author's two commands decide that, not the fixture. Cause 2 stays open with its number |

### Milestone acceptance on the author's corpus

2 119 documents, real model, default settings, `--no-rebuild`. The target is the author's Q4 plan draft.

| query | rank before M4C | rank after | score after |
|---|---|---|---|
| `q4-2026-plan-draft` | miss, below 5 | **1** | 1.0899 |
| `what is our q4 okrs?` | miss, below 50 | **1** | 0.5948 |

The second query's runner-up is 0.5503, so the margin is real and not a tie-break. In the first query
the document was outside the top 50; it is now first, ahead of the OKR concepts note and the board
synthesis for the same plan.

The result set also changed in kind: before, it was documents that mention "q4" once; now it is the
plan, its supporting notes, and the standing review that refers to it. That is the difference between
matching a word and finding a document.

**Cause 2 is closed.** Seventeen mentions across eighteen chunks did not need document-level
aboutness once the title counted and the metadata stopped punishing the document (P3-3).

### What is not measured here

Nothing. The two ranks above are the milestone's acceptance.
