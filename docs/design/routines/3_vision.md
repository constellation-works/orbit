---
title: Routines — Vision
owner: claude
last_updated: 2026-09-12
last_validated: 2026-09-12
status: Draft
feature: routines
doc_role: vision
type: design
summary: Target contract for the clock consolidation (one host tick for routines and auto-tasks, no host pins, no source role), plus open questions and prior art.
tags: [routines, scheduler]
paths: ["crates/orbit-core/src/application/routines/**", "crates/orbit-cmd/src/registry_routines.rs", "crates/orbit-cmd/src/registry_runtime.rs", "crates/orbit-registry/src/**"]
related_features: [routines, auto-tasks, activity-job, host-registry, task-migration]
related_artifacts: [ORB-10001, ORB-10021, ORB-10207, ORB-10270, ORB-10319, ORB-11315, ORB-12236, ORB-12237]
---

# Routines — Vision

Forward-looking questions for the routines feature. Everything here is explicitly *not*
part of the v1 contract in [2_design.md](./2_design.md); items graduate through an explicit
task, implementation, and validation evidence, not by drifting in.

---

## 0. Graduating: clock consolidation

Decided 2026-09-12; implemented by [ORB-12236] (hosts/role removal) then [ORB-12237] (tick, `orbit clock`, scheduler retirement). The reasoning is in
[One host tick evaluates routines and auto-task definitions in-process](./4_decisions.md#one-host-tick-evaluates-routines-and-auto-task-definitions-in-process),
[Definitions carry no host pin: every owner checkout is an independent schedule](./4_decisions.md#definitions-carry-no-host-pin-every-owner-checkout-is-an-independent-schedule),
and [Registration is the automation opt-in; there is no routine-source role](./4_decisions.md#registration-is-the-automation-opt-in-there-is-no-routine-source-role).
This section is the target contract those decisions imply; when it ships, it replaces the
corresponding parts of [2_design.md](./2_design.md) and this section is deleted.

### 0.1 Surfaces

```
orbit clock status            # cadence, native-manager state, health (was: orbit routine clock status)
orbit clock pause | enable    # host-wide, untouched routine/auto-task state
orbit clock set <seconds>     # whole-minute cadence, reloads the unit
orbit clock tick [--dry-run] [--verbose] [--json]   # the pass the OS unit invokes (alias: orbit sweep)
```

`orbit routine clock` is removed. `orbit routine list|show|pause|resume|init` and
`orbit auto-task add|list|show|update|toggle|mint` are unchanged. The launchd/systemd unit
templates invoke `orbit clock tick`. The dashboard clock card and its typed control set are
unchanged in shape; the clock is one host-scoped card, routines and auto-tasks two
workspace-scoped panels.

### 0.2 The tick

Per pass, after taking the host sweep flock:

1. Load `~/.orbit/workspaces.json`; build a runtime for every active **owner** checkout
   whose `.orbit/` exists. Replica checkouts are skipped. No `[routines]` config key is
   consulted.
2. Load `.orbit/routines/*.yaml` and `.orbit/routines/local/*.yaml` from each. There is no
   placement validation step: a `hosts:` key is ignored with a load warning (one release),
   then rejected as unknown.
3. Routine evaluation — unchanged from [2_design.md §3](./2_design.md) steps 4–10: outcome
   sync, cursor due-math, overlap, fire intent, `submit_pipeline_run`, worker identity gate.
4. Auto-task evaluation — for each runtime, call the existing scheduler pass
   (`run_auto_task_scheduler_at`) directly: load `.orbit/auto_tasks/*.yaml`, hold the
   `.auto-tasks.json.lock` sidecar, baseline/skip/fire per definition, mint into that
   checkout's store, checkpoint the cursor. Dry-run threads through. Recovery semantics
   (`pending`, unresolved claims, mint rollback) are unchanged.
5. Emit one report with routine rows and auto-task rows. Quiet mode prints only noteworthy
   actions (`fired`, `retry_fired`, `baselined`, `error`, `minted`) so a healthy host does not
   grow the log every tick.

Auto-task evaluation runs second so a slow mint cannot delay routine dispatch, and is
bounded; an evaluator error for one workspace or one definition is a report row, never an
aborted tick.

### 0.3 Eligibility

A definition (routine or auto-task) is evaluated on a host iff:

| Switch | Where it lives | Set by |
|---|---|---|
| owner checkout registered | `~/.orbit/workspaces.json` (host-local) | `orbit workspace init` / registration |
| clock enabled | native manager + `~/.orbit/clock.toml` (host-local) | `orbit clock enable` |
| definition `enabled: true` | the YAML (git-shared) | PR review |
| no local pause (routines only) | `~/.orbit/orbit.db` (host-local) | `orbit routine pause` |

N owner checkouts of one repository are N independent schedules; each acts only on its own
store under its own `task_prefix`. Nothing coordinates across hosts.

### 0.4 Retirements

- Embedded defaults: `routines/auto_task_scheduler.yaml`, `jobs/auto_task_scheduler_pipeline.yaml`,
  `activities/run_auto_task_scheduler.yaml`, and the `run_auto_task_scheduler` dispatch arm.
  Existing seeded copies retire through the managed-asset manifest
  (`orbit doctor --fix-stale-artifacts`); an operator-edited copy is preserved under
  `.retired-managed/`.
- `RoutineDefinition::hosts`, `validate_committed`/`validate_local`'s host checks,
  `RoutinePlacementProvider`, `owner_host_ids` projection, and the `host_belongs_elsewhere` /
  `host_unresolvable` diagnostics. `RoutineSeedIdentity` keeps only the workspace name.
- `[routines] role` in `orbit-config` raw/resolved config.
- The `hosts` column in `orbit routine list`, `GET /api/routines`, and the dashboard routine
  rows.
- Docs and skills that teach `orbit routine clock`, `role = "source"`, or `hosts:` (the
  `orbit` skill's `setup/automation.md` and `setup/auto-tasks.md`, the website
  `concepts/scheduling` and `how-to/recurring-work` pages, `docs/runbooks/health-checks.md`).

### 0.5 Migration on an existing host

1. Upgrade the binary; `orbit clock status` reports the installed unit as stale (it still
   invokes `orbit sweep`, which continues to work as an alias) and `orbit clock enable`
   rewrites it.
2. `orbit workspace sync` re-seeds defaults: `auto_task_scheduler` is retired, other seeded
   routines lose their `hosts:` line (adopted as a managed refresh, not a collision).
3. Delete `[routines]` from `.orbit/config.toml` and `hosts:` from any workspace-authored
   routine at leisure; both warn until the following release.
4. Enable the clock on any additional owner host (e.g. a laptop that also ships tasks). Every
   enabled committed definition becomes live there against that host's store.

---

## 1. Open Questions

0. **First-class `activity:` targets.** v1 rejects `activity:<name>` at parse time because
   run dispatch is job-shaped ([Routine targets are catalog references only — no inline command payloads](./4_decisions.md#routine-targets-are-catalog-references-only-no-inline-command-payloads)); the wrapper-job idiom covers current needs. A
   standalone activity run entrypoint (or auto-wrapping) would let routines fire
   activities directly — worth doing only if the wrapper friction proves real.
1. **Single-fire across hosts.** Under the multi-owner model (§0.3) every owner checkout
   is its own schedule and nothing needs to fire exactly once across hosts — each host's
   automation acts only on its own store. The residual case is a definition with a
   repo-global side effect (one PR per owner instead of one). If that ever bites, the
   additive answer is an `owner:` field on the definition, not a lease protocol; it is
   deliberately not designed now.
2. **State-driven triggers.** The [shared trigger proposal](../automation-triggers/2_design.md)
   from [ORB-11315] defines bounded reconciliation of deliveries, preparation eligibility
   and settled failures over the existing sweep clock. This does not require a resident
   process. Immediate file-watch/webhook wakeups remain optional future optimizations;
   the proposal is unimplemented and does not change the current cron-only contract.
3. **Routine-emitted tasks.** A routine whose job files an Orbit task on findings (nightly
   drift check → task per drift) works today via job semantics; what's open is whether
   routines should get first-class dedup support ("don't file a duplicate of an open task
   from a previous fire") or leave that to job logic.
4. **Sub-minute and jitter.** Minute granularity is a v1 floor. Per-routine jitter matters
   only if many routines land on the same slot and contend; revisit when there are enough
   routines for it to be observable.
5. **Missed-run variants.** `catch_up_once | skip` covers current needs; a count-preserving
   `catch_up_all` (anacron-style) is additive if a routine ever needs per-slot semantics.
6. **Cross-host visibility.** Each host's state is local and, under §0.3, each host's
   schedule is independent, so "did the nightly commit fire on the other box?" is a
   question about that box's own automation and requires asking it. The single-host half of this is now built:
   `GET /api/routines` projects this host's routine health (last fire, outcome, duration,
   next due) over the dashboard HTTP API [ORB-10138], so a stopped sweep is visible remotely
   without box ssh. True cross-host *aggregation* (one surface querying every box's store)
   remains open; state *sync* is still explicitly not the answer.

### Graduated

- **Workspace-local ship-sweep convergence ([ORB-10207], [Delegate workspace ship routines through a synchronous wrapper job](./4_decisions.md#delegate-workspace-ship-routines-through-a-synchronous-wrapper-job)).** The default
  `ship_sweep` routine delegates synchronously to the normal shipment pipeline for only
  its source workspace. It is seeded disabled and enabled through the versioned
  definition. The legacy global CLI entrypoint remains during burn-in; removing it is a
  separate compatibility task.

---

## 2. Prior Work

### OS schedulers
- **cron / anacron** — the trigger vocabulary (5-field expressions) and the missed-run
  problem anacron exists to solve; routines adopt the vocabulary and make missed-run policy
  per-definition instead of system-wide.
- **systemd timers / launchd** — v1's actual clock. launchd wake behavior and systemd's
  monotonic startup/post-activation triggers guarantee another sweep without replaying
  every missed clock tick; routine cursors and `missed_run` own cron-gap semantics.

### Workflow engines
- **Temporal / Cadence schedules** — durable schedules attached to durable executions,
  with overlap policies (`skip`, `buffer_one`) that v1's `overlap: forbid` and
  `catch_up_once` consciously echo at much smaller scale. The full replayable-execution
  model is what this feature deliberately does *not* adopt.
- **Kubernetes CronJob** — `concurrencyPolicy`, `startingDeadlineSeconds`, and the
  documented pain of missed-fire semantics; a compact catalog of the edge cases §6 of
  [2_design.md](./2_design.md) must test.

### CI schedulers
- **GitHub Actions `schedule:`** — git-versioned schedule definitions co-located with the
  code they operate on; the definition-review-as-security-boundary posture routines share.

---

## 3. What May Be Distinctive

- **Git-versioned, PR-reviewed schedules over a knowledge-integrated runtime.** Fires are
  ordinary Orbit runs with audit envelopes, linkable to tasks — the scheduler
  and the knowledge system share one substrate.
- **Agent-invoking targets.** A routine can fire an `agent_loop` activity: scheduled agent
  work (nightly triage, periodic research) with the same policy and audit surface as any
  other run — most schedulers fire commands; this one fires accountable agent runs.
- **Definitions-shared / state-local as a stance.** Hosts converge on *what* to schedule
  through git alone and never on *whether it fired*; there is no scheduler network protocol.
  Under the multi-owner model that stance is the whole coordination story: the store an
  owner's automation writes to is the store that owner's prefix names.

---

## 4. References

Orbit-internal:
- [../activity-job/1_overview.md](../activity-job/1_overview.md) — the execution substrate
  routines trigger into.
- [../activity-job/4_decisions.md](../activity-job/4_decisions.md) — [The v2 shell activity surface is removed, not sandboxed](../activity-job/4_decisions.md#the-v2-shell-activity-surface-is-removed-not-sandboxed), the
  removed-shell posture routines inherit.
- [../executors/4_decisions.md](../executors/4_decisions.md) — [External Executor Protocol for dynamic out-of-process executor registration (retired)](../executors/4_decisions.md#external-executor-protocol-for-dynamic-out-of-process-executor-registration-retired), sandbox caveats
  relevant to what scheduled targets may do.

External:
- systemd.timer(5), launchd.plist(5) — monotonic restart and wake semantics.
- Temporal "Schedules" documentation — overlap/catch-up policy vocabulary.
- Kubernetes CronJob documentation — concurrency and missed-fire edge cases.

---

## Task References

- [ORB-12236] — implements §0.3 eligibility: removes `hosts:` pins and `[routines] role`.
- [ORB-12237] — implements §0.1, §0.2, §0.4, §0.5: `orbit clock`, the combined tick, scheduler
  retirement, and the docs fold; depends on [ORB-12236].
- [ORB-11315] — proposes shared state-driven triggers and durable coverage semantics.

- [ORB-10001] — authored this design-doc folder (proposal; no implementation).
- [ORB-10021] — implemented routines v1.
- [ORB-10207] — graduated workspace-local ship-sweep scheduling from this vision.
- [ORB-10319] — historical boundary separation; current local registry/runtime composition lives in `orbit-cmd` over `orbit-registry`.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
