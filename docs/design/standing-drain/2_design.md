---
title: Standing Drain — Design
owner: claude
created_date: 2026-11-19
updated_date: 2026-11-28
last_validated: 2026-11-20
status: Draft
feature: standing-drain
doc_role: design
type: design
summary: "Design: an opt-in, config-gated drain intent that re-submits a bounded auto drain on the owner when none is live, work is waiting and nobody paused it, so deploys and window ends stop leaving ws_orbit idle"
tags: [standing-drain, distributed-drain, routines]
related_features: [distributed-drain, routines, resident-orchestrator]
---

# Standing Drain — Design

This doc holds the mechanisms behind [1_overview.md](./1_overview.md), which states the problem and the goal.

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
  person starts a drain. The breaker latches: the hour is only how closely the failures must cluster to trip it, so
  it stays open however much time passes with no hand start.
- Owner only, and only on hosts where the clock runs. If the host is off, nothing happens. The 18.25 h shutdown
  gap is not in scope.

### Configuration

| Key | Default | Meaning |
|---|---|---|
| `workflow.standing_drain.enabled` | `false` | Turns the keeper on for this workspace. When off, intent and holds are still recorded, but nothing is submitted |
| `workflow.standing_drain.window` | `"24h"` | Window for each submitted drain (≤ 24 h) |
| `workflow.standing_drain.concurrency` | unset | The worker ceiling (`max_active_leaf_runs`) of each submitted drain. Unset omits it, so the job default applies: 5, from `workspace_auto_pipeline` `default_input`. This matches a plain `run auto` without `--concurrency` |
| `workflow.standing_drain.complete` / `.approve_proposed` / `.auto_fix` | `false` | Same meaning as the `run auto` flags |
| `workflow.standing_drain.min_resubmit_interval` | `"10m"` | Minimum time from one drain's end to the keeper's next submission |
| `workflow.standing_drain.max_failed_resubmits` | `3` | Crash-breaker threshold: failures within one hour trip it, and it stays open until a hand start |

The `false` default applies to every workspace and every host, followers and workspaces that
never keep a drain included.

The worker ceiling and the forecast mode are separate settings, and the keeper keeps them apart ([ORB-26009] corrected the worker-ceiling and forecast-mode claims in this section).
The ceiling bounds how many `task_auto_pipeline` leaf runs are live at once. The forecast admission
mode is `machine.auto_concurrency.mode` (default `off`), which `run auto --auto-concurrency` can
override for one run. An unset `concurrency` never reads the forecast mode. The keeper has no
forecast-mode flag, so each drain it submits inherits `machine.auto_concurrency.mode` with source
`configured`, as a plain `run auto` without `--auto-concurrency` does. The drain reads the host's current
setting at each pass, so changing `machine.auto_concurrency.mode` during its window changes that drain's
forecast mode from the next pass. A live retune (`orbit run concurrency`,
`orbit run extend --concurrency`, or the operator `auto` action in `mode: "resize"`) changes only that run's ceiling.
The next keeper drain starts from the configured value, or from the job default when it is unset.

### Authority

The flags are authority: `approve_proposed` lets the drain approve work, and `enabled` lets the
keeper start drains with nobody present.

- Setting `enabled` and `approve_proposed` is Daniel's, as creating a routine is. The
  [configuration reference](../../config/workflow.md#workflowstanding_drain--the-opt-in-drain-keeper) has no operator-only class of config key, so this is a rule, not something code enforces.
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

1. **Surface only ([ORB-25965]), built.** The intent is a `drain_intents` store row (schema
   v95). A hand stop (`orbit run auto --stop`, the dashboard's Stop, or the `orbit.workflow` auto
   stop that the desktop control also uses) records `paused` with the acting identity, surface,
   reason and time. Only the `orbit.workflow` auto stop takes `force`; `orbit run auto --stop` and
   the dashboard's Stop take none. A hand start from the CLI, dashboard or MCP, local or
   `--pull`, records `on`; routine submissions record nothing. `orbit run readiness`
   (`drain_intent`) and the dashboard's Auto-drain card show it. The owner host's `orbit doctor`
   `standing-drain` row reports the idle-with-work hours, task-hours and a per-UTC-day series
   for the last 7 days, rebuilt from task status histories and drain admission windows. That
   series is the daily figure the perf review reads. It counts `backlog` and `proposed` tasks
   as waiting, so it is a ceiling on admissible work. Nothing reads the intent, nothing is
   submitted and no config key was added. Operation:
   [distributed-drain runbook](../../runbooks/distributed-drain.md#drain-intent-standing-drain-phase-1).
2. **Keeper ([ORB-25991]), built, off by default.** The clock tick, after upgrade resume and
   only on the owner, submits one drain through `submit_workspace_auto_run` (the `orbit run
   auto` path) when `workflow.standing_drain.enabled` is set, the intent is `on`, no drain is
   pending, running or held, a `backlog` or `proposed` task waits, `min_resubmit_interval`
   has passed since the newest drain ended and the crash breaker is closed. The submission's
   trigger is the routine `standing-drain-keeper`, so it records no intent. Upgrade
   interruptions do not count toward the breaker, and a hand start closes it. Each
   submission is audited as `drain.keeper.submit` with the effective flags; a skip is audited
   as `drain.keeper.skip { reason }` when its reason differs from the previous decision, so an
   unchanged reason is not rewritten every tick. Readiness (`drain_keeper`), the doctor row
   and the attention kinds `drain_paused` and `drain_breaker_open` report it only while it is
   enabled. Operation: [distributed-drain runbook](../../runbooks/distributed-drain.md#standing-drain-keeper-phase-2).
   It still gets an independent review, and is in that night's security review if it lands
   before it.
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

## Task References

- [ORB-25965] — built phase 1: the drain intent, pause attribution, doctor row, readiness field and idle-with-work figure.
- [ORB-25991] — built phase 2, the keeper, off by default.
- [ORB-25992] — is to build phase 3, the deploy hold, after ORB-25991 (someday).
- [ORB-25951] — is the on-call review that measured the gaps and recorded the chief's ruling on phases 1–2.
- [ORB-26009] — corrected this scope's worker-ceiling and forecast-mode claims.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
