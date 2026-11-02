#!/usr/bin/env bash
#
# Runs the retrieval-quality report (§3.8, S-15).
#
# This is not a gate: the test is `#[ignore]`d, the numbers are printed with
# their denominators, and nothing here asserts a threshold. Milestone acceptance
# (`active top-1 >= 0.80`) is read from this output by a human and recorded in
# the PR that changes the ranking.
#
# The clock is fixed inside the test (`FixedClock(2025-04-01)`), so a run in
# January and a run in July compare the same rankings.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test --test eval_quality -- --ignored --nocapture
