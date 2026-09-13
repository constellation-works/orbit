---
summary: "Activity / Job — Overview"
type: design
title: "Activity / Job — Overview"
owner: codex
last_updated: 2026-07-20
last_validated: 2026-09-13
status: Draft
feature: activity-job
doc_role: overview
tags: ["activity-job"]
---

# Activity / Job — Overview

Activity / Job is Orbit's execution substrate. Activities describe runnable units; jobs compose them sequentially, in parallel, across collections, or through bounded loops. Orbit's product story is moving toward goals, graphs, sessions, and locks, but this layer remains the runtime underneath. [2_design.md](./2_design.md) is the current contract; [3_vision.md](./3_vision.md) captures open questions. The planned opt-in durability contract for automatic task recovery is specified in [recoverable-auto-workflows.md](./specs/recoverable-auto-workflows.md).

> **Release scope.** Orbit executes agent activities through the CLI agent path only. The `backend: http | cli | auto` selector and the engine-driven HTTP agent loop were removed in [ORB-10801]; see [specs/backend-resolution.md](./specs/backend-resolution.md) for the migration.

---

## 1. Motivation

Orbit needs a runtime layer that humans can inspect and code can execute. Activity / Job solves four practical problems:

1. **Typed execution.** Agent loops and deterministic actions share one schema family after [T20260418-2010].
2. **Durable local control flow.** Retry, parallelism, fan-out, and loops survive outside one model turn via `JobV2` DAG constructs from [T20260418-2018].
3. **Clean runtime boundaries.** orbit-core coordinates runs without naming `orbit-agent` internals through the unified `RuntimeHost` boundary in [T20260418-2143] and [T20260418-2210].
4. **One canonical schema.** `schemaVersion: 1` assets fail load-time parsing after [T20260419-2156].

---

## 2. Core Concepts

### 2.1 Activities are the runnable units

An `ActivityV2` carries shared metadata plus one runtime spec:

- `agent_loop`
- `deterministic`

The shared shape shipped in [T20260418-2010]. The `shell` type was removed as a fail-closed security fix in [ORB-00374]; see [The v2 shell activity surface is removed, not sandboxed](./4_decisions.md#the-v2-shell-activity-surface-is-removed-not-sandboxed). The `groundhog` activity kind was later removed as unused in [ORB-10332].

### 2.2 Jobs are the orchestration grammar

A `JobV2` is a step tree with:

- `when`
- `retry`
- flat target steps
- `target: activity:<name>` references
- `parallel`
- `fan_out` / `fan_in`
- `loop`

That grammar landed in [T20260418-2018], with `workflow` / `subroutine` job kinds added in [T20260419-0339].

### 2.3 Load-time normalization is part of the contract

orbit-core normalizes assets before a run starts:

- loads YAML through a two-pass schema loader
- resolves `target: activity:<name>` references for jobs
- rejects retired declarations (`backend: http | auto`, any `session:` binding) that cannot execute as written

Name resolution arrived in [T20260418-2019]; `run-v2` entrypoints in [T20260418-2143]; CLI agent support in [T20260419-0104]. Backend selection was retired in [ORB-10801].

### 2.4 Provider is the only agent runtime choice

For `agent_loop`, the asset declares a **provider** — `claude`, `codex`, `gemini`, `grok`, `copilot`, `ollama`, `openai_compat`, `cursor`, `pi`, `antigravity`, or `opencode`. Orbit's CLI entry point uses the registered direct-agent executors for the supported provider lanes; `openai_compat` and providers without a supported executor fail structurally instead of falling back.

### 2.5 Audit, policy, and seeded assets make the runtime inspectable

This layer also owns:

- `fsProfile` attachment on activities and target steps
- the v2 audit envelope with `workspace_path` provenance
- seeded reference assets and pipeline jobs used by `orbit init`

`workspace_path` entered the envelope in [T20260419-0002], runtime/CLI `fsProfile` enforcement landed in [T20260419-0503], and init seeding landed in [T20260419-2347].

### 2.6 Automatic recovery is explicit and evidence-gated

The planned `orbit run auto --recover` mode isolates task-local failures, invokes bounded
task-scoped recovery, and keeps draining unrelated work. It does not trust an agent's claim that no
work was needed: semantic search retrieves possible prior implementations, while a deterministic
gate verifies delivered commits, current acceptance checks, and workspace policy before persisting
an `already_satisfied` disposition. Unrecoverable tasks become `blocked`; systemic workflow
failures still terminate the parent.

---

## 3. At a Glance

| Concern | Where it lives | Primary task ID |
|---------|----------------|-----------------|
| v2 activity type system | `crates/orbit-types/src/workflow/activity_job/activity_v2.rs` | [T20260418-2010] |
| v2 job step grammar | `crates/orbit-types/src/workflow/activity_job/job_v2.rs` | [T20260418-2018] |
| Job kinds (`workflow`, `subroutine`) | `crates/orbit-types/src/workflow/activity_job/job_v2.rs` | [T20260419-0339] |
| Target-ref resolution | `crates/orbit-engine/src/activity_job/catalog.rs` | [T20260418-2019] |
| `run-v2` core entrypoints and host boundary | `crates/orbit-cmd/src/activity_v2.rs`, `crates/orbit-core/src/application/job/exec.rs`, `crates/orbit-engine/src/context/hosts.rs`, `crates/orbit-core/src/adapter/engine_host/runtime_host.rs` | [T20260418-2143], [T20260418-2210] |
| Retired-declaration rejection | `crates/orbit-types/src/workflow/activity_job/retired.rs` | [ORB-10801] |
| v2 DAG executor | `crates/orbit-engine/src/activity_job/job_executor/` | [T20260418-2018], [T20260509-2] |
| V2 audit envelope and disk sink | `crates/orbit-types/src/workflow/activity_job/audit_envelope.rs`, `crates/orbit-engine/src/activity_job/audit_writer.rs` | [T20260419-0002] |
| CLI agent runtime path | `crates/orbit-engine/src/activity_job/cli_runner/mod.rs` | [T20260419-0104] |
| `fsProfile` enforcement | `crates/orbit-policy`, `tool_context_for_activity`, CLI describe/get surfaces | [T20260419-0503] |
| Seeded reference activities and pipeline jobs | `crates/orbit-core/assets/activities/`, `crates/orbit-core/assets/jobs/` | [T20260419-2347], [T20260419-0622-3], [T20260419-0623] |
| Recoverable automatic workflow contract | [`specs/recoverable-auto-workflows.md`](./specs/recoverable-auto-workflows.md) | planned |

---

## Task References

- **[T20260418-2010]** — Add the first v2 activity runtime scaffolding.
- **[T20260418-2018]** — Add `JobV2` DAG constructs (`parallel`, `fan_out`, `loop`, `retry`, `when`).
- **[T20260418-2019]** — Add v2 activity name resolution and pipeline skeleton assets.
- **[T20260418-2143]** — Wire the v2 runtime host in orbit-core and add `orbit activity run-v2`.
- **[T20260418-2210]** — Reshape the v2 runtime host to keep `orbit-agent` types out of orbit-core.
- **[T20260419-0002]** — Add `workspace_path` provenance to the v2 audit envelope.
- **[T20260419-0104]** — Add `backend: cli` dispatch for v2 `agent_loop`.
- **[T20260419-0339]** — Add v2 job kinds to the job catalog.
- **[T20260419-0503]** — Enforce `fsProfile` rules across runtime and CLI surfaces.
- **[T20260419-0622-3]** — Add `task_gate_pipeline`.
- **[T20260419-0623]** — Add `task_auto_pipeline`.
- **[T20260419-2156]** — Retire v1 assets and drop the transitional v2 naming.
- **[T20260419-2347]** — Seed activities and workflows on `orbit init`.
- **[ORB-10332]** — Remove the unused Groundhog activity kind and the epic/parallel pipeline layer.
- **[T20260430-19]** — Shorten the Activity / Job design docs while preserving required structure.
- **[T20260509-2]** — Split the v2 job executor into responsibility-focused modules without changing runtime behavior.
- **[ORB-00374]** — Remove the `shell` activity variant and `run_shell` dispatch (fail-closed resolution of security bug [ORB-00363]).

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
