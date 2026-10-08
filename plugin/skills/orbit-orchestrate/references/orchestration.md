# Orchestrating work

Dispatch and supervise authorized tasks. Use [workflows.md](workflows.md) only
when job/activity internals or a particular run command need explanation.

## Entry points

Use `orbit_workflow_ship` with explicit `task_ids`, `workspace`, and attribution
when driving an authoritative MCP connection. Observe with
`orbit_workflow_run_show/list`; resume eligible terminal work with
`orbit_workflow_run_resume`, which returns a new linked run. These operations
require operator authority. Managed leaf runs cannot dispatch follow-up runs.
See [tool-surface.md](../../orbit/references/tool-surface.md).

The CLI offers additional discovery modes below. Use them only where the user
and workspace dispatch policy permit auto-discovery; creating tasks or enabling
a filing routine does not itself authorize execution.

```bash
orbit run ship                      # ship ready backlog tasks through the gated pipeline
orbit run ship <task-id> ...        # ship exactly these
orbit run ship <task-id> --allow-crew sol # ship exactly these, only on Sol
orbit run ship --mode local         # implement in a worktree, merge to the base; no PR
orbit run auto --for 2h             # drain the backlog for a window
orbit run auto --for 2h --concurrency 8   # ... with 8 tasks in flight at a time
orbit run auto --for 2h --allow-crew opus,sonnet  # ... using only these crews
orbit run auto --stop                      # stop new admissions; children keep running
orbit run concurrency <run-id> --set 7     # retune a live drain, without replacing it
orbit run readiness                        # explain current auto-drain eligibility, read-only
orbit run readiness <task-id> --json        # explain selected task IDs as JSON
orbit run ship <task-id> --complete  # ... and also carry it through to `done`
orbit run ship-sweep --dry-run      # what every registered workspace would ship
```

`ship` with no IDs discovers ready backlog work itself. `--mode` defaults to the
workspace's registered ship mode (`pr` unless set otherwise), and `--base`
defaults to the registered workspace base branch, else `workflow.base_branch`.
An explicit task ID must be in `backlog` or `in-progress`: shipping a `blocked`,
`proposed`, `review` or `done` task is refused at submission, naming its status,
so return a blocked task to the backlog (`orbit task update <id> --status backlog`)
before shipping it again.

`run auto` drains backlog leaf tasks for a bounded window of at most 24 hours.
The window bounds only the *start* of new work — a task already shipping when
it expires still finishes.

It keeps `--concurrency` tasks in flight (5 by default) and re-lists the whole
backlog every pass, so a slot is refilled as soon as its own task finishes and a
task filed mid-window starts without waiting for the batch around it. That
number is a workspace-wide bound: workers left running by stopped coordinators,
explicit `run ship` deliveries, and unsettled pull admissions all consume slots.
A wrapper and its delivery consume one slot together. The delivery jobs impose
no active-run limit of their own, so size the ceiling to what the host can carry.

That ceiling is adjustable while the drain runs. `orbit run concurrency <run-id>
--set N` (MCP: `orbit_workflow_auto` with `action: "resize"` and `concurrency`;
omit `id` to target the workspace's one live drain) records a live ceiling on the run
itself, so **do not cancel a drain to change how many workers it uses** — that
mints a new run id, restarts the window, and re-states the completion
authorization. The retune keeps all of them:

- The next admission pass reads the new ceiling. Raising it fills the extra
  slots from the same backlog; lowering it stops new admissions until enough
  children finish, and cancels nothing that is already running.
- It is refused, with the reason, for a run that is not a drain (an `orbit run
  auto` window or a replica's `orbit run auto --pull` drain), has not started,
  or has already finished.
- `--if-revision N` makes the change conditional on the ceiling still being the
  one you read, so two operators cannot silently overwrite each other. The
  current value and who last moved it are on `orbit run show <run-id>`
  (`drain_worker_limit`) and `orbit run readiness`.

`orbit run auto --stop` ends new admissions for this workspace's active
coordinator. It does not need a run id, does not cancel children, and is
idempotent when nothing is running. `orbit run show` reports
`Admissions: stopped by ...` and lists remaining children. To cancel workers
already in flight, `orbit run cancel <child-run-id> --confirm` each one —
do not cancel the coordinator for this.

If you stop and restart instead of retuning in place, the replacement counts
those still-running workers against its new ceiling: with K inherited slots and
a ceiling of N, it admits at most N-K new tasks (zero when K exceeds N).
`run show` reports `Capacity: occupied=… inherited=… limit=…` (JSON:
`drain_summary.capacity`) from the last admission pass, before that pass starts
new work. The `Leaves:` outcomes remain those of the displayed coordinator's
own children; inherited work is not counted as its success or failure.

A drain's own `State: success` says the coordinator ran, not that its leaves
shipped: it dispatches them detached. Read the `Leaves:` line on `orbit run
show <drain-run-id>` (JSON: `drain_summary`) for admitted / succeeded / failed
counts. It lists each failed leaf with its `orbit job resume <leaf-run-id>`, and
a `Still waiting:` block for backlog tasks the last pass never started.

`--allow-crew` restricts a ship or drain to the crews you name — the lever for
a provider that is unavailable, rate-limited, or out of budget. For an
explicit ship it is checked at submission and again before provider dispatch;
for an auto drain it is opt-in and scoped to that run's window:

- Names must be crews this workspace configures. An unknown or empty one fails
  the command; nothing is dispatched, and no configuration is written.
- Scope is the run and everything it admits: the leaf pipelines it starts
  inherit the same restriction, and the check runs again at each activity
  against the crew that was *actually* resolved — including an activity that
  names `workflow.system_crew` — so an excluded provider cannot be reached
  through an alias. Matching is by effective configured identity, so a differently
  named crew resolving to the same provider/model is permitted; naming a wrapper
  is not itself provider usage. Precedence is unchanged: explicit > task.crew >
  `[workflow].default_crew`, and the allowlist gates the winner rather than
  choosing one.
- A backlog task whose crew is excluded is **skipped, not remapped**. It stays in
  `backlog` on its own crew, and `orbit run readiness --allow-crew ...` reports it
  as `crew_not_allowed` with the crew it would have run as. Moving that work to a
  permitted crew is an operator decision — reassign the task, then it drains
  normally. Everything permitted keeps filling the slots at the usual rate.
- It governs only what this drain *starts*. Tasks another invocation already has
  in flight keep running to completion; nothing is cancelled. It carries no
  completion or promotion authority, and there is no automatic fallback to a
  different provider.
- On a replica pull drain (`orbit run auto --pull <selector> --allow-crew ...`)
  it limits the crews the drain declares to the owner, on every pass and on
  resume, so the owner hands it only tasks on those crews. The owner's before-PR
  reviewer is not restricted but must still run on that host. A pull drain
  without `--for` makes one admission pass, then only settles.

Runs are asynchronous: these commands return once the run is durable, printing a
run ID. They do not claim the eventual outcome.

`orbit run readiness` is the diagnostic counterpart to auto-drain. It reads a
bounded snapshot of the explicit workspace and reports each selected backlog
task as ready or waiting, naming unmet dependency IDs/statuses, context-lock
holders, live child-run claims, capacity saturation, and — with `--allow-crew`
— crew exclusion. It never creates a run, reconciles stale runs, reserves
files, or mutates a task.
Its answer can change immediately after the snapshot, so `eligible` means
"would be admitted by this snapshot", never a guarantee that work will start.

Two of its answers separate contention from capacity:

- `conflict_deferred` means a slot was free and this task did not take it,
  because its files overlap something already spoken for.
  `blocking_task_ids` and `conflicts` name the tasks and selectors, and
  `provenance` says whether the blocker holds the lock (`held_lock`), is a
  task a live child is already carrying (`live_claim`), or was chosen earlier
  in the same admission wave (`same_wave`). `capacity_saturated`, by contrast,
  means there was no free slot at all.
- `surface_reserved` means a critical or high-priority task ranked ahead of
  this one waits only on context locks, and this task overlaps its surface.
  It is withheld so it cannot take each lock as it frees, and admits once the
  reserving task (`blocking_task_ids`) is admitted or leaves backlog. The
  reserving task reports `context_lock_conflict` with a `detail` saying it
  reserves. At most two tasks reserve per pass; work that does not overlap a
  reserved surface admits normally.
- `capacity.occupancy` breaks the occupied slots down by what each is doing —
  `lock_waiting`, `implementing`, `post_implementation`, or `unknown` — with
  the wrapper, task, and descendant run IDs behind each. A drain whose slots
  are all `lock_waiting` is queued on itself, which the occupancy total alone
  cannot show. Phase comes from durable run state only; where that evidence is
  missing the phase is `unknown` and names the reason.

```bash
orbit run history -j task_auto_pipeline
orbit run show <run_id>
```

## Prepare selectors before dispatching under traffic

`context_files` is what conflict detection and file reservation read. Prepare a verified footprint before dispatch under traffic. An empty
surface is admitted without a context lock, so it protects no files.

Do **not** fill them inline. Use `orbit run task-pilot`: it audits tasks
read-only in bounded partitions, and its apply step persists only selectors it
validated.

```bash
orbit run task-pilot                            # zero-input discovery
orbit run task-pilot <id> <id>                  # audit exactly these
orbit run job task_pilot_pipeline --input 'task_ids=["<id>"]' --json
```

The shipped pilot steps explicitly select the `system` crew. Neither a
`--crew` flag nor `--input crew=<name>` changes those steps. Inspect the effective
job and system-crew configuration before promising a provider; see
[crew selection](loop.md#select-an-allowed-crew).

Zero-input mode discovers only `proposed`/`backlog` tasks in the invoking
workspace whose `context_files` is empty, and skips tasks tagged as needing no
diff. It also excludes a task already named by the durable prepare checkpoint
of an active pilot run and reports the owning run ID, so a later discovery run
can inspect new work without repeating the expensive assessment. Explicit
`task_ids` audits exactly the named tasks, including ones that already have
selectors, but refuses an ID already prepared by an active run; inspect or
resume the named run instead.

At the prepare activity boundary, an omitted optional `base_branch` is bound as
an empty string. Prepare treats an omitted or empty value as the registered
workspace base branch, else `workflow.base_branch`, fetches that landing branch, and pins one
`source_revision` while preserving primary HEAD, index, dirty and untracked
files. Manual preparation without a pinned source stops on remote failure.
State-triggered preparation best-effort fetches once during evaluation and
pins origin, keeping a local branch already ahead of origin; fetch failure
uses the local head captured before fetching. Its prepare step retains that
claim's pin. Each pilot runs in its own detached checkout at that revision, with
its cwd, input paths, and read-only filesystem profile bound there. Task tools
retain the owning logical workspace, and apply still checks task snapshots
with compare-and-set on that authority. Inspection checkouts use at most 16
exclusive slots in the common Git directory; normal return, error, and timeout
remove the checkout before releasing its lease. A crash releases the kernel
lease, and the next holder reclaims the abandoned checkout before reuse. No
registered worktrees or primary branches are removed or modified. Apply
validates selector existence against the same snapshot, not a later working tree, so a newly merged file is not reported as missing merely
because the primary lagged origin, and a later origin advance cannot admit a
path that did not exist at prepare.

The pilot agent inspection is read-only; its deterministic apply step mutates
validated task selectors. It runs five
partitions concurrently, and returns selector proposals plus duplicate,
already-landed, dependency, and conflicting-decision warnings. An enabled
workspace routine may already run the zero-input job every few hours — an extra
run before a large dispatch is still appropriate.
The task-pilot pipeline never promotes tasks or dispatches them; promotion and
shipping remain separate operator-authorized steps.

Apply is isolated by partition, then by task. A partition whose assessments are
malformed (duplicate, or not matching the prepared task IDs) fails as a whole
and mutates none of its tasks. Within a valid partition each task settles on
its own: a task that went stale at the write boundary is `stale` with a reason,
and a task with an invalid assessment is `invalid`. Independently valid siblings
and partitions still apply.

Durable edits that race a pilot settle as `superseded`, not as failures. This
covers a task whose fields, material, status or ownership changed after
preparation, a task admitted to or claimed for execution, a task that became
terminal, an operator rejection, and a task superseded by a source move under a
routine claim. Each skipped task carries a `reason` (for example `task_edited`,
`status_changed`, `execution_claim`, `workflow_admission`, or
`superseded_by_source`) and is not written. Such a task needs no repair.

Each partition in the durable apply output has one outcome:

- `applied`: every task applied or was already applied.
- `superseded`: every task applied, was already applied, or was superseded, and
  at least one was superseded. The partition may still list applied siblings in
  `applied_task_ids`, so read `task_outcomes`, not the partition label alone.
- `skipped_stale`: no task applied and every task was stale.
- `partial`: some tasks applied and others did not resolve.
- `failed`: no task applied and at least one did not resolve, or the partition
  was malformed.

The run succeeds when every partition is `applied` or `superseded`. Any
`failed`, `partial`, or `skipped_stale` partition fails the run. The durable
apply output lists each partition's outcome, its task outcomes, and the exact
task IDs actually applied. `orbit run show <run_id>` is therefore the recovery
source of truth. Resuming the failed run reuses its successful prepare, pilot,
and apply checkpoints (it does not rerun those agents); start a fresh zero-input
pilot only for tasks that remain empty after reviewing the recorded outcomes. A
superseded task was not written, so a fresh pilot assesses it against current
state if it still lacks selectors. Do not re-pilot the whole backlog for it.

## Keeping parallel runs off each other

- **Reservation is the system's job, not a worker's.** There is no
  worker-callable lock tool and none should be reached for.
- **Conflict detection reads live from in-flight tasks**, not only from
  reservation records — which is why current `context_files` matter more than
  they look.
- **A selector added mid-run binds only reservations requested after it.** It
  cannot retroactively revoke a reservation a concurrent run already holds.
- **Inspect and repair stale reservations** with `orbit task locks list` and
  `orbit task locks release <reservation_id>` — never by editing the store. The
  signature to match first is in [common-failures.md](common-failures.md).

## Large tasks and hierarchy

Parent/child relations describe a backlog; they do not order execution. Every
task is admitted as a leaf on its own declared `context_files`, by its own
priority, age, and dependencies — a parent inherits nothing from its children
and reserves nothing on their behalf. To sequence work, declare dependencies.

The `epic` tag is a size hint: *one large task a top-tier crew takes on whole*.
Crew selection reads it; admission ignores it. A root that used to rely on its
children's context declares none of its own, so it reserves nothing and
`reserve_locks` refuses it — give it real `context_files` or retire it.

## Failed runs

A failed run can park its task in `blocked` with the failure attached. The
owner's clock can dispatch final recovery for an eligible block; inspect its
decision before intervening. See [automation.md](../../orbit-setup/references/automation.md#built-in-final-recovery-of-blocked-tasks).
If it remains blocked, read the run and decide whether a rerun can succeed
before deliberately returning it to backlog.

```bash
orbit task list --status blocked
orbit run show <run-id> --json
orbit task update <task-id> --status backlog   # only once you know a rerun can succeed
```

For blocks caused by a missing provider launcher, `orbit task recheck-blocked`
reports whether the launcher now resolves. Its `--confirm` option requeues only
the cleared launcher blocks; it leaves implementation failures blocked.

## Multi-operator workspaces

When two operators act on the same authoritative workspace store, one can hold
an exclusive claim and another must present its token. Claims do not coordinate
independent stores on different machines. These examples present an existing
token; they do not acquire a claim:

```bash
orbit run ship --claim-token <token>
ORBIT_WORKSPACE_CLAIM_TOKEN=<token> orbit run auto --for 1h
```

For splitting work across machines so their task IDs and schedules don't collide
in the first place, see [multi-host.md](../../orbit-setup/references/multi-host.md).

## Unattended shipping

`orbit run ship-sweep` and the `ship-sweep` routine dispatch without a human
present, and neither ever grants `--complete`. The command dispatches only
workspaces with `workflow.auto_ship = true`. The routine does not read
`workflow.auto_ship`; its `enabled: true` is the only switch, and it ships only
its own workspace. Before enabling either:

- Confirm the registered workspace base branch (else `workflow.base_branch`) points where PRs should actually land.
- Enable worktree GC first — unattended shipping is the fastest way to fill a
  disk with abandoned worktrees. → [maintenance.md](../../orbit-setup/references/maintenance.md)
- Watch `orbit run ship-sweep --dry-run` across a realistic backlog first.

## Handoff discipline

Task state, run state, and the durable stores are the handoff — never agent
prose. An orchestrator that reads a summary paragraph instead of
`orbit.task.show` or `orbit run show` is guessing.

## Delivery and completion

By default the task pipelines end in `review`. PR mode prepares a source branch
and opens a PR, then stops with that PR unmerged unless `--complete` was
authorized. Local mode implements in an isolated worktree and fast-forwards the
configured local base branch before the task reaches `review`; the leaf job's
`auto_push` input controls its optional push. Review is not a pre-merge stop in
local mode. Inspect the effective wrapper and child job inputs rather than
assuming local mode means the current checkout was edited or a remote branch was
updated.

Record validation, commit, branch/PR, and run evidence, then follow the user's
approval policy for completion. Task snapshot publication is independent of
source delivery and lifecycle.

`orbit run ship --complete` and `orbit run auto --complete` are the operator's
explicit authorization for one submitted run to finish delivery and take the
tasks it ships from `review` to `done`. Default-off, and never enabled by
workspace configuration, an environment variable, or an unattended routine such
as `ship-sweep`.

- Local mode completes only after the bundle merged *and* pushed; a failed
  publication leaves the task in `review`.
- PR mode completes only after the PR is verified merged. Branch protections and
  required checks are respected and never bypassed; pending checks may use
  GitHub auto-merge, but enabling auto-merge is not success. A closed or blocked
  PR, a refused auto-merge, or an expired wait leaves the task in `review`.
- Validated `no-diff-expected` work completes without a PR.
- `run auto --complete` is blanket authorization for every task the drain admits
  during its whole window, including work filed after it starts. Do not use it
  where the user authorized only the currently visible backlog.
- It authorizes delivery completion and `review -> done` only. It never approves
  `proposed` work into the backlog and is not an independent review verdict; the
  transition is recorded against the authorizing run and operator.

`orbit run auto --approve-proposed` (MCP `orbit_workflow_auto` `start` with
`approve_proposed: true`) is the explicit authorization for one drain to approve
`proposed` work, independent of `--complete`. Default-off and never enabled by
configuration.

- Each pass selects up to ten qualifying proposed tasks, including ones filed
  mid-window. A task qualifies with the `no-diff-expected` tag, or with
  non-empty `context_files` and an assessed complexity.
- The drain pilots them through `task_pilot_pipeline`, and the pilot's apply
  step approves a task only under the existing promotion rules: no duplicate,
  already-landed, blocked-by, conflict or warning finding, and selectors that
  resolve at the pinned revision. The authority is verified against the drain
  that dispatched the pilot.
- An approved task gets the ordinary approve transition with a history note
  naming the drain run, and is admitted by that same pass.
- A task tagged `no-auto-approve` is never approved by any automatic
  promotion authority (this drain or the CI sweep). The drain does not pilot
  it; it stays `proposed`, held with reason `no-auto-approve`, until a human
  approves it. File a task with that tag when it needs a human decision.
- Everything else stays `proposed`. `orbit run show` and `orbit run readiness`
  report approved and held counts with each hold reason. A held task is not
  piloted again until it changes.
- `--approve-proposed` with `--pull` is refused before anything is submitted:
  a follower does not approve another host's tasks.

Submission stays asynchronous, so a `--complete` run's eventual outcome is not
known when the command returns — confirm with `orbit run show <run_id>` and
`orbit.task.show` rather than assuming it completed.
