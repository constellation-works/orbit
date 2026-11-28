---
title: Executors — Overview
owner: claude
created_date: 2026-11-28
updated_date: 2026-11-28
status: Draft
stage: shipped
next: "—"
feature: executors
doc_role: overview
type: design
summary: "Executor definitions name how Orbit launches an agent CLI or a deterministic local shell step; v2 dispatch resolves each kind on its own path"
tags: [executors]
paths: ["crates/orbit-types/src/workflow/executor_def.rs", "crates/orbit-core/assets/executors/**"]
related_features: [executors, activity-job, policy-sandbox]
related_artifacts: [ORB-00384, ORB-00400, ORB-10395, ORB-11294]
---

# Executors — Overview

An executor definition (`kind: Executor`) tells Orbit how to launch a process for an
activity: the program, its static arguments, environment additions, default timeout and
the OS sandbox it runs under. Orbit ships one definition per supported agent CLI and one
`local-shell` definition for deterministic shell steps. The mechanisms are in
[2_design.md](./2_design.md) and [specs/local-shell.md](./specs/local-shell.md); the
decisions that explain surprising code are in [4_decisions.md](./4_decisions.md).

## 1. Motivation

Activities need a declared, reviewable place that says which program runs and under which
sandbox, separate from the job that schedules the work. The v1 executor registry and the
External Executor Protocol were retired in [ORB-10395]; v2 dispatch is the only execution
path, so the remaining question is how the shipped definitions map onto it.

## 2. Core Concepts

- **Executor definition.** A YAML asset under `crates/orbit-core/assets/executors/` with an
  `executor_type` in `crates/orbit-types/src/workflow/executor_def.rs::ExecutorType`.
- **Agent CLI executors** (`direct_agent`, `agent_cli`) launch a provider CLI with a
  prompt, a model and the agent's tool authority.
- **The `local-shell` executor** (`local_shell`) backs the `local_shell` deterministic
  action restored by [ORB-11294]. A shell step gets no prompt, model or agent authority.
- **Retired `external` type.** It still deserializes so old definitions parse, but nothing
  dispatches it.

## 3. At a Glance

| Concern | Where | Task |
| --- | --- | --- |
| Executor type enum and definition schema | `crates/orbit-types/src/workflow/executor_def.rs` | [ORB-10395] |
| Shipped executor definitions | `crates/orbit-core/assets/executors/` | — |
| Agent and shell executor resolution | `crates/orbit-core/src/adapter/engine_host/actions/cli_executor.rs` | [ORB-11294] |
| Deterministic shell action | `crates/orbit-engine/src/executor/automation/shell.rs` | [ORB-11294] |
| Local-shell contract | [specs/local-shell.md](./specs/local-shell.md) | [ORB-11294] |

## Task References

- [ORB-10395] — retired the v1 executor stack and the External Executor Protocol.
- [ORB-11294] — restored local shell execution as the `local_shell` deterministic action.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
