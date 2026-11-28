---
title: Standing Drain — Overview
owner: claude
created_date: 2026-11-19
updated_date: 2026-11-28
last_validated: 2026-11-20
status: Draft
stage: building
next: "Phases 1 (intent and surfaces) and 2 (the opt-in keeper) are built; enabling the keeper is Daniel's; next is the deploy hold, ORB-25992"
feature: standing-drain
doc_role: overview
type: design
summary: "An opt-in, config-gated drain intent that re-submits a bounded auto drain on the owner when none is live, work is waiting and nobody paused it, so deploys and window ends stop leaving ws_orbit idle"
tags: [standing-drain, distributed-drain, routines]
related_features: [distributed-drain, routines, resident-orchestrator]
---

# Standing Drain — Overview

The standing drain is an opt-in keeper that submits a new owner drain when the newest one has ended, a `backlog` or `proposed` task is waiting and nobody has paused it. It adds no admission logic: a keeper-started drain behaves exactly like `orbit run auto` with the same flags. The deploy hold that would carry it across an update is planned for phase 3 ([ORB-25992]) and is not built. Until then, an update's `orbit run auto --stop` records a pause, so the shipped keeper stays idle after the update until someone starts a drain.

## 1. Motivation

Bearing: owner-side work reaches the task pipeline by two routes. The owner drain is the
`workspace_auto_pipeline` run that an operator starts with `orbit run auto --for <window>` from the
CLI or the dashboard (at most 24 h, `MAX_DRAIN_WINDOW_SECONDS` in
`crates/orbit-core/src/application/workflow.rs`). A live drain can be lengthened
(`orbit run extend`), it re-reads the configured crew pools while it runs (ORB-25866,
1510e55ff0; ORB-25893, 1e1a8dbb07), it gives usage-limited crews weight 0 instead of ending,
and it can survive a binary swap (`drain-adopt-v1`, and the clock's upgrade resume in
[upgrades.md](../../runbooks/upgrades.md) §"switch"). Its worker dispatches `task_auto_pipeline`
leaves for the backlog work the admission ladder admits, and `orbit.workflow` action `auto` is
operator-only, so no resident can submit a drain. The explicit ship route is separate ([ORB-26010] corrected this account of the dispatch routes):
`orbit run ship`, `orbit.workflow` action `ship` and the dashboard's ship submit a
`task_auto_pipeline` run directly, for the task ids they name or, with none, a run whose worker
discovers the backlog work itself. It needs no live drain, and it passes the same entry admission as the owner drain
(`drain_entry_admission` in `crates/orbit-core/src/application/distributed/entry.rs`), which
refuses every entry point on a replica. The admission ladder, which covers
pilot, approval, auto-concurrency, build budget and host labels, is in
[distributed-drain](../distributed-drain/2_design.md). This scope adds none of that. It adds the
one thing missing: a drain that has *ended* is never started again without a person.

Approval: the formation chief approved phases 1 and 2 to build on 2026-11-20 (ruling on
[ORB-25951], 06:30:42Z). The chief acted under Daniel's delegation: "You have the full authority to make
decisions on my behalf." (chief's chat, 2026-11-19). Daniel has not reviewed this doc,
and its status stays Draft. Enabling the keeper, its flags and its window remain his decisions. Phase 3
is filed but not built this shift.

### The gap

These figures cover dk-server-2, ws_orbit (`orbit-5c61b3`), from 2026-11-13 06:00Z to 2026-11-20 06:00Z. They come from a read-only
pass over `job_runs`, `job_run_states` and `v2_audit_events`; the per-drain table is in the
ORB-25951 evidence notes.

- No drain ran for 61.9 of 168 h. The two long gaps were 18.25 h (11-13 08:46Z → 11-14 03:01Z) and
  35.38 h (11-18 14:11Z → 11-20 01:33Z). Twelve more gaps ran from 0.13 to 1.5 h.
- All 38 drains were started by hand: 29 from the dashboard and 9 from the CLI. 33 ended through an
  admissions stop (26 dashboard, 7 CLI), 2 ended when their window ran out, 2 were interrupted and 1 was cancelled while pending.
  23 of the 33 stops were followed by a new drain within 15 min (p50 0.2 min). Most
  restarts are a stop-and-start done in one sitting. The cost is in the few that nobody followed up.
- The waiting it caused is at least 581 task-hours over 189 distinct tasks. That figure comes from hourly
  queue snapshots of admissible work and is a floor. The perf review's 1,468 task-hours over 257
  tasks, which also counts proposed work, is the ceiling.

**(a) Why each restart was needed** (the cause of each new start, classified from the previous
drain's end, binary changes and `input_json` diffs):

| Cause | Count | Evidence (examples) |
|---|---|---|
| Deploy (new binary, or a stop taken just before a deploy) | 13 | 11-18 `jrun-20261118-1240-t1` restarted after `deploy-20261118-1134` (audit 2802526). 11-15 CLI stop (audit 2698944) one second before `deploy-20261115-0841`, which failed |
| Config / flag / pool change | 13 | `jrun-20261116-0410-t1`: max leaves 2→20, window 12h→5h. `jrun-20261120-0242-t1`: muse crews removed (audit 2824807) |
| Other (identical inputs and binary; reason unrecorded) | 8 | e.g. `jrun-20261114-0345-t2`, `jrun-20261118-0307-t1` |
| Window expired | 2 | `jrun-20261118-1353-t1` (17-min window sized to end before the 14:48Z deploy), then `jrun-20261120-0133-t1` 35.4 h later |
| Host | 1 | Box powered off on purpose (`sudo shutdown now`) at 11-13 08:46Z, restart 11-14 03:01Z |
| Crash | 1 | `jrun-20261114-0735-t1` worker died at 12:26Z ("database is locked" in orbit-web), restarted 71 min later |
| Usage limit | 0 | Grok and argon exhausted their weekly limits, but the drain excluded them and kept running |

9 of the 13 deploy restarts also changed settings.

The 35.4 h gap is the expensive one. During the 11-18 deploy series, two drains were given
65-min and 17-min windows so they would end before the deploy. The last deploy finished at
16:16Z, and nobody started a drain until 11-20 01:33Z. The queue grew from 1 to 21 tasks, and
that one gap accounts for about 370 task-hours.

### What exists today, and why it did not cover these restarts (b)

| Mechanism | Exists? | Would cover | Why it was not in use |
|---|---|---|---|
| Standing drain service / `--forever` | No | — | Every drain is a bounded, submitted run. `--for` is capped at 24 h. Without `--for`, a drain makes a single pass. No unit for this exists on the box |
| A routine that targets `job:workspace_auto_pipeline` | Possible in principle | Window end, crash | A routine can't pass caller parameters ([routines 2_design](../routines/2_design.md) §parameters), so it gets single passes without `--complete` or `--approve-proposed`. It also doesn't see an operator stop, which is stored on one run. None of the 9 ws_orbit routines on the box targets the drain, and routines are Daniel's to create |
| Resident orchestrator with `dispatch` | Yes, opt-in | No restart: it can ship named tasks, but cannot submit a drain | A dispatch-granted resident can call `orbit.workflow` action `ship` with backlog task ids, which submits a `task_auto_pipeline` run and no drain, plus the `orbit.task.recover` actions `repilot` and `requeue`. It cannot call `orbit.workflow` action `auto`: that is operator-only and answers `capability_denied` even with `dispatch` (`crates/orbit-core/assets/skills/orbit/references/resident-cycle-act.md`). `resident-cycle-orbit` is disabled on the box, and each drain would still need an operator to submit it. Its design already names this pain (`drain-relaunch-after-binary-update`, [resident-orchestrator 2_design](../resident-orchestrator/2_design.md)) |
| `orbit run extend` | Yes | Window expiry, if run before the end | It works only on a live drain. A closed or stopped window is refused. It can't be scheduled from a routine |
| `drain-adopt-v1` and the clock's upgrade resume | Yes | Deploy | The deploy wrapper defeats it. `update-orbit.sh` (constellation, ORB-19698) runs `orbit run auto --stop` on every poll and "never restarts" what it stopped. The clock's upgrade resume skips drains whose admissions were stopped |
| Live pool re-read (ORB-25866, ORB-25893) | Yes, since 11-20 | Pool-change restarts | Landed after most of the week. The worker ceiling (`max_active_leaf_runs`) can be retuned on a live drain with `orbit run concurrency <run> --set N`, with `orbit run extend <run> --for <d> --concurrency N`, or with the operator `orbit_workflow` tool's `auto` action in `mode: "resize"`. A live window can be lengthened with `run extend`, but not shortened. Review timing and the flags are still fixed when a drain starts; no live verb changes them |
| Paused / idle surface | Partial | — | `run readiness` says "no drain is live". Attention has no item for "work waiting, no drain", and doctor has no drain check |

So existing pieces cover the pool-change and usage-limit causes. A dispatch-granted resident covers
none of the rest: it can ship named tasks, but only an operator session can submit an owner drain,
so turning the resident on does not restart one. The gap that remains is
that **nothing re-submits a drain after it ends, and nothing records whether a stop meant "pause" or
"restart soon"**.

### Intended outcome

When Daniel opts in, ws_orbit on the owner keeps a drain open whenever a `backlog` or `proposed`
task is waiting and nobody has paused it. That is meant to include a window end, a crash and an RB-15
deploy. Phases 1 and 2 (built) cover a window end and a crash. The deploy case needs the deploy hold
in phase 3. A deliberate pause still works, and it stays in force until someone lifts it.
Unchanged: who admits work and how (the admission ladder), the 24 h window cap, follower drains, and
every default (the feature is off).

### Out of scope

- **Followers.** Pull drains on dk-server-1 and the Mac keep their operator-started windows. The
  owner approves and lands their work.
- **Changing admission.** No new dispatch, priority or crew logic. A keeper-started drain behaves
  exactly like `orbit run auto` with the same flags.
- **Enabling it anywhere.** Turning it on, choosing flags and choosing the window are Daniel's. This scope
  enables nothing and creates no routine.
- **Waking a powered-off host.**

### Acceptance (feature level)

- With the keeper enabled and the intent `on`, after a drain's window ends or it crashes, and
  a `backlog` or `proposed` task is waiting, a new drain is submitted within one clock tick plus
  `min_resubmit_interval`, with the configured flags, unless a skip condition in §2 applies.
- After an RB-15 deploy, admission resumes on the new build without anyone starting a drain
  (phase 3).
- A dashboard or CLI stop pauses. The pause survives deploys and window ends until a person starts a
  drain, and readiness, doctor and the dashboard show it with who paused it and when.
- With the keeper off (the default, on every workspace and host), no drain is ever submitted
  automatically and nothing new reaches attention. The owner host's doctor row reports the
  measured idle-with-work hours.
- A keeper-submitted drain is recorded as submitted by the keeper, with its effective flags in
  `drain.keeper.submit`.
- The keeper never admits a task outside the drain's admission ladder. A keeper-started drain
  and a hand-started drain with the same flags make the same admission decisions.

The mechanisms are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## 2. Core Concepts

- **Owner drain.** The `workspace_auto_pipeline` run an operator starts with `orbit run auto --for <window>`, at most 24 h; the keeper restarts one after it ends, when the Keeper conditions below hold.
- **Keeper.** Shipped in phase 2 ([ORB-25991]) and off by default. On the owner's clock tick, after upgrade resume, it submits one drain through the same path as `orbit run auto` when all of these hold: `workflow.standing_drain.enabled` is set; the drain intent is `on`; no drain is pending, running or held; the newest drain ended at least `min_resubmit_interval` ago (default 10 min); a `backlog` or `proposed` task is waiting; and the crash breaker is closed. The keeper checks only these conditions, not each task's admission eligibility, which the drain's admission ladder still applies. A skip reason is audited as `drain.keeper.skip` when it changes: `intent_off` (no hand start has recorded an intent yet), `intent_paused`, `drain_live`, `drain_held` (the newest drain is in the held state), `min_resubmit_interval`, `no_work` or `breaker_open`. The breaker opens once `max_failed_resubmits` (default 3) keeper drains fail or are interrupted within an hour of each other, and stays open until a hand start.
- **Drain intent.** The durable per-workspace state `off`, `on` or `paused { by, at, reason }`; a stop pauses it and a hand start sets it back to `on`, so a window ending or a crash never pauses.
- **Deploy hold (planned, phase 3, [ORB-25992]; not built).** Designed as the hold a deploy sets before its admission stop and the updater clears when the install finishes, so the next tick starts a drain on the new binary; a hold past the deploy budget would expire and raise an attention item. The shipped keeper reads no hold. The stop an update runs (`orbit run auto --stop`) records a pause, as any stop does, so until phase 3 the keeper stays idle after an update until a person starts a drain.
- **Admission ladder.** The unchanged pilot, approval, concurrency, budget and crew-health checks; the keeper never admits a task outside it.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Measured idle hours and why each restart was needed | [1_overview.md §1](#the-gap) | [ORB-25951] |
| Drain intent | [2_design.md](./2_design.md#drain-intent) | [ORB-25951] |
| Re-submission by the keeper | [2_design.md](./2_design.md#re-submission) | [ORB-25951] |
| Pause, restart and deploy hold | [2_design.md](./2_design.md#pause-restart-and-deploy-hold) | [ORB-19698] |
| Live pool re-read of a running drain | [2_design.md](./2_design.md#re-submission) | [ORB-25866], [ORB-25893] |
| Existing dispatch routes | [1_overview.md §1](#what-exists-today-and-why-it-did-not-cover-these-restarts-b) | [ORB-26010] |
| Standing rules | [4_decisions.md](./4_decisions.md) | — |

## Task References

- [ORB-25951] — is the on-call review that measured the gaps and recorded the chief's ruling on phases 1–2.
- [ORB-25866] — made a running drain re-read its configured crew pools while it runs.
- [ORB-25893] — followed up the live pool re-read for crews removed from a running drain's pools.
- [ORB-19698] — made update-orbit.sh hold drain admissions for the wait itself, the deploy hold this scope relies on.
- [ORB-26010] — corrected this scope's account of the existing dispatch routes.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
