---
# CRLF plus aliased keys: name -> title, created -> date, labels -> tags.
name: CRLF line endings
created: 2025-02-14
labels: [line-endings, fixtures]
written_by: fixture-generator
# A block sequence written in CRLF: the indentation and the line endings
# both have to survive a read unchanged.
related:
  - crlf
  - fixtures
---
# CRLF spec

Every line in this file ends with CRLF; the parser must preserve it verbatim.
