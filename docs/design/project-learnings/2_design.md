---
title: Project Learnings — Design
owner: claude
created_date: 2026-11-28
updated_date: 2026-11-28
status: Accepted
feature: project-learnings
doc_role: design
type: design
summary: "What remains after the project-learning removal: a forward migration that drops the retired tables and inert, ignored leftover files"
tags: [project-learnings]
related_features: [project-learnings]
---

# Project Learnings — Design

The subsystem summarized in [1_overview.md](./1_overview.md) is retired, so this doc
records only what the removal left behind. The earlier design is in git history.

## 1. Store and migrations

The shipped SQLite migration ledger is preserved. A forward migration drops the retired
learning tables and learning vector rows, for both upgraded and freshly initialized
databases ([ORB-10736]).

## 2. Leftover files

Files under `.orbit/learnings/**` stay byte-for-byte as they were. No runtime consumer
reads, indexes, injects, rewrites or copies them. They are ignored with the rest of
`.orbit/`, with no `!.orbit/learnings/` exception ([ORB-12718]).

## 3. Concerns & Honest Limitations

- Automatic delivery of scoped project rules is gone, with no native replacement or
  content migration.
- Clones no longer carry the inert files; anyone who needs them reads git history.

## Task References

- [ORB-10736] — removed the native project-learning subsystem.
- [ORB-12718] — stopped re-including `.orbit/learnings/` in git.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
