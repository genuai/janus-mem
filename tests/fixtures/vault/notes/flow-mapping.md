---
title: Flow mapping
date: 2025-03-10
# A flow mapping on one line, exactly as §7.3's worked example has it.
owner: {name: me, team: infra}
status: active
---
# Flow mapping

`owner` is a flow mapping. Patching `status` has to leave this line
byte-identical, which is only possible because the reader records spans instead
of re-serializing the block.
