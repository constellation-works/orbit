---
title: Auto-tasks — Overview
owner: claude
last_updated: 2026-09-12
last_validated: 2026-09-12
status: Accepted
feature: auto-tasks
doc_role: overview
type: design
summary: Dynamically-defined recurring task templates minted by the host clock tick — periodic work as data, not code.
tags: [auto-tasks]
paths: ["crates/orbit-core/src/application/auto_tasks/**"]
related_features: [auto-tasks, routines]
related_artifacts: [ORB-10149, ORB-10318, ORB-10348, ORB-10439, ORB-10446, ORB-10514, ORB-10549, ORB-10950, ORB-11054, ORB-11095]
---

# Auto-tasks — Overview

Auto-tasks turn recurring chores into **data instead of code**. An auto-task
definition is a git-versioned YAML record with a schedule, an `enabled` toggle,
a task template, and a dedupe policy. One generic scheduler pass reads the
enabled definitions, fires the due ones, and mints a task from each template.
Adding a new periodic chore is a new definition (`orbit auto-task add`), never
new orbit code or a new routine.

The scheduler pass is called directly by the host clock tick (`orbit clock tick`, alias
`orbit sweep`) for every registered owner checkout. No routine, job, activity, or job run
mediates auto-task evaluation. See
[Auto-task definitions are evaluated by the host tick, not fired by a routine](./4_decisions.md#auto-task-definitions-are-evaluated-by-the-host-tick-not-fired-by-a-routine).

## 1. Motivation

Every periodic need in orbit used to be bespoke: qa-sweep (ORB-10039) was an
inline check sweep, ship-sweep had its own systemd unit, and each future
recurring chore would mean another hardcoded routine. The marginal cost of a new
chore was a code change, a review, and a release. Auto-tasks replace the pattern
with a primitive so the marginal cost is a YAML record. qa-sweep V1 (ORB-10148)
becomes just the first definition.

## 2. Core Concepts

- **Definition** — a `.orbit/auto_tasks/<name>.yaml` record; `name` is identity.
  Modeled on the file-backed routine convention (directory scan, fail-closed
  parse, `deny_unknown_fields`), not the SQLite-indexed artifact convention.
- **Schedule (cadence)** — a 5-field `cron` or an `every_minutes` interval, in
  the definition's `schedule` field. Catch-up always collapses: a downtime gap
  mints one make-up task, not one per slot. Cadence is per-definition data, not
  a knob in the identity `config.yaml` ([L-0014] keeps runtime config out of
  `config.yaml`).
- **Cursor** — per-definition last-fired state, host-local at
  `<orbit_dir>/state/auto-tasks.json`, so the git-versioned definition is never
  churned by a scheduler fire.
- **Scheduler** — the host clock tick calls the evaluator directly after routine
  evaluation. No job run is created; fire evidence is the minted task, the cursor,
  and the tick report row. Auto-tasks are shown on the Operations auto-task panel,
  not on the routines surface.
- **Dedupe & provenance** — each minted task carries an `auto-task:<name>` tag;
  `skip_if_open` uses that tag to avoid firing while a prior instance is open.
- **Manual mint** — `orbit auto-task mint <name>` mints one task from a
  definition immediately, reusing the scheduler's mint path so the result is
  indistinguishable from a fired instance. Unconditional and cursor-inert: it
  ignores schedule, `dedupe`, and `enabled`, and never touches
  `<orbit_dir>/state/auto-tasks.json` (ORB-10439).
- **Default catalog** — Orbit embeds a small catalog of workspace definitions.
  Initialization materializes a missing catalog file under `.orbit/auto_tasks/`
  but every default is `enabled: false`. Seeding neither mints a task nor
  enables a routine or auto-task; an operator must explicitly enable the
  definition or use the existing manual-mint surface. Re-initialization
  preserves an existing definition byte-for-byte, including with workspace
  reconciliation `--force`; destructive lower-level initialization reseeds
  only because it recreates the Orbit directory.

## 3. At a Glance

| Concern | File | Task |
|---|---|---|
| Definition schema | `crates/orbit-types/src/workflow/auto_task.rs` | ORB-10149 |
| Discovery (fail-closed) | `crates/orbit-core/src/application/auto_tasks/loader.rs` | ORB-10149 |
| Due-math + catch-up | `crates/orbit-core/src/application/auto_tasks/schedule.rs` | ORB-10149 |
| Host-local cursor | `crates/orbit-core/src/application/auto_tasks/state.rs` | ORB-10149 |
| Scheduler pass | `crates/orbit-core/src/application/auto_tasks/scheduler.rs` | ORB-10149 |
| CRUD (CLI + MCP shared) | `crates/orbit-core/src/application/auto_tasks/crud.rs` | ORB-10149 |
| Manual mint (`mint`, CLI + MCP) | `crates/orbit-core/src/application/auto_tasks/crud.rs` | ORB-10439, ORB-10798 |
| Deterministic action (slated for retirement) | `crates/orbit-core/src/adapter/engine_host/v2_host/dispatch.rs` | ORB-10149 |
| Seeded assets (scheduler routine/job/activity slated for retirement) | `crates/orbit-core/assets/{activities,jobs,routines}/…` | ORB-10149 |
| Default auto-task catalog | `crates/orbit-core/assets/auto_tasks/…` | ORB-10549, ORB-10550, ORB-10950 |

## Embedded default catalog

These four YAML files live under `crates/orbit-core/assets/auto_tasks/` and are
registered in `DEFAULT_AUTO_TASK_FILES`. `orbit workspace init` materializes a
missing file as `enabled: false`; re-init does not overwrite a workspace-authored
definition of the same name.

- `qa-sweep` — hands-on validation of recent changes (ORB-10148, embedded by ORB-10550).
- `friction-curation` — disabled-by-default daily evidence-first pass that deduplicates open
  friction records, re-verifies survivors against current behavior, resolves
  records that no longer reproduce, and files non-duplicate fix tasks for
  verified-real issues (ORB-10440, embedded by ORB-10549).
- `security-review` — disabled-by-default weekly evidence-backed review of
  applicable application code, dependencies, secret handling, and configuration;
  each actionable finding is filed as a durable Orbit task, and a clean review
  is a successful no-op (ORB-10950).
- `code-review` — disabled-by-default six-hourly review of commits merged
  since the previous sweep's recorded cursor.

## Workspace-authored definitions in this repo

Orbit's own checkout also carries extra `.orbit/auto_tasks/` files that are
**not** embedded defaults. They may be enabled, name a family-specific crew, or
encode this repository's branches and gates. Re-init preserves them:

- `doc-duties`, `model-price-audit`, `release-prep`, and this repository's
  enabled copies of catalog names such as `code-review` and
  `security-review`.

## Task References

- ORB-10149 — Auto-task primitive.
- ORB-10148 — qa-sweep V1 (first definition; depends on this).
- ORB-10439 — `orbit auto-task mint <name>`, the on-demand manual mint.
- ORB-10440 — Daily friction-curation definition.
- ORB-10514 — Original workspace-authored CI-failure auto-task.
- ORB-10549 — Embedded the portable, disabled friction-curation default and
  workspace materialization contract; [Auto-task primitive: file-backed recurring task templates + one generic scheduler routine](./4_decisions.md#auto-task-primitive-file-backed-recurring-task-templates-one-generic-scheduler-routine) should be updated through the
  Orbit ADR surface after this task lands.
- ORB-10550 — Added the disabled qa-sweep default and standardized agent-facing
  friction tool invocations on the registered `orbit tool run` surface.
- ORB-10950 — Added the disabled weekly `security-review` default to the
  embedded catalog so new workspaces can opt into a recurring security review.
- ORB-11054 — Split this overview so repo-local workspace-authored definitions
  are no longer listed as if they were embedded defaults.
- ORB-11095 — Added centralized finding-title provenance and established the
  shipped definition's canonical `code-review` name.
- ORB-11115 / ORB-11383 — Retired the shipped CI-failure auto-task;
  runner workflows only emit fail-open run/job/commit provenance, while the
  host-owned `ci_failure_sweep` routine performs durable CI-failure filing.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
