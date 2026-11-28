---
title: Task Migration — Design
owner: claude
created_date: 2026-11-28
updated_date: 2026-11-28
status: Draft
feature: task-migration
doc_role: design
type: design
summary: "Index of the task-migration mechanisms: export/import/reindex, collision and recovery procedures, and prefix-based multi-host authority"
tags: [task-migration, multi-host, task-prefix]
paths: ["crates/orbit-store/src/migrate/task/**", "crates/orbit-cli/src/command/task/**"]
related_features: [task-migration, host-registry]
---

# Task Migration — Design

The task-migration mechanisms were written into [1_overview.md](./1_overview.md) before the
STD-06@5 scaffold existed, and inbound links still address them there. This doc names
where each one lives; a later change that moves a section here fixes its inbound links in
the same change. The forward-looking sync is [3_vision.md](./3_vision.md), and why
authority follows `task_prefix` is in [4_decisions.md](./4_decisions.md).

## 1. Export, import and reindex

The three-command migration recipe, collision prevention and index recovery are
[1_overview.md §4](./1_overview.md#4-the-migration-recipe).

## 2. Multi-host authority

How a canonical owner, admitted followers and fenced claims keep one writer per task is
[1_overview.md §5](./1_overview.md#5-multi-host-authority).

## 3. Concerns & Honest Limitations

- Migration is one-shot export/import; repeatable sync between hosts is vision work.
- The mechanism text lives in the overview, so the overview is longer than its role
  suggests. Moving it is deferred to keep inbound section links stable.

## Task References

- None cited here; the cited sections carry their own task references.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
