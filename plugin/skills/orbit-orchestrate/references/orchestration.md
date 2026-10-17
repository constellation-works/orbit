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
orbit run auto --host <owner-name> --pull <workspace-name-or-ws_id> --for 2h # on a replica
orbit run auto --stop                      # stop new admissions; children keep running
orbit run concurrency <run-id> --set 7     # retune a live drain, without replacing it
orbit run extend <run-id> --for 4h         # lengthen a live drain's window in place
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

It re-lists the whole backlog every pass, so a slot is refilled as soon as its
own task finishes and a task filed mid-window starts without waiting for the
batch around it. For owner/local drains, `--concurrency` (5 by default) is a
workspace-wide bound: workers left running by stopped coordinators, explicit
`run ship` deliveries, and unsettled pull admissions all consume slots. A
wrapper and its delivery consume one slot together. Delivery jobs impose no
active-run limit of their own.

A replica's pull drain uses `--concurrency` as an admission ceiling with capped
before-landing review credit. Before each claim it counts
`occupied - min(reviewing, N)` against N. A leaf becomes reviewing after the
owner's persisted before-landing admission has `applies=true`; validation and
publishing before then count as implementing, while later review revalidation
and handoff remain reviewing. The credit is capped at N, but reviewing leaves
can exceed N as replacements reach review. With an unchanged N and no
pre-existing excess, total occupied leaves stay at most 2N and implementing
leaves at most N. Lowering N preserves running leaves, so existing excess can
remain temporarily. `orbit run show` reports the leaf stages. Review rebuilds
still compete for repository build-budget slots, so N is not a hard total-live
task cap for pull drains.

Resource forecasting is an additional admission check, not a replacement for
that worker ceiling. Global `machine.auto_concurrency.mode` defaults to `off`;
`shadow` records what forecast fit would do while admitting as `off`, and
`enforce` uses forecast CPU/memory fit within the free slots left by the ceiling
and resource throttle. Inspect read-only `orbit run profiles` for delivery
profiles and sparse fallback, `orbit run profiles --accuracy` for forecast
error and shadow decisions, and `orbit run show <drain-run-id> --json` for the
recorded pass. For one local drain, `orbit run auto --auto-concurrency <MODE>`
selects its captured mode (`off`, `shadow` or `enforce`); omitting the flag
captures `machine.auto_concurrency.mode` at submission. MCP
`orbit_workflow_auto` `action: "start"` accepts the same `auto_concurrency`
values. The selected mode and its source (`explicit` or `configured`) survive
restart/resume without writing configuration. Run show and readiness expose
`mode`, `source`, and the current `configured_mode`. Pull drains refuse this
override; set the executor host's mode instead. No profiles means forecasting
adds no restriction. Preserve operator policy when explaining waits: this
per-drain choice does not authorize starting work or changing machine settings,
host thresholds, or the worker ceiling. Settings and limits:
[configuration.md](../../orbit-setup/references/configuration.md#common-settings).

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

The window is adjustable the same way. `orbit run extend <run-id> --for <d>
[--concurrency N]` moves a live drain's deadline to the later of its current
one and now plus `<d>` (at most 24 hours ahead), on the same run, and retunes
its ceiling when `--concurrency` is given. **Extend a drain whose window is
about to lapse instead of starting another**: a second `orbit run auto`
queues behind the live drain and admits nothing until that drain ends, which
for a pull drain is only after every leaf it claimed has settled.

- A shorter window is a no-op (`outcome: unchanged`); a window never shrinks.
- A pull drain can be extended in its settling tail, after its window closed,
  and resumes requesting. An owner drain whose window has closed is refused:
  it has stopped admitting.
- It is refused for a drain stopped with `--stop` or being cancelled (an
  extension never reopens one) and for a drain started without `--for`, which
  makes a single admission pass.
- Each request is audited on the run with the old and new deadline, and
  `orbit run show` prints `Window: extended to …` (`drain_window_extension`
  in JSON). `orbit run readiness` reports the extended `ends_at`.

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

An admission pass that times out on a busy task-store lock (`lock_busy`) is
skipped, not fatal: `run show` reads `Admission pass deferred:
consecutive_failures=N last_pass_error=...`, naming the lock's holder, and
the next pass retries after the poll interval. Ten in a row fail the drain
with that error; find the holder in `orbit.jsonl` before restarting it. Any
other admission error still fails the drain at once.

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
  draws from a complexity pool — so an excluded provider cannot be reached
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

For a pull drain, register the owner with `orbit host add <ssh-target>` and
check `orbit host list`. `--host <name-or-machine_id> --pull <workspace-name-or-ws_id>`
resolves the owner workspace from that host's live list. Without `--host`,
`--pull` takes only the full host-qualified selector copied from discovery;
do not compose one by hand. Run the drain from that owner's replica checkout.
The flag selects a pull owner, not the execution host for `run ship`,
`run show/history/logs` or other host-local commands. For task reads and
updates on the CLI or federated MCP, omit the workspace to route by task ID
prefix; task creation/listing and dispatch retain workspace selection.
See [tool-surface.md](../../orbit/references/tool-surface.md#task-ids-and-host-selection)
for the routed tools and error remedies, and
[distributed-drain.md](../../orbit/references/setup/distributed-drain.md)
for pull setup, captured review policy and owner landing authority.

Runs are asynchronous: these commands return once the run is durable, printing a
run ID. They do not claim the eventual outcome.

`orbit run readiness` is the diagnostic counterpart to auto-drain. It reads a
bounded snapshot of the explicit workspace and reports each selected backlog
task as ready or waiting, naming unmet dependency IDs/statuses, context-lock
holders, live child-run claims, capacity saturation, and — with `--allow-crew`
— crew exclusion. It never creates a run, reconciles stale runs, reserves
files, or mutates a task.
Without `--concurrency`, the snapshot's ceiling is the live local drain's,
including an `orbit run concurrency` retune (`capacity.limit_source`
`run_control`, or `run_input` when the drain still has its submitted value).
When no local drain is live, a live pull drain supplies that ceiling the same
way (`pull_run_control` or `pull_run_input`), and text adds a `Leaf ceiling:`
line naming the pull run. Otherwise the ceiling is 5 (`default`). An explicit
`--concurrency` is `requested` and wins over both. `capacity.max_active_leaf_runs`
and `capacity.free_slots` use that ceiling, so a follower pull drain submitted
at 3 is not reported against the default of 5.
Its answer can change immediately after the snapshot, so `eligible` means
"would be admitted by this snapshot", never a guarantee that work will start.

It also names a scheduled host shutdown or reboot and its mode. Within
`machine.host_shutdown_hold_minutes` (default 180), tasks report
`host_shutdown_scheduled` and new unattended admissions hold; running workers
are left alone. A later schedule is reported while admissions continue. Pull
drains report the same hold as `Pull drain requests held:`. Report the time and
configured window, and wait or follow an explicit operator instruction; do not
cancel the schedule or override the hold automatically. See
[authorization.md](authorization.md#keep-authorized-work-moving).

Two of its answers separate contention from capacity:

- `conflict_deferred` means a slot was free and this task did not take it,
  because its files overlap something already spoken for.
  `blocking_task_ids` and `conflicts` name the tasks and selectors, and
  `provenance` says whether the blocker holds the lock (`held_lock`), is a
  task a live child is already carrying (`live_claim`), or was chosen earlier
  in the same admission wave (`same_wave`). When a blocker is held by a
  follower's live execution claim, `blocking_claims` names the claim, its
  machine and run, and the text view prints `claimed-on=<task>@<machine>`: the
  wait clears when that claim settles. The drain's hold reason names it the
  same way, and a candidate overlapping a claim is deferred with
  `execution_claim` provenance. `capacity_saturated`, by contrast,
  means there was no free slot at all.
- `surface_reserved` means a critical or high-priority task ranked ahead of
  this one waits only on context locks, and this task overlaps its surface.
  It is withheld so it cannot take each lock as it frees, and admits once the
  reserving task (`blocking_task_ids`) is admitted or leaves backlog. The
  reserving task reports `context_lock_conflict` with a `detail` saying it
  reserves. At most two tasks reserve per pass; work that does not overlap a
  reserved surface admits normally.
- `unverified_review_finding` means the workspace sets
  `review.verify_findings = true` and a review-filed bug has no CONFIRMED
  verdict for its current material. Claim and evidence-selector edits require
  verification again; comment-only edits retain confirmation. Legacy verdicts
  without a finding fingerprint also remain held. Run
  `finding_verification_pipeline` on it, or change its type
  or remove its review tag to accept it without verification, or set
  `review.verify_findings = false` to stop gating findings; `detail` gives
  the command.
- `awaiting_footprint` and `awaiting_exclusive_slot` mean the task declares
  no `context_files` in a run with more than one slot. It waits for the task
  pilot, then for a slot with nothing else in flight; `detail` names the
  deadline and the fix. A `conflict_deferred` whose `provenance` is
  `unknown_footprint` is blocked by a running task with no footprint, and
  `exclusive_reservation` by such a task waiting to go alone. See
  [Prepare selectors](#prepare-selectors-before-dispatching-under-traffic).
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
surface holds no context lock, so it protects no files.

**Never file with `--status backlog`, or promote, a task with empty
`context_files` while a drain is live.** File it `proposed`, run the pilot or
set selectors, then promote. On 2026-10-10 three tasks filed straight into a
live four-slot drain's backlog with no selectors were admitted together and
all edited `website/src/styles/custom.css`; two of the runs were cancelled.
A drain or ship with more than one slot now treats such a task (no
`context_files`, no `no-diff-expected` tag) as a whole-tree footprint. While
an enabled task-pilot routine this host owns would still prepare it, the task
waits (`awaiting_footprint`). After that, or once the pilot has assessed it
and left it empty, it starts only when no other leaf is in flight
(`awaiting_exclusive_slot`) and, while it runs, no other editing work is
admitted. A single-slot drain and `orbit run ship <id>` admit it as before.

Do **not** fill them inline. Use `orbit run task-pilot`: it audits tasks
read-only in bounded partitions, and its apply step persists only selectors it
validated.

```bash
orbit run task-pilot                            # zero-input discovery
orbit run task-pilot <id> <id>                  # audit exactly these
orbit run job task_pilot_pipeline --input 'task_ids=["<id>"]' --json
```

The shipped pilot steps draw their crew from `workflow.low_complexity_crews`.
Neither a `--crew` flag nor `--input crew=<name>` changes those steps. Inspect
the effective job and that pool before promising a provider; see
[crew selection](loop.md#select-an-allowed-crew).

Zero-input mode discovers only `proposed`/`backlog` tasks in the invoking
workspace whose `context_files` is empty, and skips tasks tagged as needing no
diff. It also excludes a task already named by the durable prepare checkpoint
of an active pilot run and reports the owning run ID, so a later discovery run
can inspect new work without repeating the expensive assessment. Explicit
`task_ids` audits exactly the named tasks, including ones that already have
selectors, while excluding tasks held by another active run as
`already_preparing` with `prepared_by_run_ids`. Preparation continues for the
free named tasks. Apply settles each held task as `superseded` with reason
`piloted_elsewhere`; if every named task is held, the run succeeds without
agent work. Inspect or resume the owning run for that task's assessment.

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
already-landed, dependency, and conflicting-decision warnings. The shipped
workspace routine is disabled by default and uses a `preparation_eligible`
state trigger, not a periodic cron: each tick fingerprints eligible tasks and
queues those without a fresh assessment after the 2-minute debounce or the
10-minute maximum wait. A material change can make a task due again; an
unchanged backlog stays quiet. Inspect the workspace's actual `task_pilot.yaml`
before relying on that cadence, because a customized routine or preserved
legacy cron definition can differ from the shipped seed.
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

## Retry a stopped owner landing

A healthy accepted handoff can keep its native leaf binding while its owner landing
stops on a provider gate. Inspect the owner's distributed claim panel, original stop,
latest landing attempt and provider state. Do not resume or rebind the foreign leaf
just to retry landing. Only a trusted operator may request a same-handoff retry:

```bash
orbit task retry-handoff <handoff-id> --owner-workspace <owner-workspace-id> \
  --candidate <accepted-commit> --base <accepted-base> \
  --attempt <latest-attempt> --state stopped \
  --reason '<why the same candidate can land now>' --request <saved-request-key>
```

The MCP equivalent is `orbit.task.retry_handoff` with `workspace`, `handoff_id`,
`expected_candidate_commit`, `expected_base_commit`, `expected_latest_attempt`,
`expected_latest_state`, `reason`, and `request_key`. The workspace must be the owner's
logical workspace ID or absolute checkout path. This is operator-only and unavailable
inside a managed run. The dashboard offers **Retry landing** with the same expectations
and retains its replay key in browser storage.

Reuse the key and exact inputs after a lost reply. The durable receipt reserves one
attempt atomically and resolves the same run on replay. Only a stopped landing or an
uncertain intent with no live owner job qualifies. The normal landing job reconciles
external intent before any merge: an already-merged exact candidate is observed and
completed without a repeated merge. Every native certificate, review, required check,
remote identity/head/base, lease and merge-certainty gate remains in force. Original
stopped evidence stays in history. Changed or invalidated work needs its own repair
and fresh handoff; the retry accepts no `auto_fix`, raw pipeline input or head override.

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
- Completed `no-diff-expected` work completes without a PR. In owner or claimed
  mode, it runs only commands its own brief requires, with no workspace gates,
  required validation commands, or `no-diff.json` and workspace validation logs.
  No-op implementations without the tag still need validated no-diff evidence.
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

- When none of its pilots is running, a pass selects up to ten qualifying
  proposed tasks, including ones filed mid-window. A task qualifies with the
  `no-diff-expected` tag, or with non-empty `context_files` and an assessed
  complexity.
- The drain pilots them through a detached `task_pilot_pipeline` run, and the
  pilot's apply step approves a task only under the existing promotion rules:
  no duplicate, already-landed, blocked-by, conflict or warning finding, and
  selectors that resolve at the pinned revision. The authority is verified
  against the drain that dispatched the pilot.
- Admission never waits for the pilot: eligible backlog tasks keep starting
  and freed slots keep refilling while it runs. Idle passes wait on the pilot,
  at most one poll interval, instead of sleeping. A task the running pilot
  carries is held as `pilot_running` and is not piloted again.
- An approved task gets the ordinary approve transition with a history note
  naming the drain run, and is admitted by the pass that sees the pilot
  finish. A pilot still running when the window closes holds the drain open
  until it finishes; what it approves then waits in the backlog for the next
  drain.
- A task tagged `no-auto-approve` is never approved by any automatic
  promotion authority (this drain or the CI sweep). The drain does not pilot
  it; it stays `proposed`, held with reason `no-auto-approve`, until a human
  approves it. File a task with that tag when it needs a human decision.
- A task whose current pilot assessment is `verified_no_diff`, and that is
  not tagged `no-diff-expected`, is held as `pilot_verified_no_diff` with the
  pilot's evidence and the commits it cites. When Orbit automation filed it
  (`ci-failure-sweep`, `delivery-code-review` or `auto-task:*`), the pass
  archives it with a system comment once every cited commit exists and at
  least one is on the base branch. Human- and orchestrator-filed tasks are
  never closed this way; archive or re-scope them yourself.
- Everything else stays `proposed`. `orbit run show` and `orbit run readiness`
  report approved, closed and held counts with each hold reason. A held task
  is not piloted again until it changes.
- `--approve-proposed` with `--pull` is refused before anything is submitted:
  a follower does not approve another host's tasks.

### Auto-fix authorization

`orbit run auto --auto-fix` is the explicit authorization for one drain to
repair the pull requests its own runs opened. It is default-off, never enabled
by configuration, and independent of `--complete`. It authorizes repair only:
`--auto-fix` alone never merges, and only `--complete` takes a task to `done`.
It cannot be combined with `--pull` or `--stop`.

- The completion watcher detects the blockers; no second watcher exists. Pending
  checks wait. Failed required checks are repaired by the job's
  `step_failure_recovery`, with the failed head's error signature and log
  excerpt as evidence. Conflicts are repaired by `pr_conflict_recovery` against
  the current base.
- A missing protected check may trigger one infrastructure rerun without an
  agent: the forge must read effective classic and ruleset policy with integration
  identity, bind the PR, repository, base, head, workflow, event and latest attempt,
  and read complete workflow source, jobs and logs proving a direct dependency on
  a failed API-read prerequisite with an explicit HTTP 502, 503 or 504. Literal
  names and a bounded single-axis literal matrix are supported; ambiguous or
  dynamic dependencies hold. The workflow blob must match the event base and
  exact PR head, proving the merge-ref DAG; changed definitions hold.
  Credential/permission diagnostics veto recovery.
- The owner reserves that rerun in the primary task's durable history under its
  write lock before mutation, separately from the agent-repair budget. One
  infrastructure rerun is allowed per repository/PR across heads, resumes and
  siblings. Original evidence, the reply and current-attempt observations are
  retained under `auto-fix/<run>/infrastructure-<workflow-run>-*.json`. A lost reply
  is reconciled by observation, never another mutation. A second failure, missing
  protected result, superseded attempt, changed policy or identity, or completion
  deadline expiry holds the PR. Repair-only authority still cannot merge.
- Approvals, permissions, administrative rules, unknown or unreadable evidence,
  infrastructure failures outside that narrow prerequisite case, and optional
  failures alone launch no agent; the leaf stops in `review` with the reason.
- The repair goes through the ordinary commit, required final validation and
  lease-checked push. The watcher then re-checks the new head, and a reviewed
  head goes back through re-review before it can merge.
- The crew comes from `workflow.low_complexity_crews` through the run's frozen
  pool draw, restricted by `--allow-crew`. Conflict recovery uses this pool too
  under `--auto-fix`. Task complexity and crew are never changed, and nothing
  escalates.
- Every attempt is written to the primary task's history with the PR, head,
  blocker, attempt number, crew and draw source before any agent runs. A PR
  head gets at most one repair per blocker across polls, restarts and
  owner/follower overlap. Each PR has at most three repairs in total. When they
  are spent, the leaf stops with `auto_fix_budget_exhausted` and lists the
  attempts.
- Closed or merged PRs and superseded heads get no repair.

Submission stays asynchronous, so a `--complete` run's eventual outcome is not
known when the command returns — confirm with `orbit run show <run_id>` and
`orbit.task.show` rather than assuming it completed.
