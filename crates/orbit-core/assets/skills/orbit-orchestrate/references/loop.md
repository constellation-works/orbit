# Preparing and supervising work

## Establish authority and inspect

Call the connected `orbit_workspace_list`, select the owning host/workspace,
and reuse its exact returned selector in every durable tool call. Prefer MCP;
CLI examples here assume an authorized administrative surface on that same
host with workspace routing already established. A checkout path alone does
not establish authority. See [tool-surface.md](../../orbit/references/tool-surface.md).

Read workspace goals, existing tasks, recent runs, and the relevant repository
or runtime evidence. Use `orbit_task_list` with its advertised filters and a
`fields` projection (full records are large), and `orbit_task_show` with field
projections. For run diagnostics:

```bash
orbit run readiness --json
orbit run history --limit 20 --json
orbit run show <run-id> --json
```

CLI `orbit run readiness`, MCP `orbit_workflow` action `auto` with `mode: "status"`,
and HTTP `GET /api/workflows/auto/readiness` render the same readiness snapshot.
For MCP status, `task` accepts one task id or a list, `concurrency` previews a
ceiling, `limit` defaults to 50 (1–500), and `allow_crew` selects configured
crews. HTTP accepts comma-separated `task` and `allow_crew` query values plus
`concurrency` and `limit`. Omitted crews use the live drain's restriction when
present. All three JSON responses include `replica` and
`validation_env_warning` (null when absent); CLI text prints a warning when set.

Readiness is a snapshot, not a reservation or promise of dispatch. Request
bounded lists and project the needed fields; full run bundles can contain
large provider logs. Keep those logs for targeted failure diagnosis.

## Search, then author

Search open and closed tasks before filing findings from sweeps, CI, review or
curation. For direct user requests, follow the task-authoring guidance rather
than adding an unnecessary discovery step. Use `orbit_search` with
`kind: "task"`, `all: true`, the explicit workspace, and concrete problem
terms; inspect likely matches and their merge evidence. Use distinctive terms
from the title and description.

A useful task states the observed problem or feature opportunity, its value,
bounded scope, and acceptance criteria describing observable behavior.
Separate independent changes so file reservations do not unnecessarily
serialize them. Do not turn a single incident into a speculative framework.
For research, state a falsifiable hypothesis, validation method, and what
would refute it; a simulation of assumed equations does not validate nature.
See [task-authoring.md](../../orbit/references/task-authoring.md).

Create as `proposed` with required `complexity` (`low`, `medium`, `hard`, or
`xhard`).
Use the current session's preparation authorization; task creation alone does
not grant implementation or completion. Leave `context_files` empty unless
modification targets are already verified. Selectors identify modification/deletion
targets and known creation intent (using supported missing-context input), not
background reading or invented paths.

## Select an allowed crew

Inspect configured crews and the user's current cost/provider constraints.
Persist the selected task crew, using the workspace's complexity mapping;
record a reason for a deliberate exception. Do not silently remap tasks or
undo an operator's crew change. Reuse existing authorization rather than
asking for the same permission again.

When comparing the implementer models behind candidate crews, read the
per-model ranking in [model ranking](model-ranking.md): `orbit agents rank`
pools each model's crews and effort levels, awards tie bands only on a
qualified board, and leaves sparse or unmeasured evidence unranked. It is
not the model-and-effort scorecard, and an observation from it grants no
authority to change crew policy or a task's crew.

If you take over implementation personally, assign your own configured crew
before continuing and record the takeover; do not retain the delegation choice
as though that crew performed the work. `orchestrator` remains supervision
attribution and does not correct a stale execution crew.

Task crew and system-activity crew are different. Inspect the effective job
and resolved provider/model, not just its crew label. The shipped
`task_pilot_pipeline` draws its pilot crew from `workflow.purpose_crews.task_pilot`,
else `workflow.low_complexity_crews` (`crew_config_key` on the pilot step) and has no per-run crew input: passing
`--input crew=...` does not override that step. Step-failure and PR conflict
recovery draw from `workflow.purpose_crews.failure_recovery`, else
`workflow.medium_complexity_crews`. Each run draws once per
pool and records it in its state; an empty pool falls back to
`workflow.default_crew`. A host `provider_unavailable` or `provider_capacity`
hold withholds the task-pilot routine while it covers every crew that draw
would use, and the draw skips a held crew while another remains. A success
that started after the failure, or the end of the backoff, resumes the routine;
a recovered auth probe clears unavailability.
Inspect those pools in the active config; change
persistent configuration only within the user's authorization.

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
tasks the session creates: existing tasks are never rewritten. Creation also
records the crew's configured model as the task's `orchestrator_model`; pass
`orchestrator_model` on the call only when your session runs a different model.

```bash
orbit tool run orbit.task.add --input '{"workspace":"<selector>","title":"<title>","description":"<description>","complexity":"medium","orchestrator":"<crew>"}'
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
[orchestration.md](orchestration.md) for source
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
concurrently, and two of their runs had to be cancelled. A multi-slot drain
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

Feed CI/QA/operational findings through [recovery.md](recovery.md). Keep user
updates focused on changed outcomes, actionable blockers, and decisions.

## Reconcile stalled pull requests

A live drain claims new backlog work; it does not adopt older open PRs by
itself. Reconcile each open head independently from durable evidence before
assigning the next action. The forge is authoritative for open versus merged;
the task and run stores are authoritative for ownership and holds.

Read PR head to task, attempt, checks, and hold with supported commands:

1. List open heads on the owning repository:

   ```bash
   gh pr list --state open --repo <owner/repo> --json number,title,headRefOid,headRefName,baseRefName,updatedAt
   ```

2. Bind each head to its task. Orbit delivery branches read
   `orbit/<task>-…` and the PR body carries the task; confirm the binding on
   the task itself rather than trusting the branch name alone:

   ```bash
   gh pr view <pr-number> --repo <owner/repo> --json number,title,headRefName,headRefOid,baseRefName,body
   orbit tool run orbit.task.show --input '{"id":"<task-id>","fields":["status","job_run_id","external_refs","pr_status","open_decisions","completion_hold","final_recovery","history","comments"]}'
   ```

   `external_refs`, delivery evidence, and history bind the PR to the task.
   When the head, branch, or specification changed since the attempt published,
   the old attempt's evidence no longer covers the head.

3. Read the task's last completion attempt and whether it stopped:

   ```bash
   orbit run history --task <task-id> --json
   orbit run show <run-id> --json
   ```

   `run.state` with its `error_code` names the outcome; the task history
   carries the landing and watch entries (`landing_checks_observed`,
   `landing_waiting_for_base`, `landing_stopped`). A stopped attempt reads a
   terminal state with no live run or claim of its own; a live process is not
   stopped merely because an observation timed out. Never restart, requeue, or
   bypass an older attempt while its run is still live.

4. Read checks on the pinned head independently of the run record:

   ```bash
   gh pr view <pr-number> --repo <owner/repo> --json number,url,state,headRefOid,baseRefName,mergeStateStatus,statusCheckRollup
   gh pr checks <pr-number> --repo <owner/repo> --required
   ```

   Classify a red head before choosing recovery — inherited, transient,
   base-suspect, candidate, unknown, or inconclusive — following
   [hosted-check diagnosis](run-debugging.md#hosted-check-failures-and-inherited-red-bases),
   not the rollup color alone. A red check alone grants no retry or merge
   authority, and owner landing may already be waiting on base recovery with a
   recorded wait and deadline.

5. Read the review or decision hold on the same head:

   ```bash
   orbit decision list --task <task-id> --all
   orbit decision show <request-id>
   ```

   The task's `open_decisions` and `completion_hold` name the same hold. A
   green head held for review or a `completion.hold` rule is a review hold,
   not a passing delivery: the next action belongs to whoever owns the gate.

Attribute the next action to its actor once the attempt has stopped:

| Head state | Owner of the next action |
|---|---|
| Red required checks, classified candidate-owned | Orchestrator routes a repair through [recovery.md](recovery.md); the next run repairs the kept candidate rather than re-implementing. |
| Red required checks, classified inherited, transient, or base-suspect | Owner landing or the base owns the wait or bounded rerun per the diagnosis; do not run candidate recovery first. |
| Green checks held by an open decision request or `completion.hold` rule | The human or operator who owns the gate; agents read the request and report its id, gate, and allowed answers, never answer it. |
| Held on a delivery-refusal code (draft, closed, moved head, stale pins, conflict) | Whoever the code names in [delivery-refusal holds](run-debugging.md#delivery-refusal-holds); start the named new attempt rather than resuming the held run. |
| Failure-handoff or preserved candidate with work remaining | The salvage route in [recovery.md](recovery.md#salvage-an-incomplete-candidate); resume or requeue the kept candidate, discarding it only deliberately. |
| Merged delivery | Settlement via `orbit task settle-merged <task-id>` or the drain's own pass, per [recovery.md](recovery.md#close-a-rescued-blocked-task). |

A policy answer arrives as a delivered human decision, never as a notice. A
fleet notice (`orbit notice list`, `orbit notice show`) grants no authority
and no delivery gate reads it; see
[fleet notices](../../orbit/references/tool-surface.md#fleet-notices). Verify a
claimed decision by its exact request id — `orbit decision show <request-id>`
reads `answered` with its resolution (answer, human, note, time), the request
leaves the open list, the task history holds `decision_answered`, and the
task's own state shows the outcome the answer asked for — following
[decision requests](run-debugging.md#decision-requests-on-held-work). If the
request still reads `open`, the answer was not applied.

Distinguish pending action from verified recovery. Pending is an open PR, a
red or pending check, an open decision, or a task still in `review` or
`blocked`. Verified is the decision read above plus the task state it moved,
or a merge observed on the forge with the task settled to `done` and its
landing note recorded. A preserved failure-handoff PR is preserved work, not
proof it is safe to merge or that the task is done. A stopped landing that
can go again keeps its guards: same-handoff retry is operator-only through
`orbit task retry-handoff` under
[retry a stopped owner landing](orchestration.md#retry-a-stopped-owner-landing),
and rework of a published candidate takes the explicit source-repair route in
[recovery.md](recovery.md#explicit-source-repair-of-stopped-owner-publications).
