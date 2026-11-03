# M4B — Works on a vault you did not prepare

> Branch: `m4b-plan` (this document), then one branch per phase from `main`.
> Spec: `SPEC.md` §6.3 (frontmatter contract), §6.4 (identity), §6.10 (valid time), §7.1
> (intents), §7.2 (scoring), §9.4 (response envelope), §11, §15.
> M4 evidence: `docs/m4-breakdown.md` §14.
>
> **Milestone statement.** A new user points janus at a folder of Markdown. That folder has no
> frontmatter, no dates, and no types. Janus must still give stable, useful results.
> **Done when:** the same vault gives the same ranking on two machines, and every result shows
> where its date came from.

## Words used in this document

- **frontmatter**: the metadata block at the top of a Markdown file.
- **mtime**: the time the operating system last changed a file.
- **decay**: a multiplier that makes an old document score lower.
- **ranking**: the order of search results.
- **snapshot**: the index cache that janus builds from the vault.

## 0. Scope

### Why this milestone exists

M4 was tested against a real vault. The vault is a zip download of a repository. It has 2 929
Markdown files. Results:

| measurement | value |
|---|---|
| documents with a frontmatter date | 163 (5.6%) |
| documents with a date in the file name | 921 (31%) |
| documents that get a date from mtime | 1 035 |
| documents of type `decision` | 0 |
| documents with status `superseded` | 1 |

Three problems follow.

1. **The ranking is not reproducible.** The mtime is a property of the disk. A zip download
   sets one extraction time on the files. Two downloads give two values. Decay and the date
   tie-break read the date. So the same vault gives two different orders.
2. **Most documents have no real date.** A user will not add frontmatter to 2 929 files. The
   tool must work without it.
3. **The output hides its assumptions.** A result shows a date. It does not show where the
   date came from. The user cannot tell a written date from a disk timestamp.

### In scope

- A date ladder that uses only authored or name-derived dates: frontmatter, then file name,
  then folder name, then no date.
- Month precision (`2026-08`) and year precision (`2026`) in file and folder names.
- The rule: a value from mtime never scores and never sorts. The mtime leaves the date ladder.
- A document with no date never decays. It is treated as always current.
- Provenance in the output: `date_source` on each result, and one `notes` line when results
  have no date.
- `janus facts` explains an empty result instead of printing a near-empty list.
- A test that proves the fix: two copies of one vault, one with shifted mtimes, must give
  identical result lists.

### Not in scope

- The supersession graph, `--as-of`, `effective_status`, scope-relative windows (M5).
- `janus doctor` (M5). The provenance goes in the search output, where the user looks.
- Guessing a type from content. Types stay `[paths]` hints plus the `note` default.
- Any write to the vault. M7 owns writes.
- A `date_precision` field separate from `date_source`. The source variant carries it (B-3).

## 1. Dependencies

None. No new crate. No feature flag.

## 2. Data model

```rust
// src/document.rs — the date ladder's result, which also records precision.
pub enum DateSource {
    Frontmatter,      // a date the user wrote
    Filename,         // YYYY-MM-DD at the start of the file name
    FilenameMonth,    // YYYY-MM at the start of the file name
    FilenameYear,     // YYYY at the start of the file name
    Folder,           // a date in a folder name, day precision
    FolderMonth,      // a date in a folder name, month precision
    FolderYear,       // a date in a folder name, year precision
    None,             // no authored or name-derived date: the document is always current
    // Mtime is removed. See B-1.
}

// src/index/mod.rs — one new persisted field.
pub struct DocMeta {
    /* … unchanged … */
    /// Where `date` came from. Persisted so a result can explain itself (§3.5).
    pub date_source: DateSource,
}
```

`DateSource` must derive `Serialize` and `Deserialize`. `DocMeta` gains the field, so
`format_version` becomes **3**. An M4 index is refused and rebuilt. This is automatic and
needs one warning line, as in M4 (`V-1`).

## 3. Contracts

### 3.1 The date ladder

Janus reads a date in this order. The first match wins.

1. The frontmatter date key.
2. The file name, as a whole prefix:
   - `YYYY-MM-DD` → day precision;
   - `YYYY-MM` → month precision;
   - `YYYY` → year precision.
3. A folder name, same three shapes. Janus uses the **deepest** folder that has a date.
4. No date.

A date token must be a whole prefix. The separator set is `-`, `_` and `.`, both inside a date
token and immediately after it. So `2026-08-22_notes.md` matches, and `2026-08-22notes.md` does
not. This rule already exists for `YYYY-MM-DD`. M4B applies it to the new shapes and separators.

A longer complete shape wins. A day token starts when a separator follows a valid month and a
digit or the end follows that separator. A separator followed by text closes the month token. A
day token must be a valid day; it does not fall back to a month. So `2024-11-0-post.md`,
`2026-08-1_notes.md`, `2026-08-22x.md`, `2026-08_22x.md`, `2026-13-01.md` and `2026_13_01.md`
produce no date. `2026_08_notes.md` stays at month precision, and `2024_11_02_notes.md` is day
precision (P1-2, P1-3, P1-9, P1-10). The check runs before the ladder tries a shorter shape.

A year must be between 1970 and 2099. A name like `1.0` is a version. A name like `2026` at the
start of a file name is a date. The range check rejects the common false positives.

**The mtime is not in this ladder.** A value that changes between two copies of one vault must
not affect a result.

### 3.2 The decay anchor

A date with day precision is the anchor. A month or year date uses the **start of its period**:
the first day of the month, or the first day of the year.

Why the start: janus must never make an old document look newer than it is. A `2024` document
is at least as old as 2024-01-01. Decay from that day is the safe value.

A document with `DateSource::None` has no anchor. Its decay factor is **1.0**. So it never
loses score for age. It is always current. This is correct: a note with no date might be
timeless, and janus must not invent an age for it.

### 3.3 The rule at its three call sites

A mtime value no longer exists after M4B. The rule is still stated once, because the failure
mode returns if anyone adds a date source that is not authored:

> Only an authored or name-derived date may change a score or an order.

The three places that read a date today:

| place | today | after M4B |
|---|---|---|
| decay | `effective_from`, else `date` | `effective_from`, else `date`; `None` means factor 1.0 |
| document tie-break | `date` desc, undated last | unchanged, but there are more undated documents |
| `--intent evolution` order | oldest to newest by date | unchanged; undated documents go last |

### 3.4 Identity is unchanged

`usable_date` in `src/vault/identity.rs` already returns `None` for `DateSource::Mtime`. So no
`DocId` moves because of this milestone. M4B may simplify that function, but it must not change
its output for any existing document. A test asserts this over the fixture vault.

### 3.5 Provenance in the output

Each search and facts result gains one field:

```json
"date": "2026-08-22",
"date_source": "filename"
```

The values are `frontmatter`, `filename`, `filename_month`, `filename_year`, `folder`,
`folder_month`, `folder_year`, and `absent`. The field is additive. `api` stays `1`.

When one or more returned documents have `date_source: "absent"`, the report adds one line to
`notes`:

> `N result(s) have no date and do not decay`

The line states a fact about the results. It does not ask the user to change a file.

### 3.6 `janus facts` with no facts

`facts` includes only documents of type `decision`, `spec`, or `principle`, with declared status
`active`. A cold-start vault has none. Today the command prints a short list and no reason.

After M4B, an empty or short list adds one note that names the cause:

> `no document declares type decision, spec or principle; showing the newest documents instead`

The fallback list has the same inclusion rule as `search --intent current` and the same sort,
newest first. It is not a new result contract: the command reports what it found, and it says
what it could not find.

## 4. Decisions

| # | Question | Default | Where it lands |
|---|---|---|---|
| B-1 | Does mtime stay in the ladder? | **No.** It leaves. A value that differs between two copies of one vault is not a property of the document, and showing it invites trust. | `src/vault/resolve.rs` |
| B-2 | Which date shapes? | `YYYY-MM-DD`, `YYYY-MM`, `YYYY`, in file and folder names, as a whole prefix, year 1970–2099. | `src/vault/resolve.rs` |
| B-3 | How is precision recorded? | In `DateSource` variants, not a second field. One value answers both "where" and "how precise". | `src/document.rs` |
| B-4 | Anchor for a coarse date | The **start** of the period. Janus must never make a document look newer than it is. | `src/score.rs` |
| B-5 | What does an undated document do | Decay **1.0**, sorted as undated, and reported. It does not guess. | `src/score.rs` |
| B-6 | Where does provenance appear | `date_source` on each result, plus one `notes` line. Not in a new command. | `src/search/mod.rs` |
| B-7 | Format version | **3**. `DocMeta` gains a field; a v2 index is refused and rebuilt with one warning. | `src/index/mod.rs` |
| B-8 | Folder date depth | The deepest folder with a date wins. A nearer date is more specific than a further one. | `src/vault/resolve.rs` |

## 5. Phases

Two phases. Each is one PR from `main` after the previous merge.

### Phase 1 — the date ladder and the rule

**Goal:** the ranking no longer depends on the disk, and the two-copy test passes.

**Files:** `src/vault/resolve.rs`, `src/document.rs`, `src/vault/identity.rs`,
`src/vault/frontmatter.rs`, `src/index/mod.rs`, `src/index/build.rs`, `src/index/store.rs`,
`src/score.rs`,
`tests/date_ladder.rs` (new), `tests/common/mod.rs`, `tests/fixture_scan.rs`, `tests/cli_scan.rs`,
`tests/cli_index.rs`, `tests/publish.rs`, `tests/snapshots/golden_scan__scan.snap`, and this plan.
`tests/no_wall_clock.rs` only if the new file needs it.

**Build (~250 logic lines):**
1. Extend `DateSource` with the six new variants and remove `Mtime`. Keep `None`.
2. Generalise `filename_date` into `date_prefix(name) -> Option<(NaiveDate, DateSource)>`. It
   tries day, then month, then year. It keeps the whole-prefix rule and adds the year range.
3. Add `folder_date(rel_path)`, which walks the folder components from the deepest to the root
   and returns the first match. It uses the same prefix parser.
4. Change `resolve_date` to the ladder in §3.1. Remove the mtime argument from the call chain
   if nothing else uses it.
5. Add `DocMeta.date_source`, set it in `build::plan`, and raise `format_version` to 3.
6. `score::decay`: when there is no anchor, return 1.0. It already does this for a missing
   date; add a test that an undated document is never penalised.
7. `cmp_date_desc` and the evolution order already put undated documents last. Add tests; do
   not change the comparator unless a test shows a defect.

**Tests (~300 lines):**
- The ladder table: one row per rung, including folder and the three precisions. Each expected
  date is a literal.
- Prefix rules: `2026-08-22_x.md` matches; `2026-08-22x.md` does not; `1.0.md` does not;
  `0000-01-01.md` is rejected by the year range; `2026-13-01.md` is rejected by the parser. All
  three separators (`-`, `_`, `.`) work at both positions. An incomplete day such as
  `2024-11-0-post.md` or `2026-08-1_notes.md` has no date.
- Folder dates: `2026.08.22_notes/x.md`, `2026-08/x.md`, `2026/x.md`; the deepest folder wins;
  a file-name date beats a folder date.
- M4B's own regression: an undated document has decay 1.0 and never loses to a dated document
  on age alone.
- Identity: no `DocId` changes over the fixture vault (§3.4).
- **The two-copy test.** Build a vault twice. Shift every mtime in copy B with
  `filetime`-free code: write each file again, or set the mtime with `std::fs` where the test
  can. Assert that `search --json` returns the same `rel_path` order and the same `final`
  scores. This test fails on `main` and passes after the phase.
- `format_version = 2` is refused with one warning and rebuilt.

**Handoff (paste):** "Implement Phase 1 of M4B per `docs/m4b-breakdown.md` §3.1–§3.4 and Phase 1.
The mtime leaves the date ladder. Add month and year precision as `DateSource` variants, and
folder dates. `DocMeta` gains `date_source`, so `format_version` becomes 3. Do not touch search
output in this phase. Write the two-copy test first and show it failing on `main`."

**Accept:** all gates green; the two-copy test passes; no `DocId` moved; `format_version = 3`.

### Phase 2 — provenance, `facts`, docs

**Goal:** the user can see where a date came from, and `facts` explains an empty result.

**Files:** `src/search/mod.rs`, `src/search/facts.rs`, `src/vault/resolve.rs` (one predicate),
`src/scan.rs`, `tests/cli_search.rs`, `tests/cli_scan.rs`, `tests/golden_search.rs`,
`tests/golden_scan.rs` and its snapshot, `docs/m4b-breakdown.md` §11, `SPEC.md`, `README.md`.

**Build (~120 logic lines):**

1. `date_source` in `HitDto` and `FactDto` (both render through `search::render`, so the field
   is added once).
2. The `notes` line from §3.5.
3. The `facts` note and fallback from §3.6.
4. **Added to this phase after the plan was written.** A near-miss count in `scan`. A name
   that starts with a four-digit year and a separator, then a month or a day that does not
   parse, is refused by the date ladder. Those names are counted, and `scan` reports them in
   one line and in its JSON. Why: a vault whose names are written day-first gets no date at
   all, and silence leaves the user with no way to find out why. There is no date-order
   setting. ISO order is the decision, and this line is how a user discovers that their vault
   differs. The count needs one predicate beside the parser; the ladder and `parse_prefix`
   do not change (P2-5, P2-6).

**Tests (~200 lines):** the JSON field for each source value; the `notes` line appears only
when an returned document is undated; `facts` on a vault with no fact types prints the note and
the newest documents; `scan` counts the near-miss names and omits the line on an ISO corpus;
the goldens move only by the new field and the note.

**Docs:** SPEC §6.3 (the ladder), §6.10 (mtime is not a valid-time source), §9.4 (`date_source`
in the example), §11 (add M4B), §15 (one row: mtime is never a date). README: one paragraph on
what janus derives and what it refuses to guess.

**Handoff (paste):** "Implement Phase 2 of M4B per §3.5, §3.6 and Phase 2. `date_source` is
additive, so `api` stays 1. The `notes` line states a fact; it does not ask the user to edit
files. `facts` explains an empty result and shows the newest documents of any type."

**Accept:** `search --json` and `facts --json` show `date_source`; both goldens reviewed field
by field; SPEC and README match the code.

## 6. Cross-cutting

- **Determinism.** No wall clock in the library. `resolve_date` no longer reads the filesystem
  time, so the resolver becomes pure. That is a side benefit of B-1.
- **Reads fail open.** A name janus cannot parse is not a date, and that is not an error. It is
  a missing date.
- **No writes.** M4B never changes a vault file.
- **No panics.** Every parser uses `get` and `ok`, never slicing.
- **Small.** Two PRs, both under 300 logic lines.

## 7. Definition of done

1. `touch`-ing every file in a vault does not change any result order or score. The test exists
   and failed before the change.
2. A document with no date has decay 1.0, is reported as `absent`, and is not sorted above a
   dated document on a tie.
3. `2026-08-22_notes.md`, `2026-08_notes.md`, `2026_notes.md` and the folder equivalents each
   produce the date and precision in §3.1.
4. `2026-08-22x.md` and `1.0.md` produce no date.
5. No `DocId` changes over the fixture vault.
6. `format_version = 3`; a v2 index is refused with one warning and rebuilt.
7. `search --json` and `facts --json` carry `date_source`; the `notes` line appears when a
   result has no date.
8. `facts` on a vault with no fact types explains why and shows the newest documents.
9. Gates: fmt, clippy, tests, doc, Windows cross-check, `scripts/pr-size.sh`.

## 8. PR plan and size budget

| PR | Branch | Logic | Tests | Content |
|---|---|---|---|---|
| P1 | `m4b-date-ladder` | ~250 | ~300 | the ladder, the variants, the format bump, the two-copy test |
| P2 | `m4b-provenance` | ~120 | ~200 | `date_source` in the output, the notes line, `facts`, SPEC and README |

Both are well under the ceiling. If P1 passes 400 logic lines, split the folder-date rung into
P1b: it is the only part P2 does not need.

## 9. SPEC corrections

1. **§6.3's date ladder ends at mtime.** Replace the last rung with `none`, and state the whole
   ladder and the precision rule.
2. **§6.10 says `effective_from` never defaults to mtime.** The code now obeys this in effect,
   not only in words. Add the sentence that a mtime value is not a date.
3. **§9.4's example** gains `"date_source": "frontmatter"`.
4. **§11 gains M4B**, between M4 and M5.
5. **§15 gains one row:** a derived timestamp is never a date. One line of why.

## 10. Implementation log — Phase 1

| ID | Decision | Why |
|---|---|---|
| P1-1 | `DateSource::Mtime` is gone. The ladder is frontmatter, file name, folder name, then `None`. A `None` date gives decay `1.0` and no age. | A disk time differs between two copies of one vault, so it cannot affect a score or an order (B-1, B-5). |
| P1-2 | The parser tries the longest complete shape only. A complete day shape must be a valid day. `2026-08-22x.md`, `2026_08_22x.md`, `2026-13-01.md` and `2026_13_01.md` give no date. | A fallback to a shorter shape would turn a glued or invalid day token into a month or a year. |
| P1-3 | The separator set is `-`, `_` and `.` at both internal positions and at the token boundary. So `2026.08.22_notes` and `2026_08_22_notes` are folder dates at day precision. | Folder and file names use different conventions, and §5 names `2026.08.22_notes/x.md` as a required case. |
| P1-4 | `usable_date` keeps only `Frontmatter` and day-precision `Filename`. All new variants return `None` for identity. | This keeps every existing `DocId` unchanged (§3.4), including documents that gain a month, year or folder date. |
| P1-5 | A month date is stored as the first day of the month; a year date as the first day of the year. | The decay anchor is the start of the period, so janus never makes an old document look newer (B-4). |
| P1-6 | `tests/common` skips `.janus/` when it copies the fixture vault. A test covers a synthetic stale index and the real fixture copy. | A developer who runs `janus index` on the fixture must not hand a built index to a CLI test. |
| P1-7 | `FORMAT_VERSION` is 3. A version-2 index is refused with one `stale:` line and rebuilt. | `DocMeta` gains `date_source`, so the old layout cannot be read (B-7). |
| P1-8 | The `DateSource::None` label is `absent`. The scan golden and two fixture tests now expect a null date for `notes/undated.md`. | §3.5 names `absent` as the output value. The label must name the new sources. |
| P1-9 | An underscore is an internal date separator. `2026_08_notes.md` is month precision and `2024_11_02_notes.md` is day precision. The M1 test that expected `2024_11_02.md` to have no date now expects day precision. | The requirement names `YYYY_MM`. A fallback to year precision would decay the document from January, up to eleven months too old. |
| P1-10 | A day token starts only when a digit or the end follows the separator after a valid month. `2024-11-0-post.md` and `2026-08-1_notes.md` have no date. `2026_08_notes.md` stays at month precision. | A bare separator rule cannot tell `2026_08_notes` from a malformed day. The digit-or-end check keeps the month suffix and still refuses an invalid day. |

**Two-copy evidence.** The test `shifting_every_mtime_changes_no_order_and_no_score` failed before
the change. The undated fixture document moved from rank 2 with final `0.2733` to rank 4 with final
`4.5e-17` after its mtime moved to 2001. It passes after the change: the same order and the same
scores on both copies.

## 11. Implementation log — Phase 2

| ID | Decision | Why |
|---|---|---|
| P2-1 | `date_source` is one string field, from `DateSource::as_str`, on every search and facts result. `api` stays 1. | The field is additive and the vocabulary was fixed in B-3. A consumer that ignores the field sees the Phase 1 report (B-6). |
| P2-2 | The undated-count line is built once, in `search::render`, from the hits that were returned. | `search` and `facts` share one renderer, so the line cannot appear in one command and not the other. The count is over the results, so `--limit` and the status gates change it (§3.5). |
| P2-3 | The fallback list in `facts` is announced in `notes`, not in `degraded`, although Phase 2 said `degraded`. | `degraded` means "this query is not fully hybrid" and it names a lost capability. The fallback does find documents; it says what it could not find. That is a note about the result (§3.6, §9.4). |
| P2-4 | The fallback trigger is corpus-wide and tests types only: no document has type `decision`, `spec` or `principle`. Scope and tag filters are not part of the test. | A filter that hides every fact must not claim the vault has none. A vault whose only `decision` is still a draft does declare a fact type, so it keeps the fact list and the empty result (§3.6). |
| P2-5 | **Added to Phase 2 after the plan was written.** `scan --json` carries the near-miss names in `date_near_misses`, not the formatted sentence. The human output prints the sentence once, with the count. The list is sorted and deduplicated, so a folder name counts once however many documents sit under it. | A consumer can count and locate the names without parsing prose, and the sentence has one owner. The count exists because a day-first vault gets no dates at all, and silence gives the user nothing to act on. |
| P2-6 | **Added to Phase 2 after the plan was written.** The near-miss test is a new `pub(crate) fn is_date_near_miss` in `src/vault/resolve.rs`. `parse_prefix`, the ladder and `DocMeta` are unchanged. A name whose year is out of range is not a near miss, and nor is `2026-08-22x.md`, whose day parses. | The test needs the same separators and digit rules as the parser, and a second copy of them in `scan` would drift. The count reports a date *order* problem, so an implausible year and a bad token boundary are different problems. |
| P2-7 | SPEC §9.4 needed no edit: its example already carried `"date_source": "frontmatter"`. SPEC §6.3 had one stale sentence more than §9 listed: the `effective_from` bullet said a document can take its only date from mtime. It now says "no date at all". | §9.4 was corrected when the field was first specified. The leftover sentence described a rung that no longer exists (B-1). |
| P2-8 | The comment in the fixture `notes/undated.md` still calls its source `mtime`. It is not changed here. | A comment in a fixture is not output, and editing the file changes its `content_hash` in the scan golden. A golden must move only by this phase's field and note, so this wording waits for a change that already touches the fixture. |

**Golden evidence.** The two search goldens gained three lines each: `"date_source": "filename"`
on the three `cache` results, which have no frontmatter `date`. The scan golden gained one field,
`date_near_misses: []`. No score, span, chunk id, breadcrumb, date or note line moved.
