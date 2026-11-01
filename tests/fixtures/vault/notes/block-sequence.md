---
title: Block sequence
date: 2025-03-09
# A block sequence: M7 must refuse to patch it, not rewrite it as `[a, b]` (§7.3).
tags:
  - splice
  - block
status: active
---
# Block sequence

`tags` is a block sequence. A patch that replaced it would reflow three lines into
one, so M7 refuses with `UnsupportedPatch { key: "tags" }` and suggests
`write_document`.
