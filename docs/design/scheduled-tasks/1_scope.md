---
title: "Scope: Scheduled tasks (not_before)"
owner: claude
last_updated: 2026-10-27
last_validated: 2026-10-30
status: Draft
stage: shipped
next: "Consumer calendar and Schedule picker in orbit-office"
feature: scheduled-tasks
doc_role: scope
type: design
summary: "Scope: let a task wait for a date — an optional not_before instant that every dispatcher honours as a self-lifting admission hold, readable by date range so a calendar can show it"
tags: [tasks, admission, scheduling, calendar, distributed-drain]
---

# Scope: Scheduled tasks (`not_before`)

Status: shipped in Orbit. Phase 1 [ORB-19214] supplies the field, the
`Scheduled` hold, the ship refusal and operator override, and readiness and
run-show reporting. Phase 2 [ORB-19215] supplies range listing, scheduled
CLI output, dashboard editing and forecast placement. The first consumer is orbit-office's
calendar, where "do this on Friday at 9" should be a task that already exists,
shows on the calendar, and starts at that time.

Bearing: Orbit has two kinds of time-driven work today.

- **Recurring work:** routines and auto-tasks run on a cron, interval or
  deliveries schedule ([auto-tasks](../auto-tasks/2_design.md)). An auto-task
  mints a new task at each fire.
- **Waits:** a backlog task can be held out of admission for a typed reason
  that lifts by itself ([workflow task admission](../activity-job/2_design.md#810-workflow-task-admission)).
  `ProviderBackoff` already holds a task until a `not_before` instant [ORB-14266].

The scheduled-task field lets one existing task wait for a date.

## Problem

A user, or an orchestrator acting for them, wants one piece of work done at a
chosen time: "send the weekly numbers on Friday at 9", "check the renewal on
1 December". Today there are three ways to get that, and none fits:

- **Keep it in `proposed` or `someday` and remember to promote it.** That is
  manual, and nothing shows the date.
- **Make a one-off auto-task.**
  - The task doesn't exist until the fire, so it can't be prepared, piloted,
    given knowledge or edited beforehand.
  - The fire depends on the routine clock being up at that minute.
  - Spent one-off definitions pile up.
- **Put the date in the description.** No dispatcher reads it.

## Goal

A task can carry a time before which no dispatcher starts it. Everything else
about the task stays normal. It is created, piloted, edited and approved as
usual, and it is visible and listable by its date.

## Shape

### The field

`not_before`: an optional RFC 3339 instant on the task, stored in UTC.

- **Absent** means today's behaviour. The field is additive and serialised
  only when set, so existing records and readers are unchanged.
- **Set** through `orbit.task.add`, `orbit.task.update` and
  `orbit task add|update --not-before <time>`, and the dashboard Scheduled editor.
  On the tool, an empty string clears it; on the CLI, `--clear-not-before`
  does. An explicit `null` leaves it unchanged: the operation wire reads
  `null` as an omitted field for every parameter, and MCP clients send
  `null` for fields they leave unset, so `null` clearing would silently drop
  a schedule.
- **Input:** an RFC 3339 instant with an offset. The CLI also accepts a local
  `YYYY-MM-DDTHH:MM` and converts it with the host's zone. The value is
  echoed back in UTC and local time, so the target is unambiguous.
- **Lifecycle:** status transitions never change it. A task can be scheduled
  in any status. The hold only matters in `backlog`.

### The hold

A new admission exclusion, `Scheduled`, beside `ProviderBackoff`:

- A `backlog` task whose `not_before` is later than now is excluded, and
  `detail` names the time. The hold lifts by itself on the first admission
  pass after the time. No clock, routine or timer is involved; whichever
  dispatcher looks next sees that the time has passed.
- **Every dispatcher honours it:**
  - the workspace auto drain's backlog listing;
  - the owner's pull-drain claim admission, so followers inherit it;
  - explicit `orbit run ship <ids>` / `orbit.workflow.ship`. This path is
    the one that matters most: explicit ids skip the backlog listing today,
    so the check must be added there. A scheduled id is refused with a
    typed `task_scheduled` error naming the time. An operator-only
    `orbit run ship <ids> --ignore-schedule` starts it anyway. Once every
    other ship check passes, and before the run is submitted (its worker
    starts at once), it records a `schedule_overridden` history event naming
    the operator and the scheduled time. Every admission route reads that
    event, so the override holds for that `not_before` value, including
    if the submission then fails; changing `not_before` reinstates the
    hold. The MCP/tool surface and the dashboard have no override.
- **Precision is "at or after", not "at".** The task starts on the next
  admission pass after `not_before`, which depends on the drain or
  dispatcher cadence (orbit-office's dispatch pass runs every 30 seconds). A
  host that is off at that time starts the task when it next dispatches.
  Surfaces must show the actual start as well as the scheduled time, never
  imply the two are the same.
- **Not work in hand:** scheduled tasks are reported as waiting
  (`orbit run readiness`, `run show`), not runnable: readiness gives
  `reason: "scheduled"`, the `detail` and `not_before`; a pull drain records
  the owner's hold under the same `scheduled` code. They don't keep a drain
  alive. A drain that has nothing else to do ends as it does today.
- **Forecast:** the [backlog forecast](../backlog-forecast/1_scope.md) places
  a scheduled task no earlier than its `not_before`.

### Reading by date

- `orbit.task.list` takes `not_before_from` and `not_before_until` (RFC 3339,
  either or both). It returns tasks whose `not_before` falls in the range,
  with both boundary instants included. Unscheduled tasks do not match a
  bounded range. Every default list row carries `not_before` (null when
  unset); an explicit `fields` projection selects only its requested keys.
  Malformed bounds are refused naming the input. That is what a calendar
  needs for a week view.
- `orbit task list --scheduled` lists tasks with a future `not_before`,
  soonest first, including `proposed` tasks labelled by status. Other filters
  still apply, and limiting happens after schedule ordering.
- `orbit.task.show` and the dashboard task detail show the time, and once
  the task has started, `started_at`: its first recorded entry into
  `in-progress`, rather than the requested time. The dashboard Scheduled
  editor accepts RFC 3339 with an offset; save an empty value to clear it.
  A backlog card carries a Scheduled chip only while its hold applies,
  respecting an operator override.

### What stays the same

- No new scheduler. Recurring work stays with routines and auto-tasks.
- No new statuses. Holds are admission reasons, not lifecycle states.
- Nothing in the core knows about calendars, meetings or orbit-office.
  Consumers read the field and the list filter.

## Non-goals

- **Deadlines (`due`).** A due date says when work must finish, not when it
  may start. It is a different field with different semantics (overdue
  states, ordering) and is out of scope here.
- **Recurring one-offs and time windows** ("only between 09:00 and 17:00").
  Use auto-tasks.
- **Exact-minute execution guarantees.** See precision above.
- **Changing when proposed tasks are approved.** A drain that approves
  pilot-verified proposed tasks still may. The hold then keeps the approved
  task waiting.

## Concerns and honest limitations

- **The explicit-ship path has to be found everywhere.** Ship by id
  bypasses the backlog-exclusion listing, so the hold lives in the shared
  workflow-entry check as well. The direct admission routes, as built:
  - **Honour the hold.** The auto drain's backlog snapshot and the
    explicit-ids listing
    (`application/admission/backlog_exclusion/snapshot.rs`, `listing.rs`,
    `scheduled_exclusion` in `exclusions.rs`). The owner's pull admission,
    before and inside the claim section
    (`application/distributed/serve/admission.rs`, `admit_pull_request`). And the
    workflow-entry check
    `ensure_task_can_enter_workflow_honouring_schedule`
    (`application/task/transitions/`), which is shared by explicit ship
    (`application/job/pipeline/submit.rs`), the gate's dispatch re-check
    (`application/job/pipeline_actions/gate/admission.rs`) and worktree
    admission (`admit_task_for_workflow_as_system`, reached from
    `adapter/engine_host/runtime_host/claims.rs`). So a resumed or
    replayed gate re-checks it too. Source repair checks the hold before
    reserving or readmitting a backlog task; a linked replay reconciles its
    existing run first. An unlinked reservation waits until the hold lifts.
  - **Cannot start a scheduled task.** Job resume
    (`reclaim_task_for_resumed_run` in `application/job/resume/plan.rs`)
    readmits a `backlog` task only for a completion repair of a requeue made
    by its own lineage's final recovery, that is, a task that had already
    started. Source repair can also readmit `review`, `in-progress`, `blocked`,
    or `backlog` tasks. A reserved admission's `in-progress` stage, failed
    published sources with final-recovery evidence (`blocked` or `backlog`),
    and acknowledged superseded sources (`in-progress`, `blocked`, or
    `backlog`) can be eligible outside review. The hold
    applies to backlog, not blocked tasks. Claim recovery
    (`recover_claim_as_operator` in `application/review/handoff.rs`) and
    requeue (`application/task/requeue.rs`) only move a task to `blocked`
    or `backlog`; the hold applies only to backlog.
  - **Outside admission.** A human `orbit task update --status in-progress`
    is a manual transition, not a dispatch, and stays available.
- **Version skew.** The task envelope refuses unknown fields, so a host
  running an older binary refuses a bundle that carries `not_before` rather
  than ignoring it. Records without the field are byte-identical. Pull
  followers are safe because the owner decides admission. Deploys stay in
  lockstep (RB-15), and the field must not ship to a subset of owners.
- **Clock trust.** The hold compares against the owner's clock. A badly
  skewed owner clock starts tasks early or late, as it already does for
  routines.
- **Calendar honesty.** A consumer that shows only the scheduled time will
  mislead. The started-at time must be shown beside it once known.

## Phasing

1. **Core:** the field, the `Scheduled` exclusion, the ship-by-id refusal
   with the operator override, and readiness and run-show reporting.
2. **Surfaces:** the `orbit.task.list` range filter and `--scheduled`,
   the dashboard detail and an editable field, and
   the forecast placement.
3. **Consumers:** orbit-office's To-dos "Schedule…" picker and calendar
   blocks (in that repository's own docs).

## Open questions

- Should `orbit task list --scheduled` include `proposed` tasks with a
  future `not_before`? Decided and built: yes. The calendar shows planned work
  before it is approved, labelled as not approved.
- Should the override be recorded as a forced transition or as its own
  history event? Decided and built: its own event, `schedule_overridden`,
  naming the operator and the scheduled time.

## Acceptance (feature level)

- A backlog task with a future `not_before` is not started by an auto drain,
  a pull drain or an explicit ship. The first two report it as waiting
  `scheduled`. The explicit ship refuses with `task_scheduled` unless an
  operator passes `--ignore-schedule`.
- After `not_before` passes, the next admission pass starts it with no
  operator action.
- `orbit.task.list` with a `not_before` range returns exactly the tasks in
  that range, with the field on every row.
- Records without the field read and write unchanged.

## Task References

- [ORB-14266] — added the `ProviderBackoff` hold and its `not_before`, the pattern this hold follows.
- [ORB-19214] — builds the field, the `Scheduled` hold and the ship refusal.
- [ORB-19215] — builds the list filter, `--scheduled`, the dashboard field and the forecast placement.
