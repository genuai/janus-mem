---
title: Anchor and alias
date: 2025-03-11
# An anchor and its alias: two keys, one value. A patch must not expand the alias
# (§7.3 invariant I5).
team: &platform infra
reviewer: *platform
status: active
---
# Anchor and alias

`reviewer` is an alias for `team`. A YAML serializer would inline the alias and
lose the fact that the two keys were meant to move together.
