---
summary: "Worktree Artifacts - Overview"
type: design
title: "Worktree Artifacts - Overview"
owner: codex
last_updated: 2026-08-15
last_validated: 2026-09-12
status: Accepted
feature: worktree-artifacts
doc_role: overview
tags: ["worktree-artifacts"]
paths: ["crates/orbit-core/**", "crates/orbit-store/**", "crates/orbit-engine/**", "crates/orbit-cli/**"]
related_features: ["worktree-artifacts"]
related_artifacts: ["ORB-00199", "ORB-00200", "ORB-00201", "ORB-10501", "ORB-10535"]
---

# Worktree Artifacts — Overview

> Learning-specific storage and federation references below are retired history.
> [ORB-10736] / [Remove the native project-learning subsystem](../project-learnings/4_decisions.md#remove-the-native-project-learning-subsystem) remove the native learning subsystem and leave its
> existing repository files inert.

> Decision-store storage, federation, allocation, and repair references below are
> also retired history. [ORB-10726] retired the tool surface and moved reasoning
> into feature decision docs; [ORB-10805] removed the redundant tracked store and
> its IDs.

Historically, worktree artifacts let decision and learning body files travel with the branch that created them while preserving one shared ID authority for the repository. Tasks, audit, scoreboards, and allocator state stayed in the shared `.orbit/`; the now-retired body files lived in the current worktree's `.orbit/`. The ADR/learning stores and `id_allocations` surface described below were retired with the native learning subsystem; current Orbit retains the shared/local runtime-root split but no longer exposes those artifact commands or allocation tables.

## 1. Motivation

Linked git worktrees let agents work on several branches at once, but the old single-root artifact model wrote every ADR and learning body into the main checkout. That made a branch's code change and its knowledge artifacts land in different working trees, so agents could not stage the full change together.

The three-task sequence split this apart:

- [ORB-00199] exposed `shared_root` and `local_root`.
- [ORB-00200] moved ADR and learning ID allocation into a shared SQLite allocator and migrated learnings to `L-NNNN`.
- [ORB-00201] writes ADR and learning bodies into `local_root` and reads them through allocation metadata.

## 2. Core Concepts

| Concept | Meaning |
|---------|---------|
| Shared root | The main checkout `.orbit/`, used for tasks, audit, scoreboards, semantic.db, and (historically) allocation authority. |
| Local root | The current worktree `.orbit/`, used for (historically) ADR and learning body files. |
| Allocation row | A historical `id_allocations` row recording ID, kind, allocation status, recorded worktree, branch, and `body_path`. |
| Local-readable artifact | A historical allocation whose recorded body path exists and can be read by the current process. |
| Remote stub | A historical list row for an allocation whose body is not locally readable, shown only with `include_remote`. |
| Orphaned allocation | A historical allocation whose pinned worktree is gone from disk and whose body is unreadable everywhere locally; this was detected by the `id-allocations` doctor check and retired by `orbit doctor --fix-orphaned-allocations` [ORB-10501]. |

## 3. At a Glance

| Historical concern | Historical implementation location | Task |
|---------|------|------|
| Root split | `crates/orbit-core/src/runtime/resolve.rs` | [ORB-00199] |
| Allocator and body metadata | `crates/orbit-store/src/sqlite/id_allocator/` | [ORB-00200], [ORB-00201] |
| ADR body storage and federation | `crates/orbit-store/src/file/adr_store/api/` | [ORB-00201] |
| Learning body storage and federation | `crates/orbit-store/src/file/learning_store/api/crud.rs` | [ORB-00201] |
| CLI/tool remote listing | `crates/orbit-core/src/runtime/orbit_tool_host/` and `crates/orbit-cli/src/command/learning/` | [ORB-00201] |
| Orphaned-allocation detection and repair | `crates/orbit-core/src/command/id_allocation.rs`, `crates/orbit-cmd/src/doctor.rs` | [ORB-10501] |
| Pre-removal unique-body guard | `crates/orbit-engine/src/executor/automation/vcs/worktree/cleanup.rs` | [ORB-10535] |
| Decision log | `docs/design/worktree-artifacts/4_decisions.md` | [Worktree-local ADR and learning bodies with shared ID allocation](./4_decisions.md#worktree-local-adr-and-learning-bodies-with-shared-id-allocation), [Detect and retire id allocations pinned to a reaped worktree](./4_decisions.md#detect-and-retire-id-allocations-pinned-to-a-reaped-worktree) |

## Task References

- [ORB-00199] split Orbit runtime resolution into shared and local roots.
- [ORB-00200] introduced the global ADR/Learning allocator and `L-NNNN` learning IDs.
- [ORB-00201] moved ADR/Learning body writes to the current worktree and added read federation.
- [ORB-10501] added detection and guarded repair for allocations pinned to a worktree that no longer exists.
- [ORB-10535] blocks automated worktree removal while a learning or ADR body is readable only in the target worktree.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
