---
summary: "Task Artifacts — Overview"
type: design
title: "Task Artifacts — Overview"
owner: codex
last_updated: 2026-05-17
last_validated: 2026-09-10
status: Draft
feature: task-artifacts
doc_role: overview
tags: ["task-artifacts"]
---

# Task Artifacts — Overview

Tasks are Orbit's durable intent records: they explain what an agent or human is trying to change, how the work should be validated, what context is relevant, who acted on the work, and how the work connects to other Orbit artifacts. The v2 task artifact store keeps prose in Markdown sidecars, narrows `task.yaml` to a metadata envelope, allocates authority-scoped `ORB-00000` IDs from `~/.orbit/tasks/index.sqlite`, and makes `~/.orbit/tasks/workspaces/<workspace-id>/` the canonical local bundle home.

This document is the entry point. [2_design.md](./2_design.md) describes the live v2 store; [3_vision.md](./3_vision.md) names open questions and prior work; [4_decisions.md](./4_decisions.md) captures the design decisions that constrain implementation.

---

## 1. Motivation

Orbit tasks sit between transient agent sessions and durable code history. A good task artifact has to satisfy four different readers:

- Humans need a plain-English record of intent, acceptance criteria, and review context.
- Agents need a stable, machine-readable handoff surface that survives session loss.
- Automation needs structured fields for lifecycle, dependencies, locks, PR generation, semantic search, and audit.
- Git history needs a compact identifier that can be cited in commits and traced later.

The v2 artifact matches the work to its readers:

- `task.yaml` is a small, mergeable envelope of metadata.
- Markdown sidecars (`description.md`, `acceptance.md`, `plan.md`, `execution-summary.md`) hold prose and long-form human/agent reasoning.
- Append-only logs (`events.jsonl`, `comments.jsonl`) carry events and comments without rewriting the envelope.
- The default `ORB-00000` format uses a five-digit minimum width inside the configured allocation authority and explicitly scopes tasks when they cross registries; bare task IDs remain local search keys, in line with the graph-attribution removal in [T20260506-11].
- Workspace task bundles live in one canonical local store under `~/.orbit/tasks/workspaces/<workspace-id>/`; task tools resolve them through the registry rather than a checkout-local filesystem view.
- Local execution bindings stay in `~/.orbit/tasks/index.sqlite` rather than leaking into synced task identity.

---

## 2. Core Concepts

### 2.1 Task bundle

A task bundle is the complete on-disk representation of one task. The canonical bundle lives under `~/.orbit/tasks/workspaces/<workspace-id>/<task-id>/`:

```text
~/.orbit/tasks/
  index.sqlite
  workspaces/
    orbit-a3f9c2/
      ORB-00000/
        task.yaml
        description.md
        acceptance.md
        plan.md
        execution-summary.md
        events.jsonl
        comments.jsonl
        artifacts/
```

The canonical directory name is the task ID. Status lives in `task.yaml`; it is not encoded in the path. `.orbit/config.yaml` stores the checkout's `workspace_id` so Orbit can resolve the canonical workspace partition after a repo move or checkout recreation.

### 2.2 Envelope

`task.yaml` is the envelope. It owns identity, lifecycle state, priority, type, typed relations, context selectors, external references, actor attribution, timestamps, and schema version. It does not carry long prose or append-heavy conversation history.

### 2.3 Prose sidecars

Long-form fields are Markdown sidecars:

- `description.md` explains the problem, desired behavior, and relevant context.
- `acceptance.md` records validation expectations, preferably as Markdown checkboxes.
- `plan.md` records the execution plan.
- `execution-summary.md` records what changed, validation performed, and residual risk.

The public API exposes these as first-class task documents.

### 2.4 Authority-scoped task ID

The canonical ID format is `ORB-00000`: `ORB` identifies Orbit and the five-digit decimal suffix is unique within the configured allocation authority. A local-only installation uses one machine-local allocator in `~/.orbit/tasks/index.sqlite` across all local workspaces; synced or hosted workspaces must allocate against their shared registry before a task is visible. Two unrelated machines may both allocate `ORB-00042`, so cross-machine references still need registry, hosted-tenant, or external-reference context.

V2 bundles do not carry `legacy_ids` and lookup surfaces do not resolve old `T<YYYYMMDD>-<N>` values.

### 2.5 Task event stream

Lifecycle changes, comments, and automation updates are naturally append-heavy. The store keeps those out of YAML arrays in append-only JSON Lines files (`events.jsonl`, `comments.jsonl`), so concurrent writes can be merged intentionally and audited without rewriting the envelope.

### 2.6 Typed relations

Typed links live in a single directed `relations` array on the envelope. Task-to-task relation types remain source-implied (`child_of`, `blocked_by`, `spawned_from`, `regression_from`, `supersedes`, and `related_to`) and task-only. Cross-artifact provenance uses `produces` and `resolves`, whose targets may be task, friction, or ADR IDs. The bundle stores one directed edge per relationship. Consumers reason about relation meaning rather than inferring it from legacy field names.

### 2.7 Local task store

The canonical bundle lives in the local task store under `~/.orbit/tasks/workspaces/<workspace-id>/<task-id>/`. `~/.orbit/tasks/index.sqlite` holds the machine-local allocator, workspace bindings, local execution overlays, and generated indexes. Task and artifact tools resolve the canonical bundle from that registry; checkouts do not expose task-bundle symlinks.

---

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| V2 envelope, relations, JSONL rows, and manifest types | [crates/orbit-types/src/task/artifacts.rs](../../../crates/orbit-types/src/task/artifacts.rs) | — |
| Public `Task` DTO | [crates/orbit-types/src/task/model.rs](../../../crates/orbit-types/src/task/model.rs) | — |
| V2 bundle primitives (file layout, atomic writes, JSONL append) | [crates/orbit-store/src/driver/file/task_bundle/](../../../crates/orbit-store/src/driver/file/task_bundle/) | — |
| V2 task backend adapter (create/get/list/search/mutations) | [crates/orbit-store/src/repository/task/v2/](../../../crates/orbit-store/src/repository/task/v2/) | — |
| Home registry: allocator, workspace bindings, generated indexes | [crates/orbit-store/src/driver/sqlite/task_registry/](../../../crates/orbit-store/src/driver/sqlite/task_registry/) | — |
| V2 runtime wiring (`build_v2_task_backends`) | [crates/orbit-core/src/runtime/builder.rs](../../../crates/orbit-core/src/runtime/builder.rs) | — |
| Local task store and canonical lookup | [2_design.md §6](./2_design.md#6-local-task-store-and-canonical-lookup) | — |
| Task sync design over `ORB-*` IDs (archived) | [docs/design/_archive/task-sync/2_design.md](../_archive/task-sync/2_design.md) | [T20260505-12] |
| Task ID local-only doctrine after graph attribution removal | [docs/POSITIONING.md](../../POSITIONING.md), [_archive/knowledge-graph/4_decisions.md](../_archive/knowledge-graph/4_decisions.md) | [T20260506-11] |
| V2 task bundle contract | [specs/task-bundle-v2.md](./specs/task-bundle-v2.md) | — |
| Glossary | [references/glossary.md](./references/glossary.md) | — |
| ADR log | [4_decisions.md](./4_decisions.md) | — |

---

## Task References

- [T20260505-12] — Designed git-orphan-branch task sync and preserved `T<YYYYMMDD>-<N>` for that proposal.
- [T20260506-11] — Removed knowledge-graph task attribution and made task IDs local search keys rather than cross-machine references.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
