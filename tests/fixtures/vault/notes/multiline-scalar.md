---
title: Multi-line scalar
date: 2025-03-12
# `|` keeps newlines, `>-` folds them. Both are multi-line scalars §7.3 refuses
# to patch, because patching one reflows text the user did not ask to change.
summary: |
  First line of a literal block.
  Second line, indented and significant.
note: >-
  A folded scalar joins its lines into one,
  which is why patching it is a refusal.
---
# Multi-line scalar

Two YAML block scalars with different line-ending semantics. The reader keeps the
bytes; a writer that re-serialized them would silently change what `summary`
means.
