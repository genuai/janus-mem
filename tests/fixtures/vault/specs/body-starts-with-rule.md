---
title: Body starts with a rule
date: 2025-03-14
type: spec
status: active
---
---
A body whose first line is `---` is the F-2 trap: the frontmatter ends at the
*closing* fence above, so `span.end` points between the two and the body keeps
its leading rule. Treating the second `---` as an opening fence, or the first as
a setext underline, would swallow the whole document.
