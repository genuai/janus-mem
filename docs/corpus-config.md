# Configuring a real corpus

A retrieval score is a product: relevance, then authority from the document's
type, then status, then decay from its date. A corpus that declares no type and
no date multiplies every document by the same constants, and no ranking setting
can recover the difference. In the gate of 2026-11-08, 13.8% of a
2,320-document corpus declared a type, 7.3% declared a frontmatter date, and 34%
had neither. Authority times status times decay was near-constant across that
corpus, and the tool said nothing about it.

This document is the four configuration decisions that fixed it on two corpora.
Each rule names the finding it prevents.

## Check the corpus first

Two read-only commands answer "can this corpus support authority and decay?":

```bash
janus scan            # every document with its type, status and date source,
                      # then one line: metadata: 28 typed, 39 dated, 16 undated (29%)
janus status --json   # metadata.dated, metadata.undated, metadata.share_undated
```

**Typed means one thing here, in `scan`, and in `status`:** the document's
resolved type is not the configured default type. A document that falls back to
`[defaults] type` is not typed for this measure, although it has a type. So a
corpus at "0 typed" has no authority signal at all, however many documents it
holds. `scan --json` also reports `by_source`, one count per date source, so you
can see whether the dates come from frontmatter or from file names.

If more than half the documents are undated, or nobody is typed, fix the corpus
before you touch a weight. A weight you tune against a wrong denominator is a
tuned mistake.

## 1. Exclude synthetic trees; retire a real tree into an archived corpus

Two kinds of document carry strong metadata and no claim on the present, and they
need two different instruments. A **synthetic** fixture is not part of the corpus,
so exclude it. A **retired** tree is part of the corpus's history, so declare it as
an archived corpus: a default query still cannot answer from it, and a query that
names it can.

**The finding it prevents.** In this repository a fixture `decision` at authority
1.50 and a 2020 date outranked every real design document: **9 of the 10 graded
questions were answered wrong**. One exclude line fixed it, and it stays fixed
that way, because a fixture is not history. A retired `6.archive/` tree behaved
the same way in the larger corpus: **226 documents** of type `spec`, timeless by
default, served as current doctrine and appeared in the top three for **3**
questions.

A fixture is excluded, and that line stays in the corpus config:

```toml
[index]
exclude = ["tests/fixtures/**"]   # synthetic, so not part of the corpus
```

A retired tree is declared in a workspace file instead, with the role `archived`:

```toml
# workspace.toml, beside the corpus
[[corpora]]
id = "main"
root = "vault"
role = "primary"

[[corpora]]
id = "context"
root = "vault/6.archive"
role = "archived"

[search]
archived_weight = 0.8
```

A relative `root` resolves against the workspace file's own directory, so a
workspace can sit beside its corpora and needs no machine path. A workspace in the
platform config directory (`~/.config/janus/workspace.toml`) needs absolute roots.
An archived corpus is read-only unless its entry sets `read_only = false`.

An archived corpus is **hard-excluded** from a default query, not down-weighted:
the weight applies only when a query names the corpus. Three commands show the
difference:

```bash
janus --workspace workspace.toml corpora                  # both corpora, roles, freshness
janus --workspace workspace.toml search "cache"           # the primary only, with one note
janus --workspace workspace.toml search "cache" --corpus context --intent history
```

A default query prints one note naming the archived corpora it excluded. The
third command is the reason to keep the tree: it answers "what did I used to
believe" over the archive, weighted at `archived_weight` so an old fact cannot
outrank a current one. Naming the corpus does not promote it; the weight still
applies, because a name says where to look and not that the facts are current.

`--workspace` is a global flag, so you write it before or after the subcommand.
Only the commands that read a workspace take it: `search`, `facts`, `read`,
`status`, `corpora`, `rpc` and `mcp`. Every other command works on one corpus and
refuses it with a usage error — `init`, `scan`, `chunks`, `index`, `doctor`,
`ids`, `moves`, `lineage` and `suggest-links` — because a command that ignored
the flag would report success for a corpus you did not name.

**A retired tree inside the primary's root needs no exclude line.** The workspace
prunes every corpus root that lies inside another corpus root from that parent's
walk. So the primary indexes `6.archive/` once, as the corpus that owns it, and
no `janus.toml` says so. The prune is derived from the workspace and kept in
memory, never written to a corpus file: one corpus can be primary in one
workspace and archived in another.

A workspace with a nested corpus prints one line on load, naming the parent, the
nested corpus and the document count it pruned:

```text
warning: workspace.toml: corpus "context" is inside corpus "main": 226 documents pruned from the parent's walk; --corpus "context" reads them
```

`janus corpora` marks the parent with the child it skips (`prunes: context`). A
read of the pruned path asked of the parent is a usage error that names the
corpus which owns it, so the parent never answers with its old copy.

## 2. Type frontmatter-less documents by path

A spec, a decision listing and a journal entry usually differ by where they live,
not by what their frontmatter says. `[paths]` types them without editing one
file.

**The finding it prevents.** Every plan document and every `docs/*.md` in this
repository carries no frontmatter, so each resolved to the default type `note` at
authority 0.90, below every file that declared `spec`. Adding `[paths]` alone
moved this repository from **1 of 10 to 5 of 10 top-1 answers**. On the
2,320-document corpus, typing by path after the tree split moved **1 of 10 to 6
of 10**.

```toml
[paths]
"SPEC.md" = "spec"
"docs/" = "spec"
"decisions/" = "decision"
"journals/" = "journal"
```

The hint is consulted only when frontmatter declares no type, and the longest
matching pattern wins, so a nested folder can refine a broad one. A bare
`folder/` form matches that directory anywhere in the tree.

## 3. Check that the authority order matches what you want to be true

`[authority]` is a claim about which document wins when two documents disagree.
The default order suits a code base: a decision beats a spec, a spec beats a
journal. A personal knowledge base usually wants the opposite, because the
journal is the primary record and the doctrine is a summary of it.

**The finding it prevents.** The larger corpus scored research notes at 1.10 and
journals at 1.00, so a summary note outranked the record it summarised:
**doctrine beat the personal record in 2 of the 10 graded questions**.

```toml
[authority]
decision = 1.50
journal = 1.10
finding = 1.00
```

Read the list as a sentence: "when these two disagree, I want the first one to
win". Then check the two or three questions where that claim is not obviously
true. Do not move a weight to fix one answer; move it when the order is wrong
for the whole corpus.

## 4. Decide the undated rule, and know which one you shipped

A document with no date must still be scored. `[decay] undated` takes two values:

| value | what an undated document gets | when it is right |
|---|---|---|
| `median` | decay `exp(-ln2 · median_age / half_life)`, the corpus's own median age | a missing date is an omission, and the corpus average is the best guess |
| `none` | decay 1.0, so no loss from the missing date | a missing date is missing metadata, not age |

```toml
[decay]
undated = "median"
```

**The finding it prevents.** An undated document lost 37% of its score before
relevance was counted (decay 0.632 against 0.99 for a dated one), and that
multiplier alone decided one of the ten gate answers. The two rules were measured
on two corpora: `median` kept **6** top-1 answers on the 2,320-document corpus,
`none` kept **5** and dropped two ranks where a dated record is the answer.
`median` is the default, because the measurement selected it.

Decide deliberately. `none` is one line away, and it is a legitimate answer for a
corpus of timeless notes; it is a wrong answer if the corpus has a real aging
story and the dates were simply not written down.

## A worked example

This configuration is read by the parser in a test, so it cannot drift from the
code. It suits a mixed vault: a code-adjacent `docs/` tree, a journal, and a
retired archive.

```toml
index_dir = ".janus"
corpus_id = "main"

[defaults]
type = "note"
status = "active"

[types]
adr = "decision"
rfc = "spec"
log = "journal"
research = "finding"

[authority]
decision = 1.50
spec = 1.35
journal = 1.10
finding = 1.00
note = 0.90

[decay]
half_life_days = 180
undated = "median"
timeless_types = ["decision", "spec"]

[paths]
"SPEC.md" = "spec"
"docs/" = "spec"
"decisions/" = "decision"
"journals/" = "journal"

[index]
exclude = ["tests/fixtures/**"]
```

Run `janus scan` again after editing. The `metadata:` line should move: `typed`
rises when the `[paths]` hints cover the frontmatter-less tree, and `undated`
falls only when the documents themselves gain dates. `janus scan` never writes
to the vault and never repairs a document.

A retired tree does not belong in this file. It is one entry in a
`workspace.toml`, as rule 1 states, and `janus corpora` reports it beside the
corpus this file configures.

## What this document does not decide

- **No default moves.** The guide names the levers; the built-in defaults stay as
  shipped. Change a weight only for a reason your corpus states.
- **No date from a modification time.** A copied file is not a new decision.
  A document with no authored date stays undated.
- **No repair.** janus-mem reports metadata quality and never edits a file
  outside an explicit write command.
