---
name: orbit-orchestrate
description: Prepares and supervises an Orbit backlog. Deduplicates and pilots tasks (task-pilot), assigns crews, promotes ready work, dispatches with `orbit run ship` and `orbit run auto` drains, diagnoses failed or stuck `jrun-*` runs, and routes CI/QA sweep findings into repair tasks. Use for delivery operations across tasks, jobs, activities and workflow runs. Use orbit for one assigned task and orbit-setup for machine configuration or scheduler installation.
---

# Orbit Orchestrate

Keep authorized work moving using durable task, run and delivery evidence.
A managed implementation worker stays within its assigned task; this skill
is for the operator supervising work.

## Operating loop

Inspect → deduplicate → author → prepare → promote → dispatch → verify → repair.

- Discover the owning workspace and use its returned selector; see
  [tool surface](../orbit/references/tool-surface.md). Never substitute a local
  store for an unavailable owner.
- Follow the user's scope, crew choices and concurrency/build budget. After
  authorized preparation succeeds, promote promptly. An active drain can claim
  backlog work immediately, so finish scope changes before exposing it.
- Creation, promotion and completion are distinct. `--complete` authorizes
  delivery for work admitted in that invocation's window; it does not itself
  authorize promotion, future windows or releases. Existing continuous-delivery
  authorization may cover them; apply the user's actual instructions.
- Verify persisted preparation, actual diffs, required checks and merge state.
  Post-merge review/QA findings become repairs under continuous delivery; do not
  invent an extra pre-merge approval gate. Repository protections still apply.
- Diagnose blocked work before retrying. Preserve user interventions and active
  work. A stopped admission loop does not mean its children were cancelled.
- Record `orchestrator` separately from execution `crew`. Neither attribution
  field grants authority. Leave a durable handoff when a lane cannot proceed.

## Choose the reference

| Need | Read |
|---|---|
| Prepare/promote work and supervise progress | [Operating loop](references/loop.md) |
| Scope completion authority and dispatch windows | [Authorization](references/authorization.md) |
| Ship/auto command examples, locks and capacity | [Dispatch mechanics](references/orchestration.md) |
| Jobs, activities, run commands and CI sweep | [Workflows](references/workflows.md) |
| Diagnose a specific failed or stuck run | [Run debugging](references/run-debugging.md) |
| Match an observed failure to a remedy | [Common failures](references/common-failures.md) |
| Route CI findings, recover delivery and verify deployment | [Recovery](references/recovery.md) |
| Work through typical orchestration decisions | [Walkthroughs](references/walkthroughs.md) |

Use [task authoring](../orbit/references/task-authoring.md) when filing work.
Use [orbit-setup](../orbit-setup/SKILL.md) when the cause is host configuration,
installation, scheduler setup or maintenance; do not load those procedures
for ordinary dispatch.
