---
summary: "Activity / Job — Overview"
type: design
title: "Activity / Job — Overview"
owner: codex
last_updated: 2026-11-07
last_validated: 2026-11-04
status: Draft
stage: shipped
next: "Release-readiness example (ORB-20166) stays disabled until its owner prerequisites are met; completion contracts/history (ORB-20165), historical reuse (ORB-22649) and condition facts over accepted history (ORB-20397) shipped"
feature: activity-job
doc_role: overview
tags: ["activity-job"]
---

# Activity / Job — Overview

Activity / Job is Orbit's execution substrate. Activities describe runnable units; jobs compose them sequentially, in parallel, across collections, or through bounded loops. Orbit's product story is moving toward goals, graphs, sessions, and locks, but this layer remains the runtime underneath. [2_design.md](./2_design.md) is the current contract; [3_vision.md](./3_vision.md) captures open questions.

**Auto-task extension.** [Auto-task steps and durable waiting](./specs/auto-task-step.md)
now have shipped schema/explain ([ORB-20157], with versioning in [ORB-20237]) and
keyed bind/mint, parked CEL waits, admission rechecks and partial-branch checkpoints
([ORB-20158]), and opt-in completion contracts validate a step's result against owner
records and a granted workspace adapter ([ORB-20165]). Condition facts over accepted-result
history ([ORB-20397]) ship as `self.last_succeeded_at`, and the release-readiness job ships as a
disabled, nonseeded example ([ORB-20166]) that is installed only after its owner prerequisites.
A step's `reuse_when` can settle it on a revalidated
accepted result instead of minting ([ORB-22649]). No separate
auto-sequence lifecycle is introduced; ordinary ordering and `when` skips remain.

> **Release scope.** Orbit executes agent activities through the CLI agent path only. The `backend: http | cli | auto` selector and the engine-driven HTTP agent loop were removed in [ORB-10801]; see [specs/backend-resolution.md](./specs/backend-resolution.md) for the migration.

---

## 1. Motivation

Orbit needs a runtime layer that humans can inspect and code can execute. Activity / Job solves four practical problems:

1. **Typed execution.** Agent loops and deterministic actions share one schema family after [T20260418-2010].
2. **Durable local control flow.** Retry, parallelism, fan-out, and loops survive outside one model turn via `JobV2` DAG constructs from [T20260418-2018].
3. **Clean runtime boundaries.** orbit-core coordinates runs without naming `orbit-agent` internals through the unified `RuntimeHost` boundary in [T20260418-2143] and [T20260418-2210].
4. **One canonical schema.** `schemaVersion: 1` assets fail load-time parsing after [T20260419-2156]; a lower-precedence layer skips them instead.

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
- granted `target: script:<name>` steps ([ORB-19983], [ORB-19985])
- `target: auto-task:<name>` steps with durable waits ([ORB-20158])
- `parallel`
- `fan_out` / `fan_in`
- `loop`

The original grammar landed in [T20260418-2018], with `workflow` / `subroutine` job kinds
added in [T20260419-0339]. Script targets are job steps, not a restored shell activity
variant; their current contract is [Script steps](../script-steps/1_scope.md).

### 2.3 Load-time normalization is part of the contract

orbit-core normalizes assets before a run starts:

- loads YAML through a two-pass schema loader
- resolves `target: activity:<name>` references for jobs
- rejects retired declarations (`backend: http | auto`, any `session:` binding) that cannot execute as written

Name resolution arrived in [T20260418-2019]; `run-v2` entrypoints in [T20260418-2143]; CLI agent support in [T20260419-0104]. Backend selection was retired in [ORB-10801].

### 2.4 Provider is the only agent runtime choice

For `agent_loop`, the asset declares a **provider** — `claude`, `codex`, `gemini`, `grok`, `copilot`, `ollama`, `openai_compat`, `antigravity`, or `muse` (plus the retired `cursor`, `pi` and `opencode` ids, which only deserialise from persisted records). Orbit's CLI entry point uses the registered direct-agent executors for the supported provider lanes; `openai_compat` and providers without a supported executor fail structurally instead of falling back.

### 2.5 Audit, policy, and seeded assets make the runtime inspectable

This layer also owns:

- `fsProfile` attachment on activities and target steps
- the v2 audit envelope with `workspace_path` provenance
- seeded reference assets and pipeline jobs used by `orbit init`

`workspace_path` entered the envelope in [T20260419-0002], runtime/CLI `fsProfile` enforcement landed in [T20260419-0503], and init seeding landed in [T20260419-2347].

### 2.6 Automatic recovery follows the delivery pipeline

Recovery applies within delivery workflows. Configured step-failure recovery can repair a failed
step and resume from it; final recovery can make a deterministic decision after that mechanism is
spent. The owner's blocked-task recovery pipeline can also process eligible blocked tasks. A
workspace auto run still treats a required child workflow failure as a parent failure.

---

## 3. At a Glance

| Concern | Where it lives | Primary task ID |
|---------|----------------|-----------------|
| v2 activity type system | `crates/orbit-types/src/workflow/activity_job/activity_v2.rs` | [T20260418-2010] |
| v2 job step grammar | `crates/orbit-types/src/workflow/activity_job/job_v2.rs` | [T20260418-2018] |
| Job kinds (`workflow`, `subroutine`) | `crates/orbit-types/src/workflow/activity_job/job_v2.rs` | [T20260419-0339] |
| Auto-task schema / parked execution (done) | `crates/orbit-engine/src/activity_job/job_executor/auto_task.rs`, `crates/orbit-core/src/application/auto_tasks/job_step.rs` | [ORB-20157], [ORB-20237], [ORB-20158] |
| Granted script steps (done) | `crates/orbit-engine/src/activity_job/job_executor/target.rs`, [Script steps](../script-steps/1_scope.md) | [ORB-19983], [ORB-19985] |
| Completion contracts / accepted history, historical reuse and condition facts (shipped); release-readiness example (disabled, nonseeded) | [Auto-task step specification](./specs/auto-task-step.md) | [ORB-20165], [ORB-22649], [ORB-20397] (shipped), [ORB-20166] (disabled example) |
| Target-ref resolution | `crates/orbit-engine/src/activity_job/catalog/resolve.rs` | [T20260418-2019] |
| v2 job run entrypoint and runtime host boundary | `crates/orbit-core/src/application/job/exec.rs`, `crates/orbit-engine/src/context/hosts/runtime_host/mod.rs`, `crates/orbit-core/src/adapter/engine_host/runtime_host/host/implementation.rs` | [T20260418-2143], [T20260418-2210] |
| Retired-declaration rejection | `crates/orbit-types/src/workflow/activity_job/activity_v2.rs`, `crates/orbit-types/src/workflow/activity_job/retired.rs` | [ORB-10801] |
| v2 DAG executor | `crates/orbit-engine/src/activity_job/job_executor/` | [T20260418-2018], [T20260509-2] |
| V2 audit envelope and disk sink | `crates/orbit-types/src/workflow/activity_job/audit_envelope.rs`, `crates/orbit-engine/src/activity_job/audit_writer.rs`, `crates/orbit-engine/src/activity_job/sqlite_sink.rs` | [T20260419-0002] |
| CLI agent runtime path | `crates/orbit-engine/src/activity_job/cli_runner/mod.rs` | [T20260419-0104] |
| `fsProfile` enforcement | `crates/orbit-policy`, `tool_context_for_activity`, CLI describe/get surfaces | [T20260419-0503] |
| Seeded reference activities and pipeline jobs | `crates/orbit-core/assets/activities/`, `crates/orbit-core/assets/jobs/` | [T20260419-2347], [T20260419-0622-3], [T20260419-0623] |

---

## Task References

- **[T20260418-2010]** — Add the first v2 activity runtime scaffolding.
- **[T20260418-2018]** — Add `JobV2` DAG constructs (`parallel`, `fan_out`, `loop`, `retry`, `when`).
- **[T20260418-2019]** — Add v2 activity name resolution and pipeline skeleton assets.
- **[T20260418-2143]** — Wire `V2RuntimeHost` in orbit-core and add its standalone activity runner.
- **[T20260418-2210]** — Reshape `V2RuntimeHost` to keep `orbit-agent` types out of orbit-core.
- **[T20260419-0002]** — Add `workspace_path` provenance to the v2 audit envelope.
- **[T20260419-0104]** — Add `backend: cli` dispatch for v2 `agent_loop`.
- **[T20260419-0339]** — Add v2 job kinds to the job catalog.
- **[T20260419-0503]** — Enforce `fsProfile` rules across runtime and CLI surfaces.
- **[T20260419-0622-3]** — Add `task_gate_pipeline`.
- **[T20260419-0623]** — Add `task_auto_pipeline`.
- **[T20260419-2156]** — Retire v1 assets and drop the transitional v2 naming.
- **[T20260419-2347]** — Seed activities and workflows on `orbit init`.
- **[ORB-10332]** — Remove the unused Groundhog activity kind and the epic/parallel pipeline layer.
- **[T20260509-2]** — split the v2 job executor into responsibility-focused modules without changing runtime behavior.
- **[ORB-00374]** — Remove the Shell activity variant and run_shell dispatch from the v2 orchestrator.
- [ORB-10801] — Make job runs asynchronous by default and retire agent backend selection.
- [ORB-19983] — Define script manifests, target parsing and the operator grant store.
- [ORB-19985] — Execute granted Linux script steps under linux-bwrap-script-v1.
- [ORB-20157] — Define, validate and explain disabled auto-task job steps (phase 1).
- [ORB-20158] — Execute keyed auto-task bind/mint, parked waits and partial-branch checkpoints (phase 2).
- [ORB-20165] — Validate readiness evidence contracts and provide accepted-result history.
- [ORB-20166] — Compose and validate disabled release-readiness job/routine examples.
- [ORB-20237] — Require schemaVersion 3 for auto-task jobs and fence older clients.
- [ORB-20397] — Expose accepted-result history through condition success facts.
- [ORB-22649] — Add historical result reuse to auto-task job steps.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
