---
title: Routines — Overview
owner: claude
last_updated: 2026-10-26
last_validated: 2026-10-19
status: Implemented
stage: shipped
next: "—"
feature: routines
doc_role: overview
type: design
summary: Durable checkout-local routine definitions dispatch catalog jobs on cron, delivery or state eligibility, with host-local checkpoints.
tags: [routines, scheduler]
paths: ["crates/orbit-cli/src/command/routine/**", "crates/orbit-cli/src/command/clock/**", "crates/orbit-core/src/application/routines/**", "crates/orbit-cmd/src/registry/routines.rs", "crates/orbit-cmd/src/registry/runtime/mod.rs", "crates/orbit-registry/src/machine_identity.rs", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-store/src/driver/sqlite/routine_store/**"]
related_features: [routines, auto-tasks, activity-job, host-registry]
related_artifacts: [ORB-10001, ORB-10021, ORB-10207, ORB-10270, ORB-10319, ORB-10739, ORB-12236]
---

# Routines — Overview

Routines make Orbit the constellation's single scheduler. A **routine** is a durable
YAML definition of recurring work — a cron, delivery or state trigger, a job target from the existing
catalog, and a retry/overlap policy — living under `.orbit/routines/` as per-user
checkout state. A stateless **`orbit clock tick`** pass, also available
through the compatibility alias `orbit sweep`, is invoked on the configured OS schedule (one
minute by default, via launchd on macOS or a systemd timer on Linux). It fires due routines
through the existing v2 run machinery and evaluates due auto-task definitions in-process.
Definitions do not travel by git; `orbit workspace init` / `sync` seed shipped defaults
from the binary. All scheduler state (last fires, pauses, locks, run history,
immutable batches and consumer checkpoints) is host-local and never synced.
Cron schedules are independent per owner checkout; delivery/state consumers
admit only on their configured or resolved owner machine.
[2_design.md](./2_design.md#1-routine-definition) is the current schema-v1 contract;
its [evaluation section](./2_design.md#31-delivery-and-state-evaluation) explains
initialization and persisted debt. [3_vision.md](./3_vision.md) holds deferred work.

> **Status.** v1 shipped in [ORB-10021]; the At a Glance table lists the actual home of
> each concern. Targets are `job:<name>` in v1 — see [Routine targets are catalog references only — no inline command payloads](./4_decisions.md#routine-targets-are-catalog-references-only--no-inline-command-payloads) for why `activity:` is
> reserved. `orbit-cmd::registry_routines` composes local host identity and the workspace
> catalog from `orbit-registry` with registered runtimes; `orbit-automation` owns
> the scheduler kernels, with Core supplying task facts, dispatch and status assembly.

`orbit workspace init` seeds eleven default routine templates (`ci_failure_sweep`,
`dependabot_alert_sweep`, `task_pilot`, `finding_verification`, `pilot_warning_triage`,
`blocked_task_triage`, `ship_sweep`, `worktree_gc`, `store_gc`, `crew_performance_refresh`,
and `task_publication`) under
`.orbit/routines/`. Their routine names use hyphens and the registered workspace name as a
suffix (for example, `task-pilot-<workspace-name>`). Ten defaults are `enabled: false`;
`crew_performance_refresh`, added by [ORB-17816], is seeded enabled and runs hourly
at :23. With `workflow.crew_performance = "off"` it skips the scan and writes no
snapshot, while still recording a job run. [ORB-18378] added disabled weekly
`task_publication` (Sunday 04:47 host-local time), which publishes only its own
owner workspace and skips an unbound publication target. Other scheduled work
requires an explicit checkout-local opt-in through `enabled: true`. Re-init
creates newly introduced missing defaults but never rewrites existing routine files; those
files belong to the workspace after seeding. A destructive force initialization recreates
templates from defaults. [ORB-10739]

[ORB-11107] introduced `ci_failure_sweep`, which files failures from host-collected CI evidence.

`task_pilot` is a `preparation_eligible` state routine targeting
`job:task_pilot_pipeline`; [ORB-12745] replaced its cron trigger with owner-bound
preparation eligibility. Init resolves this host's machine ID and the
workspace's base branch. It assesses new/materially edited eligible tasks in
bounded batches and stays quiet for fresh assessments, including unready ones.
See [State routines and task-pilot configuration](./2_design.md#11-state-routines-and-task-pilot-configuration)
for the loadable YAML, eligibility/freshness controls and preview commands.
`finding_verification` uses `finding_unverified`, `pilot_warning_triage` uses
`pilot_warnings_held` and `blocked_task_triage` uses `block_escalated`; failed-run triage's
`execution_failed` kind is retained for compatibility and fires nothing.
Delivery routine configuration and evidence requirements are in
[Delivery routines](./2_design.md#12-delivery-routines).

The files under `crates/orbit-core/assets/routines/retired/` are provenance shapes of retired defaults, not seeds: workspace sync compares a definition with them to tell an Orbit-seeded copy from an operator's.

---

## 1. Motivation

The original problem was recurring work with no shared scheduling mechanism:
chores such as vault auto-commits, session-log extraction and semantic reindexing
depended on someone remembering them. Routines provide that mechanism through
the installed OS clock; enabling it and opting into individual chores remain
operator choices. The work spans a laptop and an always-on server with different
availability profiles (a laptop that sleeps vs. an always-on box), so any solution must
handle missed-fire policy and per-host toggles.

Orbit is the right owner because the hard parts already exist here:

1. **Execution.** The [activity-job](../activity-job/1_overview.md) layer provides typed,
   auditable runnable units and an orchestration grammar. Routines add only a *trigger source*
   in front of it — not a new runtime.
2. **Cross-workspace dispatch precedent.** `orbit run ship-sweep` already enumerates the
   global workspace registry from an unattended scheduler and dispatches runs with
   per-workspace opt-in and failure isolation. Routines generalize that shape.
3. **Local durable state.** Orbit already persists run state in workspace-local SQLite
   stores; routine state follows the same pattern.

A scheduler outside Orbit would have to reimplement run history, audit, and policy — the
fragmentation this feature exists to end.

---

## 2. Core Concepts

- **Routine** — a YAML definition in a registered workspace's gitignored `.orbit/routines/`:
  name, trigger, target, `enabled`, and policy. The durable unit of scheduling.
- **Target** — what fires: a reference into the existing catalog. v1 dispatches
  `job:<name>`; `activity:<name>` is reserved (wrap the activity in a one-step job — see
  [Routine targets are catalog references only — no inline command payloads](./4_decisions.md#routine-targets-are-catalog-references-only--no-inline-command-payloads)). Routines carry no inline commands; the `shell` activity variant was
  removed fail-closed in [ORB-00374] (see [The v2 shell activity surface is removed, not sandboxed](../activity-job/4_decisions.md#the-v2-shell-activity-surface-is-removed-not-sandboxed)), and routines inherit that posture.
- **Tick** — `orbit clock tick`, the stateless due-check pass the OS clock invokes on its
  configured cadence. It loads definitions, fires due routines, evaluates auto-task
  definitions, records state, and exits. `orbit sweep` is a compatibility alias.
- **Routine source** — a registered, active owner checkout on the host: registration
  is the whole opt-in [ORB-12236]. A replica checkout schedules only its host-local
  worktree-GC and store-GC routines; its other routines are owner-only ([2_design.md §2.1](./2_design.md#21-replica-checkouts-host-local-worktree-gc-and-store-gc)).
- **Host identity** — the `machine_id`, name, and task prefix in host-local config under
  `~/.orbit/`. The ID names run ownership and display and binds state-triggered routines
  to their owner machine.
- **Owner checkout** — a registered checkout whose logical workspace this machine owns
  (host-registry). Each owner checkout evaluates its enabled cron definitions
  against its own store and `task_prefix`; delivery/state definitions additionally
  bind admission to one owner machine.
- **Local pause** — a host-local, SQLite-persisted toggle (`orbit routine pause <name>`)
  that suppresses a routine on one host without touching the shared definition.
- **Fire** — one scheduled dispatch of a routine's target, executed as a normal run with
  `origin: routine/<name>` provenance.

---

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Routine definition type + fail-closed YAML parse | `crates/orbit-types/src/workflow/routine.rs` | Done — [ORB-10021] |
| Shared delivery/state evaluation, immutable inputs and receipt validation | `crates/orbit-automation/src/delivery/`, `crates/orbit-automation/src/members/`, `crates/orbit-core/src/application/automation/` | Done — [ORB-11330], [ORB-11331] |
| Registry-neutral loading, due computation and sweep kernel; Core assembly and status | `crates/orbit-automation/src/routines/`, `crates/orbit-core/src/application/routines/` | Done — [ORB-10021], [ORB-12236] |
| Local identity/catalog composition, workspace discovery, and runtime construction | `crates/orbit-cmd/src/registry/routines.rs`, `crates/orbit-cmd/src/registry/runtime/mod.rs`, `crates/orbit-registry/src/machine_identity.rs`, `crates/orbit-registry/src/workspace_registry/` | Done — [ORB-12236], [ORB-12725] |
| Host-local scheduler state (fires, pauses) | `crates/orbit-store/src/driver/sqlite/routine_store/` | Done — [ORB-10021] |
| Sweep advisory lock (flock, host-global) | `crates/orbit-store/src/driver/sqlite/routine_store/lock.rs` | Done — [ORB-10021] |
| `orbit clock tick` and compatibility `orbit sweep` CLI entrypoints | `crates/orbit-cli/src/command/clock/tick.rs`, `crates/orbit-cli/src/command/sweep.rs` | Done — [ORB-12237] |
| `orbit routine` CLI (`list/show/pause/resume/init`) | `crates/orbit-cli/src/command/routine/` | Done — [ORB-10021] |
| `orbit clock` CLI (`status/pause/enable/repair/set/tick`) | `crates/orbit-cli/src/command/clock/` | Done — [ORB-10720], [ORB-12237] |
| launchd/systemd unit templates + installer | `crates/orbit-core/assets/clock/` + `crates/orbit-core/src/application/routines/clock/` | Done — [ORB-10021] |
| Disabled default routine seeding + workspace ship wrapper | `crates/orbit-core/assets/{routines,jobs}/` | Done — [ORB-10207] / [Delegate workspace ship routines through a synchronous wrapper job](./4_decisions.md#delegate-workspace-ship-routines-through-a-synchronous-wrapper-job) |
| Per-user definitions, seeded through init/sync | `crates/orbit-core/src/application/routines/seed.rs` | Done — [ORB-12718] replaced git-shared definitions |
| Replica-local cron GC | `crates/orbit-core/src/application/routines/loader.rs::runs_in_replica` | Done — [ORB-14173], [ORB-16354] |
| Hourly performance refresh and weekly publication defaults | `crates/orbit-core/assets/routines/crew_performance_refresh.yaml`, `crates/orbit-core/assets/routines/task_publication.yaml` | Done — [ORB-17816], [ORB-18378] |

---

## Task References

- [ORB-00374] removed the shell activity variant and run_shell dispatch.
- [ORB-10001] authored the routines design proposal (historical citation).
- [ORB-10021] implemented routines v1 and its OS clock integration.
- [ORB-10207] seeded disabled defaults and the workspace-local shipment wrapper.
- [ORB-10270] implemented historical fleet-aware routine pin diagnostics and reassignment baselines.
- [ORB-10319] extracted historical registry/MCP composition boundaries, later replaced by the local registry composition.
- [ORB-10720] made the host sweep-clock cadence configurable and controllable.
- [ORB-10739] seeded the disabled task-pilot routine.
- [ORB-11107] added host-owned CI evidence collection and failure filing.
- [ORB-11330] implemented shared delivery evaluation, frozen batches and receipt-based coverage.
- [ORB-11331] implemented state-driven preparation and failure triage.
- [ORB-12236] removed routine host pins and the routine-source role.
- [ORB-12237] combined routine and auto-task evaluation under orbit clock and retired the scheduler wrapper.
- [ORB-12718] made all .orbit definitions per-user checkout state rather than git-shared files.
- [ORB-12725] moved local machine identity into global config and removed owner display-name projections.
- [ORB-12745] replaced task-pilot cron scheduling with owner-bound preparation eligibility.
- [ORB-14173] allowed replica-local scheduled worktree GC.
- [ORB-16354] enabled replica-local store GC.
- [ORB-17816] added the enabled hourly crew-performance snapshot refresh routine.
- [ORB-18378] seeded disabled weekly task publication and shared owner-bound publishing.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
