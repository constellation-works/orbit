# Preparing and supervising work

## Establish authority and inspect

Call the connected `orbit_workspace_list`, select the owning host/workspace,
and reuse its exact returned selector in every durable tool call. Prefer MCP;
CLI examples here assume an authorized administrative surface on that same
host with workspace routing already established. A checkout path alone does
not establish authority. See [tool-surface.md](../../orbit/references/tool-surface.md).

Read workspace goals, existing tasks, recent runs, and the relevant repository
or runtime evidence. Use `orbit_task_list` with its advertised filters and
`orbit_task_show` with field projections. For run diagnostics:

```bash
orbit run readiness --json
orbit run history --limit 20 --json
orbit run show <run-id> --json
```

Readiness is a snapshot, not a reservation or promise of dispatch. Request
bounded lists and project the needed fields; full run bundles can contain
large provider logs. Keep those logs for targeted failure diagnosis.

## Search, then author

Search open and closed tasks before creating work. Use `orbit_search` with
`kind: "task"`, `all: true`, the explicit workspace, and concrete problem
terms; inspect likely matches and their merge evidence. Search descriptions,
not a new task ID that has no embedding yet.

A useful task states the observed problem or feature opportunity, its value,
bounded scope, and acceptance criteria describing observable behavior.
Separate independent changes so file reservations do not unnecessarily
serialize them. Do not turn a single incident into a speculative framework.
For research, state a falsifiable hypothesis, validation method, and what
would refute it; a simulation of assumed equations does not validate nature.
See [task-authoring.md](../../orbit/references/task-authoring.md).

Create as `proposed` with required `complexity` (`low`, `medium`, or `hard`).
Use the current session's preparation authorization; task creation alone does
not grant implementation or completion. Leave `context_files` empty unless
modification targets are already verified. Selectors identify existing
modification/deletion targets, not background reading or future files.

## Select an allowed crew

Inspect configured crews and the user's current cost/provider constraints.
Persist the selected task crew, using the workspace's complexity mapping;
record a reason for a deliberate exception. Do not silently remap tasks or
undo an operator's crew change. Reuse existing authorization rather than
asking for the same permission again.

Task crew and system-activity crew are different. Inspect the effective job
and resolved provider/model, not just its crew label. The shipped
`task_pilot_pipeline` names `crew: system` on its pilot step and has no
per-run crew input: passing `--input crew=...` does not override that step.
Inspect `[crews.system]` and any compatibility fallback in the active config;
change persistent configuration only within the user's authorization.

## Record who orchestrated the work

Orchestrator attribution is a separate field from the execution crew. It names
the crew that prepared and supervised the task; it selects nothing, grants no
authority, and never changes which crew or model executes the task. Set it on
creation, when the task is `proposed` or `backlog` — after that it is fixed.

A session can carry the value so every task it creates is attributed without
each call remembering to pass it. Start the server with the crew you orchestrate
as:

```bash
orbit mcp serve --workspace <selector> --orchestrator <your-crew>
```

Precedence is explicit-then-session: a call that passes `orchestrator` uses that
value, a call that omits it inherits the session's, and a session started
without the flag attributes nothing at all. The name is resolved against the
crews of whichever workspace the call lands in, so an unconfigured crew fails
that call rather than being replaced by another one. The default applies only to
tasks the session creates: existing tasks are never rewritten.

```bash
orbit tool run orbit.task.add --input '{"workspace":"<selector>","title":"<title>","description":"<description>","complexity":"medium","orchestrator":"<crew>","model":"<agent-family>"}'
```

Treat the flag as configuration, not as evidence of who is calling. An MCP
connection outlives a model switch in the client, so a session started under one
crew keeps stamping that crew after the client moves to another. When the
orchestrating crew actually changes, pass `orchestrator` on the individual call
or restart the connection with the new value. `--operator` is unrelated and
unchanged: it decides what a session may do, this decides only what gets
recorded.

## Prepare context with task-pilot

```bash
orbit run job task_pilot_pipeline
```

Zero-input discovery finds proposed/backlog tasks with empty context in the
selected workspace, excluding no-diff tasks and tasks already prepared by an
active pilot. No task IDs are needed for normal preparation. Use explicit
`--input 'task_ids=["<task-id>"]'` only for a deliberate targeted re-audit,
including a task that already has selectors. Do not clear valid context just
to force automatic discovery.

The pilot reads a pinned source revision; deterministic apply validates and
persists context selectors. Read applied task IDs and partition outcomes from
`orbit run show <run-id> --json`, then read the tasks themselves. A failed
pilot may still have applied independent valid partitions. Do not rerun all
of them or infer success from the agent's prose. See
[orchestration.md](../../orbit/references/orchestration.md) for source
preparation, partial apply, and checkpoint behavior.

An enabled routine may already prepare tasks. Inspect its schedule and live
runs before adding another. Adjust frequency to arrival rate within existing
authorization; avoid overlapping assessments. If you temporarily pause a
routine, record and restore its previous state when the intervention ends.

## Promote promptly when ready

Inspect applied selectors and evidence behind duplicate/already-landed
warnings. Resolve real ambiguity in the task, not by imposing a new agent
output schema. Once preparation and promotion authorization are satisfied,
update status to `backlog` immediately:

```bash
orbit tool run orbit.task.show --input '{"workspace":"<selector>","id":"<task-id>","model":"<agent-family>"}'
orbit tool run orbit.task.update --input '{"workspace":"<selector>","id":"<task-id>","status":"backlog","model":"<agent-family>"}'
```

Use your own agent family for `model`. Ordinary pilot runs prepare context;
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

Feed CI/QA/operational findings through [recovery.md](recovery.md). Keep user
updates focused on changed outcomes, actionable blockers, and decisions.
