# Prepare a task footprint with task-pilot

## Contents

- [Prepare selectors before dispatching under traffic](#prepare-selectors-before-dispatching-under-traffic)
  - [Skip a stale pilot](#skip-a-stale-pilot)
  - [Choose a re-check or full assessment](#choose-a-re-check-or-full-assessment)

## Prepare selectors before dispatching under traffic

`context_files` is what conflict detection and file reservation read. Prepare a verified footprint before dispatch under traffic. An empty
surface holds no context lock, so it protects no files.

**Never file with `--status backlog`, or promote, a task with empty
`context_files` while a drain is live.** File it `proposed`, run the pilot or
set selectors, then promote. On 2026-10-10 three tasks filed straight into a
live four-slot drain's backlog with no selectors were admitted together and
all edited the website's then-shared stylesheet; two of the runs were cancelled.

A drain or ship with more than one slot now treats such a task (no
`context_files`, no `no-diff-expected` tag, no operator `readiness-waived` tag)
as a whole-tree footprint. While an enabled task-pilot routine this host owns
would still prepare it, the task waits (`awaiting_footprint`). After that, or
once the pilot has assessed it and left it empty, it starts only when no other
leaf is in flight (`awaiting_exclusive_slot`) and, while it runs, no other
editing work is admitted.

A single-slot drain and `orbit run ship <id>` admit
it as before. A task an operator tagged `readiness-waived` skips this: the
drain admits it like ready work, beside other editing work, with no file lock
and no whole-tree reservation.

Do **not** fill them inline. Use `orbit run task-pilot`: it audits tasks
read-only in bounded partitions, and its apply step persists only selectors it
validated.

```bash
orbit run task-pilot                            # zero-input discovery
orbit run task-pilot <id> <id>                  # audit exactly these
orbit run task-pilot <id> --json                # machine-readable dispatch output
```

The first pilot is automatic once for eligible tasks. Creators can add `no-pilot`
when the footprint, complexity, testable criteria and deduplication are already
settled; readiness still applies and an explicit pilot remains available. The routine queues a later pilot automatically only when a certified description or acceptance-criteria edit makes the applied assessment stale on a proposed or backlog task, and no live execution claim or unresolved failed pilot holds the task.

A person's or agent session's `context_files` edit on such a task whose latest assessment holds a warning or an admission hold also queues one and releases an unresolved failed pilot; comments, plan and execution-summary updates, the pilot's own selector writes and a run's `context_files_widened` records do not.

Other material edits (title, plan, and selectors not covered by that rule) or policy never queue one through the routine; every other later routine assessment requires an explicit
request: `orbit task repilot <id>`, `orbit.task.recover` action `repilot` with `id` and the
owning `workspace`, or `orbit task update <id> --repilot` / `orbit.task.update`
with `repilot: true`. The update flag requires operator capability, and the
task must start and finish the update in `proposed` or `backlog`.

An `--approve-proposed` drain still queues a fresh pilot automatically when a stale explicit/routine assessment's certified description/criteria or other material drifted and no live execution claim holds the task, while scheduled discovery still queues one automatically only when its certified description/criteria drifted and no live execution claim holds the task; a failed repilot holds it until its meaning changes again or an operator acts.

These operator requests submit one pending pilot and
refuse a second request while a pilot is pending or running, while delivery
holds the task, or on a replica. A refusal reports the same stable `code` on
the CLI, MCP and dashboard (`task_pilot_unsupported_status`, `_human_crew`,
`_already_running`, `_delivery_running` or `_replica_checkout`). A decision option with action `re_pilot`
also explicitly requests one pilot; it needs no material rewrite.

An answered decision that rewrites the description or acceptance criteria of
a previously assessed task queues a pilot when the task returns to backlog
with a stale assessment. Dispatch can proceed when the new assessment clears
admission. If submission fails, the task comment names the operator command
to request the pilot.

Existing
`orbit run task-pilot <id>` remains an explicit assessment command.

### Skip a stale pilot

**Skipping a stale pilot.** When an operator has settled a task's edited
requirements, admission's `pilot_assessment_stale` hold need not wait for a new
assessment: adding `no-pilot` (the dashboard Readiness card's "Skip pilot", or
`orbit task update <id> --tag <current tags>,no-pilot`, since `--tag` replaces
the list) makes drain, `orbit run ship` and pull admission ignore a stale
assessment and an ordinary receipt hold (operator validation, native OS,
duplicate, already landed).

A current host-operational hold is the exception:
the tag does not release it, Skip pilot is not offered for
`host_operational_handoff`, and dispatch does not proceed while that hold
remains. Re-scope the acceptance criteria, or record an operator decision
(`task-pilot-admission: evaluated`, `clear` or `approve-anyway`, with the
evidence on the following lines).

Readiness gaps, context locks,
dependencies and delivery holds still apply; removing the tag restores the
gate for the holds the tag releases. On a task with a pilot receipt or a pending pilot this is operator
authority: a managed agent's `orbit.task.update` adding the tag is refused, and
the operator's write records `pilot_skipped` in history with the requirements
digest the write leaves. Never add it from a managed run to get
past your own gate.

Do not add it to get past a host-operational hold.

### Choose a re-check or full assessment

**Re-check and full modes.** An explicit re-pilot of a task whose latest
assessment holds promotion runs as a *re-check*. The pilot receives the prior
assessment, its holding findings (each with an id such as `blocked_by[0]` or
`duplicate_of`) and a field-level diff of the task since that assessment:
title and description (with a word-overlap similarity), criteria, context
files, tags, dependencies, relations and answered decisions.

It returns a
verdict per holding finding, `resolved`, `still_holds` or `changed` (with the
replacement finding), plus only the fields the diff touched. Apply keeps every
other prior field, drops resolved findings, records `mode: recheck` and
`prior_assessment_id`, and validates it with the same source-revision proofs as
a full assessment. A re-check missing a verdict for any holding finding goes to
repair.

A pilot that judges a re-check insufficient may return a complete
assessment instead; it is recorded as full with `mode_reason:
recheck_escalated`.

| When to run a full assessment |
|---|
| The pilot runs a full assessment, recording why as `mode_reason`, when there is no held prior assessment, the operator passed `orbit task repilot <id> --full` (`full: true`; job input `full_pilot: true`), the prior assessment is older than the job input `recheck_max_age_hours` (default 72; 0 disables re-checks), the prior was itself a re-check, the prior receipt predates the recorded re-check basis, the objects at the task's selector paths changed between the prior and current source revisions, or the title or description was rewritten beyond the bounded edit threshold (word-set similarity below 0.6) or first added. |

Analytics › Agents › Pilots reports `pilot_modes`: per
mode, assessments, runs, median and total run seconds and tokens, plus
re-checks later followed by a full pilot and the combined re-check-to-full
rate.

Certified description/criteria or other material drift makes the assessment stale for an `--approve-proposed` drain, while only certified description/criteria drift makes it stale for scheduled discovery. The drain queues a fresh pilot automatically for either kind of drift, and scheduled discovery queues one automatically only for description/criteria drift, when no live execution claim holds the task; a failed repilot holds it until its meaning changes again or an operator acts.

Dispatch fails closed as `pilot_assessment_stale` until
a new assessment completes against the edited requirements, unless an
operator skips the pilot with `no-pilot` as described above. A held
warning keeps its one pending decision across routine windows; routine ticks
create no additional pilot, triage, or decision for that same assessment.

When explicit ids store different crews, `orbit run task-pilot` submits one
homogeneous run per crew. Each run audits only its own task group.

The shipped pilot steps draw their crew from `workflow.purpose_crews.task_pilot`,
or from `workflow.low_complexity_crews` while that is unset.
Neither a `--crew` flag nor `--input crew=<name>` changes those steps. Inspect
the effective job and that pool before promising a provider; see
[crew selection](loop-crew.md#select-an-allowed-crew).

Zero-input mode discovers only `proposed`/`backlog` tasks in the invoking
workspace needing its first assessment, and skips tasks tagged as needing no
diff or carrying an already applied assessment (ready or held), even after edits. It also excludes a task already named by the durable prepare checkpoint
of an active pilot run and reports the owning run ID, so a later discovery run
can inspect new work without repeating the expensive assessment.

Explicit
`task_ids` audits exactly the named tasks, including ones that already have
selectors, while excluding tasks held by another active run as
`already_preparing` with `prepared_by_run_ids`. A child pilot, such as one an
approve-proposed drain dispatched, is not this operator request: it reuses an
assessment an explicit or routine pilot already certified instead of calling
the provider again. Preparation continues for the
free named tasks.

Apply settles each held task as `superseded` with reason
`piloted_elsewhere`; if every named task is held, the run succeeds without
agent work. Inspect or resume the owning run for that task's assessment.

The dashboard's **Run pilot** action on a `proposed` or `backlog` task in the
task details panel makes this same single-task submission (`POST /api/tasks/<id>/pilot`,
operator session required). It is refused, with a typed reason and no run, when
a pilot run already holds the task, a delivery run carries it, it is not
`proposed`/`backlog`, or the checkout is a replica. On success the panel shows
`piloting…` with a link to the run until it settles. It never promotes or
dispatches the task.
