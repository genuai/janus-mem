---
title: "Quoted: a scoped title"
date: 2025-03-13
# A trailing comment on a key, and escapes inside quotes. `# not a comment` is
# literal text, so a naive line scanner would truncate the value there.
note: 'it''s fine'
motto: "say \"hello\" # not a comment"
status: active  # this comment must survive a patch to this line
---
# Quoted scalars

The quotes and escapes are part of the file, not decoration: a patcher that
re-emitted `motto` would have to re-escape it perfectly or change its value.
