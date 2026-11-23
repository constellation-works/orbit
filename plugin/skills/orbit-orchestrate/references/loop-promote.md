# Prepare and promote ready work

## Prepare context with task-pilot

```bash
orbit run job task_pilot_pipeline
```

Zero-input discovery finds proposed/backlog tasks with empty context in the
selected workspace, excluding no-diff tasks and tasks already prepared by an
active pilot. No task IDs are needed for normal preparation. Use explicit
`orbit run task-pilot <task-id>` only for a deliberate targeted re-audit,
including a task that already has selectors. Do not clear valid context just
to force automatic discovery.

The pilot reads a pinned source revision; deterministic apply validates and
persists context selectors. Read applied task IDs and partition outcomes from
`orbit run show <run-id> --json`, then read the tasks themselves. A failed
pilot may still have applied independent valid partitions. Do not rerun all
of them or infer success from the agent's prose. See
[orchestration-pilot-apply.md](orchestration-pilot-apply.md) for source
preparation, partial apply, and checkpoint behavior.

An enabled routine may already prepare tasks. Inspect its schedule and live
runs before adding another. Adjust frequency to arrival rate within existing
authorization; avoid overlapping assessments. If you temporarily pause a
routine, record and restore its previous state when the intervention ends.

## Promote promptly when ready

Inspect applied selectors and evidence behind duplicate/already-landed
warnings. Resolve real ambiguity in the task, not by imposing a new agent
output schema. Once the task carries verified `context_files` (or a
`no-diff-expected` tag) and promotion is authorized, update status to
`backlog`:

```bash
orbit tool run orbit.task.show --input '{"workspace":"<selector>","id":"<task-id>"}'
orbit tool run orbit.task.update --input '{"workspace":"<selector>","id":"<task-id>","status":"backlog"}'
```

**Never file with `--status backlog`, or promote, a task whose
`context_files` is empty while a drain is live.** File it as `proposed`, run
the pilot (or set selectors yourself), then promote. Empty context holds no
file lock. On 2026-10-10 three website tasks filed straight into the backlog
of a live four-slot drain each edited the website's then-shared stylesheet
concurrently, and two of their runs had to be cancelled.

A multi-slot drain
holds such a task (`awaiting_footprint`, then `awaiting_exclusive_slot`) and
runs it only alone, which serializes the drain behind it, unless an operator
has tagged it `readiness-waived`; agents never add that tag. Task add and
update warn when they leave one in the backlog.

Attribution is your session, not a `model` argument. Ordinary pilot runs prepare context;
they do not promote. The CI sweep has a separate, explicit admission path
that can pilot and promote its own warning-free repairs.

A real unmet dependency can remain represented on a prepared backlog task;
readiness will hold it until satisfied. It is not a reason to erase the
dependency or keep unrelated tasks proposed. A duplicate or already-delivered
change should be rejected or narrowed with evidence before promotion.

## Dispatch and observe

Follow [authorization.md](authorization.md) for `--complete`, duration,
concurrency, and crew filters. If a live authorized drain already owns this
workspace, let it claim newly eligible tasks rather than also shipping them
manually. Otherwise submit only the authorized work.

Submission returns a durable run ID, not a completed delivery. Verify run
state, task state, and PR merge independently. While a process is confirmed
live, observe that same run with sensible backoff. An observation timeout
is not a failed worker and is not grounds for restarting it.

Feed CI/QA/operational findings through [recovery-ci-findings.md](recovery-ci-findings.md). Keep user
updates focused on changed outcomes, actionable blockers, and decisions.
