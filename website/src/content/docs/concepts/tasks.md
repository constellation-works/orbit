---
title: Tasks
description: "How a task moves from filed to done: statuses, the two approval gates, dependencies, tool grants, file reservations, artifacts and review layers."
---

A task is a durable unit of work that an agent executes and you review. Use
this page to predict when a task starts, what holds it back, and which steps
need you.

A task carries a title, description, acceptance criteria, status, context,
review notes and history. Its bundle lives under
`~/.orbit/tasks/workspaces/<workspace>/<task-id>`, not in your repository.
The dashboard also estimates each task's regression risk from its size and
the history of completed tasks.

## Lifecycle

```text
proposed -> backlog -> in-progress -> review -> done
```

Two steps need approval: `proposed → backlog` before any run picks the task
up, and `review → done` after the agent delivers. One command takes whichever
step is next:

```bash
orbit task update "$TASK_ID" --approve --note "Scope reviewed."
```

`--approve` can't be combined with field edits or `--status`. The note goes
into the status history. A task you file yourself can skip the first gate
with `orbit task add --status backlog`.

While an unfinished local run lists the task, `--approve` and the dashboard's
**Approve** are refused, and the error names the run. Stop the run, then
approve.

Each gate can also pass without you, but only when you opt in:

| Gate | What else can pass it |
|---|---|
| `proposed → backlog` | Your agent, with `orbit.task.update`, when you ask. A drain started with `orbit run auto --approve-proposed`, which approves the tasks its pilot verifies; see [Authorize the backlog](/how-to/continuous-delivery/#2-authorize-the-backlog). The CI failure sweep, once you enable it, for the repair tasks it files. |
| `review → done` | A run started with `--complete`, or the owner's `workflow.distributed_completion = "done"` for distributed handoffs. See [Completing work with `--complete`](/getting-started/workflows/#completing-work-with---complete). |

`--approve-proposed` is off by default and refused with `--pull`: only the
owner approves work. The `no-auto-approve` and `no-drain` tags each keep a
task proposed through such a drain. Completion never approves `proposed` work.

A task minted by an [auto-task](/concepts/scheduling/#auto-task) enters
`proposed` by default.

### Statuses

| Status        | Meaning |
|---------------|---------|
| `proposed`    | Waiting for your approval. |
| `backlog`     | Approved and queued. |
| `someday`     | Wanted, but not actionable yet. Agents skip it. |
| `in-progress` | Being worked on. |
| `review`      | Delivered and waiting for review or merge. |
| `done`        | Accepted and closed. Terminal. |
| `blocked`     | Paused on a dependency, a decision or a failed run. If dispatch couldn't find the provider CLI's launcher, `orbit task recheck-blocked --confirm` returns the task to `backlog` once the launcher resolves. |
| `archived`    | Soft-deleted with `orbit task archive`. Terminal; restore it with `--status <status> --force`. |
| `rejected`    | Declined. It can be reconsidered. |

### Transition rules

`orbit task update`, the dashboard and the `orbit.task.update` tool all check
a status change against one table:

| From | Allowed targets |
|---|---|
| `proposed` | `backlog`, `someday`, `in-progress`, `rejected` |
| `backlog` | `proposed`, `someday`, `in-progress`, `rejected` |
| `someday` | `backlog`, `in-progress`, `rejected` |
| `in-progress` | `backlog`, `someday`, `review`, `rejected` |
| `review` | `backlog`, `in-progress`, `done`, `rejected` |
| `blocked` | `backlog`, `in-progress` |
| `rejected` | `backlog`, `in-progress` |
| Any open status | `blocked`, `archived` |
| `done`, `archived` | None |

Three rules sit on top of the table:

- **`done` needs evidence.** It is reachable only from `review`, with a
  non-empty `execution_summary` or a `job_run_id` whose run succeeded.
- **`done` and `archived` are terminal.** A regression in delivered work gets
  a new task with a `regression_from` relation. The old task stays closed.
- **Starting work needs a plan.** Moving to `in-progress` from any status
  other than `backlog` is refused while the plan is empty. Send `--plan` on
  the same command.

Acceptance criteria that the task pilot marks as needing a deployed build can
stay owed after delivery. The task then waits in `review`, even with
`--complete`, until an operator records each one as met.

A refused transition is an `invalid_input` error that names both statuses and
the missing precondition. As a human you can override the table with
`orbit task update <id> --status <status> --force`, or with the
**force (off-table)** group in the dashboard's status menu. Both record a
`forced` event in the task's history. The `orbit.task.update` tool refuses
`force`, so agents stay on the table.

## Dependencies and relations

A dependency makes one task wait until another is `done`. Every other
relation records lineage or a cross-reference and never holds a task back.

```bash
orbit task add --title "Publish the API docs" --complexity low \
  --dependencies "$API_TASK_ID,$SCHEMA_TASK_ID"
orbit task show "$TASK_ID" --fields dependencies,resolved_dependencies
```

On `orbit task update`, `--dependencies` replaces the whole list, and an
empty string clears it. Orbit refuses a self-dependency and a cycle.
Dependencies resolve inside the workspace: an ID from another workspace
counts as missing.

### When a task starts

Dispatch starts a task only when every dependency is `done`. Until then it
waits in the backlog, and `orbit run readiness <task-id>` says what it is
waiting for.

A dependency that can never reach `done` (a missing task, a rejected one, or
one archived before it completed) is reported as
`task.dependencies.unsatisfiable`. Restore or reopen the target, or remove it
from `--dependencies`.

Three more things affect a start:

- **A manual start skips the check.** `orbit task update --status in-progress`
  ignores dependencies.
- **`not_before`** holds a backlog task until a set time. See
  [Scheduled tasks](/concepts/scheduling/#scheduled-tasks).
- **The `no-drain` tag** keeps automatic drains away from a task an
  interactive session owns. `orbit run auto`, pull drains and auto-approval
  skip it, and `orbit run readiness` reports `no_drain_tag`.
  `orbit run ship <id>` still runs it.

### Relation types

Relations are an array of `{type, target}` objects on the `orbit.task.add`
and `orbit.task.update` tools. The CLI sets only the three kinds that have a
flag.

| Type | Target | Meaning | CLI flag |
|---|---|---|---|
| `blocked_by` | Task | This task waits for the target to reach `done`. The only kind that gates a start. | `--dependencies` |
| `child_of` | Task | The target is this task's parent. | `add --parent` |
| `regression_from` | Task | The target introduced the defect this task fixes. | `add --source-task` |
| `spawned_from` | Task | This follow-up was filed from the target. | |
| `supersedes` | Task | This task replaces the target. | |
| `related_to` | Task | A plain cross-reference. | |
| `covered_by` | Task, or `github-pr:<number>` or `github-pr:<URL>` | The target handles this task's failure. | |
| `produces` | Task or friction | This task produced the target. | |
| `resolves` | Friction | Reaching `done` closes that friction. | |

```json
{
  "id": "<task-id>",
  "relations": [
    { "type": "blocked_by", "target": "<task-id>" },
    { "type": "resolves", "target": "<friction-id>" }
  ]
}
```

`relations` replaces the whole list, `blocked_by` included, so send every
relation you want to keep. Don't send `dependencies` and `relations` in the
same update: `relations` is applied last and overwrites the dependency edges.
Read relations back with `orbit task show "$TASK_ID" --fields relations`.

A `regression_from` relation takes an optional `regression` object:

| Field | Values |
|---|---|
| `nature` | `broke_existing`, `defect_in_new`, `incomplete`, `merge_reintroduced`, `contract_drift`, `unclassified` |
| `escape` | `no_test`, `test_not_selected`, `platform_only`, `review_missed`, `spec_ambiguous`, `nondeterministic`, `unclassified` |
| `severity` | `critical`, `high`, `medium`, `low`, `nit`. Required on a new classified link. |

### Closing frictions

Friction reports are separate records under `orbit friction`, not task
statuses. A task closes a friction in two ways:

- A `resolves` relation closes its friction when the task reaches `done`.
- A task that reaches `done` through a landed pull request also closes the
  frictions named in its description or acceptance criteria. A manual move
  to `done` closes only the `resolves` relations.

A `resolves` target in another workspace refuses the move to `done`: remove
the relation, or resolve the friction where it lives. To close a friction
without a task, run `orbit friction resolve <friction-id>`.

## Task tool grants

A task has two tool lists:

- `required_tools` adds Orbit tools to the activity's allowlist. It is fixed
  at creation and can't override a deny list. See
  [Agent tool lists](/reference/activity-job-yaml/#agent-tool-lists).
- `granted_tools` lets an operator grant the implementer a tool its deny
  list excludes. Use it for one reviewed operation, not to give an agent
  operator authority.

```bash
orbit tool show orbit.auto_task
orbit task update "$TASK_ID" --granted-tools orbit.auto_task:update
```

`orbit task add` takes `--granted-tools` too. Names must be exact, registered
and agent-facing; wildcards and operator-only tools are refused. An update
replaces the whole list, and `""` clears it. Only an operator can set the
field.

What a grant does depends on the tool:

- **A read-safe tool** becomes callable: the grant lifts its exact deny
  entry. A wildcard deny entry still wins.
- **A mutating tool** stays denied during the run. The grant instead lets the
  implementer propose the call for review, as described below.

Rules that apply to every grant:

- Only the implementing activity (`agent_implement`) reads grants. Reviewers
  and recovery don't.
- A grant can change only while the task is `proposed`, `backlog` or
  `someday`, with no live run and no preserved worktree. After a stopped
  attempt, requeue the task and remove its worktree first.
- A task with a grant runs on its owner host. If the task's labels or OS
  exclude that host, it waits.
- A grant doesn't bypass capability, filesystem or other runtime checks.

### Proposing a mutation

For a granted mutating tool, the implementer attaches a JSON artifact at
`mutation-proposals/<name>.json` with `orbit.task.artifact` action `put`. It
names the tool, the exact input and a rationale:

```json
{"tool": "orbit.auto_task", "input": {"action": "update", "name": "<auto-task>", "description": "<updated description>"}, "rationale": "Explain why this change is needed."}
```

Orbit accepts the proposal only if the task grants that tool and an agent
session could make that exact call. The review gate shows the proposal next
to the diff. After approval the
pipeline applies the reviewed input once and writes a
`mutation-receipts/<key>.json` receipt. Changed input needs a new review. A
failed or unreviewed proposal stops delivery, and a task can't complete with
an unapplied proposal. If the agent needs the change during its run, have an
operator make it first.

## File reservations

Orbit locks files for you: a task holds the files its context selectors cover
while it is `in-progress` or `review`, and an overlapping task waits.

Reserve a surface yourself when work outside the pipeline needs the same
protection, such as an editor session or a manual migration:

```bash
ORBIT_OPERATOR=1 orbit task locks reserve --file dir:src/billing --ttl 2h
```

| Flag | Use |
|---|---|
| `--file` | `file:` or `dir:` selectors inside the workspace. Repeat or comma-separate. |
| `--task` | Reserve a task's declared context surface instead. Repeat or comma-separate for several tasks. Can't be combined with `--file`. |
| `--ttl` | How long the claim holds, such as `45m`. Default `30m`, maximum `24h`. |

A reservation is all or nothing. A denied one exits 3 and names who holds
the overlap. Reserving and releasing need the `operator` or `runner`
capability; from a plain shell, set `ORBIT_OPERATOR=1`.

Reservations expire on their own, so pick a TTL that covers the work. To
release one early, when the session that held it is gone:

```bash
orbit task locks list --json
orbit task locks release <reservation_id> --confirm
```

`orbit task locks contention` lists the files that pending backlog tasks
collide on. For the tasks these locks hold back, see
[Check what will actually start](/how-to/continuous-delivery/#3-check-what-will-actually-start).

## Acceptance criteria

Every task needs an observable outcome. A good task states what should
change, where, and how to observe success:

```bash
orbit task add --title "Retry failed fetches" --complexity low \
  --context dir:src/fetch \
  --acceptance-criteria "The fetch tests pass" \
  --acceptance-criteria "README documents the retry setting"
```

Write criteria someone can check. Prefer "command X exits successfully" or
"file Y contains Z" to "the behavior feels better."

## Task artifacts

A task artifact is a file stored on the task, outside git: a note, a log, a
check result or a screenshot that you or an agent can open later.

```bash
mkdir -p .orbit/tmp
printf 'checked the build\n' > .orbit/tmp/notes.txt
orbit task artifact put "$TASK_ID" .orbit/tmp/notes.txt --path evidence/notes.txt
orbit task show "$TASK_ID" --fields artifacts
orbit task artifact get "$TASK_ID" evidence/notes.txt
```

- **The source file must be inside the checkout.** Write it under
  `.orbit/tmp/` (`$ORBIT_SCRATCH_DIR`). The path resolves against the current
  directory, and anything outside the workspace, including `/tmp`, is
  refused.
- **`--path` is the stored name,** relative to the task's artifacts. It
  defaults to the source file's name, and an absolute path or `..` is
  rejected. `get` takes the same name.
- **List with `orbit task show --fields artifacts`.** It returns each
  artifact's path, media type and size. There is no list subcommand.
- **The limit is 1 MiB** for both `put` and `get`.
- **Names starting with `review-` are reserved** for the review gate, in any
  letter case. The one exception is `review-report.json`.
- **`get` prints text only.** Save an image or other binary with `--out`:

```bash
orbit task artifact get "$TASK_ID" screenshots/diagram.png --out .orbit/tmp/diagram.png
```

`put` and `get` follow the task to the host its ID prefix names. For a short
text payload on this machine, skip the file:

```bash
orbit task update "$TASK_ID" --artifact evidence/notes.txt=checked
```

Agents use `orbit.task.artifact` with action `put` (`id`, `source_path`,
optional `path`) or `get` (`id`, `path`), under the same rules. Text comes
back in `content`, anything else as base64 in `content_base64`.

The dashboard previews and downloads artifacts on the task's **Artifacts**
tab. See [Use the Dashboard](/how-to/dashboard/#tasks).

## Review layers

Orbit can have a second agent review a delivery at three points. Before-PR
and before-landing are alternatives selected by `review.timing`, which
defaults to `off`. After-landing is a separate switch that ships disabled.

| Layer | When it runs | What the reviewer can do | Enable it | What it blocks |
|---|---|---|---|---|
| Before-PR (deprecated) | Before Orbit opens the pull request. | Fix the candidate before the PR exists. | `orbit config set review.timing before_pr` | PR creation. Needs the PR delivery route. |
| Before-landing | After the PR opens, while hosted CI runs. | Fix the PR head; Orbit validates and pushes the fix. | `orbit config set review.timing before_landing` | The merge. Anything but an approval leaves the PR open and the task in `review`. |
| After-landing | In batches after deliveries land. | File confirmed findings as follow-up bug tasks. | `orbit auto-task toggle delivery-code-review on` | Nothing. |

On a workspace with no config file yet, add `--seed-from-global` to the
first `orbit config set`. `orbit config show`, `orbit doctor` and the
dashboard's Settings page show which layers are on.

A workspace can also map a task tag to skip all three layers for bounded
low-risk work. See [Review-skip tags](/reference/config/#review-skip-tags).

### Adding review to one run

`orbit run ship` and `orbit run auto` accept `--review before-landing` or
`--review before-pr`. The flag adds a layer to that run only and leaves the
workspace configuration unchanged. It can add review, never remove or swap
it:

| Workspace policy | `--review before-pr` | `--review before-landing` |
|---|---|---|
| No layer | Adds before-PR review | Adds before-landing review |
| `review.timing = "before_pr"` | Accepted; already applied | Refused |
| `review.timing = "before_landing"` | Refused | Accepted; already applied |

Orbit also refuses the flag before the run starts when:

- neither `workflow.purpose_crews.pre_landing_review` nor
  `workflow.purpose_crews.review` is set, so there is no reviewer crew;
- the run uses local-only delivery, which has no pull request to review; or
- the run is a pull drain (`orbit run auto --pull`), where the owner decides
  review.

Child runs and resumed runs keep the layer. `orbit run show` reports it on
its `Review:` line. The dashboard's **Start a window** card offers the same
choice as a **Review before landing** switch.
