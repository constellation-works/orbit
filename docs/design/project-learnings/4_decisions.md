---
summary: "Project Learnings — Decisions: why the native learning subsystem was removed and why leftover .orbit/learnings/ files stay ignored."
type: design
title: "Project Learnings — Decisions"
owner: claude
last_updated: 2026-09-24
last_validated: 2026-09-24
status: Accepted
feature: project-learnings
doc_role: decisions
tags: ["project-learnings"]
---

# Project Learnings — Decisions

The native project-learning subsystem is retired. This file keeps only the two decisions that still govern behavior: why the subsystem was removed and how leftover files are treated. The earlier design decisions it superseded are in git history.

## Remove the native project-learning subsystem

**Recorded:** 2026-08-13 · [ORB-10736]
**Supersedes:** every earlier project-learning decision (removed from this file; see git history).

### Context

The native resource was justified primarily by automatic scope-matched delivery. In practice that signal did not justify a separate model, file store, SQLite projections, lifecycle API, CLI/MCP/HTTP surface, search corpus, hook and session state, dashboard, metrics, and scheduled curation machinery. Keeping a read-only compatibility resource or translating records into another artifact would preserve most of the maintenance burden and create a new long-term contract.

### Decision

Remove the native project-learning subsystem and every executable or advertised surface that depends on it. Existing files under `.orbit/learnings/**` remain byte-for-byte inert historical data: they are not read, indexed, injected, migrated, rewritten, or copied elsewhere. Preserve the shipped SQLite migration ledger and append a forward migration that drops the retired tables and learning vector rows for both upgraded and freshly initialized databases. Unified search continues for tasks, docs, ADRs, and frictions; the retired kind is rejected.

### Consequences

- Orbit has no native learning model, store, lifecycle, delivery path, tool, route, dashboard surface, metric, hook installer, or scheduled curation task.
- Existing historical files remain available only to repository archaeology and confer no compatibility promise.
- The narrower product surface reduces schema, conformance, security, and cross-layer maintenance cost.
- Cost: automatic delivery of scoped project rules is gone. Teams that need durable guidance must use ordinary reviewed documentation or existing repository instructions, without a native replacement resource or content migration.
- Rejected alternative: retaining a read-only compatibility layer. It would continue advertising a resource whose lifecycle and delivery semantics no longer exist.

## Ignore leftover `.orbit/learnings/` with the rest of `.orbit/`

**Recorded:** 2026-09-20 · [ORB-12718]
**Supersedes:** the git-check-in half of the retired "Workspace-scoped, checked into git" decision.

### Context

The original design stored learnings under `.orbit/learnings/` so they travelled with the repo. [Remove the native project-learning subsystem](#remove-the-native-project-learning-subsystem) left those files as inert historical data. The orbit repo's `.gitignore` still re-included `!.orbit/learnings/` even though the managed init block never did. [Per-user ownership of `.orbit/` (no git re-includes)](../routines/4_decisions.md#per-user-ownership-of-orbit-no-git-re-includes) asked whether learnings should be the one remaining exception.

No runtime consumer reads `.orbit/learnings/`. Shared knowledge already has the docs corpus and task publication.

### Decision

Ignore `.orbit/learnings/` with the rest of `.orbit/`. No `!.orbit/learnings/` exception in the managed block or in this repository's `.gitignore`. Archaeology is git history.

### Consequences

- The managed block and this repo's `.gitignore` agree: a single `.orbit/` line.
- Cost: clones no longer carry the inert YAML tree. Anyone who still wants those records reads them from history.

## Task References

- [ORB-10736] — removed the native project-learning subsystem.
- [ORB-12718] — stopped re-including `.orbit/learnings/` in git.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
