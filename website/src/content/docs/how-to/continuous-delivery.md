---
title: Run a delivery window
description: "Prepare and approve tasks, check readiness, run a time-bounded drain, retune or stop it, and recover failures."
---

A delivery window drains your approved backlog for a set time. Orbit starts
backlog tasks in parallel until the window closes; work already in flight
finishes. Starting a window returns a run ID, not finished work, so check each
task's status before you treat it as delivered.

Each step gives the [dashboard](/how-to/dashboard/) or agent route, then the
CLI. To drain the same workspace from a second machine, see
[Set up a distributed drain](/how-to/distributed-drain/).

:::tip[Let your agent run it]
Ask your agent to run a delivery window. The `orbit-orchestrate` skill follows
these steps: it pilots and promotes the tasks you authorize, starts the drain,
and diagnoses any run that fails.
:::

## 1. Prepare proposed work

The task pilot fills in each task's `context_files` (the files and directories
it will work in) and assesses its complexity. It never approves or dispatches
a task. For a bug that a review filed, it can reject the task when the code
disproves the finding.

The `task-pilot` routine ships enabled and pilots eligible new tasks on the
[host scheduler clock](/how-to/recurring-work/). To pilot now:

- **Agent:** ask your agent to pilot the proposed tasks.
- **Dashboard:** in **Automation → Jobs**, click **Run ▸** on
  `task_pilot_pipeline`.
- **CLI:**

  ```bash
  orbit run task-pilot --wait                              # every task that needs a pilot
  orbit run task-pilot "$TASK_ID" "$OTHER_TASK_ID" --wait  # exact tasks
  ```

Without task IDs, the pilot picks up `proposed` and `backlog` tasks whose
`context_files` is empty or whose complexity is unassessed. Name a task ID to
audit it again.

Before you approve, check the pilot run and the context files it applied: in
the dashboard, **Jobs › History** and the task's **context files** in
**Tasks**; from the CLI:

```bash
orbit run history -j task_pilot_pipeline
orbit run show "$PILOT_RUN_ID"
orbit task show "$TASK_ID" --fields status,context_files
```

### If you edit a piloted task

Editing the description or acceptance criteria makes the assessment stale.
Dispatch holds the task as `pilot_assessment_stale` until a fresh pilot
completes. The routine and the drain queue that pilot themselves; other edits
do not, so request one on the owner:

```bash
orbit task repilot "$TASK_ID"
orbit task repilot "$TASK_ID" --full   # reassess everything, not only the held findings
```

The request is refused while a pilot or a delivery holds the task, for a
human crew, and on a replica checkout.

### If the pilot holds a task

The pilot holds a task whose criteria the implementing agent cannot satisfy.
Read the finding in the pilot run (`orbit run show "$PILOT_RUN_ID"`) or in the
task details under the dashboard's [Tasks view](/how-to/dashboard/#tasks).

| Finding | Cause | Fix |
|---|---|---|
| `utility_warnings` | A criterion needs evidence the agent cannot produce, such as an owner-only audit report. | Rewrite it to a check the agent can repeat, for example "Run `npm run check` and record the outcome". Keep owner-only evidence as a separate operator validation. |
| `start_prerequisites` | The task says not to start until something else happens. | Record a dependency on the real task, or set `not_before` for a known start time. |

After you add the evidence or rewrite the criterion, run
`orbit task repilot "$TASK_ID"`. To clear the hold from the dashboard, see
[Clear a pilot admission hold](/how-to/dashboard/#clear-a-pilot-admission-hold).
An operator can skip an ordinary pilot-admission hold with the `no-pilot`
tag; a current host-operational hold stays. See
[Skip pilot in Readiness](/how-to/dashboard/#skip-pilot-in-readiness).

## 2. Authorize the backlog

Approving a `proposed` task moves it to `backlog`, where a window can start
it. The pilot does not approve work.

- **Dashboard:** in **Tasks**, click **Approve** on the task under **Awaiting
  approval**.
- **Agent:** tell your agent which tasks to approve.
- **CLI:**

  ```bash
  orbit task update "$TASK_ID" --approve --note "Pilot context reviewed."
  ```

A task already in `backlog` needs no approval.

### Let the window approve

```bash
orbit run auto --for 4h --approve-proposed
```

While the window is open, the drain pilots and approves `proposed` tasks,
including tasks filed after it starts. A task qualifies when it has context
files and an assessed complexity, or the `no-diff-expected` tag, and its pilot
reports no duplicate, already-landed, conflict or warning finding.

- Tasks tagged `no-auto-approve` stay `proposed` until a human approves them.
- `orbit run show` and `orbit run readiness` report approval counts and hold
  reasons, including each warning finding. The dashboard shows the same on the
  live window.
- The flag is off by default and conflicts with `--pull`: only the owner
  approves work.
- Approval does not complete delivery. Add `--complete` for that.

## 3. Check what will actually start

Readiness is a read-only snapshot of what a window would start now and why the
rest would wait. It reserves nothing, so an eligible task is not guaranteed to
start.

- **Dashboard:** the Tasks dock's **Drain** tab shows eligible and waiting
  counts and the slots in use. The task list's **Backlog** card sorts tasks
  into **Eligible now**, **Waiting on locks**, **Waiting on capacity** and
  **Waiting, other**; each row's tooltip names what it waits on. **Locks
  held** lists the held file locks.
- **CLI:**

  ```bash
  orbit run readiness
  orbit run readiness --concurrency 8
  orbit run readiness "$TASK_ID" --json
  ```

Without `--concurrency`, readiness uses the live local drain's ceiling, else a
live pull drain's, else 5. `--json` names the choice in
`capacity.limit_source`.

### Why a backlog task waits

Unsatisfied dependencies and file locks are the common reasons. Two tasks
whose `context_files` overlap cannot run at once;
`orbit task locks contention` shows which files the backlog collides on.

| Reason | Meaning | What to do |
|---|---|---|
| `surface_reserved` | A critical or high-priority task ahead in the queue is waiting on file locks this task overlaps. | Nothing. It starts once that task starts. |
| `awaiting_footprint`, `awaiting_exclusive_slot` | The task has no `context_files`, so it holds no lock. The drain waits for the pilot to fill them in, then runs the task alone. | File such tasks as `proposed` so the pilot prepares them first, or have an operator [waive readiness](/how-to/dashboard/#waive-readiness) to let the drain run it beside other work. |
| `pilot_assessment_stale` | The description or criteria changed after the last assessment. | [Repilot the task](#if-you-edit-a-piloted-task). |
| `pilot_duplicate`, `pilot_already_landed` | The latest pilot assessment found duplicate or already-landed work. | Read the finding in the task comments, then repilot or [record a decision](#override-a-pilot-admission-hold). |
| `native_os_required` | The criteria need native-host evidence this host cannot produce. | Route the task to a capable host, re-scope the criteria, or [record a decision](#override-a-pilot-admission-hold). |
| `host_operational_handoff` | The latest pilot disposition is host-operational, and the work still matches that assessment. | Re-scope the acceptance criteria, or [record a decision](#override-a-pilot-admission-hold). Adding `no-pilot` does not release this hold. |
| `crew_not_allowed` | A [crew restriction](#restrict-a-window-to-some-crews) excludes the task's crew. | Change the task's crew or widen the restriction. |
| `human_crew` | The task is assigned to a [human crew](/reference/config/#human-crews), so no drain admits it. | Reassign it to an agent crew. |
| `no_drain_tag` | The task carries `no-drain`: it ships only when you ship it explicitly, and a drain does not auto-approve it. | Remove the tag. |
| `delivery_job_unavailable` | The task's `delivery:<job>` tag names a plugin delivery job that is disabled, uninstalled, or lacks the drain's ship mode. | Enable the plugin the detail names, or remove the tag. |
| `pr_forge_remote_missing` | The workspace ships through pull requests, but the checkout has no Git remote on a network host. | Run `orbit workspace ship-mode local`, add a forge remote, or tag the task `delivery:task_local_pipeline`. |
| `local_route_before_pr`, `local_route_before_landing` | `review.timing` is `before_pr` or `before_landing`, but the task ships locally, where neither review runs. | Set `review.timing` to `off`, or ship through the PR route. |

`orbit doctor` also reports a missing forge remote, on its `forge-remote` row.

Neither approval nor the drain bypasses dependencies or locks, so a window can
end with tasks still in the backlog.

### Override a pilot admission hold

To approve the work anyway, or to clear a mistaken finding, append a human
decision as a task comment:

```bash
orbit task update "$TASK_ID" --comment 'task-pilot-admission: approve-anyway'
orbit task update "$TASK_ID" --comment 'task-pilot-admission: clear'
```

The first line must match exactly; put any explanation on later lines. The
decision releases the hold and keeps the finding as audit evidence. A later
pilot assessment supersedes it, except that a `native_os_required` decision
stays valid until the criteria or requirements change. Agent-authored
decisions release nothing, and all other admission checks still apply.

## 4. Start a bounded window

- **Dashboard:** on **Drain → Start**, set **Window length** (1 to 24 hours)
  and **Parallel tasks**, choose the options below, then click **Start …
  window** and confirm. See [Auto-drain](/how-to/dashboard/#auto-drain).
- **Agent:** ask for a window and say how long, how many tasks at once, and
  whether to complete them.
- **CLI:**

  ```bash
  orbit run auto --for 3h
  orbit run auto --for 3h --concurrency 8
  orbit run auto --for 3h --complete
  ```

| Flag | Dashboard control | Effect |
|---|---|---|
| `--for <duration>` | **Window length** | How long new work may start. A task in flight when it expires still finishes. |
| `--concurrency <n>` | **Parallel tasks** | Tasks in flight at once (default 5). Not a batch size: the drain refills each free slot from the whole backlog. |
| `--complete` | **Mark shipped tasks done** | Moves every task the window ships from `review` to `done`; without it they stop at `review`. See [Completing work with `--complete`](/getting-started/workflows/#completing-work-with---complete). |
| `--approve-proposed` | **Approve qualifying proposals** | [Lets the window approve](#let-the-window-approve) qualifying `proposed` tasks. |
| `--auto-fix` | **Auto-fix PR blockers** | [Repairs the window's blocked pull requests](#repair-blocked-pull-requests). |
| `--allow-crew <crews>` | None | [Restricts the window to some crews](#restrict-a-window-to-some-crews). |
| `--auto-concurrency <mode>` | None | Forecast-based admission: `off`, `shadow` (record what the forecast would admit) or `enforce` (fit new work within measured CPU and memory capacity, under the `--concurrency` ceiling). Omitted, the host's configured mode applies. |

In the dashboard, the three optional controls need an operator session.

### Follow the window

Open the run from the **Drain** card header; its detail lists the child runs.
A task in progress has **View run** on its row. From the CLI, record the
returned ID as `$AUTO_RUN_ID`:

```bash
orbit run show "$AUTO_RUN_ID"
orbit run trace "$AUTO_RUN_ID"      # run tree with child run IDs
orbit run logs "$CHILD_RUN_ID"
orbit task show "$TASK_ID" --fields status,job_run_id,comments
```

A submitted run is not a delivered task; check the task itself.

### Repair blocked pull requests

`--auto-fix` lets the window repair the pull requests it opened when a required
check fails or the branch conflicts with its base. It is off by default and
never merges; add `--complete` to also merge and finish tasks.

```bash
orbit run auto --for 4h --auto-fix
orbit run auto --for 4h --auto-fix --complete
```

In the dashboard, **Auto-fix PR blockers** needs an operator session on the
owner machine and is disabled on pull replicas.

The drain watches each pull request it opened:

- Pending checks wait.
- Infrastructure and transient failures are rerun, up to
  `workflow.ci_infrastructure_retry.max_attempts` times per PR head
  (default 3).
- A failed required check or a conflict goes to a repair agent, drawn from
  `workflow.purpose_crews.auto_fix`, else `workflow.low_complexity_crews`, and
  narrowed by `--allow-crew`. The repair must pass your required validation
  before it is pushed, and the drain then checks the new head.
- Missing approvals, permissions, admin rules, unknown status and failed
  optional checks start no agent.

Each PR gets at most one repair per head and blocker, and three in total,
recorded in the task's history. For the crew settings, see
[System work](/reference/config/#system-work).

### Restrict a window to some crews

`--allow-crew` limits one drain to the crews you name, for example when a
provider is down, rate-limited or out of budget. The dashboard's start card
has no crew restriction.

```bash
orbit run auto --for 4h --allow-crew opus,sonnet
orbit run readiness --allow-crew opus,sonnet   # preview what would be skipped
```

- The restriction applies to this run only and changes no configuration. An
  unknown or empty crew name fails before anything is dispatched.
- Every activity in the run is checked, including recovery steps. Aliases
  with the same provider, model and account pass; a crew with its own `env`
  is a separate account and must be named.
- An excluded task stays in `backlog` as `crew_not_allowed`. Orbit never
  falls back to another provider. To move the task, change its crew in
  **Tasks** or with `orbit task update "$TASK_ID" --crew <name>`.

## 5. Retune a running drain

Change a live drain in place instead of cancelling it. Cancelling mints a new
run ID, restarts the window, and makes you restate `--complete` and
`--allow-crew`. Ask your agent or use the CLI; the dashboard has no retune
control.

```bash
orbit run show "$AUTO_RUN_ID"                       # current ceiling and who set it
orbit run concurrency "$AUTO_RUN_ID" --set 7
orbit run concurrency "$AUTO_RUN_ID" --set 3 --reason 'provider rate limited'
orbit run extend "$AUTO_RUN_ID" --for 2h            # keep admitting for 2h from now
```

- Raising the ceiling fills the extra slots on the next admission pass.
- Lowering it pauses admissions until enough children finish. Tasks in flight
  are never cancelled or cut short.
- `--if-revision N` applies the change only while the ceiling is still at the
  revision `orbit run show` reported, so two operators cannot overwrite each
  other.
- `orbit run extend` never shortens a drain or reopens a finished one.

## 6. Stop admissions, or cancel work

These are different actions.

**Stop new admissions.** Click **Stop** on the **Drain** card (it needs an
operator session), or run:

```bash
orbit run auto --stop
```

Tasks already started keep running with the completion authority they were
admitted under. Other workspaces and jobs are untouched, and stopping twice
does nothing. `--stop` cannot be combined with the flags that start a drain.

**Cancel work in flight.** Cancel each child you want to abandon: open it in
**Jobs** and click **cancel**, or:

```bash
orbit run trace "$AUTO_RUN_ID"                  # find the child run IDs
orbit run cancel "$CHILD_RUN_ID" --confirm
```

A cancelled task returns to `backlog` with the reason, and its candidate stays
available to resume. Add `--block` to keep the task `blocked` for manual
recovery instead.

Cancelling a local drain's own run leaves its children running. To stop the
drain and every task run it started:

```bash
orbit run cancel "$AUTO_RUN_ID" --confirm --force
```

The command exits 1 when a child's stop cannot be confirmed. Cancelling a
[pull drain](/how-to/distributed-drain/#stop-cancel-and-settle) is graceful:
unlaunched claims return to the owner's backlog and launched tasks finish.
`--force` stops those too.

## 7. Recover from a failed delivery

Ask your agent first. The `orbit-orchestrate` skill reads the run evidence,
matches it to a known failure, and files a repair task when the code needs
fixing.

By hand, find out why the run failed before you retry. Fix a real code, review
or dependency problem first.

1. **Find the failure.** In **Jobs › History**, filter to **Failed** and open
   the run; it opens on the failed step and its error. From the CLI:

   ```bash
   orbit run history --failures      # one row per task and root cause
   orbit run show "$AUTO_RUN_ID"     # the window's failed children
   orbit run logs "$CHILD_RUN_ID"
   ```

2. **Check the recovery steps.** A failure the agent declares itself goes
   through step recovery before the task becomes `blocked`. Read those steps
   before you decide what the task needs.
3. **Retry deliberately.** `orbit task show "$TASK_ID"` prints the exact
   command on its `Next:` line. Once the cause is fixed, either:
   - click **Resume** on the failed run in **Jobs**
     (`orbit job resume <run-id>`) to continue from its first unsuccessful
     step; or
   - move the task back to `backlog` in **Tasks**
     (`orbit task update "$TASK_ID" --status backlog`) so a later window runs
     it again and resumes any saved candidate.

A task that you cancelled with `--block` also needs the move back to
`backlog`.

### If the block was infrastructure

A task blocked by a missing provider launcher or a transient infrastructure
failure can return to the backlog once the cause clears:

```bash
orbit task recheck-blocked             # report which blocks have cleared
orbit task recheck-blocked --confirm   # return those tasks to backlog
```

A task blocked by its own failure stays blocked. Each published candidate is
requeued this way at most twice; after that, use `orbit task requeue`. A host
or environment failure that keeps recurring needs the host fixed, not another
attempt.

### If the blocked task offers Approve and land

A blocked task whose failed run handed off a pull request can offer **Approve
and land** on its block decision (`orbit decision answer <request-id> land`).
Only an operator can choose it. Orbit merges and completes the task only when
the pull request is open, not a draft, still at the handed-off head, free of
conflicts, and free of failed or pending required checks. Otherwise it refuses
and leaves the request open.

### If the blocked task has an open review finding

When a reviewed candidate can still return to the backlog, its block decision
offers an answer that accepts the open findings and resumes that same
candidate. Choosing it does not change the task's description or acceptance
criteria. The successor delivery validates the candidate and follows its
selected review policy; a fresh independent review runs only when that policy
calls for one. The normal landing gates still apply.

An option that would change the description or acceptance criteria says that
the reviewed candidate is discarded. Choosing it discards the candidate and
closes its pull request, including when required checks have not passed. A
delivery that is still running is refused until that run stops; the refusal
is not caused by the option's rewrite. On the dashboard that refusal stays
on the decision card until you dismiss it or it expires.

### If a run is stuck pending

A run stuck `pending` with no live worker still holds its task reservations.
Cancel it in **Jobs** or with `orbit run cancel "$RUN_ID" --confirm`. The
[stuck job runs runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/stuck-job-runs.md)
explains how to tell a stuck run from a slow one.

### Repair a cancelled owner-local published PR

`orbit job resume` refuses cancelled runs. When you cancelled an owner-local
PR run after it published, and its task is still in `review`, an operator on
the owning host can authorize the remaining work on the same branch and pull
request. The pull request must be open and unmerged, with automatic merge
disabled.

First update the task so its description, criteria and context files describe
the work you authorize. Then inspect:

```bash
orbit tool run orbit.run --input \
  '{"action":"resume","id":"<cancelled-run-id>","source_repair":{"action":"inspect","task_id":"<task-id>"}}'
```

Copy the whole returned `binding` object into a request file, unchanged, and
submit it:

```json
{
  "action": "resume",
  "id": "<cancelled-run-id>",
  "source_repair": {
    "action": "submit",
    "task_id": "<task-id>",
    "request_key": "published-source-repair-1",
    "binding": { "<replace with the entire inspected binding>": "" },
    "diagnosis": "Name the failing CI job or source defect, its published head, and the source edits you authorize."
  }
}
```

```bash
orbit tool run orbit.run --input-file source-repair-request.json
```

Repeating the identical request returns the same run, so a lost reply is safe
to retry. A changed payload under the same `request_key` is refused, and each
pull request has a bounded repair budget. If the workspace has an exclusive
claim, add its `claim_token` as a top-level field in both requests.

The new run continues the saved candidate, reruns your required validation,
and stops at `review` with auto-fix off. The
[stuck job runs runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/stuck-job-runs.md)
covers refusals and edge cases.

## 8. Clean up and keep the record

Check the workspace, then reclaim the worktrees that settled tasks left:

```bash
orbit doctor
orbit gc worktrees                       # report what it would remove
orbit gc worktrees --confirm             # remove settled worktrees
orbit gc worktrees --reclaim --confirm   # free build output, keep checkouts
```

`orbit gc worktrees` collects worktrees whose task is `done`, `rejected` or
`archived`, or has been parked in `someday` for
`worktree.parked_reclaim_after_hours` (default 336). `--reclaim` deletes only
the paths in `worktree.reclaim`. A failed run's checkout stays available for
rescue. The [`worktree-gc` routine](/how-to/recurring-work/#2-turn-on-routines)
does the same on a schedule, and also prunes entries in the checkout's
`.orbit/tmp` that have not changed for 24 hours.

A delivery window changes a lot of task state. To snapshot it somewhere you
can restore from, see [Publish and restore tasks](/how-to/task-publication/).
To operate across hosts, see
[federated MCP setup](/how-to/mcp-integration/#register-the-federated-mux).
