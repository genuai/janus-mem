#!/usr/bin/env bash
#
# Rehearse every write path over a generated corpus and measure it (M7-8).
#
# This script builds 200 documents, one per shape the scanner knows, runs each
# write command with `--dry-run` over them, and reports three numbers: visited,
# changed on disk, and refusals grouped by shape.
#
# A dry run never writes, so it cannot show that a write is idempotent. A second
# pass writes each document with the value it already holds, on a copy, twice,
# and reports how many documents changed. A value that is already right must
# change none.
#
# A refusal counts only when it names the shape its document was built for: a
# refusal nobody expected is a failure, not a number. The corpus is generated,
# never committed.
#
# Usage: scripts/write-rehearsal.sh [--janus PATH] [--keep] [--prove-it-fails]
#
#   --janus PATH       binary to rehearse (default: target/debug/janus, then
#                      `janus` on PATH)
#   --keep             keep the temporary corpus and print where it is
#   --prove-it-fails   append one byte to a document after the dry runs, so the
#                      comparison must report one changed document and exit 1
#
# Exit: 0 every document survived, 1 a byte changed or a refusal missed its
# shape, 2 usage or not 200 documents.
set -eu

JANUS=""
KEEP=0
PROVE=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --janus) JANUS="$2"; shift 2 ;;
    --keep) KEEP=1; shift ;;
    --prove-it-fails) PROVE=1; shift ;;
    -h|--help) sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [[ -z $JANUS ]]; then
  JANUS=janus
  [[ -x target/debug/janus ]] && JANUS=target/debug/janus
fi
if ! "$JANUS" --version >/dev/null 2>&1; then
  echo "error: no janus binary at $JANUS (build it, or pass --janus)" >&2
  exit 2
fi

ROOT=$(mktemp -d "${TMPDIR:-/tmp}/janus-rehearsal.XXXXXX")
trap '[[ $KEEP -eq 1 ]] && echo "corpus kept at $ROOT" || rm -rf "$ROOT"' EXIT

CORPUS="$ROOT/primary"
ARCHIVE="$ROOT/archive"
WS="$ROOT/workspace.toml"
MANIFEST="$ROOT/corpus.manifest"

mkdir -p "$CORPUS/notes/ünïcode" "$ARCHIVE/decisions"
printf 'index_dir = ".janus"\ncorpus_id = "rehearsal"\n\n[embedding]\nenabled = false\n' > "$CORPUS/janus.toml"
printf 'index_dir = ".janus"\ncorpus_id = "archive"\n\n[embedding]\nenabled = false\n' > "$ARCHIVE/janus.toml"
printf -- '---\ntitle: Adopt Postgres\ntype: decision\nstatus: active\ndate: 2020-01-01\n---\n\nUse Postgres.\n' \
  > "$ARCHIVE/decisions/postgres.md"
# A workspace, because `promote` reads its source from another corpus. Every
# command names the primary, so one workspace serves all four.
{
  printf '[[corpora]]\nid = "main"\nroot = "primary"\nrole = "primary"\n\n'
  printf '[[corpora]]\nid = "context"\nroot = "archive"\nrole = "archived"\n\n'
  printf '[search]\nallow_cross_corpus_links = true\n'
} > "$WS"
# Two inputs for `write`, outside the corpus so the snapshot covers documents.
printf 'title: Rehearsed\n' > "$ROOT/frontmatter.yaml"
printf '# Rehearsal\n\nbody\n' > "$ROOT/body.md"

# One shape, `count` documents: the bytes, the key a patch names, and what each
# write command owes. `ok` is a receipt; `refuse:REASON` is a refusal that must
# name REASON, the reason the scanner returns for that shape.
build() {
  local shape=$1 count=$2 n path key set_owed sup_owed pro_owed same
  # `same` is the request that asks for the value the document already holds.
  # Empty means the shape has no such request, and the second pass skips it.
  same=""
  case $shape in
    block_sequence) set_owed=refuse:BlockSequence ;;
    flow_mapping) set_owed=refuse:FlowMapping ;;
    multiline) set_owed=refuse:MultilineScalar ;;
    anchor) set_owed=refuse:Anchor ;;
    alias) set_owed=refuse:Alias ;;
    duplicate_key) set_owed=refuse:DuplicateKey; sup_owed=refuse:DuplicateKey ;;
    no_frontmatter) set_owed=refuse:NoFrontmatter; sup_owed=refuse:NoFrontmatter ;;
    *) set_owed=ok ;;
  esac
  sup_owed=${sup_owed:-ok}
  pro_owed=${pro_owed:-ok}

  for ((n = 1; n <= count; n++)); do
    path="notes/$shape-$n.md"
    case $shape in
      scalar) key=title; same="status=Active"; printf -- '---\ntitle: Plain %s\nstatus: active\n---\n\n# Note %s\n\nbody\n' "$n" "$n" > "$CORPUS/$path" ;;
      quoted) key=title; printf -- '---\ntitle: "Line %s\\nwith \\"quotes\\""\n---\n\nbody\n' "$n" > "$CORPUS/$path" ;;
      flow_sequence) key=tags; printf -- '---\ntags: [db, rust, note%s]\n---\n\nbody\n' "$n" > "$CORPUS/$path" ;;
      comment) key=status; same="status=Active"; printf -- '---\nstatus: active  # keep me, note %s\n---\n\nbody\n' "$n" > "$CORPUS/$path" ;;
      empty) key=title; printf -- '---\ntitle:\n---\n\nbody\n' > "$CORPUS/$path" ;;
      crlf) key=title; printf -- '---\r\ntitle: Plain %s\r\n---\r\n\r\nbody\r\n' "$n" > "$CORPUS/$path" ;;
      bom) key=title; { printf '\xef\xbb\xbf'; printf -- '---\ntitle: Plain %s\n---\n\nbody\n' "$n"; } > "$CORPUS/$path" ;;
      nonascii) key=title; same="title=È stato deciso — 日本語 $n"; path="notes/ünïcode/naïve-$n.md"; printf -- '---\ntitle: È stato deciso — 日本語 %s\n---\n\nbody\n' "$n" > "$CORPUS/$path" ;;
      block_sequence) key=steps; printf -- '---\nsteps:\n  - one\n  - two\n---\n\nbody\n' > "$CORPUS/$path" ;;
      flow_mapping) key=owner; printf -- '---\nowner: {name: me}\n---\n\nbody\n' > "$CORPUS/$path" ;;
      multiline) key=summary; printf -- '---\nsummary: |\n  one\n  two\n---\n\nbody\n' > "$CORPUS/$path" ;;
      anchor) key=team; printf -- '---\nteam: &platform infra\n---\n\nbody\n' > "$CORPUS/$path" ;;
      alias) key=reviewer; printf -- '---\nteam: &platform infra\nreviewer: *platform\n---\n\nbody\n' > "$CORPUS/$path" ;;
      duplicate_key) key=status; printf -- '---\nstatus: active\nstatus: draft\n---\n\nbody\n' > "$CORPUS/$path" ;;
      no_frontmatter) key=title; printf -- '# no frontmatter %s\n\nbody\n' "$n" > "$CORPUS/$path" ;;
    esac
    printf '%s|%s|%s|%s|%s|%s|%s\n' "$path" "$shape" "$key" "$set_owed" "$sup_owed" \
      "$pro_owed" "$same" >> "$MANIFEST"
  done
}

for spec in scalar:30 quoted:20 flow_sequence:22 comment:22 empty:22 crlf:18 bom:18 \
  nonascii:12 block_sequence:8 flow_mapping:6 multiline:6 anchor:5 alias:4 \
  duplicate_key:4 no_frontmatter:3; do
  build "${spec%:*}" "${spec#*:}"
done

VISITED=$(wc -l < "$MANIFEST" | tr -d ' ')
[[ $VISITED -eq 200 ]] || { echo "error: the corpus is $VISITED documents, not 200" >&2; exit 2; }

cp -R "$CORPUS" "$ROOT/snapshot"

# The etag a read reports for one document in one workspace, which is what a
# write must present.
etag_in() {
  "$JANUS" --workspace "$1" read "$2" --json 2>/dev/null \
    | sed -n 's/^ *"content_hash": "\([0-9a-f]*\)",*$/\1/p'
}

# The etag of a document in the corpus under rehearsal.
etag() {
  etag_in "$WS" "$1"
}

# One command's answer as `ok`, `refuse:REASON`, or `other:CODE`. Anything else
# fails the expectation for that document.
outcome() {
  local out code reason
  if out=$("${@:2}" 2>/dev/null) && [[ -n $out ]]; then
    echo ok
    return
  fi
  code=$(printf '%s' "$out" | sed -n 's/^ *"code": "\([a-z_]*\)",*$/\1/p')
  case $code in
    unsupported_patch) reason=$(printf '%s' "$out" | sed -n 's/.*cannot splice: \(.*\)"$/\1/p'); echo "refuse:$reason" ;;
    no_frontmatter) echo refuse:NoFrontmatter ;;
    *) echo "other:${code:-exit}" ;;
  esac
}

MISMATCH="$ROOT/mismatch"
REFUSALS="$ROOT/refusals"

check() {
  local command=$1 path=$2 shape=$3 owed=$4 got=$5
  [[ $got == "$owed" ]] || printf '%s %s: owed %s, got %s\n' \
    "$command" "$path" "$owed" "$got" >> "$MISMATCH"
  [[ $got != refuse:* ]] || printf '%s %s %s\n' "$command" "$shape" "${got#refuse:}" >> "$REFUSALS"
}

while IFS='|' read -r path shape key set_owed sup_owed pro_owed same; do
  hash=$(etag "$path")
  [[ -n $hash ]] || { echo "error: no etag for $path" >&2; exit 2; }
  check set "$path" "$shape" "$set_owed" \
    "$(outcome x "$JANUS" --workspace "$WS" set "$path" --expect-hash "$hash" --key "$key=Rehearsed" --dry-run --json)"
  check write "$path" "$shape" ok \
    "$(outcome x "$JANUS" --workspace "$WS" write "$path" --frontmatter "$ROOT/frontmatter.yaml" --body "$ROOT/body.md" --overwrite --expect-hash "$hash" --dry-run --json)"
  check supersede "$path" "$shape" "$sup_owed" \
    "$(outcome x "$JANUS" --workspace "$WS" supersede "$path" "notes/next-${path##*/}" --expect-hash "$hash" --dry-run --json)"
  check promote "$path" "$shape" "$pro_owed" \
    "$(outcome x "$JANUS" --workspace "$WS" promote context:decisions/postgres.md --link-from "$path" --expect-hash "$hash" --dry-run --json)"
done < "$MANIFEST"

# A dry run changes no bytes. Compare the whole tree: a changed document, an
# added one and a removed one are three different faults.
if [[ $PROVE -eq 1 ]]; then
  first=$(head -1 "$MANIFEST" | cut -d'|' -f1)
  printf 'x' >> "$CORPUS/$first"
  echo "sabotage: appended one byte to $first, as a dry run that wrote would"
fi

DIFF="$ROOT/diff"
diff -r -q "$ROOT/snapshot" "$CORPUS" > "$DIFF" 2>&1 || true
CHANGED=$(grep -c '^Files ' "$DIFF" || true)
ALIEN=$(grep -c '^Only in' "$DIFF" || true)

# ── the real-write pass ──
#
# The dry runs above never write, so they cannot show that a write is stable.
# This pass copies the corpus, writes each document with the value it already
# holds, and counts the documents whose bytes changed. It runs twice: the second
# run is the one that proves the first was not a rewrite (cases 2.7 and 2.8).

REAL="$ROOT/real"
cp -R "$CORPUS" "$REAL"
{
  printf '[[corpora]]\nid = "main"\nroot = "real"\nrole = "primary"\n\n'
  printf '[[corpora]]\nid = "context"\nroot = "archive"\nrole = "archived"\n\n'
  printf '[search]\nallow_cross_corpus_links = true\n'
} > "$ROOT/real-workspace.toml"

# The identity of one file: its length and its checksum, in one word.
file_sum() {
  cksum < "$1"
}

# Writes every document with the value it already holds, and prints
# "<changed> <total> <failed>".
same_value_pass() {
  local changed=0 total=0 failed=0 before after hash
  while IFS='|' read -r path shape key set_owed sup_owed pro_owed same; do
    [[ -n $same ]] || continue
    total=$((total + 1))
    hash=$(etag_in "$ROOT/real-workspace.toml" "$path")
    before=$(file_sum "$REAL/$path")
    "$JANUS" --workspace "$ROOT/real-workspace.toml" set "$path" \
      --expect-hash "$hash" --key "$same" --json > /dev/null 2>&1 \
      || failed=$((failed + 1))
    after=$(file_sum "$REAL/$path")
    [[ $before == "$after" ]] || changed=$((changed + 1))
  done < "$MANIFEST"
  echo "$changed $total $failed"
}

read -r SAME_1_CHANGED SAME_TOTAL SAME_1_FAILED <<< "$(same_value_pass)"
read -r SAME_2_CHANGED _ SAME_2_FAILED <<< "$(same_value_pass)"
SAME_CHANGED=$((SAME_1_CHANGED + SAME_2_CHANGED))
SAME_FAILED=$((SAME_1_FAILED + SAME_2_FAILED))

echo "rehearsal: $ROOT"
echo "  visited        $VISITED documents"
echo "  write paths    set, write, supersede, promote (every one with --dry-run)"
echo "  changed        $CHANGED documents"
echo "  added/removed  $ALIEN"
echo "  refusals       $(wc -l < "$REFUSALS" | tr -d ' '):"
[[ -s $REFUSALS ]] && sort "$REFUSALS" | uniq -c | sed 's/^ */    /'
echo "  same-value     $SAME_1_CHANGED of $SAME_TOTAL documents changed, run 1 of 2"
echo "  same-value     $SAME_2_CHANGED of $SAME_TOTAL documents changed, run 2 of 2"

STATUS=0
[[ $CHANGED -eq 0 && $ALIEN -eq 0 ]] || { cat "$DIFF" >&2; echo "FAIL: a dry run touched the corpus" >&2; STATUS=1; }
[[ ! -s $MISMATCH ]] || { sed 's/^/  /' "$MISMATCH" >&2; echo "FAIL: a command did not answer what its shape owed" >&2; STATUS=1; }
[[ $SAME_CHANGED -eq 0 ]] || { echo "FAIL: $SAME_CHANGED same-value writes changed bytes" >&2; STATUS=1; }
[[ $SAME_FAILED -eq 0 ]] || { echo "FAIL: $SAME_FAILED same-value writes were refused" >&2; STATUS=1; }
exit $STATUS
