# Tools: task-pilot dependencies and warnings

## Task-pilot dependencies and retries

A task's `dependencies` may name a task in another workspace registered on
the same machine. Task-pilot apply never locks a foreign task in the local
store: it reads the dependency's status through the registered owner (the
lookup behind `resolved_dependencies`), and only a dependency that no
registered workspace holds fails as `task not found`. Use `relations`
(`related_to`) for a reference that should not gate readiness.

When a
pilot's approval fails deterministically after its assessment was applied,
the drain holds the task as `pilot_unresolved` for the operator instead of
piloting it again on every pass; edit the task or approve it to retry. In
a multi-task apply, one task's failure does not fail the other tasks'
applied results: the pipeline guard passes a partially applied apply as
held, and only the failed task is held.

Standalone scheduled discovery also
holds failed stale repilots for proposed and backlog tasks. A later description
or acceptance-criteria edit, a person's or agent session's `context_files` edit,
or an explicit repilot request, can retry; provider outages and missing
inspection crews do not create this task-dependent hold.

A person's or agent
session's `context_files` edit on a proposed or backlog task whose latest
assessment holds a warning or an admission hold also queues a fresh automatic
pilot, so an operator repilot is not needed for that hold.

## Pilot-warning triage

`orbit_task_block_triage` (`orbit.task.block_triage`) serves pilot-warning
triage only. Over MCP it is opt-in in the operator profile (`--toolset
task-triage` or `machine.mcp.toolsets`); without MCP, call it through
`orbit tool run orbit.task.block_triage --input '<json>'`.

A blocked task is
decided by final recovery, which opens a decision request an operator answers,
so `subject: block` is refused with a message naming final recovery. The default
subject is `pilot_warning`: the actions read `proposed` tasks whose latest
task-pilot assessment holds promotion on warning findings.

`candidates` lists each held assessment that is not yet triaged and has no
operator decision, quoting its holding findings. `propose` opens a decision card
(a summary, two or three options with their consequences, and a recommendation;
an option approves the task, re-pilots it or rewrites a field) and records one
comment quoting those findings plus a `pilot_warning_triaged` history marker,
which excludes that assessment afterwards.

`report` records the human action the
same way. Both refuse a task that no longer qualifies. `requeue`, `resume_run`,
`rerun_ci` and `friction_reported` are refused. A pilot-warning triage never
approves, promotes, clears the hold or edits any other task field.

Inspection proof: a decision on a task that a live escalation triage run holds
must carry that run's proof, in three fields:

- `source_revision`: the commit the run pinned.
- `observed_revision`: `git rev-parse HEAD` in the run's inspection checkout.
- `observed_tree`: `git rev-parse 'HEAD^{tree}'` in the same checkout.

Core checks that `observed_revision` equals the pinned revision and that
`observed_tree` equals that revision's tree. In a non-Git workspace the run pins
no revision: pass `source_revision` as null and omit both observed values. A
missing or mismatched proof is refused, naming the run and the expected values,
and nothing is recorded. A proof passed on any other decision is checked the same
way.
