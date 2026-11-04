---
title: Cache warmup beta
type: finding
status: active
date: 2021-08-01
---
# Cache warmup beta

A cache warmup reads the hot keys before traffic arrives. The warmup costs one
full scan of the hot set, and it removes the cold-start spike that a new instance
sees on its first requests. Measure the hot set before you warm it, because a
warmup that guesses wastes the scan.

The near-identical pair for `suggest-links` in M5 phase 7. No link joins them.
