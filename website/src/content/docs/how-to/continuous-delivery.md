---
title: Run a Delivery Window
description: "Prepare and approve tasks, check readiness, start a bounded drain from the dashboard or CLI, retune or stop it, and recover failures."
sidebar:
  order: 3
---

A delivery window drains your approved backlog for a set time. Orbit starts
backlog tasks in parallel until the window closes; work already in flight
finishes. Starting a window returns a run ID, not finished work, so check each
task's status before you treat it as delivered.

A second machine can drain the same workspace as a replica
(`orbit run auto --pull <selector>`) while the owner keeps landing authority.
See [Set Up a Distributed Drain](../distributed-drain/).

:::tip[Let your agent run it]
Ask your agent to run a delivery window. The `orbit-orchestrate` skill follows
these steps: it pilots and promotes the tasks you authorize, starts the drain,
and diagnoses any run that fails.
:::

Each step shows the [dashboard](../dashboard/) or agent route first and the
CLI as the fallback.

## 1. Prepare proposed work

The task pilot fills in each task's `context_files` (the files and directories
it will work in) and assesses its complexity. It does not approve or dispatch
anything.

- **Agent:** ask your agent to pilot the proposed tasks.
- **Dashboard:** in **Automation → Jobs**, click **Run ▸** on
  `task_pilot_pipeline`. To pilot new and edited tasks automatically, turn on
  the `task_pilot` routine in **Automation → Routines**. It is off by default
  and runs on the host scheduler clock.
- **CLI:**

  ```bash
  # Tasks that need a pilot.
  orbit run task-pilot --wait
  orbit run task-pilot "$TASK_ID" "$OTHER_TASK_ID" --wait  # exact tasks
  ```

Without task IDs, the pilot picks up `proposed` and `backlog` tasks whose
`context_files` is empty or whose complexity is unassessed. It skips a task it
already found to have no in-workspace targets while that assessment is fresh;
editing the task's description, criteria, status, or source makes it eligible
again, and naming its ID always audits it again.

When preparation selects no tasks, the run skips agent dispatch and applies
an empty result collection successfully, retaining any held-task skip details.
For a nonempty selection, at least one pilot partition must succeed before
apply runs; valid partitions can still apply when a sibling pilot fails.

Before you approve, check the pilot run (in **Runs**) and the selectors it
applied (the task's **context files** in **Tasks**). From the CLI:

```bash
orbit run history -j task_pilot_pipeline
orbit run show "$PILOT_RUN_ID"
orbit task show "$TASK_ID" --fields status,context_files
```

## 2. Authorize the backlog

Approving a `proposed` task moves it to `backlog`, where a window can start
it. The task pilot alone does not approve work; approve tasks individually or
authorize a local drain to approve qualifying proposals.

- **Dashboard:** in **Tasks**, click **Approve** on the task under **Awaiting
  approval**.
- **Agent:** tell your agent which tasks to approve.
- **CLI:**

  ```bash
  orbit task update "$TASK_ID" --approve --note "Pilot context reviewed."
  ```

Do not approve a task that is already in `backlog`. Once its pilot has
applied the context it needs, it is ready.

To authorize approval throughout a local window:

```bash
orbit run auto --for 4h --approve-proposed
```

On every pass, the drain pilots proposed tasks with the `no-diff-expected`
tag, or with context files and an assessed complexity. It approves those
with no duplicate, already-landed, conflict or warning finding, including
tasks filed while the window is open. Tasks tagged `no-auto-approve` stay
proposed until a human approves them. Other held tasks also stay proposed;
`orbit run show` and `orbit run readiness` report approval counts and hold
reasons. Each approval's history note names the drain run.

`--approve-proposed` is off by default and conflicts with `--pull`, since
only the owner approves work. Approval still respects dependencies, file
locks, and crew restrictions when the drain admits a task. Add `--complete`
separately if the window should also finish delivery.

## 3. Check what will actually start

Readiness is a read-only snapshot of what a window would start now and why the
rest would wait. It reserves and submits nothing, so an eligible task is not
guaranteed to start.

- **Dashboard:** the **Drain** card in the Tasks dock shows **Eligible now**,
  **Blocked by running**, the tasks waiting on a running task, and what a
  window started now would admit. **Locked files** below it shows held locks
  (`orbit task locks list`).
- **CLI**, for the reason behind each task:

  ```bash
  orbit run readiness
  orbit run readiness --concurrency 8
  orbit run readiness "$TASK_ID" --json
  ```

Common reasons a backlog task waits:

- **Unsatisfied dependencies.**
- **An active file lock.** Two tasks whose selectors overlap cannot run at
  once. `orbit task locks contention` shows which files the backlog collides
  on, which is what really limits your parallelism.
- **`surface_reserved`**: a critical or high-priority task ahead of it in the
  queue is waiting on file locks, and this task overlaps the files it needs.
  It waits so the higher-priority task gets each lock as it frees, instead of
  losing it to smaller tasks one lock at a time. It starts once that task
  starts. Unrelated work is not held.
- **`crew_not_allowed`**, when you preview a
  [crew restriction](#restrict-a-window-to-some-crews).
- **`pilot_duplicate`** or **`pilot_already_landed`**: the latest applied
  task-pilot assessment found duplicate work or work already landed. This
  also holds tasks filed directly into backlog. Read the finding in the task
  comments. Run task-pilot again to reassess it; a new assessment without
  either finding releases the hold. To approve the work anyway or clear a
  mistaken finding, append an explicit human decision from your CLI:

  ```bash
  orbit task update "$TASK_ID" \
    --comment 'task-pilot-admission: approve-anyway'
  # Or clear a mistaken finding:
  orbit task update "$TASK_ID" \
    --comment 'task-pilot-admission: clear'
  ```

  The first line must match exactly; put any explanation on later lines.
  Both decisions release the current assessment's admission hold while
  preserving its audit evidence. A later pilot assessment supersedes the
  decision. Ordinary edits, unrelated comments, and agent-authored decisions
  do not release it. All other admission checks still apply.
- **`delivery_job_unavailable`**: the task's `delivery:<job>` tag selects a
  plugin delivery job whose plugin is disabled or uninstalled, or that does not
  declare the drain's ship mode. The detail names the plugin. Enable it or
  remove the tag.
- **`pr_forge_remote_missing`**: the workspace ships through pull requests,
  but no Git remote of the checkout names a network host (only a local bare
  repository, say), so `pr_open` could never succeed. The detail names the
  remotes. Run `orbit workspace ship-mode local`, add a remote on the forge
  host, or tag the task `delivery:task_local_pipeline` to deliver it locally.
  `orbit doctor` reports the same verdict on its `forge-remote` row.
- **`local_route_before_pr`**: `review.before_pr` is on and this workspace
  ships locally, or the task's `delivery:task_local_pipeline` tag routes it
  locally. Before-PR review holds pull-request creation and does not run
  on the local-only route, so the task stays in the backlog instead of failing
  after dispatch. The detail names whether the global or workspace config
  turned the switch on. Turn `review.before_pr` off, or ship through the PR
  route. `orbit doctor` names the same combination.

Neither approval nor the drain bypasses dependencies or locks, so a window may
end with some tasks still in the backlog.

## 4. Start a bounded window

- **Dashboard:** in the **Drain** card, pick a **Window length** (`15m` to
  `8h`), set **Parallel tasks**, and under **When a task finishes** choose
  **Stop at review** or **Mark done**. Click **Start … window** and confirm.
  See [Auto-drain](../dashboard/#auto-drain).
- **Agent:** ask for a window and say how long, how many tasks at once, and
  whether to complete them.
- **CLI:**

  ```bash
  orbit run auto --for 3h
  orbit run auto --for 3h --concurrency 8
  orbit run auto --for 3h --complete
  ```

The window length bounds only when new work may start; a task in flight when
it expires still finishes. **Parallel tasks** (`--concurrency`, default 5) is
not a batch size: the drain refills each free slot from the whole backlog.

**Mark done** (`--complete`) authorizes this window to move every task it
ships from `review` to `done`, including tasks admitted later in the window. It
never approves `proposed` tasks. In the dashboard it needs an operator
session. See [Completing work with
`--complete`](../../getting-started/workflows/#completing-work-with---complete).

To follow the window, open its run from the **Drain** card header; the run
detail lists the child runs it started. A task in progress has **View run** on
its row. From the CLI, record the returned ID as `$AUTO_RUN_ID`:

```bash
orbit run show "$AUTO_RUN_ID"
orbit run trace "$AUTO_RUN_ID"      # run tree with child run IDs
orbit run logs "$CHILD_RUN_ID"
orbit task show "$TASK_ID" --fields status,job_run_id,comments
```

A task's submitted run is not its status; check the task itself.

### Restrict a window to some crews

`--allow-crew` limits one drain to the crews you name, for example when a
provider is down, rate-limited, or out of budget. Ask your agent or use the
CLI; the dashboard's **Start** has no crew option.

```bash
orbit run auto --for 4h --allow-crew opus,sonnet
# Preview what readiness would skip.
orbit run readiness --allow-crew opus,sonnet
```

- **For this run only.** Without it, the drain runs every crew. No
  configuration is changed. An unknown or empty crew name fails before
  anything is dispatched.
- **Enforced throughout.** Every activity in the run is checked against the
  crew that actually resolved, including `[workflow] system_crew`. The check
  compares effective provider and model, not crew name: an alias of a
  permitted crew runs, and a crew that resolves to an excluded provider or
  model is refused. The allowlist gates the crew that normal precedence picks;
  it never picks one.
- **Skips, never remaps.** A task whose crew is excluded stays in `backlog`,
  and readiness reports it as `crew_not_allowed`. **There is no automatic
  fallback to another provider.** To move the task, change its crew in
  **Tasks** or with `orbit task update <id> --crew <name>`.
- **Leaves other work alone.** Work another invocation already started keeps
  running.

## 5. Retune a running drain

To change how many tasks a live drain keeps in flight, retune it rather than
cancel it. Ask your agent or use the CLI; the dashboard has no retune control.

```bash
# Show the current ceiling and who last set it.
orbit run show "$AUTO_RUN_ID"
orbit run concurrency "$AUTO_RUN_ID" --set 7
# Reduce the ceiling after a provider rate limit.
orbit run concurrency "$AUTO_RUN_ID" --set 3 --reason 'provider rate limited'
```

Retuning keeps the run ID, deadline, completion authorization, and every child
already dispatched. Cancelling and starting again mints a new run ID, restarts
the window, and makes you restate `--complete` and `--allow-crew`.

- **Raising** the ceiling fills the extra slots on the next admission pass,
  usually within a poll interval.
- **Lowering** it pauses admissions until enough children finish. Tasks in
  flight are never cancelled or cut short.
- The ceiling cannot exceed the leaf pipeline's own active-run limit. A run
  that is not a drain, has not started, or has finished is refused.
- `--if-revision N` applies the change only while the ceiling is still the one
  you read, so two operators cannot silently overwrite each other.

## 6. Stop admissions, or cancel work

These are different actions.

**Stop new admissions.** Click **Stop** on the **Drain** card (it needs an
operator session), or run `orbit run auto --stop` (no run ID needed). Children already started keep
running under the completion authority they were admitted with, and the
coordinator is not cancelled. Stopping again, or with no active window, does
nothing. Other workspaces and jobs are untouched. `orbit run show
"$AUTO_RUN_ID"` then reports who stopped admissions. `--stop` cannot be
combined with flags that start a drain, including `--for`, `--concurrency`,
`--complete`, `--approve-proposed`, or `--allow-crew`.

**Cancel work in flight.** Cancel each child you want to abandon: open it in
**Runs** and click **cancel**, or:

```bash
orbit run trace "$AUTO_RUN_ID"           # find the child run IDs
orbit run cancel "$CHILD_RUN_ID" --confirm
```

Cancelling a task leaf returns its task to `backlog` with the reason and keeps
its candidate available to resume. Add `--block` to keep the task blocked for
manual recovery instead.

A plain cancel of a local drain leaves its children running. Stop admissions
first, then cancel only the children you want to abandon, or use
`orbit run cancel "$AUTO_RUN_ID" --confirm --force` to stop the drain and all
the task runs it started. A child whose stop cannot be confirmed is reported
and makes the command exit 1.

Cancelling a [pull drain](../distributed-drain/#stop-cancel-and-settle) is
graceful: unlaunched claims return to the owner's backlog while launched
leaves finish and settle. `--force` stops those leaves too and returns their
claims to the owner's backlog once their stop is confirmed.

## 7. Recover from a failed delivery

Ask your agent first. The `orbit-orchestrate` skill reads the run evidence,
matches it to a known failure, and files a repair task when the code needs
fixing.

By hand, find out why the run failed before you retry. Fix a real code,
review, or dependency problem first; do not rerun blindly.

1. **Find the failure.** In **Runs**, filter to **Failed** and open the run;
   it opens on the failed step and its error. From the CLI,
   `orbit run show "$AUTO_RUN_ID"` lists the window's failed children, and
   `orbit run show` or `orbit run logs` on a child gives the detail.
2. **Check recovery steps.** A failure the agent declares itself goes through
   step recovery before the task is sent to `blocked`, so check those steps
   before deciding what a blocked task needs.
3. **Retry deliberately.** A task a failed run left `blocked` stays there;
   nothing re-queues it for you. Once the cause is fixed, either:
   - click **Resume** on the failed run in **Runs** (`orbit job resume
     <run-id>`) to continue from its first unsuccessful step; or
   - move the task back to `backlog` with its status dropdown in **Tasks**
     (`orbit task update "$TASK_ID" --status backlog`) so a later window runs
     it again, resuming any saved candidate.

   `orbit task show "$TASK_ID"` prints the exact command on its `Next:` line.

A cancelled task leaf is already back in `backlog` by default, with its
candidate resumable on the next delivery. If you cancelled with `--block`,
return it to `backlog` when it is ready to run again.

A host-specific or environmental failure that keeps recurring needs the host
fixed, not another attempt.

A run stuck `pending` with no live worker holds its task reservations; cancel
it in **Runs** or with `orbit run cancel "$RUN_ID" --confirm`. The
[stuck job runs runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/stuck-job-runs.md)
explains how to tell a stuck run from a slow one.

Then check the workspace and reclaim worktrees left by settled tasks:

```bash
orbit doctor
# Report what it would reap.
orbit gc worktrees
# Remove the settled worktrees.
orbit gc worktrees --confirm
# Free build output while keeping checkouts.
orbit gc worktrees --target-only --confirm
```

`orbit gc worktrees` collects only worktrees whose task is `done`, `rejected`,
or `archived`, and removes nothing without `--confirm`. On a replica it reads
task status from the owner machine. `--target-only` deletes only
`<worktree>/target` for terminal runs with no live worker, so a failed run's
checkout stays available for rescue. To limit it by age or to one run, see
the [CLI reference](../../reference/cli/).

The scheduled `worktree_gc_pipeline` run also prunes the checkout's
`.orbit/tmp` scratch directory: a top-level entry goes once nothing inside it
has changed for 24 hours. Pass `--input scratch_older_than_hours=<hours>` to
`orbit run job worktree_gc_pipeline` to change the window for a run. Entries a
live process holds open or an active run names are skipped and reported.

## 8. Keep the task record durable

A delivery window changes a lot of task state. To snapshot it somewhere you
can restore from, see [Publish and Restore Tasks](../task-publication/). To
operate across hosts, [federated MCP
setup](../mcp-integration/#register-the-federated-mux) explains how to select
the owning workspace, without implying automatic failover.
