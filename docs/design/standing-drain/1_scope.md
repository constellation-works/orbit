---
title: "Scope: standing drain intent for the owner workspace"
owner: claude
last_updated: 2026-11-20
last_validated: 2026-11-20
status: Draft
stage: building
next: "Phase 1 (intent and surfaces) is ORB-25965; the keeper, ORB-25991, follows it"
feature: standing-drain
doc_role: scope
type: design
summary: "Scope: an opt-in, config-gated drain intent that re-submits a bounded auto drain on the owner when none is live, work is waiting and nobody paused it, so deploys and window ends stop leaving ws_orbit idle"
tags: [standing-drain, distributed-drain, routines]
related_features: [distributed-drain, routines, resident-orchestrator]
---

# Scope: standing drain intent for the owner workspace

Bearing: work is admitted only by a live `workspace_auto_pipeline` drain, which an operator
submits with `orbit run auto --for <window>` (at most 24 h, `MAX_DRAIN_WINDOW_SECONDS` in
`crates/orbit-core/src/application/workflow.rs`). A live drain can be lengthened
(`orbit run extend`), it re-reads the configured crew pools while it runs (ORB-25866,
1510e55ff0; ORB-25893, 1e1a8dbb07), it gives usage-limited crews weight 0 instead of ending,
and it can survive a binary swap (`drain-adopt-v1`, and the clock's upgrade resume in
[upgrades.md](../../runbooks/upgrades.md) §"switch"). The admission ladder, which covers
pilot, approval, auto-concurrency, build budget and host labels, is in
[distributed-drain](../distributed-drain/2_design.md). This scope adds none of that. It adds the
one thing missing: a drain that has *ended* is never started again without a person.

Approval: the formation chief approved phases 1 and 2 to build on 2026-11-20 (ruling on
[ORB-25951], 06:30:42Z). The chief acted under Daniel's delegation: "You have the full authority to make
decisions on my behalf." (chief's chat, 2026-11-19). Daniel has not reviewed this doc,
and its status stays Draft. Enabling the keeper, its flags and its window remain his decisions. Phase 3
is filed but not built this shift.

## Problem

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

## What exists today, and why it did not cover these restarts (b)

| Mechanism | Exists? | Would cover | Why it was not in use |
|---|---|---|---|
| Standing drain service / `--forever` | No | — | Every drain is a bounded, submitted run. `--for` is capped at 24 h. Without `--for`, a drain makes a single pass. No unit for this exists on the box |
| A routine that targets `job:workspace_auto_pipeline` | Possible in principle | Window end, crash | A routine can't pass caller parameters ([routines 2_design](../routines/2_design.md) §parameters), so it gets single passes without `--complete` or `--approve-proposed`. It also doesn't see an operator stop, which is stored on one run. None of the 9 ws_orbit routines on the box targets the drain, and routines are Daniel's to create |
| Resident orchestrator with `dispatch` | Yes, opt-in | All six causes | `resident-cycle-orbit` is disabled on the box. Each restart would be a model decision made on that model's schedule, which is a heavy and slow way to do a deterministic step. Its design already names this pain (`drain-relaunch-after-binary-update`, [resident-orchestrator 2_design](../resident-orchestrator/2_design.md)) |
| `orbit run extend` | Yes | Window expiry, if run before the end | It works only on a live drain. A closed or stopped window is refused. It can't be scheduled from a routine |
| `drain-adopt-v1` and the clock's upgrade resume | Yes | Deploy | The deploy wrapper defeats it. `update-orbit.sh` (constellation, ORB-19698) runs `orbit run auto --stop` on every poll and "never restarts" what it stopped. The clock's upgrade resume skips drains whose admissions were stopped |
| Live pool re-read (ORB-25866, ORB-25893) | Yes, since 11-20 | Pool-change restarts | Landed after most of the week. Window, `max_active_leaf_runs`, review timing and the flags are still fixed when a drain starts. A live window can be lengthened with `run extend`, but not shortened |
| Paused / idle surface | Partial | — | `run readiness` says "no drain is live". Attention has no item for "work waiting, no drain", and doctor has no drain check |

So existing pieces cover the pool-change and usage-limit causes. The resident covers the rest only if
Daniel turns on a model-driven loop for what is a deterministic step. The gap that remains is
that **nothing re-submits a drain after it ends, and nothing records whether a stop meant "pause" or
"restart soon"**.

## Goal

When Daniel opts in, ws_orbit on the owner keeps a drain open whenever admissible or pilotable
work exists and nobody has paused it. That includes after a window ends, a crash and an RB-15 deploy.
A deliberate pause still works, and it stays in force until someone lifts it. Unchanged: who admits
work and how (the admission ladder), the 24 h window cap, follower drains, and every default
(the feature is off).

## Shape (c)

### Drain intent

Each workspace gets one durable drain intent, stored in the workspace store beside the drain
runs: `off` (the default, today's behaviour), `on`, or `paused { by, at, reason }`. The drain's
launch spec is taken from config, never from the last run's input. Its flags are the window,
concurrency, `complete`, `approve_proposed` and `auto_fix`.

### Re-submission

The existing clock tick (the sweep that already performs upgrade resume) gains one check per
workspace. It submits one drain through the same submission path as `orbit run auto` when every one
of these holds:

1. `workflow.standing_drain.enabled = true` and the intent is `on`;
2. this host is the workspace's owner (followers keep their own pull drains);
3. no `workspace_auto_pipeline` run is live, pending or held for the workspace;
4. no deploy hold is set (see below);
5. admissible or pilotable work exists (the readiness forecast's input, not a new query);
6. at least `min_resubmit_interval` has passed since the last drain ended, and the crash
   breaker is not open.

The keeper never admits a task itself. The drain it submits goes through the unchanged
admission ladder, so pilot, approval, concurrency, budgets and crew health all still apply. That
satisfies the chief's gate: there is no dispatch path outside the ladder.

### Pause, restart and deploy hold

- `orbit run auto --stop`, and the dashboard's stop, set the intent to `paused` and record who did it and when. Starting a
  drain by hand (CLI or dashboard) sets it back to `on`. A stop-and-start restart therefore leaves the
  intent `on`, and that pattern covers 23 of the 33 stops. With the keeper off, recording the
  pause changes no behaviour.
- Phases 1 and 2 add no `--stop --restart`. A hand start already sets the intent to `on`.
- In phase 3, deploys take a **deploy hold** instead of a pause. `update-orbit.sh` and the Mac RB-15 sequence set it
  before their admission stop, and the guarded updater clears it when the install finishes,
  whatever the outcome. While the hold is set, the keeper doesn't submit. When it clears, the next tick
  starts a drain on the new binary. A hold older than the deploy budget expires on its own and
  raises an attention item, so a crashed deploy can't hold admission forever.
- A window ending does not pause, and neither does a crash. The keeper simply resubmits.

### Bounds

- At most one keeper-submitted drain per workspace at a time. The window is capped at 24 h, as today.
- `min_resubmit_interval` (default 10 min) limits how fast it can churn.
- Crash breaker: if `max_failed_resubmits` (default 3) keeper-started drains end failed or
  interrupted within an hour, the intent stays `on`, the keeper stops submitting, and attention reports it until a
  person starts a drain or lifts the breaker.
- Owner only, and only on hosts where the clock runs. If the host is off, nothing happens. The 18.25 h shutdown
  gap is not in scope.

### Configuration

| Key | Default | Meaning |
|---|---|---|
| `workflow.standing_drain.enabled` | `false` | Turns the keeper on for this workspace. When off, intent and holds are still recorded, but nothing is submitted |
| `workflow.standing_drain.window` | `"24h"` | Window for each submitted drain (≤ 24 h) |
| `workflow.standing_drain.concurrency` | unset | Unset uses the configured auto-concurrency, as a plain `run auto` does |
| `workflow.standing_drain.complete` / `.approve_proposed` / `.auto_fix` | `false` | Same meaning as the `run auto` flags |
| `workflow.standing_drain.min_resubmit_interval` | `"10m"` | Minimum time from one drain's end to the keeper's next submission |
| `workflow.standing_drain.max_failed_resubmits` | `3` | Crash-breaker threshold per rolling hour |

The `false` default applies to every workspace and every host, followers and workspaces that
never keep a drain included.

### Authority

The flags are authority: `approve_proposed` lets the drain approve work, and `enabled` lets the
keeper start drains with nobody present.

- Setting `enabled` and `approve_proposed` is Daniel's, as creating a routine is. docs/CONFIG.md
  has no operator-only class of config key, so this is a rule, not something code enforces.
- No session sets `enabled = true` on a live host, for a test or for any other reason. QA runs
  under a disposable home.
- A keeper-submitted drain is recorded as submitted by the keeper, not by a person.
  `drain.keeper.submit` records the effective flags (window, concurrency, `complete`,
  `approve_proposed`, `auto_fix`).

### Surfaces

Off means silent. With `workflow.standing_drain.enabled = false`, nothing new reaches
attention.

- `orbit run readiness --json` and the dashboard show the intent (`off`, `on`, `paused` with
  who/when, `deploy-hold`, `breaker-open`) and the last keeper decision.
- On the owner host, `orbit doctor` gets a `standing-drain` row. With the keeper off, the row
  reports the hours with work waiting and no drain; that figure appears only there and in the
  daily perf figure. With the keeper enabled, the row shows its state and the effective flags.
- Attention items exist only while the keeper is enabled: "paused for over 12 h with work
  waiting", "deploy hold expired" (from phase 3) and "crash breaker open".
- An audit event `drain.keeper.submit` / `drain.keeper.skip { reason }` for each decision,
  plus a daily idle-with-work figure for the perf-review auto-task.

## Cost of the idle hours (d)

While a drain was running this week, ws_orbit averaged 12.6 leaf runs and 8.6 landed tasks per
drain-hour, which is about 1.46 leaf runs per landed task. For implementation runs, reading 3,015 saved outputs, a run used a median of 3.98 M
and a mean of 6.36 M tokens including cache reads. Excluding cache reads it was a median of 0.18 M and a mean of 0.25 M.

- An unbounded upper figure is 12.6 × 61.9 h ≈ 777 leaf runs. That is wrong, because the queue, not drain
  time, set the limit.
- Bounded by the queue: 189–257 waiting tasks × 1.46 ≈ **276–375 leaf runs**, which is
  ≈ **1.8–2.5 B tokens including cache reads (≈ 70–95 M without them)**.
- These runs were delayed, not lost, and the tokens were spent later anyway. The real cost is
  **581–1,468 task-hours of waiting**. The keeper would have recovered the 35.4 h gap, the
  71-min crash gap and the short post-deploy gaps: about 40 of the 62 h, and most of the
  task-hours. It would not have recovered the 17.5 h the box was powered off.

## Non-goals

- **Followers.** Pull drains on dk-server-1 and the Mac keep their operator-started windows. The
  owner approves and lands their work.
- **Changing admission.** No new dispatch, priority or crew logic. A keeper-started drain behaves
  exactly like `orbit run auto` with the same flags.
- **Enabling it anywhere.** Turning it on, choosing flags and choosing the window are Daniel's. This scope
  enables nothing and creates no routine.
- **Waking a powered-off host.**

## Concerns and honest limitations

- **What a stop means.** Today's stops don't say whether they mean pause or restart. Making a plain
  stop mean pause is the safe default. The cost is that an operator who stops a drain in order to restart it later
  must start it again, which is what they do today anyway.
- **8 restarts have no recorded reason.** Dashboard actions don't record their parameters. If
  those were deliberate pauses, the keeper must not undo them. The pause rule above ensures that.
- **Flags come from config, not from the last run.** An operator who launched with special flags
  gets the configured ones back after a window ends. That is intended: config is reviewable, and a one-off
  launch is not.
- **The deploy hold spans two repositories.** `update-orbit.sh` lives in the constellation
  repository, so the hold needs a change there and a CLI verb in Orbit.

## Phasing

1. **Surface only ([ORB-25965]).** Add the intent, pause attribution, the owner-host
   `standing-drain` doctor row, the readiness field and the daily idle-with-work figure. Nothing
   is submitted. This shows the numbers before anything steers.
2. **Keeper ([ORB-25991]).** Clock-tick re-submission behind `workflow.standing_drain.enabled`,
   with the bounds, authority rules and audit events above. It depends on phase 1, gets an
   independent review, and is in that night's security review if it lands before it.
3. **Deploy hold ([ORB-25992]).** Add the CLI verb and its use in `update-orbit.sh` and the Mac
   RB-15 sequence, and update RB-15 step 9. This is filed but not built this shift: no change
   to deploy tooling while the shift still owes binary updates.

Testing follows the repository rule that a test is added only for a reproduced defect. Where
that rule admits no test, the evidence for phase 2 is manual QA through the real CLI under a
disposable home, recorded on the task. It shows that:

- a window end leads to a resubmission;
- a pause is never resumed across a window end or a restart of the clock;
- with the keeper off, nothing is submitted;
- the breaker opens after 3 failed keeper-started drains.

## Resolved questions

Each of these was resolved by the chief's ruling on [ORB-25951].

1. **`--stop --restart`:** it is not in phases 1-2. A hand start already sets the intent to `on`.
2. **Extend or resubmit:** resubmit. Extension keeps one run id across a day, but it can't change
   flags and it can't follow a crash.
3. **Where the intent lives:** a store row, because a pause is runtime state with an author.
   Config only says whether the keeper may act.

## Acceptance (feature level)

- With the keeper enabled and the intent `on`, after a drain's window ends or it crashes, and
  admissible or pilotable work exists, a new drain is submitted within one clock tick plus
  `min_resubmit_interval`, with the configured flags.
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

## Task References

- [ORB-25965] — phase 1: intent, pause attribution, doctor row, readiness field, idle figure
- [ORB-25991] — phase 2: the keeper (depends on ORB-25965)
- [ORB-25992] — phase 3: deploy hold (depends on ORB-25991; parked in someday)
- [ORB-25951] — the on-call perf review that measured the gaps
- [ORB-25866] — live crew-pool re-read in a running drain
- [ORB-25893] — follow-up to the live pool re-read
- [ORB-19698] — the deploy wrapper's admission hold

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
