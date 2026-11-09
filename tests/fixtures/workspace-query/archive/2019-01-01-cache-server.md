---
title: Run a dedicated cache server
type: principle
status: active
date: 2019-01-01
tags: [cache, storage]
---

# Run a dedicated cache server

This principle governed storage for one year. It says that a dedicated cache
server owns the hot set, that every writer talks to it directly, and that the
database is read only for cold rows. The retired tree keeps the principle for
analysis, and the primary corpus replaced it with one cache in front of the
database. The rest of the text describes a fleet that no longer exists, one
queue per region, a nightly rebuild, a manual failover drill, and the metric
names that the operations team used at the time.
