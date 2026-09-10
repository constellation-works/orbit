# Orchestrating work

Driving a backlog through Orbit rather than executing one task by hand. Read
[workflows.md](workflows.md) first for what jobs, activities, and runs are; this
covers the choices an orchestrator makes on top of them.

## Entry points

Use `orbit_workflow_ship` with explicit `task_ids`, `workspace`, and attribution
when driving an authoritative MCP connection. Observe with
`orbit_workflow_run_show/list`; resume eligible terminal work with
`orbit_workflow_run_resume`, which returns a new linked run. These operations
require operator authority. Managed leaf runs cannot dispatch follow-up runs.
See [tool-surface.md](tool-surface.md).

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
orbit run readiness TASK-123 --json        # explain selected task IDs as JSON
orbit run ship <task-id> --complete  # ... and also carry it through to `done`
orbit run ship-sweep --dry-run      # what every registered workspace would ship
orbit run triage                    # diagnose tasks blocked by failed runs
```

`ship` with no IDs discovers ready backlog work itself. `--mode` defaults to the
workspace's registered ship mode (`pr` unless set otherwise), and `--base`
defaults to `workflow.base_branch`.

`run auto` drains loose leaf tasks plus one epic for a bounded window. The window
bounds only the *start* of new work — a task already shipping when it expires
still finishes.

It keeps `--concurrency` tasks in flight (5 by default) and re-lists the whole
backlog every pass, so a slot is refilled as soon as its own task finishes and a
task filed mid-window starts without waiting for the batch around it.

That ceiling is adjustable while the drain runs. `orbit run concurrency <run-id>
--set N` (MCP: `orbit.workflow.run.workers`) records a live ceiling on the run
itself, so **do not cancel a drain to change how many workers it uses** — that
mints a new run id, restarts the window, and re-states the completion
authorization. The retune keeps all of them:

- The next admission pass reads the new ceiling. Raising it fills the extra
  slots from the same backlog; lowering it stops new admissions until enough
  children finish, and cancels nothing that is already running.
- It is refused, with the reason, for a run that is not a drain, has not started,
  has already finished, or asks for more workers than the leaf job's own
  `max_active_runs` allows.
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

`--allow-crew` restricts a ship or drain to the crews you name — the lever for
a provider that is unavailable, rate-limited, or out of budget. For an
explicit ship it is checked at submission and again before provider dispatch;
for an auto drain it is opt-in and scoped to that run's window:

- Names must be crews this workspace configures. An unknown or empty one fails
  the command; nothing is dispatched, and no configuration is written.
- Scope is the run and everything it admits: the leaf and epic pipelines it
  starts inherit the same restriction, and the check runs again at each activity
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

Runs are asynchronous: these commands return once the run is durable, printing a
run ID. They do not claim the eventual outcome.

`orbit run readiness` is the diagnostic counterpart to auto-drain. It reads a
bounded snapshot of the explicit workspace and reports each selected backlog
task as ready or waiting, naming unmet dependency IDs/statuses, context-lock
holders, epic management, live child-run claims, capacity saturation, and — with
`--allow-crew` — crew exclusion. It
never creates a run, reconciles stale runs, reserves files, or mutates a task.
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

`context_files` is what conflict detection and file reservation read. Tasks with
an empty list can be dispatched, but they cannot be kept off each other's files
— so under high ship traffic, fill them first.

Do **not** fill them inline. Use the task-pilot job: it audits tasks read-only in
bounded partitions, and its apply step persists only selectors it validated.

```bash
orbit job show task_pilot_pipeline
orbit run job task_pilot_pipeline                                  # zero-input discovery
orbit run job task_pilot_pipeline --input 'task_ids=["<id>","<id>"]' # audit exactly these
orbit run job task_pilot_pipeline --input crew=luna                # override the pilot's crew for this run
```

Crew selection here is `--input crew=<name>`, not a `--crew` flag — see
[workflows.md](workflows.md#running-a-job) for the full contract and how it
differs from an activity-level `system_crew` override.

Zero-input mode discovers only `proposed`/`backlog` tasks in the invoking
workspace whose `context_files` is empty, and skips tasks tagged as needing no
diff. It also excludes a task already named by the durable prepare checkpoint
of an active pilot run and reports the owning run ID, so a later discovery run
can inspect new work without repeating the expensive assessment. Explicit
`task_ids` audits exactly the named tasks, including ones that already have
selectors, but refuses an ID already prepared by an active run; inspect or
resume the named run instead.

At the prepare activity boundary, an omitted optional `base_branch` is bound as
an empty string. Prepare treats an omitted or empty value as
`workflow.base_branch`, fetches that landing branch, and pins one
`source_revision` while preserving primary HEAD, index, dirty and untracked
files. Remote failure stops before an agent call. Each pilot runs in its own
detached checkout at that revision, with
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

Apply is isolated by partition. A stale task snapshot or malformed assessment
leaves that whole partition untouched while independently valid partitions are
still applied. The run then fails deliberately, and its durable apply output
lists each partition as `applied`, `skipped_stale`, or `failed`, plus the exact
task IDs actually applied. `orbit run show <run_id>` is therefore the recovery
source of truth. Resuming the failed run reuses its successful prepare, pilot,
and apply checkpoints (it does not rerun those agents); start a fresh zero-input
pilot only for tasks that remain empty after reviewing the recorded outcomes.

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

## Epics

An epic is a parent task with descendants. `epic_pipeline` gives the whole family
one stable worktree and branch, landing unfinished descendants against it rather
than opening a worktree per child. Completion is epic-scoped: descendants left
over at the iteration ceiling fail the run closed rather than silently passing.

`orbit run auto` picks up one epic per window alongside loose leaves.

## Triage

```bash
orbit run triage                    # every blocked task attributable to a failed run
orbit run triage <task-id> ...      # narrow the scan
```

Triage separates environmental casualties — a transient lock, a provider timeout,
a sandbox denial — from real failures. The former are re-backlogged; the latter
stay blocked with a diagnosis attached. Running it on a schedule is what keeps a
failed run from quietly parking a task forever. → [automation.md](setup/automation.md)

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
in the first place, see [multi-host.md](setup/multi-host.md).

## Unattended shipping

`ship-sweep` and the `ship-sweep` routine dispatch without a human present. Both
require `workflow.auto_ship = true`. Before enabling either:

- Confirm `workflow.base_branch` points where PRs should actually land.
- Enable worktree GC first — unattended shipping is the fastest way to fill a
  disk with abandoned worktrees. → [maintenance.md](setup/maintenance.md)
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

Submission stays asynchronous, so a `--complete` run's eventual outcome is not
known when the command returns — confirm with `orbit run show <run_id>` and
`orbit.task.show` rather than assuming it completed.
