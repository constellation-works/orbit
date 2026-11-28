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
| Executor type enum and definition schema | `crates/orbit-types/src/workflow/executor_def.rs` | [ORB-10395], [ORB-11294], [ORB-11589], [ORB-19977], [ORB-25764] |
| Shipped executor definitions | `crates/orbit-core/assets/executors/` | [ORB-10112], [ORB-11290], [ORB-11294], [ORB-29847] |
| Agent and shell executor resolution | `crates/orbit-core/src/adapter/engine_host/actions/cli_executor.rs` | [ORB-11294], [ORB-19977] |
| Deterministic shell action | `crates/orbit-engine/src/executor/automation/shell.rs` | [ORB-11294], [ORB-18833] |
| Local-shell contract | [specs/local-shell.md](./specs/local-shell.md) | [ORB-11294], [ORB-29816], [ORB-29847] |

## Task References

- [ORB-10112] — selected shipped agent executor sandbox settings by host platform.
- [ORB-10395] — retired the v1 executor stack and the External Executor Protocol.
- [ORB-11290] — removed `model_pair_override` from shipped executor definitions while preserving legacy parsing.
- [ORB-11294] — restored local shell execution as the `local_shell` deterministic action.
- [ORB-11589] — added `ExecutorSandboxKind::Off` and persisted explicit sandbox-off operator choices.
- [ORB-18833] — exposed bounded `local_shell` step output summaries (`exit_code`, `log_tail`) on run views.
- [ORB-19977] — added executor `image_input` declarations and propagation through `resolve_cli_executor`.
- [ORB-25764] — retired the `cli_command` read alias of `ExecutorType::LocalShell`.
- [ORB-29816] — corrected local-shell filesystem authority descriptions in shipped executor and contributor docs.
- [ORB-29847] — corrected Linux Bubblewrap read-policy and fallback guarantees in `local-shell.yaml` and the local-shell spec.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
