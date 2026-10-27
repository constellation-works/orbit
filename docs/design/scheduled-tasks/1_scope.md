---
title: "Scope: Scheduled tasks (not_before)"
owner: claude
last_updated: 2026-10-27
last_validated: 2026-10-27
status: Draft
stage: designing
next: "Build the field, the admission hold and the ship refusal (ORB-19214); then the list filter and surfaces (ORB-19215)"
feature: scheduled-tasks
doc_role: scope
type: design
summary: "Scope: let a task wait for a date — an optional not_before instant that every dispatcher honours as a self-lifting admission hold, readable by date range so a calendar can show it"
tags: [tasks, admission, scheduling, calendar, distributed-drain]
---

# Scope: Scheduled tasks (`not_before`)

Status: designing. Nothing is built; [ORB-19214] builds the core and
[ORB-19215] the surfaces. The first consumer is orbit-office's
calendar, where "do this on Friday at 9" should be a task that already exists,
shows on the calendar, and starts at that time.

Bearing: Orbit has two kinds of time-driven work today.

- **Recurring work:** routines and auto-tasks run on a cron, interval or
  deliveries schedule ([auto-tasks](../auto-tasks/2_design.md)). An auto-task
  mints a new task at each fire.
- **Waits:** a backlog task can be held out of admission for a typed reason
  that lifts by itself ([workflow task admission](../activity-job/2_design.md#810-workflow-task-admission)).
  `ProviderBackoff` already holds a task until a `not_before` instant [ORB-14266].

Nothing lets one existing task wait for a date.

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
- **Set** through `orbit.task.add`, `orbit.task.update`,
  `orbit task add|update --not-before <time>` and the dashboard. `null` (or
  `--clear-not-before`) clears it.
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
    `--ignore-schedule` on the CLI starts it anyway and records that in
    task history; the MCP/tool surface has no override.
- **Precision is "at or after", not "at".** The task starts on the next
  admission pass after `not_before`, which depends on the drain or
  dispatcher cadence (orbit-office's dispatch pass runs every 30 seconds). A
  host that is off at that time starts the task when it next dispatches.
  Surfaces must show the actual start as well as the scheduled time, never
  imply the two are the same.
- **Not work in hand:** scheduled tasks are reported as waiting
  (`orbit run readiness`, `run show`), not runnable. They don't keep a drain
  alive. A drain that has nothing else to do ends as it does today.
- **Forecast:** the [backlog forecast](../backlog-forecast/1_scope.md) places
  a scheduled task no earlier than its `not_before`.

### Reading by date

- `orbit.task.list` takes `not_before_from` and `not_before_until` (RFC 3339,
  either or both). It returns tasks whose `not_before` falls in the range,
  and every row carries `not_before`. That is what a calendar needs for a
  week view.
- `orbit task list --scheduled` lists tasks with a future `not_before`,
  soonest first.
- `orbit.task.show` and the dashboard task detail show the time, and once
  the task has started, when it actually started.

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
  currently bypasses the backlog-exclusion listing. Any other direct
  admission route (job resume of a backlog task, claim recovery) must be
  checked too, or a scheduled task can start early.
- **Version skew.** A host running an older binary ignores the field. Pull
  followers are safe because the owner decides admission. A second owner
  binary that predates the field would admit early. Deploys stay in lockstep
  (RB-15), and the field must not ship to a subset of owners.
- **Clock trust.** The hold compares against the owner's clock. A badly
  skewed owner clock starts tasks early or late, as it already does for
  routines.
- **Calendar honesty.** A consumer that shows only the scheduled time will
  mislead. The started-at time must be shown beside it once known.

## Phasing

1. **Core:** the field, the `Scheduled` exclusion, the ship-by-id refusal
   with the operator override, and readiness and run-show reporting.
2. **Surfaces:** the `orbit.task.list` range filter and `--scheduled`,
   CLI/MCP set and clear, the dashboard detail and an editable field, and
   the forecast placement.
3. **Consumers:** orbit-office's To-dos "Schedule…" picker and calendar
   blocks (in that repository's own docs).

## Open questions

- Should `orbit task list --scheduled` include `proposed` tasks with a
  future `not_before`? Proposed: yes. The calendar shows planned work
  before it is approved, labelled as not approved.
- Should the override be recorded as a forced transition or as its own
  history event? Proposed: its own event, `schedule_overridden`, naming the
  operator and the scheduled time.

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
