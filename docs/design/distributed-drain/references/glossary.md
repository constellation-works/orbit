---
type: design
summary: "Glossary: Distributed Drain"
last_validated: 2026-09-18
title: Glossary — Distributed Drain
owner: claude
status: Draft
feature: distributed-drain
tags: [distributed-drain, glossary]
related_features: [distributed-drain, federated-mcp, host-registry, resident-orchestrator]
related_artifacts: [ORB-12488]
---

# Glossary: Distributed Drain

Vocabulary for the pull-based multi-host drain. Host-registry roles (`machine_id`, owner checkout,
replica checkout) and federated-mcp terms (selector, capability, destination, callers file) keep
their existing meanings and are not redefined here.

| Term | Meaning |
|------|---------|
| **Carried** | A task that is `in-progress` with a held task lock. The only claim state admission recognizes on any host. [4_decisions.md](../4_decisions.md#in-progress-plus-a-held-task-lock-is-the-claim) |
| **Epic (tag)** | A size hint: one large task a top-tier crew (`fable`, `astra`) takes on whole. No pipeline, worktree, reservation class, or admission rule attaches to it after the retirement. [2_design.md §7.1](../2_design.md#71-epic-machinery) |
| **Follower** | A replica checkout on another host running the drain in pull mode. Holds `execute`; never mints, never reserves locally. [2_design.md §1](../2_design.md#1-roles-one-owner-n-followers) |
| **Landing** | A follower's end of delivery: `git_push`, `pr_open`, promote to `review` through the owner. Merge stays with the owner's sweep. [2_design.md §3](../2_design.md#3-pull-mode-drain-and-the-pulled-leaf-pipeline) |
| **Owner** | The one checkout holding the coordination store, locks, drain clock, and `control_plane` tools for a workspace. Unchanged from host-registry. |
| **Pull** | `orbit.task.pull`: pop the first conflict-free ready-queue entry, reserve its locks, and set it `in-progress`, one task per call. [specs/task-pull.md](../specs/task-pull.md) |
| **Ready queue** | Owner-maintained ordered projection of `backlog` tasks with satisfied dependencies, in the owner's readiness order. The only place order is decided. [2_design.md §2](../2_design.md#2-the-ready-queue-and-orbittaskpull) |
| **Pull mode** | `orbit run auto --pull <selector>`: the drain loop with its admission step replaced by a pull call to the owner. [2_design.md §3](../2_design.md#3-pull-mode-drain-and-the-pulled-leaf-pipeline) |
| **Pulled leaf** | A `task_pr_pipeline` run started from a pulled task: skips `reserve_locks`, keeps `release_reservation`, branches as `orbit/<task-id>-<host_id>`. |
| **Slot** | One unit of a host's build capacity: `max_active_leaf_runs` minus live leaf runs on that host. A follower pulls once per free slot; the owner never sees it. |
| **Version parity** | Precondition that a follower's binary version and orchestration schema equal the owner's; refused as `version_mismatch` otherwise. [2_design.md §4](../2_design.md#4-follower-preconditions) |
