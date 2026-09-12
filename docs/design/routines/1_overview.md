---
title: Routines — Overview
owner: claude
last_updated: 2026-09-12
last_validated: 2026-09-12
status: Accepted
feature: routines
doc_role: overview
type: design
summary: Durable, git-versioned scheduler primitive that fires catalog jobs/activities on cron triggers, per host, with local state.
tags: [routines, scheduler]
paths: ["crates/orbit-cli/src/command/routine/**", "crates/orbit-automation/src/routines/**", "crates/orbit-cmd/src/registry_routines.rs", "crates/orbit-cmd/src/registry_runtime.rs", "crates/orbit-registry/src/host_identity.rs", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-store/src/sqlite/routine_store/**"]
related_features: [routines, auto-tasks, activity-job, host-registry]
related_artifacts: [ORB-10001, ORB-10021, ORB-10207, ORB-10270, ORB-10319, ORB-10739, ORB-12236]
---

# Routines — Overview

Routines make Orbit the constellation's single scheduler. A **routine** is a durable,
git-versioned definition of recurring work — a cron trigger, a job target from the existing
catalog, and a retry/overlap policy. A stateless **`orbit clock tick`** pass, also available
through the compatibility alias `orbit sweep`, is invoked on the configured OS schedule (one
minute by default, via launchd on macOS or a systemd timer on Linux). It fires due routines
through the existing v2 run machinery and evaluates due auto-task definitions in-process.
Definitions
are shared across hosts via git; all scheduler state (last fires, pauses, locks, run history)
is host-local and never synced, so each owner checkout is an independent schedule. [2_design.md](./2_design.md) is the v1 contract;
[3_vision.md](./3_vision.md) holds what is deliberately out of scope for v1.

> **Status.** v1 shipped in [ORB-10021]; the At a Glance table lists the actual home of
> each concern. Targets are `job:<name>` in v1 — see [Routine targets are catalog references only — no inline command payloads](./4_decisions.md#routine-targets-are-catalog-references-only-no-inline-command-payloads) for why `activity:` is
> reserved. `orbit-cmd::registry_routines` composes local host identity and the workspace
> catalog from `orbit-registry` with registered runtimes; Core keeps the registry-neutral
> scheduler, validation, and dispatch kernels.

`orbit workspace init` creates the complete default set (`ci_failure_sweep`,
`dependabot_alert_sweep`, `task_triage`, `task_pilot`, `ship_sweep`, and `worktree_gc`) under
`.orbit/routines/`. Every default is `enabled: false`: scheduled execution is an explicit,
versioned opt-in made by changing the reviewed definition to `enabled: true`. Re-init
creates newly introduced missing defaults but never rewrites existing routine files; those
files belong to the workspace after seeding. A destructive force initialization recreates
templates from defaults. [ORB-10739]

---

## 1. Motivation

Recurring work across the constellation currently has no home. Nothing is scheduled at the
OS level on either host (no crontab, no custom launchd agents); recurring chores — vault
auto-commits, session-log extraction, semantic reindexing — run only when a human or agent
remembers to run them. The work spans two machines (`dk-mac`, `dk-server-1`) with different
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

- **Routine** — a versioned YAML definition in a registered workspace: name, trigger,
  target, `enabled`, and policy. The durable unit of scheduling.
- **Target** — what fires: a reference into the existing catalog. v1 dispatches
  `job:<name>`; `activity:<name>` is reserved (wrap the activity in a one-step job — see
  [Routine targets are catalog references only — no inline command payloads](./4_decisions.md#routine-targets-are-catalog-references-only-no-inline-command-payloads)). Routines carry no inline commands; the `shell` activity variant was
  removed fail-closed in [ORB-00374] (see [The v2 shell activity surface is removed, not sandboxed](../activity-job/4_decisions.md#the-v2-shell-activity-surface-is-removed-not-sandboxed)), and routines inherit that posture.
- **Tick** — `orbit clock tick`, the stateless due-check pass the OS clock invokes on its
  configured cadence. It loads definitions, fires due routines, evaluates auto-task
  definitions, records state, and exits. `orbit sweep` is a compatibility alias.
- **Routine source** — any registered, active **owner** checkout on the host: registration
  is the whole opt-in [ORB-12236]. Replica checkouts are skipped; they cannot write the
  owner's coordination store.
- **Host identity** — a `host_id` (e.g. `dk-mac`) in host-local config under `~/.orbit/`.
  It takes no part in scheduling; it names run ownership and display.
- **Owner checkout** — a registered checkout whose logical workspace this machine owns
  (host-registry). The unit of scheduling: each owner checkout evaluates every enabled
  definition against its own store and `task_prefix`, so N owner checkouts of one
  repository are N independent schedules.
- **Local pause** — a host-local, SQLite-persisted toggle (`orbit routine pause <name>`)
  that suppresses a routine on one host without touching the shared definition.
- **Fire** — one scheduled dispatch of a routine's target, executed as a normal run with
  `origin: routine/<name>` provenance.

---

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Routine definition type + fail-closed YAML parse | `crates/orbit-types/src/workflow/routine.rs` | [ORB-10021] |
| Registry-neutral loading, due computation, dispatch, and status | `crates/orbit-automation/src/routines/` | [ORB-10021], [ORB-12236], [ORB-12262] |
| Host port the scheduler evaluates against (`AutomationHost`) | `crates/orbit-automation/src/host.rs` + `crates/orbit-core/src/adapter/automation_host/` | [ORB-12262] |
| Local identity/catalog composition, workspace discovery, and runtime construction | `crates/orbit-cmd/src/registry_routines.rs`, `crates/orbit-cmd/src/registry_runtime.rs`, `crates/orbit-registry/src/` | [ORB-10270], [ORB-10319] |
| Host-local scheduler state (fires, pauses) | `crates/orbit-store/src/sqlite/routine_store/` | [ORB-10021] |
| Sweep advisory lock (flock, host-global) | `crates/orbit-store/src/sqlite/routine_store/mod.rs` | [ORB-10021] |
| `orbit sweep` CLI entrypoint | `crates/orbit-cli/src/command/sweep.rs` | [ORB-10021] |
| `orbit routine` CLI (`list/show/pause/resume/init/clock`; `clock` slated to move to `orbit clock`) | `crates/orbit-cli/src/command/routine/` | [ORB-10021] |
| launchd/systemd unit templates + installer | `crates/orbit-automation/assets/clock/` + `crates/orbit-automation/src/routines/clock.rs` | [ORB-10021] |
| Disabled default routine seeding + workspace ship wrapper | `crates/orbit-core/assets/{routines,jobs}/` | [ORB-10207] / [Delegate workspace ship routines through a synchronous wrapper job](./4_decisions.md#delegate-workspace-ship-routines-through-a-synchronous-wrapper-job) |

---

## Task References

- [ORB-10001] — authored this design-doc folder (proposal; no implementation).
- [ORB-10021] — implemented routines v1 (types, store, sweep, CLI, clock units).
- [ORB-10207] — seeded opt-in defaults and the workspace-local ship-sweep wrapper.
- [ORB-10270] — historically added fleet-aware pin diagnostics and safe host reassignment;
  the current local-only projection preserves the no-backfill state behavior:
  the old host preserves its cursor/fire/pause state, while the new host baselines on first
  observation and starts at the next natural slot without backfill.
- [ORB-12236] — removed the `hosts:` pin, placement validation, and the
  `[routines] role = "source"` config key; registering an owner checkout is the opt-in.
- [ORB-10319] — historical boundary extraction; current composition lives in `orbit-cmd`
  over `orbit-registry` local files without fleet registry/cache state.
- [ORB-10739] — added the disabled `task_pilot` default routine; its zero-input target
  leaves eligibility and bounded partitioning to `prepare_task_pilot`.
- [ORB-11107] — added the disabled `ci_failure_sweep` default routine, hourly at `5 * * * *`.
  It targets `job:ci_failure_sweep_pipeline`, which collects CI evidence on the host,
  files current failure clusters into proposed quarantine, pilots them independently,
  and fails visibly if any nonempty pilot batch contains a failed child result.
- [ORB-00374] — removed the `shell` activity variant and `run_shell` dispatch (fail-closed);
  routines inherit this constraint.
- [ORB-12262] — moved the scheduler out of `orbit-core` into `orbit-automation` behind the
  `AutomationHost` port; behavior, output, and store contracts are unchanged.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
