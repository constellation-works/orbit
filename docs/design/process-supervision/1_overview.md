---
title: Process Supervision — Overview
owner: claude
last_updated: 2026-10-12
last_validated: 2026-10-18
status: Draft
feature: process-supervision
doc_role: overview
type: design
summary: Record of converging Orbit's child-process supervisors (orbit-common's bounded runner, orbit-exec's SupervisedChild, the engine CLI runner, and two ad-hoc loops) onto one kill-on-drop supervisor in orbit-common, with the caller inventory and the seven-step migration, now landed.
tags: [process-supervision, code-health, architecture]
paths: ["crates/orbit-common/src/process/**", "crates/orbit-exec/src/supervision/**", "crates/orbit-engine/src/activity_job/cli_runner/supervisor/**", "crates/orbit-exec/src/build_sandbox/supervise.rs", "crates/orbit-core/src/application/automation/source/command.rs"]
related_features: [process-supervision, policy-sandbox, executors]
related_artifacts: [ORB-15722]
---

# Process Supervision — Overview

> **Status: Migration landed.** Steps 1–7 of the migration in
> [2_design.md](./2_design.md#5-migration-order) are merged (ORB-15826 through
> ORB-15832). Every caller listed in the inventory now runs through the single
> supervisor in `orbit-common`. The steps are kept as the record of how the
> supervisors converged.

Orbit runs child processes that must not outlive their deadline, block on a
full pipe, or leave descendants behind: Git, `gh`, `ps`, plugin backends,
validation shells, and provider CLIs. Before the migration, three supervisors
did this and two smaller wait loops were written in place. Each solved the
same problem (spawn into a fresh process group, drain both pipes, enforce a
deadline, kill the group, reap) with different guarantees. This folder
records the caller inventory, the supervisor that survived, and the order
that converged the rest onto it.

## 1. Motivation

These were the problems the migration fixed.

- **Leaks on failure paths.** Only `orbit-exec`'s `SupervisedChild` killed
  and reaped on `Drop`. The bounded runner in `orbit-common` returned early
  through `?` without signalling the child. The engine CLI runner relied on
  an explicit kill at each exit, so a new early return or a panic leaked the
  provider's group. The plugin exec backend killed only the direct child when
  callback binding failed.
- **Different guarantees for the same job.** The termination grace was 200 ms
  in one supervisor, 5 s in another and none in the third. Output capture
  either kept a prefix, stopped the child, or kept a head and tail. Only
  `orbit-exec` forwarded SIGINT/SIGTERM to live child groups. Whether a fix
  reached a given child depended on which supervisor its caller happened to
  use.
- **Repeated repairs.** Escaped `setsid` writers, stopped descendants,
  `EMFILE` on wakeup pairs and reused pids had been fixed separately in each
  supervisor.

## 2. Core Concepts

- **Supervisor** — code that owns a spawned child from spawn to reap: it
  drains the pipes, enforces the deadline, terminates the process group and
  reports how the run ended.
- **Owned child** — a handle that holds a process-group leader and kills
  the group and reaps the leader on `Drop` unless the child was already
  reaped.
- **Output sink** — what a supervisor does with each stream's bytes:
  capture a prefix, fail at a limit, keep a head and tail, relay to a
  consumer, or emit trace events.
- **Stop cause** — the supervisor's own record of why a run ended (capture
  limit, forwarded signal, termination status, deadline). It never comes from
  child output.
- **Drain budget** — how long the supervisor keeps pipe workers alive after
  the child is gone, so a holder outside the group cannot keep a run open.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Bounded runner (deadline, prefix capture), on the shared supervisor | [process/bounded.rs](../../../crates/orbit-common/src/process/bounded.rs) | [ORB-15829] |
| Shared supervisor (`SupervisedChild`, kill-on-drop, signal fan-out), moved from exec — the survivor | [process/supervisor/wait.rs](../../../crates/orbit-common/src/process/supervisor/wait.rs) | [ORB-15828] |
| Engine provider supervisor (rolling capture, progress, build-budget deadline) | [supervisor/spawn.rs](../../../crates/orbit-engine/src/activity_job/cli_runner/supervisor/spawn.rs) | [ORB-15830] |
| Plugin build phases, on the shared supervisor | [build_sandbox/supervise.rs](../../../crates/orbit-exec/src/build_sandbox/supervise.rs) | [ORB-15831] |
| Automation source commands, on the shared supervisor | [source/command.rs](../../../crates/orbit-core/src/application/automation/source/command.rs) | [ORB-15832] |
| Owned process-group child handle | [process/owned_child.rs](../../../crates/orbit-common/src/process/owned_child.rs) | [ORB-15826], [ORB-15827] |
| Caller inventory, capability matrix, target API, migration order | [2_design.md](./2_design.md) | [ORB-15722] |
| Standing rules for supervision | [4_decisions.md](./4_decisions.md) | [ORB-15722] |

## Task References

- [ORB-15722] — authored this design and filed the migration tasks.
- [ORB-15826] — step 1: adds the owned process-group child handle.
- [ORB-15827] — step 2: holds every ad-hoc spawned child through that handle.
- [ORB-15828] — step 3: moves exec's supervisor into `orbit-common`.
- [ORB-15829] — step 4: runs the bounded runner on the single supervisor.
- [ORB-15830] — step 5: runs the engine CLI provider runner on it.
- [ORB-15831] — step 6: runs plugin build phases on it.
- [ORB-15832] — step 7: runs automation source commands on it.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
