---
title: Scheduled Tasks — Overview
owner: claude
created_date: 2026-10-27
updated_date: 2026-11-28
last_validated: 2026-11-14
status: Draft
stage: shipped
next: "Consumer calendar and Schedule picker in orbit-office"
feature: scheduled-tasks
doc_role: overview
type: design
summary: "Let a task wait for a date — an optional not_before instant that every dispatcher honours as a self-lifting admission hold, readable by date range so a calendar can show it"
tags: [tasks, admission, scheduling, calendar, distributed-drain]
---

# Scheduled Tasks — Overview

A scheduled task carries a `not_before` time before which no dispatcher starts it, so one existing task can wait for a date while staying visible, listable and editable.

Status: shipped in Orbit. Phase 1 [ORB-19214] supplies the field, the
`Scheduled` hold, the ship refusal and operator override, and readiness and
run-show reporting. Phase 2 [ORB-19215] supplies range listing, scheduled
CLI output, dashboard editing and forecast placement. The first consumer is orbit-office's
calendar, where "do this on Friday at 9" should be a task that already exists,
shows on the calendar, and starts at that time.

## 1. Motivation

Bearing: Orbit has two kinds of time-driven work today.

- **Recurring work:** routines and auto-tasks run on a cron, interval or
  deliveries schedule ([auto-tasks](../auto-tasks/2_design.md)). An auto-task
  mints a new task at each fire.
- **Waits:** a backlog task can be held out of admission for a typed reason
  that lifts by itself ([workflow task admission](../activity-job/2_design.md#810-workflow-task-admission)).
  `ProviderBackoff` already holds a task until a `not_before` instant [ORB-14266].

The scheduled-task field lets one existing task wait for a date.

### The gap

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

### Intended outcome

A task can carry a time before which no dispatcher starts it. Everything else
about the task stays normal. It is created, piloted, edited and approved as
usual, and it is visible and listable by its date.

### Out of scope

- **Deadlines (`due`).** A due date says when work must finish, not when it
  may start. It is a different field with different semantics (overdue
  states, ordering) and is out of scope here.
- **Recurring one-offs and time windows** ("only between 09:00 and 17:00").
  Use auto-tasks.
- **Exact-minute execution guarantees.** See [2_design.md §The hold](./2_design.md#the-hold).
- **Changing when proposed tasks are approved.** A drain that approves
  pilot-verified proposed tasks still may. The hold then keeps the approved
  task waiting.

### Acceptance (feature level)

- A backlog task with a future `not_before` is not started by an auto drain,
  a pull drain or an explicit ship. The first two report it as waiting
  `scheduled`. The tool surface's ship refuses with `task_scheduled`; an
  operator's CLI or dashboard ship overrides the hold.
- After `not_before` passes, the next admission pass starts it with no
  operator action.
- `orbit.task.list` with a `not_before` range returns exactly the tasks in
  that range, with the field on every row.
- Records without the field read and write unchanged.

The mechanisms are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## 2. Core Concepts

- **`not_before`.** The task field holding the time before which no dispatcher starts the task; everything else about the task stays normal.
- **`Scheduled` hold.** The typed admission hold that keeps a backlog task waiting until `not_before` passes, then lifts by itself on the next admission pass.
- **Operator override.** An operator's CLI or dashboard ship starts a scheduled task early; the tool surface's ship refuses with `task_scheduled`.
- **Range listing.** `orbit.task.list` with a `not_before` range, which returns exactly the tasks in the range with the field on every row.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| The `not_before` field | [2_design.md](./2_design.md#the-field) | [ORB-19214] |
| The hold, ship refusal and operator override | [2_design.md](./2_design.md#the-hold) | [ORB-19214] |
| Reading by date: range listing, CLI output, dashboard editing | [2_design.md](./2_design.md#reading-by-date) | [ORB-19215] |
| Forecast placement | [2_design.md](./2_design.md#reading-by-date) | [ORB-19215] |
| The earlier hold pattern this follows | [2_design.md](./2_design.md#the-hold) | [ORB-14266] |
| Standing rules | [4_decisions.md](./4_decisions.md) | — |

## Task References

- [ORB-14266] — added the `ProviderBackoff` hold and its `not_before`, the pattern this hold follows.
- [ORB-19214] — built the field, the `Scheduled` hold and the ship refusal.
- [ORB-19215] — built the list filter, `--scheduled`, the dashboard field and the forecast placement.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
