#!/usr/bin/env bash
#
# Runs the retrieval-quality report (§3.5, §3.8, S-15).
#
# This is not a gate: the test is `#[ignore]`d, the numbers are printed with
# their denominators, and nothing here asserts a threshold. Milestone acceptance
# (`active top-1 >= 0.80`) is read from this output by a human and recorded in
# the PR that changes the ranking.
#
# The clock is fixed inside the test (`FixedClock(2025-04-01)`), so a run in
# January and a run in July compare the same rankings.
#
# Usage:
#   scripts/eval.sh                  # one report at alpha = 0: the M3 numbers
#   scripts/eval.sh --sweep          # the six alpha values of §3.5
#   scripts/eval.sh --baseline [V]   # D20's tantivy comparison on vault V
#
# Environment:
#   JANUS_EVAL_FEATURES=embeddings   add the non-default feature to the build
#   JANUS_EVAL_MODEL=AllMiniLML6V2   override the fixture's `hash-64` test double
#   JANUS_MODEL_DIR=/path/to/model   where the real model files live
#   JANUS_EVAL_ALPHA=0.5             measure one alpha instead of the sweep
#
# The alpha decision (§3.5) needs a real model: `hash-64` is a test double whose
# numbers are never an input to it. A real run looks like
#
#   JANUS_EVAL_FEATURES=embeddings JANUS_EVAL_MODEL=AllMiniLML6V2 \
#     JANUS_MODEL_DIR=/path/to/model scripts/eval.sh --sweep
#
# `--baseline` is a separate crate (`scripts/baseline/`) with its own empty
# `[workspace]`, so `tantivy` is never part of this repository's build (§3.8).
set -euo pipefail
cd "$(dirname "$0")/.."

# Bash 3.2 (macOS) treats an empty array expansion as an unbound variable under
# `set -u`, so the argument list is built with `set --` instead.
run_eval() {
  if [[ -n "${JANUS_EVAL_FEATURES:-}" ]]; then
    set -- --features "$JANUS_EVAL_FEATURES" --test eval_quality -- --ignored --nocapture
  else
    set -- --test eval_quality -- --ignored --nocapture
  fi
  cargo test "$@"
}

case "${1:-}" in
  "")
    run_eval
    ;;
  --sweep)
    export JANUS_EVAL_SWEEP=1
    run_eval
    ;;
  --baseline)
    shift
    cargo run --release --manifest-path scripts/baseline/Cargo.toml -- "$@"
    ;;
  -h | --help)
    sed -n '2,36p' "$0" | sed 's/^# \{0,1\}//'
    ;;
  *)
    echo "unknown argument: $1 (try --help)" >&2
    exit 2
    ;;
esac
