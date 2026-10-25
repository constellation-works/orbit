---
type: runbook
summary: Diagnose, cancel, resume, or replay pending and running Orbit job runs.
tags: [operations, jobs, runs, recovery, debugging]
paths: ["crates/orbit-core/src/application/job/**", "crates/orbit-cli/src/command/run/**", "crates/orbit-runtime/src/audit/run.rs"]
related_features: [activity-job, auditability]
related_artifacts: [ORB-10070, ORB-10496, ORB-10801, ORB-17179, ORB-18166]
last_validated: 2026-10-25
---

# Recover Stuck Job Runs

Use this runbook when a run remains `pending` or `running`, a worker dies, or a failed,
timed-out, or interrupted run needs to continue from its checkpoints.

## Inspect the run

`orbit job` manages job definitions plus resume and replay. `orbit run` is the run
observability surface.

```sh
orbit run history --limit 20        # recent runs; add -j <job_id> to filter
orbit run show   <run_id>           # per-step state table
orbit run logs   <run_id>           # raw stdout/stderr blobs (agent steps)
orbit run events <run_id>           # audit events for the run
orbit run trace  <run_id>           # parent/child event tree
```

When a provider exits unsuccessfully without a more specific diagnostic, the step
error is `cli subprocess exited with code N`, followed by `stderr tail:` and the
last four non-empty stderr lines. The tail is redacted and capped at 1024
characters (plus a truncation indicator). It is context only: typed failure
markers or check-repair phrases quoted in it do not change failure
classification, recovery, incident grouping or claim settlement. Use
`orbit run logs <run_id>` for the full captured output.

Before changing run state, confirm the recorded owner PID and whether that exact process
is still alive. A long-running run is not necessarily stuck.

`orbit run job`, `orbit run task-pilot`, and `orbit job resume` accept `--wait`.
The default wait deadline is 3600 seconds; `--timeout-seconds` overrides it with
a value from 0 to 21600 and requires `--wait`. If the deadline expires, the CLI
exits 1 and reports `wait_timeout=true` alongside the run's actual `state` in
both text and JSON. The run continues independently. Inspect the reported run
ID before resubmitting work. A run whose own execution ends in `timeout`
retains that state; its JSON `wait_timeout` is `false`.

### Read `orbit run show` output

This is the field reference `orbit run show --help` used to carry.

JSON shape: {"run":<job-run>,"pull_claim":<claim|null>,"crew_window":<window|null>,"catalog_layers":[{"reference":"job:…|activity:…","layer":"workspace|shipped|plugin:<ns>|explicit","shadows":[…]}],"pipeline_state":<state|null>,"steps":[<step>],"steps_source":"record|audit","provider_processes":[{"pid":...,"liveness":"alive|exited|unknown","stopped_descendants":[...],"blocked_on_stopped_descendant":<descendant|null>,...}]} or {"run_id":...,"job_id":...,"step":<step>,"step_output":<json|null>} with -s.

The State: line above is `.run.state`, not a top-level `.state`;

`.run.review_layer` (the Review: line) is the review layer before landing a delivery run captured at submission, `{"timing":"none|before-pr|before-landing","source":...}`, where `source` is `run-option` when the run's `--review` added it, `claim` for a claimed leaf, and otherwise the policy layer that decided it; it is absent for a run that captured none.

`.pipeline_state` is the pipeline checkpoint document and is null for a run that keeps none; a run that ended `success` or `cancelled` cannot resume, so `.step_output_pointers` maps each step index whose output `.pipeline` holds to the JSON pointer of that entry, in place of its `.step_outputs` copy.

`.steps` are the steps this view renders, and `.steps_source` says whether they came from the run record or its audit trail. The `#` column of the table is one-based and Duration is human-readable (`3h 39m 51s`);

`.steps[].step_index` stays zero-based and every duration stays in milliseconds. A step from the audit trail is numbered in the order steps started, which is not the position `.pipeline_state.step_output_pointers` and `.step_outputs` key by when an earlier step was skipped (by `when:` or by resume); `-s` reads that step's output by its id, not by index. A run whose record holds only a run-level step (an interrupted run's) shows its audit steps instead. The Catalog: lines name only layers other than `shipped` unless `--verbose` is given;

`.catalog_layers` always lists every reference.

`.pull_claim` (the Claim: line) names the owner task and claim a follower's claimed leaf executes, whether its outcome reached the owner, and its `failure_class` once a failure or release is recorded (`candidate` and `task_input` block the owner's task; `operator_cancel`, `provider`, `environment`, `owner_route`, `baseline_red`, `transient` and `base_conflict` release it to the backlog); it is null for every other run.

`.drain_summary` is set for an auto drain only (the Leaves: line): admitted/succeeded/failed/running/cancelled leaf counts, `failed_leaves`, and the backlog its last pass left `waiting`, plus `capacity` (workspace `active_leaf_runs`, `inherited_leaf_runs` outside this coordinator, and `max_active_leaf_runs`, sampled before the last admission wave; the Capacity: line) and `resource_throttle` when host resource pressure held that pass (the Throttled: line, shown for pull drains too from `.pipeline_state.drain_last_pass`); a drain's own `.run.state` says the coordinator ran, not that its leaves shipped.

`.pipeline_state.drain_last_pass` also records a pull drain's `last_pass_error`, `consecutive_pass_failures`, and sticky `degraded` warning; after three consecutive failed passes it stops admitting and keeps settling until its window closes. A local drain records there an admission pass it deferred because a task-store lock stayed held past its deadline (`last_pass_error_code` `lock_busy`, the Admission pass deferred: line, whose `last_pass_error` names the holder): that pass admits nothing, keeps the previous pass's waiting backlog with its `waiting_recorded_at`, and the next pass retries after the poll interval; ten consecutive deferred passes fail the drain with that error, and a pass that completes resets the count.

`.pipeline_state.drain_pass_counters` (the Admission passes: line) counts what limited every admission pass of a drain's lineage, local or pull, since `since`: `passes`, and `outcomes` with exactly one per pass, the first that applies of `admitted`, `failed`, `held`, `throttle`, `no_candidate`, `ceiling_full`, `gap_fit_held` and `deferred`; `held_by_reason` and `throttle_by_resource` split those two, and `waiting_by_reason` counts the passes in which a reason kept at least one task waiting, so its entries overlap. The counters survive terminalization and resume; a pull pass whose window has closed only settles and is not counted. Its `admission_refusal` (the Requests held: line) says why the last pass requested no claim although the window was open: `circuit_open` (the failure breaker is open), `circuit_half_open` (it waits for this drain's claims to settle before one probe claim), `settlement_refused`, `no_runnable_crew`, `host_suppressed`, `owner_busy_backoff` (a claimed leaf was released because the owner's store stayed locked: no claim is requested for 5 minutes from when it ended, `.crew_window.owner_busy_backoff` names `until`, and a third such episode in the window suppresses the host with `.crew_window.host_suppressed_by_busy_owner` set, whose remedy is on the owner), or the owner's probe refusal; `before_pr_reviewer_unavailable` names the review's timing (before-PR or before-landing) and, while its crew is only backed off for capacity, the time it lifts, when requests resume by themselves.

`.pipeline_state.pull_breaker` (the Breaker: line) is the failure breaker while it is open or half-open: it opens after three claims in a row settle as failures (released claims and Orbit's refusals of an implementer's handoff do not count), with `failures`, their `failure_classes` newest first, `opened_at` and `half_open_at`, when it admits one probe claim; a probe that succeeds closes it and one that fails reopens it with twice the wait. It is absent while closed. A pull drain's Throttled: line is this host's own pressure; the owner's throttle never holds a pull claim.

`.claimed_leaves` lists a pull drain's launched leaves that are still running (the Claimed leaves: lines), each with its `stage`: `implementing`, or `reviewing` once it is in the before-landing review of its open PR, when the drain admits a replacement beside it, up to its concurrency; it is empty for every other run;

`.refused_settlements` (the Settlement refused: lines) lists recorded outcomes the owner refused while still holding their claims, each with the owner's `reason`, `refusals`, `retry_after` and `remedy`: the drain requests no new claim while one is held, retries it with backoff (at most every 15 minutes), and `orbit run auto --stop` retries it at once;

`.crew_window` (the Crews: lines) is a pull drain's runnable crews and the crews it excluded for its window, each with `source` (`preflight`, `provider_unavailable`, `leaf_released` or `provider_limit`) and `reason`, and `auth_exclusions` (provider, host, excluded_at, error_class, relogin_hint, credential_source and next_probe_at). Declared auth probes may re-admit these crews in the same window. A `provider_limit` exclusion, and a `provider_unavailable` one for model capacity (a backoff of 10 minutes that doubles with each repeat in the window, up to an hour), carry `until` (the Crews: line reads `(<source> until <time>)`) and lift by themselves then, in the same window; other exclusions, provider authentication among them, last until the next drain. It is null for every other run; a pull drain being cancelled gracefully stays `running` with `.run.drain_cancel` set (the Cancelling: line) until those leaves finish and settle. An Agent: line marked `blocked=stopped-descendant` (`.provider_processes[].blocked_on_stopped_descendant`) is a live agent waiting on a descendant that has stayed stopped past the supervisor's threshold and is still stopped;

`.stopped_descendants` lists every such descendant the supervisor reported, and whether it ended it.

Step records expose `lock_wait_ms` for file and store mutex acquisition inside the synchronous leaf. Positive waits appear in the step table and detail; historical steps have null timing. Child-process waits are excluded.

## Check the agent subprocess before cancelling

Two separate execution channels spawn agents, and they do not share a run store:

| Channel | Spawned by | Observable through |
| --- | --- | --- |
| `agent_invoke` | the Worker daemon | bridge `agent_run_list` / `agent_run_status` |
| ship pipeline (`workflow_ship`) `agent_implement` | the pipeline worker process | this runbook's surfaces below |

**Bridge `agent_run_list` does not cover the ship pipeline.** A `workflow_ship` implementation
agent is a direct child of the pipeline worker, so `agent_run_list` reports no matching run and
`busy=false` for a perfectly healthy one. Do not read that as a lost child [ORB-10496].

Ask the pipeline instead. `orbit run show` prints one `Agent:` line per provider subprocess that
has not reported an exit. `orbit run show --json` and `GET /api/runs/:id` both project those
children with `RunProviderProcess::to_json`, so the objects share keys — including `last_activity_at`, `latest_message`, `stopped_descendants`, and
`blocked_on_stopped_descendant` — and omit `run_id`, which the caller already has:

```text
$ orbit run show jrun-20260727-0241-2
Agent: provider=codex pid=154953 step=agent_implement liveness=alive started_at=2026-07-27T02:41:02+00:00
```

- `liveness=alive` — the child is still there and is the same process that was recorded.
  A step that has been running a long time with a live agent is working, not stuck.
- `liveness=exited` — the child is gone (or its PID was recycled by an unrelated process)
  while the step never recorded an exit. This is the lost-child case worth acting on.
- `liveness=unknown` — the host cannot probe liveness. Never read this as dead.
- `liveness=alive blocked=stopped-descendant` — the child is alive but waiting on a
  descendant that has stayed stopped (state `T`) past the supervisor's threshold and is still
  stopped, because the supervisor could not end it. The `stopped descendant:` line under it
  names the pid, command, how long it was stopped and why it was not ended. The agent makes no
  progress until that process goes: `kill -KILL <pid>` it (never the agent pid), after checking
  that the pid still names that command.

A descendant that stops itself — an interactive shell probe such as `bash -i` in a sandbox
without a terminal, or `kill -STOP $$` — would otherwise hold its parent's wait until the
activity's wall clock ends. The agent supervisor samples the child's process tree, and ends
with `SIGKILL` any descendant that stayed stopped for the whole threshold (10 minutes by
default; `ORBIT_STOPPED_DESCENDANT_THRESHOLD_MS` in the worker's environment overrides it).
It never ends the agent itself, anything while the agent is stopped too (an operator stopped
the whole group), a running or merely idle process, or a process outside the agent's tree.
Each one it ends is recorded as a `cli.invocation.stopped_descendant` event and printed under
the `Agent:` line as `stopped descendant: ... ended by the supervisor`; nested commands
supervised by `orbit-exec` (`proc.spawn`, host shell steps) append the same note to their
stderr. The watch reads `/proc`, so it is Linux only: on macOS a stopped descendant still
holds the agent until its wall clock ends, and `blocked=` is never shown there.

Liveness is probed when you ask, against the local process table, so it is only meaningful on
the host that ran the child; a historical run inspected elsewhere may report `exited` or
`unknown` depending on what that host can observe. `orbit run show --json` and
`GET /api/runs/:id` both return those full process records, and both put `input`, `pid`, and
`pid_start_time` on the run object. `orbit run events <run_id> --type cli.invocation.process`
still returns the raw audit events. An unreadable pipeline checkpoint does not fail
`orbit run show`: the command warns and reports `pipeline_state` as null, and the run record
stays readable. The HTTP detail already omits that section the same way.

## A submitted run outlives its command

`orbit run job <job_id>` / `orbit job run <job_id>` submit the run to a detached worker
and return as soon as it is durable [ORB-10801]. A zero exit means validation,
persistence, and worker startup succeeded — it says nothing about the run's eventual
outcome, which is why every submission prints the run id and the inspection commands
above. Add `--wait` to block on the run instead; that mode exits nonzero for any
non-success outcome, including a `held` run, and prints the diagnostic.

A submission that reports `queued` is durable but has not started: the job is already at
its `max_active_runs` limit and its worker is polling for a slot.

An unknown job or invalid asset fails during submission without leaving a startable run. If a
run was persisted but its detached worker could not start, Orbit terminalizes it as
`interrupted` with a startup diagnostic and worker-log path.

## Interpret run states

`pending → running → success | failed | timeout | cancelled | interrupted | held`
(plus transient `retrying` and step-level `skipped`). A `held` run is resumable through its
specific recovery path; it is not an ordinary failed terminal run.

`interrupted` means the run was orphaned: its owner process died through a crash, SIGKILL,
or reboot without finalizing the run. A job that genuinely failed is `failed`;
`interrupted` means the worker died.

Setup errors after a run starts, including a replay input naming a crew that no longer
exists, finalize the run as `failed` with the setup diagnostic. Once execution succeeds,
failures saving the final pipeline snapshot or step summary are logged as best-effort
bookkeeping and the run still reaches `success`; its summary may be incomplete. The
terminal-state write remains required.

## Understand orphan reconciliation

Newly submitted runs record their owner `pid` plus a pid-start-time token. Pipeline workers
claim their queued run at startup, so `pending` runs may carry an owner too [ORB-10070]. A
reconcile pass probes the owner and any recorded provider child, then finalizes conclusively
orphaned runs to `interrupted`, releasing their task reservations:

- `running` runs with a dead owner and no live or unverifiable provider child;
- `pending` runs whose claimed worker died and have no live or unverifiable provider child; and
- `pending` runs never claimed within a 30-minute grace window, such as queued children
  stranded when their parent run was interrupted by a reboot.

The parent-side worker observer applies the same provider guard after an unexpected
worker exit. While a recorded provider is alive or unverifiable, the run stays
nonterminal and keeps its reservations. The observer retries once per second and
retains the original exit status and worker-log diagnostic until it can finalize.
It does not kill surviving providers. If the observer itself stops, lazy orphan
reconciliation remains available with its stale-owner diagnostic.

### How the worker ended

Only the observer, the worker's parent, can reap its exit status. It records a
`pipeline.worker.exited` audit row (`orbit audit list`) as soon as it reaps the worker,
before it settles the run: `exit_code` or `signal` and `signal_name`, `core_dumped`, and
on Linux the worker scope's `memory.events` `oom_kill` and `pids.events` `max` counters
while its cgroup still exists. On Linux an unreaped worker keeps its start identity, so
reconciliation cannot condemn a run while its observer is alive and has not reaped the
worker; once it has, the two race to settle the run. A run interrupted with
`reason=process_not_found` or `token_mismatch` then ends its diagnostic with a
`worker exit:` clause:

- `its supervisor reaped pid <pid> at <time>: killed by signal 9 (SIGKILL), sender
  unknown: …` (or `exited with code <n>`): the observer recorded the exit before
  reconciliation won.
- `no supervisor observed or recorded its exit status; …`: nobody had recorded an exit.
  The launching process had exited, so the init process or the user manager reaped the
  worker, or reconciliation won the race. What follows is what the worker's scope, recorded
  when the worker claimed the run, still reports: its systemd unit `state=…, result=…` and
  counters, or `worker scope '<unit>' was already gone` with the `journalctl --user -u <unit>`
  command that still has its end. A worker that ran in no scope (macOS, containment off, no
  user manager) reads `no worker scope evidence is available`; nothing is inferred.
- `…; afterwards its supervisor reaped pid <pid> at <time>: …`: reconciliation won the
  race to settle the run after the observer had already reaped the worker. The observer
  can record that exit afterward. `finished_at` is the run's last audit activity, not
  the interruption time. The displayed reap-observation time is stamped when the
  observer records the reaped status, so it can be later than reconciliation's
  `Interrupted` write. That later timestamp does not mean the worker was still running
  when the run was interrupted.

```text
job run marked interrupted because recorded worker process is no longer alive (reason=process_not_found, pid=4242, pid_start_time=…); worker exit: no supervisor observed or recorded its exit status; worker scope 'orbit-worker-jrun-20261019-1515-c3-1a2b3c4d.scope' was already gone: no unit result or counters remain (see `journalctl --user -u orbit-worker-jrun-20261019-1515-c3-1a2b3c4d.scope`)
```

The clause adds evidence only; it changes neither which runs are interrupted nor when.

#### Who sent the signal

A signalled exit also names its sender. On Linux a worker catches SIGTERM, SIGINT and
SIGHUP for its whole life (unless it inherited them ignored or another handler owns
them). When another process sends one, the worker writes that process's pid, uid and
command name to its log and then dies from the same signal, so the reaped status and
the run's classification are unchanged. The observer reads the line before it records
the exit. The `pipeline.worker.exited` row carries it as `signal_sender` (`pid`, `uid`,
and `command`, which is `null` if the sender had already exited). The diagnostic says
`sent by pid <pid> (uid <uid>, command '<name>')`, or `command unreadable`. When the observer settles the run
itself, that clause follows the status:

```text
pipeline worker for run '…' exited with status signal: 15 (SIGTERM) (sent by pid 812 (uid 1000, command 'systemd')) after claiming the persisted run …
```

Look the pid up in `journalctl --user` or `journalctl -b` around the exit time to find
the unit or tool behind it. A signalled exit with no captured sender reads `sender
unknown: the worker captured none`; nothing is guessed. That covers SIGKILL, which no
process can catch, a signal the kernel generated, and any worker on macOS. It also
covers a signal that arrived before the worker installed its handler, or while a
supervised step was intercepting signals for its own children. Orbit forwards such a
signal itself, so the original sender is lost. An authorized `orbit run cancel` still
records the run `cancelled`; when its SIGTERM ended the worker, the worker's
`pipeline.worker.exited` row names the cancelling process as the sender.

For a claimed follower leaf, reconciliation samples this evidence immediately before
finalization records its claim release in the durable outbox. The owner receives that
sample in the release reason, with the `worker exit:` clause placed first so the
1024-byte reason bound retains the cause (or the explicit absence of an observed
status). Remaining scope and liveness detail is retained within that bound. Run
diagnostics can include a newer observation read after the interrupted write, or an
`afterwards` observation appended by the supervisor. Those later observations do not
replace the recorded release, even if delivery is still pending: retries send the
same settlement, preserve its failure class and apply it once at the owner. An
unavailable status in the release therefore describes what was known at the
settlement sample, not proof that no supervisor ever recorded an exit. Consult the
follower's run diagnostic and audit trail for evidence recorded later.

A process running a different executable than the one recorded for a live Orbit root (for
example, right after the binary was replaced while workers from the previous build are still
running) opens the state write-free. It can read every run record but finalizes and repairs
none of them, so `run history` and `run show` report stored records as they are rather than
failing; the next process that can write records reconciles them.

Reconciliation runs best-effort at workspace open and lazily on
`orbit run history` / `show` / `logs` / `events`. Pass `--no-reconcile` to any of those four
to read stored run records without finalizing anything; the run-failure and backlog-hygiene
scans do this because they promise not to mutate run state. `orbit doctor` reports orphans
read-only. A run whose PID is alive but unverifiable is deliberately left alone.

Nested sandboxed activity commands are not a liveness authority for their host worker. When an
activity child carries truthy `ORBIT_MANAGED_RUN_CONTEXT` and a non-blank `ORBIT_RUN_ID`, its
workspace open deliberately skips the opportunistic orphan scan: Bubblewrap may place it in a
private PID namespace where the live host worker appears absent. Investigate and reconcile from
the host's top-level Orbit process instead. Do not remove the private PID namespace, enable a
bare fallback, or treat the child's inability to see the worker as evidence of an orphan.

Example after a worker was SIGKILLed mid-step:

```text
$ orbit run history --limit 1
RUN_ID                 ROLE       JOB_ID           ATTEMPT  STATE        ERROR
jrun-20260704-0927-t2  top-level  demo_sleep_long  1        interrupted  job run marked interrupted because recorded worker proces...
```

`ERROR` is the last column. It is present only when a listed run failed, was held, was interrupted,
or timed out with a recorded root cause or error, and each such row shows that text on one line,
capped at 60 characters. A run that echoes a root cause shows `root cause <leaf run>` instead of its own
error; `--json` carries the full records.

`ROLE` reads the run id itself. A directly submitted run and another run's child land in the
same minute stem, so the marked sequence — `-t2` for the second top-level submission of that
minute, `-c2` for a second child — is what keeps sibling runs from reading as one run tree.
A sequence is never handed out twice: archiving or deleting a run keeps its id reserved, so an
automation key, audit row, or parent dispatch that still names it never resolves to a later run.
Ids minted before role markers existed report `unmarked`; use `orbit run show <parent>`, which
names each child it dispatched, to establish their lineage.

For a failed pipeline, `orbit run show <top-level-run-id>` follows those child dispatches and
prints a `Root cause:` line with the failed leaf run ID, step, and complete error. If separate
branches failed, `Additional root cause:` lines list the other failed leaves in child dispatch
order. The parent and child wrapper errors remain in the usual run header and step records.
`orbit run show <top-level-run-id> --json` exposes the same details in `root_cause` and
`additional_root_causes`; `step` or `message` is null when a failed leaf has no corresponding
step detail. The compact child-dispatch lines can still shorten wrapper errors.

A gate or auto parent that failed only because a child did also records that leaf on itself. Its
`--json` `root_cause` is `{leaf_run_id, task_id, step, code}`, and a nested parent inherits the
leaf its child recorded. Its error names that leaf ("echoes leaf run …") rather than repeating the
leaf's text. Codes come from recorded error codes or the terminal run state,
never from command output. Required-validation messages contain a bounded
summary and task artifact paths for the candidate and base logs; read those
artifacts for the captured output. A run that failed on its own has `root_cause: null`. So one failed delivery no longer
reads as three failures. `orbit run history --state failed --incidents` shows one row per
incident, the leaf when it is listed, and the CASCADED column (`--json`: `cascaded_run_ids`)
counts the parents folded into it. `orbit run history --failures` folds further, to one row
per task and root cause: `RUN_ID` is the root-cause run even when that run is outside the
page, `STEP` and `CREW` are that run's, `CAUSE` is its error's first line with no
60-character cap, and `SHOW` is `orbit run show` for that run. `STEP` comes from
the root's failure checkpoint or audit trail, with stored activity steps and
the echo's recorded root-cause step as fallbacks. A job-level diagnostic names
the pipeline, so it never supplies `STEP`; without step evidence, the cell is
`-` (`null` in JSON). `--json` is
`{"failures":[...]}`. It cannot be combined with `--incidents`. A parent whose waited-on
children were all cancelled ends `cancelled`, not `failed`.

The dashboard's run detail follows that same child-dispatch chain in one response.
The failure box links every run id and task id in the message and adds a Root cause
line for the deepest failed or cancelled descendant. Opening a cancelled run names
who cancelled it, when, and the recorded reason, or says `no reason recorded`.

An auto drain dispatches its leaves detached and never observes their outcomes, so the drain's
own `State: success` only means the coordinator ran. `orbit run show <drain-run-id>` adds a
`Leaves:` line (admitted, succeeded, failed, running, cancelled), a `WARNING:` with each failed
leaf, and a `Still waiting:` block naming the backlog tasks the drain's last pass never started
and what blocked them. The failed-leaf line names the root-cause run (the same id a retry would
resume), the task, that task's current status, the leaf's state, and its error. The retry line
is printed only while a named task is still `blocked` or its status cannot be read; a task that
was readmitted or completed no longer gets `orbit job resume` advice. `--json` carries the same
in `drain_summary` (null for a run that is not a drain): `run_id` stays the dispatched leaf and
`resume_run_id` is the root-cause run. The `Resume:` line on a failed run's own header is
unchanged: it names the child that did the work when the run being shown is itself resumable.

A run parked on task locks shows `Waiting on locks: file:src/lib.rs (held by ORB-1)`. The run
persists only the selectors; `orbit run show` resolves the holder from the live lock projection
when it renders (`--json`: `waiting_on_lock_holders`), so a holder that has since released drops
off the line. `orbit run readiness` names each conflicting holder as
`blocked-by=<task> (<status>) on <first file> +N more`, with additional files counted
per holder. Unfinished dependencies read `waits-on <task> (<status>)`. The drain's
`Still waiting:` block uses the same blocker wording and the `unmet_dependency`
text reason; its last-pass records carry blocker IDs but no filenames. Holder and
recorded-blocker statuses come from the current task-status projection; if a task
is unavailable, its ID still appears. JSON retains its existing payload and reason
codes, including `dependency_not_done` in drain records.

A failed, timed-out, or interrupted run ordinarily leaves its task `blocked`, which automation
skips. Typed holds use their own task states: a baseline-red or provider-failure hold returns the
task to `backlog`, while a forge hold keeps it `in-progress` as described below. An operator
cancellation returns its task to `backlog` with the cancellation reason, preserving the candidate
for a later resume. Pass `--block` to `orbit run cancel` when the existing manual recovery flow is
preferred; the dashboard cancel uses the backlog default. `orbit task show
<task-id>`, local or routed to the host the id names, prints a `Next:` line for a blocked task:
the exact `orbit job resume <run-id>` when that run is resumable (`failed`, `timeout`,
`interrupted`), or the `orbit task update <task-id> --status backlog` command for other blocks.
`--json`, `orbit.task.show` and `GET /api/tasks/:id` carry the same object in `next_step`; it is
absent for a task that is not blocked by a run. The same show prints `Final Recovery:` when a
final-recovery decision is recorded, and those JSON surfaces carry `final_recovery` with
`run_id`, `decision`, `outcome` and `at`. A task list does not include these show-only fields.

For a blocked task, those same JSON surfaces also carry `block`: the latest
block or escalation's `reason`, `at`, classified `cause`, `run_id`,
`comment_index` (zero-based into `comments`) and recorded `next_action`.
Missing evidence is `null`; other statuses omit `block`. A final-recovery
escalation can update the explanation while the task remains blocked. It
supersedes the earlier run failure, but a later new block never reuses that
old escalation. Escalations offer requeue guidance rather than automatic resume.

The dashboard shows this explanation near the status in both the dock and full
page, including the recorded time, next step and a link to the escalation
comment or run. When no reason or time was recorded, it says so explicitly.
Long reasons are summarized in the callout; open the evidence for the full text.

The same task page shows a read-only Why stuck card for a blocked task and for
a task whose latest delivery run is held. The card lists the escalation note,
the landing-review verdict with its finding count, the latest run's root-cause
line, whether the pull request is mergeable or conflicting, and each required
check by name and state. A fact that cannot be read, including a pull request
with no open request or a forge that cannot answer, says unavailable instead of
leaving the row out. The card adds no action.

## Cancel a conclusively stuck run

After verifying that the owner is gone or that the run should no longer continue:

```sh
orbit run cancel <run_id> --confirm --reason "operator stopped this delivery"
orbit run cancel <run_id> --confirm --block --reason "hold for diagnosis"
```

Use `--block` only when the task should wait for manual recovery instead of
returning to the backlog with its candidate preserved. The choice reaches every task
the cancel stops: leaves a `--force` pull-drain cancel stops and children a cancel
cascades to follow it. A forced pull-drain leaf's claim is the exception: it is always
released to the owner's backlog, with or without `--block`.

This terminalizes the run on demand. Do not cancel solely because a legitimate step has
been `running` longer than expected.

Cancellation signals the run's owner process group only when the owner leads that
group. A foreground owner that shares its caller's group is signalled by PID alone,
and cancellation verifies only that owner's exit, leaving the caller and siblings
alive. It then stops every agent process the
run's audit trail still shows open (`provider processes stopped: N` counts the requested
run's own agents; each cascaded child run stops its own). Agents run in their
own process groups, so the owner signal alone never reaches them. A process is signalled
only while its recorded start token still matches, so a recycled pid is left alone; one
whose identity cannot be verified is never signalled. Unreadable provider audit
evidence, unknown provider liveness, an unverified identity, or an agent surviving
SIGKILL makes cancellation fail and keeps the run non-terminal with its reservation
and claim retained, even if the owner already stopped. Restore readable evidence or
confirm the provider has exited, then retry cancellation.

`--reason` is optional. The CLI and dashboard record a `run.cancelled` event with the
actor and any reason; inspect it with `orbit run events <run_id> --type run.cancelled`.

Cancellation is truthful about the race with completion: a run that reached a terminal
state before the signal landed reports `already_terminal` and keeps its real outcome
rather than being overwritten with `cancelled`.

### What `orbit run cancel` does

Cancels a job run that has not reached a terminal state: signals the owner process of a running run (TERM then KILL), stops the agent processes the run still has open, releases the run's task reservations, and finalizes the run as `cancelled`. The primary remediation for a stuck `pending` run with no live worker (orphan reconciliation also clears those on workspace open). A run that already finished returns a stable `already_terminal` result without replacing its outcome.

A running follower pull drain (`workspace_pull_pipeline`) is cancelled gracefully: it stops requesting work at once, returns the claims it had not launched to the owner's backlog, and keeps running — reported as `cancelling`, with the leaves it is waiting for — until every launched leaf has finished and its outcome reached the owner; then it ends `cancelled`. The command returns immediately; `orbit run show <run_id>` follows the wait. `--force` does not wait: it stops the drain and each of its running leaves, and returns every claim to the owner's backlog with a comment naming the drain and the reason. It touches only the leaves that drain carries. A leaf whose stop cannot be confirmed keeps its claim on the owner, is listed, and makes the command exit 1. A queued drain, or one whose worker is gone, is cancelled at once. Cancelling a drain that already ended delivers whatever it left behind, and `--force` also stops leaves it left running.

`--force` on a local auto drain (`workspace_auto_pipeline`) also cancels the task runs it started, which a plain cancel leaves running. A child whose stop cannot be confirmed is listed with its reason and makes the command exit 1. For any other run it changes nothing.

Cancelling a task leaf returns its task to the backlog with the reason and keeps the candidate available to resume. `--block` keeps the task blocked for the existing manual recovery flow, including for the leaves and children a `--force` or cascading cancel stops; a pull drain's released claims always return to the owner's backlog.

## Operator agent invocations

A run of `agent_invoke_pipeline` is one operator invocation of an agent for exploration
or debugging (`orbit run agent <prompt>`, or the `orbit.agent.invoke` tool). It is a
single-step run that creates no worktree, has no task ownership or delivery tail, so most of
this runbook's task-recovery steps do not apply to one.

`--cwd` may name the registered primary checkout, a directory inside it, or an
existing linked worktree root beneath the workspace's `.orbit/state/worktrees/`.
For a linked root, governed submission derives the assigned checkout identity
from its Git common directory and carries matching internal `workspace_path`
and `repo_root` fields to the activity. The admission and audit name that linked
checkout; the provider starts there. The primary remains the registered owner,
not the assigned `repo_root`. Unrelated repositories and mismatched pairs fail
before provider startup. Primary interior directories retain their requested
cwd without declaring a linked-worktree pair.

Four differences matter when triaging one:

- **It is unsandboxed by design.** The provider subprocess runs on the host as the same
  operating-system user as Orbit, admitted per invocation by an operator. A sandbox
  denial is never the explanation for one failing, and the admission is recorded as a
  `trusted_host.execution_admitted` audit event naming the authorizing operator, the
  workspace, and the working directory. Remote admissions also retain the
  destination-resolved caller machine ID, invocation mode, and actual identity proof:
  strict grants are `key-bound`; explicitly cooperative same-OS-account SSH grants are
  `cooperative` and `self-asserted`.
- **Cancel it the ordinary way.** `orbit run cancel <run_id> --confirm` signals the owner process
  (TERM then KILL) and terminates the process tree, exactly as for any other run.
- **It cannot be resumed.** `orbit job resume` and `submit_resume_run` refuse a run that
  carries an admission, because that admission covered one invocation and a resume would
  reuse it without a new authorization. Submit a fresh invocation instead. `orbit job
  replay` is likewise not a workaround: the replayed input carries no admission, so the
  activity fails closed.
- **A retry key resolves the original run.** Reusing `--idempotency-key` returns that
  run's persisted admission, timeout, provider sandbox, and sandbox warning, even if
  the retry names different settings or the workspace default has changed. Use a new
  key to request a new invocation.

Read the outcome with `orbit run show <run_id>` — its `Invocation:` line distinguishes a
completed answer from a mid-turn stop, a timeout, and a cancellation — and
`orbit run logs <run_id>` for the full captured output.

Submission warnings in `orbit run agent` are scrubbed with the shared secret
redactor before appearing in wait progress, human output, or the JSON `warnings`
array. Queue and provider sandbox warnings remain visible with sensitive values
replaced by redaction markers.

## Read-only explorer runs

A run of `agent_explore_pipeline` is a read-only explorer that an implement or review-repair
agent delegated through `orbit.agent.invoke` (see
[CONFIG.md](../CONFIG.md#workflowexplore--read-only-exploration-for-implementing-agents)).
It is a single-step run in its caller's worktree with no task ownership; its input names
the caller as `parent_run_id`. A stuck explorer only holds one of its caller's
`workflow.explore.max_concurrent_per_run` slots: cancel it with `orbit run cancel <RUN_ID>`,
and the calling agent continues without the answer. Never resume or resubmit one by hand.

## Resume from checkpoints

The v2 executor checkpoints every completed top-level step into
`job_run_states.pipeline_state_json` (one row per run, beside `job_runs`) in
`~/.orbit/orbit.db`; there is no separate checkpoint file. Resume accepts runs in `interrupted`, `failed`, or `timeout`, and a `held` run whose push the
forge refused ([A delivery held for the forge](#a-delivery-held-for-the-forge)). Any
other state errors with `resume requires an interrupted, failed, or timed-out run`.

```sh
orbit job resume <run_id>
```

The command submits a detached worker and returns the new run ID. The worker
continues after the CLI exits. Use `orbit run show <new_run_id>` to inspect it,
or add `--wait` to block until it finishes and receive a nonzero exit status
if the run does not succeed.

Resume the run that did the failed work, not the wrapper around it. A ship or drain
coordinator fails only because a child run failed; resuming it re-checks that same failed
child result and fails again. `orbit run show <coordinator>` prints a `Resume:` line naming
the failed leaf whenever one can be resumed (`orbit job resume <leaf>`); the leaf carries
the worktree and checkpoints, and on success it hands the task to `review` like the
original run would have.

A resume reuses the worktree its checkpoints recorded. When that worktree no longer
exists, the resume is refused before the task or the run lineage changes, since the steps
it would rerun cannot start there. A blocked task whose run published a pull request is
requeued instead (`orbit task requeue`); its next run sets up a fresh worktree on the
published branch.

A **claimed** leaf cannot use this path, whether its claim is still live or
has settled. Resume would mint a different run that cannot inherit the
claim/run binding, so the command refuses and names deliberate recovery.
Inspect the claim on the owner with
`ORBIT_OPERATOR=1 orbit tool run orbit.drain.claims`, reconcile any uncertain
merge, and do not ship the same task again. See
[distributed-drain setup](./distributed-drain.md).

When final recovery of a claimed leaf escalated an exhausted review budget,
the task comment names the owner host and the exact `orbit task requeue`
command, with `--root` (the owner's Orbit data directory) and `--workspace`
(the registered workspace), so it selects the checkout from any working
directory. Run it on the owner. The task's next claim continues the candidate
the failed claim kept, under a new admission with its own review budget.

Resume starts a new linked run with `attempt + 1` and `retry_source_run_id` set.
Checkpointed steps are skipped and their outputs are replayed into the pipeline.

A run submitted from a direct job YAML file keeps that definition. Resume copies
the snapshot stored beside the source run onto the new run, and a later resume
of that new run copies it again. Deleting the original file, or installing a
catalog job with the same name, does not change which definition the resumed
run executes. A catalog-backed run still resolves its job name from the catalog.

```text
$ orbit job resume jrun-20260704-0927-2
$ orbit run events jrun-20260704-0928
│ 2026-07-04T09:28:15Z   -         run.started         cli:demo_sleep_long                                   │
│ 2026-07-04T09:28:15Z   nap_one   step.skipped        step=nap_one reason=resume: step already completed    │
│                                                      in checkpointed run (index 0)                         │
│ 2026-07-04T09:28:15Z   nap_two   step.started        nap_two                                               │
│ 2026-07-04T09:28:17Z   run.finished        success                                                         │
```

For PR workflows, the reused worktree checkpoint keeps the original run as
the task/candidate owner. Delivery and terminal failure handoff accept that
owner only when the active run's durable `retry_source_run_id` chain reaches
the checkpoint owner. Direct ownership still authorizes the original run.
An unrelated run, a broken lineage, or a task re-claimed by a superseding run
fails before Orbit commits, pushes, or updates the task.

A resumed run's own final recovery can commit a repair and resume the commit
step. Its receipt (exact before/after HEAD and worktree) is recorded in the
resumed run's state, not the owner's. The commit step accepts that HEAD only
when the receipt was recorded on a bounded same-job `retry_source_run_id` chain
from the executing run to the checkpoint owner, and the task is still bound to
that owner. Preparation, synchronization, native validation and configured
review then run again on the repaired candidate, on the same branch and PR.
A descendant of the repair, a copied receipt, or a run outside the lineage is
refused with `worktree_head_changed`. A task bound to any other run is refused
before the commit as well.

Resume also refuses before it creates a run when a task the source names is
bound to a different run, or to the same run id on another machine. The error
names that binding (run id and machine). Checkpoints, the worktree, and the
validation input are not reused, and the foreign claim is left as it is.
Continue the binding on the machine it names, or admit a new attempt after an
authorized rebind. `orbit job resume` does not rebind a foreign claim. A task
still bound to the source run, or to the checkpoint batch in its retry
lineage, resumes as before: the new run keeps that batch id in its checkpoints.

When a completion attempt was blocked after promotion, resume restores `review`
only when its reused host promotion checkpoint names the task and its latest
status history proves that this source run or an ancestor blocked it from
`review`. The checkpoint must be in the reused successful prefix; it remains
valid when a later non-completion step fails or the scan reaches the end of the
job. This includes re-review steps after PR completion. The submitted run must
carry `completion: done`, and the task must still belong to that retry lineage
and the same PR. The restoration records `resume_review_restored`, the
source and resumed run IDs, and the blocking run in task history. Repeating the
resume while the task is already in review adds no restoration event.

Early implementation retries still restore `in-progress`. A block the same
lineage wrote through its failure handoff (`pr_failure_handoff`,
`pr_conflict_blocked`, `validation_environment_blocked`,
`review_gate_escalation`) is readmitted the same way, including back to
`review` when the checkpoint rules above hold. An implementer-declared
blocker (`task_blocked_by_agent`) is not in that list: resume leaves the
task blocked, and an operator moves it back once the blocker is gone.
Missing stage evidence,
an unrelated or superseding attempt, a manual block, and withdrawn or terminal
states cannot gain review through this repair. A merged PR alone is not review
authority: normal candidate, merge and task-completion guards still run after
restoration. Source checkpoints and prior task history remain unchanged.

A lineage has at most one live resume. While any run in the source's retry lineage
(its `retry_source_run_id` ancestors and everything descended from them) is `pending`,
`running`, or `retrying`, another resume of any member is refused on every surface — CLI,
MCP, and dashboard — with `already has a live resume in its retry lineage (<run_id>)`
(HTTP 409 / code `resume_run_in_flight`, with that `run_id` in the payload). All lineage
runs share one worktree, so a second one would edit it concurrently. The refusal is
atomic: concurrent requests, even from different processes, create exactly one run.
Watch or cancel the named run (`orbit run show <run_id>`, `orbit run cancel <run_id> --confirm`);
once it is terminal, resuming is allowed again. A later resume chains from the run you
name, using that run's checkpoints and `attempt + 1`. To continue from further along,
resume the latest attempt instead of the original source. A lineage run stuck as
`running` after its worker died (for example after a host reboot) is reconciled to
`interrupted` before the check, so it does not block recovery.

Resume first uses the run's pinned definition snapshot, when one was recorded. A run
started from a raw YAML path does not need that job registered under `resources/jobs/`,
and can resume after the original YAML file is gone. Only a run without a recorded
definition snapshot needs its job in the catalog (`orbit job list --all`). Referenced
activities still resolve through the current activity catalog.
A run with no successful checkpoints degrades to a full replay.

## An unchanged published completion observation

A transient read-only completion lookup can spend the run's one final-recovery
attempt by retrying completion in the same worker. It keeps the published commit
SHA, PR and successful checkpoints; it does not reimplement, recommit or repush the
candidate, or restart hosted CI. Final recovery must be enabled for the run.

An eligible lookup is one of completion's authenticated reads made before any
mutation: the PR status, required-check and merge-policy reads, and the
base-refresh and missing-prerequisite evidence reads, such as a commit's combined
status. Only a transport failure qualifies: an HTTP 5xx answer, a GraphQL
timeout, or a TLS handshake timeout, including a REST call that never reached
GraphQL. Invalid or truncated evidence, a changed head, base, policy or
identity, a credential, permission, administrative-rule or other permanent
refusal, and an exhausted completion deadline keep their ordinary failure. A
failed read never counts as passing required checks; the resumed completion
reads them again.

The host first authenticates the successful commit, preparation, synchronization,
push and PR chain, exact clean checkout on the published head and branch,
current task ownership and remote branch lease. A base that advanced after
publication does not refuse reuse: the receipts stay bound to the base they
judged, and the resumed completion applies its own base-freshness and
required-check handling to the published head. Native validation must have
captured the same task meaning, command list, resolved job, effective
validation environment and review policy. Every command needs its complete
successful host-written log and matching artifact hash. A workspace with no
`workflow.required_validation_commands` has no log to check; its
`skipped_no_required_commands` capture must still carry the same effective
policy. A missing capture, deferred result, known-red result or damaged receipt
grants no reuse. A configured review must have an unchanged successful
settlement and matching host certificate.

This continuation is limited to a failed observation before any merge, automatic
merge, post-publication rebase or repair mutation. A `delivery_preparation`
checkpoint from a `pr_prepare` step before the first push does not exclude reuse:
the published head and validation receipts must still match the completion
binding. Enabling automatic source repair does not exclude an
otherwise unchanged original publication. The host checks the existing
repair admissions/publications, source-repair, infrastructure and base-update
reservations, including pending or unreadable records. Any such task history
refuses this narrow continuation, even if the tree or head is unchanged. Merge,
auto-merge and local rebase attempts record write-ahead mutation provenance;
unknown replies keep it. Retry descendants, other rebase recovery checkpoints,
reviewer fixes and a `failure_activity_checkpoint` require governed recovery.
Refusals identify mutation provenance, the checkpoint step, the failure-activity
checkpoint or the reviewer repair that prevented reuse. Native policy captures
from before this provenance contract cannot authorize reuse.

The retry waits 10 seconds before completion reads the PR again. Current hosted
checks, merge policy, PR identity and merge state are read again by ordinary
completion, which merges only the published head. Unknown mutation replies
retain the existing reconciliation path; they are not classified as read-only
failures. The original completion deadline and final-recovery charge survive
retries and checkpoint resumes, so a run retries once. A second failed
observation runs the failure handoff, which keeps the PR and the task in review
and records the failure on the task.

Changed task meaning, policy, candidate, review repairs, publication, ownership,
lease or a dirty checkout refuses reuse, blocks the task with the refusal as
its diagnosis and preserves the publication for explicit governed source
repair. The agent cannot override that refusal by requesting the
same stale prefix. A stopped or foreign execution cannot acquire an active
worker's continuation
through this check. No successful receipt is rewritten and no replacement PR is
created by the continuation.

## Requeue a failure-handoff task onto its published candidate

A run whose failure handoff published a `[BLOCKED]` pull request leaves the
task `blocked`. When the candidate is sound (the failure was the environment,
not the work), requeue it as an operator decision:

```bash
orbit task requeue <task-id> --reason '<why this candidate can continue>'
```

The MCP equivalent is `orbit.task.requeue` with `id`, `reason` and optional
`discard_candidate`; it needs operator authority and is unavailable inside a
managed run. Every return from `blocked` to `backlog` is authenticated the
same way: a plain `orbit task update --status backlog` (and the same move
through MCP, HTTP or the dashboard) applies the requeue publication
pre-flight in the shared core operation before writing. A publication a
requeue would refuse (a specification changed after publication, or a pull
request or preserved candidate no handoff receipt binds) refuses the plain
update with the same message and discard/explicit-source-repair guidance,
and the task stays `blocked` with nothing written. A publication a requeue
would accept is returned to `backlog` with the same `requeued` decision and
publication-preservation record a requeue writes, so its next run resumes
it; with no publication on record the task simply returns to `backlog`. A
plain update with `--discard-candidate` still discards the candidate and
closes the task's undelivered Orbit pull requests.

The requeue waits for the task's run to stop. Final recovery blocks the task
before the run's failure handoff publishes, so while the run is live the
requeue is refused and nothing changes; requeue once it has stopped. A claimed
task's run is a follower's leaf that the owner cannot probe, so while its
execution claim is unsettled the requeue is refused. The refusal identifies
the live linked run, live PR pipeline run, or unsettled execution claim and
its run ID. Run IDs are unique only within a machine's store: the task's
`job_run_machine` qualifies its `job_run_id`. When that binding names another
machine, the owner uses the claim's settlement and never a same-ID local
run, failure-handoff receipt, or publication checkpoint. Inspect the leaf on
its recorded execution host; a bare `orbit run show <run-id>` reads the
selected host's store. A settled claim fences the follower leaf, so an
unrelated same-ID owner run does not prevent requeue.

For an owner-local publication, requeue authenticates the handoff receipt
before taking the task's write lock: the
stopped run is this machine's own single-task delivery, conclusively stopped
and still the task's newest execution, its candidate was not discarded, and
the task specification is unchanged. Under the lock it re-checks that the task
is still blocked and unchanged, records a `requeued` decision that supersedes
the escalation, plus the publication-preservation record, and returns the task
to `backlog`. The decision is written only with that record: a discard, or a
requeue with nothing published, leaves the run's escalation as its recorded
outcome. Both records are written by `system`, because the next run trusts
only Orbit's own records; the operator's name and reason are on the status
change and its comment. A requeue after a re-block of the same stopped run
finds that identical record already on the task and appends no second one; the
`requeued` decision from the first requeue stands. The answer names the pull request, branch and head the
next run resumes. That run re-authenticates the same receipt and adopts the
branch without re-implementing (`resumed_carried`, `implement: false`). Commit,
validation, review, push and landing run afresh on the same pull request, which
later moves between `backlog` and `someday` do not close. Its `[BLOCKED]` title
is not rewritten.

A refusal changes nothing. A specification edited since the publication is
reported with its exit: `--discard-candidate` (MCP `discard_candidate: true`)
records the discard and returns the task to `backlog`. It then closes the
task's undelivered Orbit-authored pull requests, the same closure as
`orbit task update <task-id> --status backlog --discard-candidate`, which is
the equivalent route without a recorded requeue decision. The next run
implements fresh. A closure the forge refuses is reported in the answer
(`discard_closure: incomplete`), and the requeue stands. With
`pr.close_on_terminal = false` nothing is closed (`discard_closure:
disabled`); close the discarded pull request by hand. Any other refusal of the
publication (an interrupted or timed-out run, a newer execution, a foreign
source) offers the same discard exit. Also refused, with
the same discard exit:

- a run that published through its own push and `pr_open` steps whose
  pull request is not ready to land (see
  [An escalated run that published its own pull request](#an-escalated-run-that-published-its-own-pull-request));
- a task last delivered by an owner pull-request run, with no handoff receipt,
  that still has a pull request or preserved candidate on record, which its
  next run would refuse to start from. A claimed delivery keeps its own
  candidate contract and is not refused this way: the requeue resumes the
  candidate the owner kept from the claim, as the
  [distributed drain runbook](distributed-drain.md) describes;
- a stopped run whose publication an earlier requeue already preserved on the
  task, when the task has changed since in what its digest covers (its
  meaning, required tools, crew or complexity). That record no longer
  describes the task, and a second one would make the next run refuse, so use
  discard.

### An escalated run that published its own pull request

A run can push and open its pull request through its own `push` and `pr_open`
steps and then fail completion, for example on a required check that later
passes on a rerun. Its failure handoff keeps that pull request open and
unmerged (`review_completion_failure` or `landing_review_failure`), and when
final recovery escalates the task stays `blocked` with no `[BLOCKED]` pull
request. Explicit source repair refuses this run and names requeue: there is
no failure-handoff publication to repair.

Once every required check has passed on the recorded head, requeue the task:

```bash
orbit task requeue <task-id> --reason '<why the published head can land>'
```

A plain `orbit task update <task-id> --status backlog` applies the same check.
The requeue resumes the pull request only when all of these hold, and
otherwise refuses with nothing written:

- the failure handoff's receipt names this run and the task, and its
  recorded task specification still matches the task;
- the stopped run is this machine's own single-task delivery, conclusively
  stopped, still the task's newest execution, and its candidate was not
  discarded;
- the run's checkpoints authenticate the branch, and the pull request is open
  and unmerged at the head they record, with that head still on the local
  branch and the remote;
- the forge reports every required check on exactly that head as passed: none
  failed, none unfinished, and at least one reported. Checks on another head
  describe a different candidate and are refused.

It writes the same `requeued` decision and publication-preservation record a
final-recovery requeue writes, and the answer's `resumes.source` is
`pipeline`. The next run sets up a fresh worktree, adopts the branch and pull
request without re-implementing (`resumed_carried`), then validates, reviews,
pushes and lands it afresh. The stopped run's worktree does not have to exist.

When checks fail or have not finished, requeue again once they pass. A pull
request that already merged is settled with `orbit task settle-merged
<task-id>`; a closed one, a moved head or a changed specification leaves
`--discard-candidate` as the exit. A run whose receipt was written before the
receipt named its run and specification is refused the same way.

Do not resume such a run with `orbit job resume`. Its checkpoints reuse the
stopped run's worktree, and a resume whose recorded worktree no longer exists
is refused before the task or the lineage changes.

### A task blocked on transient infrastructure

A deterministic delivery step (worktree, locks, commit, branch, push, pull
request, review gate, completion) that lost only to a busy lock (`lock_busy`,
a file-lock timeout) or a locked task store did not judge the candidate. Final
recovery classifies it without an agent, blocks the task with a
`transient_infra_blocked` history entry, and the failure handoff still
publishes the candidate. A validation step's failure is never classified this
way, even when its output quotes the same text, and neither is a claimed
leaf's: its task belongs to its owner, and its recovery agent decides as
before. `orbit task recheck-blocked` lists these blocks with their class, pull
request, whether the probe passes now, how many requeues they have taken, and
what holds one (`held`).

The probe is a proxy, not a replay: it checks that the task store takes an
immediate write transaction and that the task's own write lock is then free.
It cannot take the lock that actually timed out, which may have been another
file lock or a remote owner's, so a condition that persists elsewhere can
recur after a requeue.

The owner's clock tick, and `orbit task recheck-blocked --confirm`, requeue a
cleared block onto its candidate through the same records as the operator
requeue, so the next run resumes the same pull request. `recheck-blocked`
reports what holds a block (`held`). Two holds are temporary, and the clock
retries once they pass:

- its run, or an execution claim of the task, has not conclusively stopped
  (its failure handoff may still publish);
- the last attempt hit a transient error (the remote, the forge or the store
  did not answer). It is recorded as `transient_infra_requeue_deferred` and
  retried no sooner than ten minutes later.

The others leave the block for an operator `orbit task requeue`:

- someone other than Orbit's automation commented on or wrote to the task
  after the block: a human's call stands;
- an attempt found that its publication does not authenticate. That refusal is
  recorded once (`transient_infra_requeue_refused`), so the forge is not asked
  again on every tick;
- five attempts in a row hit transient errors.

Each publication (or, when nothing was published, each task) takes at most
two such requeues; after that the task stays blocked for an operator. A
claimed worker that could not reach its owner (`owner_route_unavailable`) is
not blocked this way: final recovery skips it and its claim is released.

## Explicit rework of a successful unmerged publication

A successful owner PR run can still leave an acceptance criterion unmet, such
as a complete hosted Check exceeding its required runtime. An operator on the
owning host can authorize bounded source rework through the same explicit
source-repair protocol. A changed task description alone grants no continuation.
Generic resume and automatic candidate reuse do not acquire this authority.

First update the task's canonical remaining specification and record the
diagnosis and evidence for the unmet acceptance on the published candidate.
Inspect the original **successful publishing run**, using its `id` with
`orbit.workflow.run.resume` and
`source_repair: {action: "inspect", task_id: "<task>"}`. The task must remain
in review and bound to that exact stopped, owner-local single-task run. A newer
binding, a live or foreign owner, a claimed delivery, or a merged/closed PR
refuses inspection. Original host publication checkpoints, candidate/base,
retained branch, repository, PR identity and remote branch lease must agree.

Review the returned `binding`, `review` and `policy_digest`. Submit the exact
binding and policy digest with `action: "submit"`, a new `request_key`, a
`diagnosis` describing the bounded implementation work, and `unmet_acceptance`
identifying the criterion and evidence (for example, the measured Check
duration and hosted run/job on this candidate). Both text fields accept
1–32768 bytes. For example, the submit payload is:

```json
{
  "id": "<successful-publishing-run>",
  "source_repair": {
    "action": "submit",
    "task_id": "<task>",
    "request_key": "bounded-rework-1",
    "binding": "<exact binding object from inspect>",
    "policy_digest": "<current digest from inspect>",
    "diagnosis": "Repair the remaining bounded source behavior on this PR.",
    "unmet_acceptance": "Criterion and observed evidence on the published head."
  }
}
```

Replace the `binding` placeholder with the returned JSON object, preserving all
fields. This is an explicit operator decision about the current task
meaning; evidence text is retained as operator testimony, not independently
certified by the recovery endpoint. Scope, publication, authority or policy
drift requires another inspection. Policy covers the current resolved delivery
definition, native gates, validation environment, crews and configured review;
it is checked again before applying or publishing the admitted source.

When the successful publisher was itself an operator repair, its original
reservation, unique run link, successful branch adoption and immutable
predecessor still authenticate the publication. That historical admission is
provenance; a new operator decision binds the current specification separately.
It does not reuse the old admission as authority for changed work.

The append-only reservation binds that evidence and diagnosis, the inspected
publication and current task meaning, the operator and current policy. Repeat
an identical request key and payload after a lost reply to recover its one
durable continuation. A divergent payload or another key for the reserved head
cannot create a second worker. A replay reports the existing attempt, including
a stopped attempt, and never restarts it.

The continuation adopts the authenticated branch and original PR and requires
fresh implementation. It inherits no validation, review or completion
checkpoints. The current `workflow.required_validation_commands` list is captured
with the review policy and applied to the new candidate. An explicitly empty
list is supported, including workspaces that rely on hosted required PR checks:
native validation records `skipped_no_required_commands` with no command receipts,
not a passing check. A nonempty list must run successfully and retain fresh
candidate-bound evidence. Changing the policy invalidates the admission; an
empty list does not waive configured review or hosted required checks. Hosted
checks and ordinary review remain required for acceptance. Previous green
correctness checks do not establish rework acceptance. Completion stays at
`review`, automatic repair is disabled, and this authorization grants no merge.
The original successful execution, performance observations, receipts and
retained checkout stay intact. No replacement PR, synthetic failure or
cancellation, checkpoint edits, or release/install action is involved.

## A cancelled published source repair

A transient completion-observation failure can requeue an owner-local task
whose candidate is already published. Final recovery records the task meaning
with its requeue decision. The next admission authenticates the successful host
commit, preparation, synchronization, review correction (if any), push and PR
checkpoints, then checks the retained branch, remote lease and exact open PR.
If setup lost its reply after rebinding the task, admission selects only the
immediate preceding owner PR run from durable task-bound history; it never
searches past a newer attempt for a usable publication.
It adopts that PR's branch and skips implementation when the candidate applies
cleanly. Conflicts require repair. Current validation, review and completion
policy still apply; old successful checks confer no approval, and review and
recovery budgets retain their existing charges. The original execution records
and retained checkout remain available; branch adoption renames the old local
branch aside without changing its commit or files.

If Orbit's own auto-fix moved the published branch before the requeue, the
next run adopts the head that auto-fix recorded. That is a
`pr_auto_fix_pushed` record written by Orbit for the same run and pull
request, starting from the published head. When the repair changed files the
task does not declare, the next run's commit adds them to the task's context
files as usual, and that addition does not void the adoption. A branch move
with no such record is still refused.

A requeue that kept its publication does not wait behind other tasks' context
locks. In a PR-mode workspace, `orbit run readiness` and the dashboard show
the task as eligible and `resuming published delivery (PR #N)`
(`published_delivery` in JSON), even while another task holds one of its
files. Its gate takes a reservation that holds no files
(`footprint_deferred: true`, naming the holders it did not wait on). The run
then re-applies the unchanged published patch, which is not an edit. Before
the first agent that may edit, the run takes the task's footprint locks and
waits while another task holds them. Those agents are an implementer
repairing a conflicting re-application, a reviewer fix, and step, conflict or
check repair. Read-only reviewers and the final-recovery decision do not wait.
If the locks stay held for the gate's wait (`max_wait_seconds`, default one
hour), the agent is refused with `footprint_unavailable`, naming the holders.
Another resumed run that still waits for its own footprint holds no file and is
no holder, so two overlapping resumed runs take the footprint one after the
other instead of refusing each other; the run that took it excludes the other
until it ends. The run's reservation is released when the run ends.

A missing task-meaning binding, changed task/head, incomplete publication,
foreign source or ambiguous association stops admission. It never authorizes
fresh replacement implementation for that known publication. Reconcile a
merged/closed PR or uncertain merge intent explicitly before trying again.

For a publication requeued before preservation bindings were introduced, use
explicit source repair against the **original publishing run**. Inspection also
accepts an owner-local failed source with a durable applied final-recovery
requeue. A refusal that reports no publication evidence for the predecessor
does not identify that predecessor as the publisher: select the older publishing
run from `orbit.workflow.run.list` and inspect it explicitly. Stop any newer
attempt before inspection. If the task now names a newer replacement, inspection
binds every owner run that bound the task after the publishing run, up to the
current binding, in `binding.superseded`: it names the current binding and digests each
run's entire stopped execution and checkpoints. Each must be a single-task
`task_pr_pipeline` run of the same task on the same machine, created after the
source, conclusively stopped, unclaimed and with no local pull record, and have
no commit, push, PR or landing-push checkpoint, step or step-start audit row.
Gate and auto-dispatch wrappers are not bindings and are skipped. A run is
either:

- **cancelled**. Its retained files are left intact; or
- **failed before producing source**, with a settled final recovery
  (escalated or requeued). It stopped at or before candidate adoption: no step
  after `resume_candidate` has a state or a step-start audit row, adoption did
  not succeed, its failure handoff (if any) recorded `no_candidate`, and its
  retained worktree has no uncommitted changes. A failed run that reached
  implementation or left work behind is a resume candidate; resume or discard
  it explicitly instead.

Missing, live, foreign, claimed, unsettled or candidate-producing runs anywhere
in that chain refuse inspection, as does a publishing run older than the
task's 64 most recent runs.
Review the returned original head, branch, PR and successor binding, then submit
that exact binding with a new request key and a diagnosis. A change to any run
in the chain invalidates it. Do not reconstruct checkpoints, fabricate a
cancellation, clear the newer run link, or restart the replacement. A lost
submission reply is reconciled by replaying the identical request key and
payload.

A stopped, escalated owner publication can also be inspected when its terminal
hook recorded a blocked-publication receipt. The retained head must be the
original host candidate or one preservation commit on an authenticated parent.
A before-PR reviewer correction qualifies only with successful host settlement
bound to the synchronized implementation and base, plus successful host
validation of that exact reviewer head and branch before publication. A push
alone provides no validation. Inspection checks the original PR, branch and
remote lease; submission requires fresh validation and configured review and
grants no merge approval. Source receipts stay unchanged. Completion-preservation
receipts are a distinct contract and cannot substitute for blocked-publication
evidence; a direct final-recovery requeue suppresses the terminal hook. Such a
run is requeued instead, as
[An escalated run that published its own pull request](#an-escalated-run-that-published-its-own-pull-request)
describes.

A generic resume whose own final-recovery repair was refused at the commit
boundary, and whose terminal hook then pushed that repair and opened or kept
the PR, is recovered the same way: inspect the **stopped resumed run**, not its
checkpoint owner. Inspection requires the task to remain bound to the owner
and authenticates the resumed run's applied `resume` receipt for this task.
The published head must be the receipt's exact repair, made from the owner's
latest authenticated host candidate in the same worktree. The resumed run must
not have passed its own commit boundary. Every run from it back to the owner
must be a stopped run of the same job. The returned
`binding.checkpoint_owner` names the owner and digests its run, steps,
checkpoints and definition. Submit that binding as usual. A changed owner
invalidates it before admission, and the admitted run refuses before source
effects. The new run adopts the same branch and PR and revalidates the
candidate from scratch. Source and owner receipts stay unchanged.

An operator source-repair admission authorizes one attempt. Generic resume
cannot restart that attempt. If a repair publishes a new head on the original
PR and is then cancelled with its task still in review, inspect the cancelled
repair run through `orbit.workflow.run.resume` with
`source_repair: {action: "inspect", task_id: "<task>"}` and the run's `id`.
Submit that fresh binding with a new `request_key` and a diagnosis of the
remaining work to authorize another attempt.

After a confirmed source-repair replacement push, publication can wait for PR
metadata that still names exactly the push receipt's previous head. Each read
must retain the admitted owner, task scope, PR identity and merge intent, and
an independent origin branch read must name the candidate, including when
metadata catches up. Only a successful, matching, leased replacement push from
this run supplies the previous head; unknown outcomes and foreign heads refuse.
The bound is three re-reads with five-second waits (15 seconds reserved wait),
shared across repeated calls in that run. Git/provider operation deadlines
remain separate. Once a candidate is observed, regression to its previous head
refuses. Settling performs reads only: it cannot push again or create a PR.

The task history's `source_repair_publication_observed` entries retain waiting
and converged observations with cumulative `settling` counters, reserved delay,
independent `remote_branch` evidence and the authenticated push hash. Delay is
recorded before waiting, so interruption cannot replenish the budget. Automation
replays the typed `source_repair_publication` decision on each history entry;
only older entries without a decision replay the diagnostic JSON note. Editing
the note on a typed entry does not change its reservation. Original pipeline
receipts are unchanged. A later exact candidate read can reconcile an
exhausted wait without repeating the push.

If publication refuses the PR identity after pushing, inspect the task history's
`source_repair_publication_refused` entries. Each entry retains a versioned JSON
record with named `mismatches` (`field`, `expected`, `observed`), the admitted
source/task digests and binding hash, the checked-out `published_head`, and the
PR read's operation, selector and timestamps. `successful_push` quotes only a
matching successful step/checkpoint from this run; null means that provenance
could not be confirmed. It never infers publication from caller input.
Entries are retained per refused read, even when the ordinary handoff comment
for that run and phase already exists.

Missing fields are distinguished from present values and unreadable responses.
Merge absence requires explicit null `mergeCommit` and `mergedAt` GraphQL
metadata; a REST provisional `merge_commit_sha` supplies no merge evidence.
Evidence retains only selected identity fields, redacts secrets and URL
credentials/query payloads, and bounds each string to 512 UTF-8 bytes (long
values carry a prefix, byte count and truncation marker). Arbitrary provider
objects and error streams are not copied. The refusal names the failing fields
and still forbids creating or replacing the PR. A diagnostic record does not
authorize retry, replacement, merge or a change in completion policy; reconcile
it against the reservation and exact publication before further operator action.

Inspection and submission corroborate the authenticated publication against the
task's PR references, regardless of their order. Historical references are
preserved. The current PR number must be recorded on that task; when its
reference includes a URL, it must match the authenticated repository and PR.
A reference giving that number a different URL, or that URL a different number,
refuses recovery even alongside a matching reference.
Key-only references remain supported because repository authority comes from
the host checkpoints, origin and provider. The association is checked again
before applying the admitted source.

Inspection selects the final authenticated host publication, including a
reviewer's correction before PR creation or before landing. A correction must
extend the head its successful review settlement identifies, and successful
host validation must name that exact corrected head. A before-landing push must
also retain the original publication's lease and PR binding. Missing, failed or
inconsistent review, validation or publication evidence refuses recovery; a
local or remote head change alone never authorizes it.

Inspection authenticates the repair's successful `resume_candidate` branch
adoption against its immutable operator admission, durable reservation and run
link. Its temporary setup branch may differ from the original PR branch. The
task must still belong to that exact stopped owner-local run, and the retained
branch, published lease, repository and open PR must agree. Missing or changed
adoption or admission evidence refuses recovery. An old request only returns
its existing run; it never restarts a cancelled worker. Requests for an already
reserved published head are refused, and cancelled attempts keep their charge
against the shared per-PR repair budget.

## A task held for a red base

Sometimes a run stops at `validate` with `[baseline_red]` and ends `held`, not
`failed`; its diagnostic step carries code `baseline_red`, and the gate and auto
parents waiting on it pass it as held. That means a required
command also fails on the base, exactly as it fails on the candidate, and the
candidate fails a test the base does not, names no failing test to compare,
or names a failure that is not a test (a build or lint error).
A candidate whose failing tests all fail on the base too is not held: it
proceeds, with a `known_red_validation` history event naming those tests. Do not
resume or replay that run, and do not unblock the task. No `[BLOCKED]` PR was
opened. The task is in `backlog` under a `baseline_red_hold` history event, and
its candidate is kept. `orbit run readiness --json` reports the task with
`reason: "baseline_red_hold"`. Its `detail` names the red base commit and
the tests failing there (or, for a command that names no test, the end of its
output).

Fix the base instead: land a repair of the failing command on the integration
branch. The owner's clock tick has already filed one `baseline-red-repair`
task for it, naming the failing tests, the first red base and every task the
same failures hold; find it with `orbit task list --tag baseline-red-repair`
and work that task rather than filing another. After the ref advances, the owner's clock tick dispatches one
`baseline_hold_refresh_pipeline` run, which runs the held command on the new
tip and records a `baseline_red_hold_verdict` history event; the hold lifts
only when that check passes. The tick itself never runs the command, so a
long check does not stall routines or auto-tasks. A failing or inconclusive
check keeps the task in the backlog. Admission and readiness only read that
verdict, so a task stays held until a refresh run has checked the new tip; a
stopped clock (`orbit clock status`) leaves it held. Find the refresh runs
with `orbit run history -j baseline_hold_refresh_pipeline`; only one runs per
workspace at a time, and a tip whose check was inconclusive gets a new run
after the last one ends. The next delivery then resumes the kept candidate. A
claimed leaf on a follower releases its claim the same way, and the owner
withholds the task from pulls until its own refresh run sees the base pass.
Both runs' logs are attached to the task: `validation/<run>/<n>.json` and
`validation/<run>/<n>.baseline.json`. See
[CONFIG.md](../CONFIG.md#a-red-base-is-held-not-blocked).

## A task held after a provider failure

A local run that ends with `[provider_capacity]`, `[provider_unavailable]` or
`[provider_refusal]` failed on its provider, not on the work. The task is not
blocked. It is in `backlog` under a `provider_failure_hold` history event. The
event's note names the excluded crews and the `not_before` time. The candidate
is committed on the run's branch, and no `[BLOCKED]` PR was opened. Do not
unblock or replay anything. A claimed leaf on a follower that a provider
refused is held the same way: its claim is released as a `provider` failure,
and the owner records the hold, naming the leaf run.

Until `not_before`, the drain gives the task to a crew the hold does not
exclude. A refusal excludes every crew of the refusing provider. When no crew
is left, `orbit run readiness --json` reports the task with
`reason: "provider_backoff"` and the release time.

To run the task sooner, set its `crew` to one the hold does not exclude, or
move it through another status: any later status change lifts the hold. If a
provider keeps refusing the content, pin the task to a crew on another
provider. See
[CONFIG.md](../CONFIG.md#provider-failure-holds).

A `[provider_unavailable]` failure (an expired login, say) is the host's. Its
hold names the host's machine, so a follower whose login works may still pull
the task on that crew. The host also records a crew outage: until the hold's
backoff ends, its local drain draws no task onto that crew, and moving the held
task through another status does not lift that. Sign the provider in again on
the host; tasks draw the crew once the outage lapses (30 minutes after the
first failure, longer when the failed task was held before).

## A task waiting on a provider usage limit

`orbit run readiness` reports `provider_limit` when every crew the task may
run as belongs to a provider this host reads at or above its usage threshold,
or exhausted. The task's line carries the detail: the provider, window, used
percent, threshold, reset and the crews skipped. A `Provider limits:` line
above the tasks names each gated window, and `orbit doctor` warns on the
provider's `provider-limits:<provider>` row. Nothing failed and nothing needs
repair: the wait lifts by itself at the next admission pass after the reset.

To run it sooner, give the pool an unlimited member, set the task's `crew` to
a crew on another provider, or raise the threshold
(`workflow.provider_limit_max_used_pct`, or `workflow.provider_limit_overrides`
for that provider). To keep using a provider whose plan window is exhausted
while paid credits remain, set that provider's override to `off`
(`["codex:off"]`). Readings stay recorded and shown: readiness JSON lists
them with `threshold` `off` and `gated` false, and `orbit doctor`'s
`provider-limits:<provider>` row says gating is off. No reading then limits
a crew. A later run that actually fails on a usage limit still records the
task's provider failure hold, which is the signal that paid credits have run
out. Removing the `off` entry gates the same readings again. A task whose crew was drawn from a purpose pool moves to another enabled
member of that pool. An explicitly
crewed task waits by default; with
`workflow.provider_limit_explicit_crews = "pool"` it is drawn from its
complexity pool instead. See
[CONFIG.md](../CONFIG.md#provider-usage-limits).

## A delivery held for the forge

Sometimes GitHub refuses every push for a while: `! [remote rejected] <branch> ->
<branch> (Internal Server Error)`, `Service Unavailable`, `Bad Gateway`, `Gateway
Timeout`, `missing necessary objects`, or `remote: Internal Server Error`. Reads and API calls can keep working.
The push step treats these as transient. It makes up to 6 attempts, waiting 10 s
after the first refusal and doubling to a 160 s cap between attempts, with jitter:
at most about 5 minutes of waiting in all. A
`missing necessary objects` rejection is retried with `--no-thin`, so the next
pack does not depend on thin-pack bases the server advertised while refs were
moving. The step's output
records `push_attempts` and `push_waited_ms`, including that retry. A refusal that names a policy
(protected branch, `GH013`, a hook, authentication, non-fast-forward) is permanent and fails at once,
including when that refusal is printed beside `missing necessary objects`.

When the budget runs out, the run does not fail. It ends `held` at that step:
`orbit run show <run_id>` names `[forge_unavailable]` and the pushed head. Step
recovery, final recovery and the failure handoff do not run. The task stays
`in-progress` with one comment explaining the hold. The candidate, its worktree and
its review are kept. Do not unblock, replay or re-run the task.

Each `orbit clock tick` resumes the held run from its checkpoints. The resumed run
skips implementation and review, pushes the same head, and continues to the pull
request. If the forge still refuses, the resumed run holds again, and the next tick
resumes that one. The clock retries for 2 hours from the first hold. After that, it
expires that hold once and blocks only in-progress tasks still coupled to that run,
including a resumed attempt that retains its checkpoint's earlier batch binding.
The `forge_unavailable_expired` history event names the held run. The run stays
`held`, with its expiry recorded, so later ticks leave it alone. A task re-queued
and admitted under an unrelated run is also left alone, even if its old hold has
not yet expired. Once pushes work again, `orbit job resume <held_run_id>` re-admits
tasks still owned by that lineage and pushes the kept candidate; the resumed run
gets its own expiry acknowledgement if it holds again. A cancel (`orbit run cancel`)
stops a push that is waiting between attempts.

A claimed leaf on a follower does not wait for the clock: the clock never resumes a
claimed run. Its pipeline sets `forge_retry.window_ms`, so the push keeps retrying
the same head inside the run, holding the claim, for up to 2 hours from the first
refusal. When the forge accepts, the leaf continues to the pull request. When the
window closes first, the run holds and the leaf releases its claim to the owner as a
`transient` failure naming the head it kept. That release does not stop the drain
offering the crew or the host, and the task's next claim on that follower continues
the kept candidate ([distributed drain](distributed-drain.md)).

## A delivery held at the landing review gate

During delivery, the pipeline runs the before-landing review gate at step
`landing_review_gate_settle`. When final recovery escalates from this step
(recording the task history event `final_recovery_escalated`), the delivery run
ends `held` or the task becomes `blocked` with an open pull request.

Unlike a transient forge failure (see [A delivery held for the forge](#a-delivery-held-for-the-forge)),
a run held at the landing review gate requires an operator decision.

### Read the situation

Before deciding how to recover the task, gather three pieces of evidence:

1. **Review verdict and findings:** Inspect the captured logs for the review gate step:
   ```bash
   orbit run logs <run-id> -s landing_review_gate_settle
   ```
   Check whether the reviewer issued `verdict complete`, `verdict incomplete`,
   or `verdict reject`, and note the finding count (such as 0 findings versus
   concrete defect findings).
2. **Escalation note in task history:** Inspect the task's history entries:
   ```bash
   orbit task show <task-id>
   ```
   Look for the `final_recovery_escalated` event under `History:` and read its note,
   which details why final recovery escalated rather than settling or retrying
   automatically.
3. **Pull request required checks:** Inspect the open pull request on the forge (such
   as GitHub CI check status). Confirm whether all required checks are passing on
   the candidate head.

### The decision rule

The evidence determines which recovery path to take:

- **0 findings, every required check green, and the failure unrelated to the diff:**
  The reviewer returned `verdict incomplete` with 0 findings because of an
  environmental or baseline condition rather than a defect in the candidate diff—such
  as an empty `workflow.required_validation_commands`, a host-resident baseline
  failure on a specific runner (see [A task held for a red base](#a-task-held-for-a-red-base)),
  or `validation_contradicted` from a reproduction check using a reverted fix. If every
  required check on the pull request is green, merge the pull request directly on the
  forge, then settle the task to `done`:
  ```bash
  orbit task settle-merged <task-id>
  ```
  See [A delivery pull request merged outside Orbit](#a-delivery-pull-request-merged-outside-orbit)
  for settlement rules.
- **A real finding:**
  The reviewer returned `verdict reject` with a genuine defect (such as an unbounded
  scan or a summary-only finding where the implementation omitted required deliverables).
  Steer the agent by appending an operator comment explaining the correction, then requeue:
  ```bash
  orbit task update <task-id> --comment '<guidance on fixing the finding>'
  orbit task requeue <task-id> --reason '<why the task is requeued>'
  ```
  See [Requeue a failure-handoff task onto its published candidate](#requeue-a-failure-handoff-task-onto-its-published-candidate)
  and [An escalated run that published its own pull request](#an-escalated-run-that-published-its-own-pull-request).
- **Unreachable criteria:**
  The escalation arose from acceptance criteria the leaf cannot measure or reach inside
  an isolated execution worktree (such as live-drain throughput numbers or deployed-service
  metrics). Re-scope the task's acceptance criteria to what the leaf's own build can
  demonstrate, keep the existing candidate as an attached patch artifact, and update the
  criteria:
  ```bash
  orbit task artifact put <task-id> .orbit/tmp/<patch-file> --path <artifact-name>
  orbit task update <task-id> --acceptance-criteria '<scoped-criterion-1>' --acceptance-criteria '<scoped-criterion-2>'
  ```
  (or attach via `orbit task update <task-id> --artifact <path=content>`). Then requeue or
  re-run the task under the reachable criteria.

### Refusals and workarounds

An operator attempting to recover a held or blocked delivery may meet two common refusals:

- **A held run refuses source-repair requeue:**
  Source-repair requeue requires a conclusively stopped, failed run. When the run is `held`,
  `orbit task requeue` without discard refuses because the source is not in `failed` or
  `cancelled` state (`published source is live, discarded, changed, or lacks an applied requeue`).
  **Workaround:** Save the candidate patch first as a task artifact using `orbit task artifact put`
  (or `orbit task update --artifact`), then discard the candidate to implement fresh:
  ```bash
  orbit task requeue <task-id> --discard-candidate --reason '<reason for fresh run>'
  ```
  (or `orbit task update <task-id> --status backlog --discard-candidate`).
- **A task in `review` cannot be requeued:**
  `orbit task requeue` accepts only tasks currently in `blocked` status (`only a blocked task can be requeued; <task-id> is review`).
  If the held delivery left the task in `review`, requeue is refused.
  **Workaround:** Move the task directly to `backlog` with `--discard-candidate`:
  ```bash
  orbit task update <task-id> --status backlog --discard-candidate
  ```
  This discards the preserved candidate, closes any unmerged Orbit pull request, and returns
  the task to `backlog` for fresh execution. If no live run watches the pull request, see
  also [A conflicting or red pull request no run is watching](#a-conflicting-or-red-pull-request-no-run-is-watching).

## A delivery leaf that died after opening its pull request

An owner-local delivery leaf (`task_pr_pipeline`) watches the pull request it
opened: `--complete` merges it, and `--auto-fix` repairs its failed required checks
and conflicts. If the leaf's worker dies after `pr_open` and the task already moved
to `review`, orphan reconciliation finalizes the run `interrupted` and the task waits
in `review`. Nothing is watching the pull request at that point. A leaf that died
before promoting its task leaves the task `blocked` with a `workflow_run_interrupted`
history entry; the blocked-task recovery backstop ([final recovery pool](../CONFIG.md#final-recovery-pool)) decides that one,
not the drain.

Each pass of a running owner drain (`orbit run auto`) adopts such a leaf before it
admits new work. It resumes the interrupted run from its checkpoints, as
`orbit job resume` would, so the resumed run continues landing review, completion
and auto-fix on the same pull request and head. The retry lineage carries the
run's repair ledger and authority. Completion still refuses a head the run did not
publish. A leaf is adopted only when all of these hold:

- its run ended within the last 24 hours, measured from its last recorded activity
  (the time orphan reconciliation stamps as its end), not at an upgrade boundary
  (the clock resumes those), and it is not a claimed execution;
- its worker is dead and no retry has resumed it yet;
- it is an original attempt, not itself a resume: a leaf that dies again after a
  drain or an operator resumed it is left for you to resume. The test reads the run
  records, so it holds even if the adoption's audit row was lost;
- its `pr_open` succeeded and that pull request is still open on the same branch;
- every task it carries is still in `review`, checked again just before the resume
  is submitted;
- the drain holds the `--complete` and `--auto-fix` grants the leaf was started
  with;
- checkpoint resume accepts it.

Adopted leaves take free drain slots. A stopped drain, or a pass held by host
resource pressure, adopts nothing. Each adoption, and each final refusal, is decided
once and recorded in the audit log as `pipeline.run.orphan_adoption`. The final
refusals are:

- `pull_request_not_open`
- `task_moved` (the task left `review` or no longer exists)
- `no_pull_request`
- `no_task`
- `claimed_execution`
- `resumed_attempt`
- `not_resumable` (checkpoint resume refused it)

To see why a leaf was left alone, read the drain's latest `adopt_orphans` step output
with `orbit run show <drain-run> --step adopt_orphans --json` (the `step_output`
field). Each completed pass replaces this checkpoint immediately, including
while the enclosing drain loop is still running. The following are not decisions; a later
pass considers the leaf again:

- `authority`: a missing grant;
- `store_unreadable`: an unreadable store;
- `forge_unreadable`: an unreadable forge. After the first failed read, a pass
  defers its remaining leaves without asking the forge again, so an unreachable
  GitHub delays admission by at most one read. In the separate out-of-run
  auto-fix scan, GitHub's `UNKNOWN` mergeability answer is a per-pull-request
  `mergeability_unknown` skip, so other repair candidates are still checked;
- `resume_unplannable`: a resume that could not be planned for a reason other than
  validation, including an activity catalog that does not load (a file caught
  mid-edit);
- `resume_failed`: a failed submission.

The step never ends the drain. When it cannot read its candidates or the drain's
slots, its output carries a `pass_error` and the pass goes on to admission.

When a leaf is not adopted, resume it yourself with `orbit job resume <leaf-run>`,
or requeue the task. When an operator moves the task back to `backlog`, any of its
pull requests that no live run is delivering are closed
([`[pr]` rules](../CONFIG.md#pr--pull-requests)). A claimed leaf on a follower is
never adopted this way: its claim's release keeps the candidate and pull request,
and the task's next claim continues them ([distributed drain](distributed-drain.md)).

## A conflicting or red pull request no run is watching

A delivery leaf's completion step repairs its pull request only while it runs.
When the leaf ends without completing (its completion deadline ran out, final
recovery escalated or requeued the task, its worker died after a resume), the
pull request can go red or conflict with its base while nothing watches it.

A drain started with `orbit run auto --auto-fix` repairs it. After adoption, each
pass takes every task in `review`, `blocked` or `backlog` that records a GitHub
pull request, however long ago its run ended and however many deliveries ran
since, looks at that task's newest `task_pr_pipeline` run and hands the task back
to that run's completion step when all of these hold. No window of recent runs
bounds the pass; a task whose newest run finished is left alone:

- the run carries only that task, ended `failed`, `timeout` or `interrupted`,
  and is not a claimed execution. A leaf that finished (a review-only handoff)
  or was cancelled is not resumed;
- the task is in `review`, or in `blocked` or `backlog` because that run's
  lineage put it there (a delivery failure handoff, an interruption, or a final
  recovery escalation or requeue);
- no run of the task is live and no claim of it is unsettled;
- the drain holds `--complete` when the run was started with it;
- the run opened the pull request the task records, on the task's `orbit/<task>-`
  branch, and the forge reports it open on that branch, naming the task and
  authored by a delivery identity (`pr.delivery_authors`, else the authenticated
  `gh` login);
- it conflicts with its base or a required check failed on its current head;
- the pull request's auto-fix ledger would admit a repair of that head and
  blocker, and no earlier hand-back covered them.

The hand-back is a checkpoint resume of the run, rewound to the first
`pr_complete` round after the last publication (`git_push`) the run checkpointed, with `auto_fix` and deferred footprint locks
granted and the repair recorded under `out_of_run_auto_fix` in its input. When a
reviewed head conflicted and a re-review republished it (`re_push`, `re_push_2`),
the rerun round is the one that completes that publication, so it pins the head
the pull request carries. The last round (`complete_reviewed_pr_2`) has no
re-review left, so a reviewed head that conflicts there ends `review_gate_stale`,
and a red one ends `auto_fix_stopped`; requeue the task for a fresh delivery. The
task returns to `review`. The watcher
admits the repair against the pull request's attempt ledger, and the
`workflow.purpose_crews.auto_fix` crew takes the task's footprint locks before it
edits, then pushes to the same branch. Completion then merges only when the run
holds `--complete`; a review-only run leaves the repaired pull request in
`review`. Each hand-back is recorded in task history as `pr_auto_fix_out_of_run`.
A pull request is handed back at most once per head and blocker, and at most
three times in all, so a resume that dies before it repairs cannot loop.

Repairs take free drain slots after adoption. A pass held by host resource
pressure starts none, and a pull request found unblocked is not read again for
five minutes. `orbit run readiness` lists live repairs and the latest pass's
skipped candidates under `out_of_run_auto_fix`. Skipped entries have
`state: "skipped"`, `reason`, `detail`, `pr_number`, `task_ids`, `source_run_id`,
`drain_run_id`, `drain_live` and `observed_at`. A `blocker` is present when the
pass verified a conflict or failed required checks; it is null for a refusal
before the forge read, such as an operator hold or no capacity. These entries
are a snapshot of the latest pass, not a fresh forge query. When no drain is live, readiness labels the most recent drain's snapshot
as ended. `orbit run show <run>` shows a live repair in the resumed run's input.

For the full persisted pass, including every skip and any `pass_error`, run
`orbit run show <drain-run> --step adopt_orphans --json` and read
`step_output.auto_fix.skipped`. Each entry names `task_id`, `pr_number`,
`source_run_id`, `blocker`, `reason` and `detail`. The reasons are:

- `no_capacity`: no free leaf slot remains after adoption and earlier repairs;
- `resource_throttled`: host resource pressure holds this pass;
- `operator_hold`: an operator blocked or requeued the task. Release it first;
- `opted_out`: the task is tagged `no-auto-approve`;
- `authority`: the run was started with `--complete` and the drain was not;
- `not_orbit_pull_request`: a branch, body or author that is not Orbit's;
- `pull_request_not_recorded`, `pull_request_not_open` or `no_pull_request`;
- `no_blocker` and `checked_recently`: nothing to repair;
- `mergeability_unknown`: GitHub is still calculating this pull request's
  mergeability; other candidates in the pass are still checked;
- `already_repaired`, `already_started` or `budget_exhausted`: the ledger refused;
- `live_run` or `claimed_execution`: another run or machine owns the delivery;
- `not_resumable`: checkpoint resume refused the run, for example one that
  stopped before its completion step;
- `store_unreadable`, `forge_unreadable`, `resume_unplannable`, `resume_failed`
  and `task_moved`: a later pass tries again.

A write-free pass reports `hold_reason: "write_free"` and does not scan or
persist candidates. A stopped drain or a host shutdown hold also starts no
scan. A drain without `--auto-fix` never repairs out of run. To repair one
task's pull request without starting a drain, use **Fix PR** in the dashboard
(below). Otherwise repair the pull request yourself, or requeue the task.

### Fix PR from the dashboard

The task details panel shows **Fix PR** beside approve, reject and archive on a
`review` task that records a pull request. It starts the hand-back described
above for that one task, through the same checks, ledger, budget, footprint
locks and `pr_auto_fix_out_of_run` record, with no drain. The record and the
resumed run's `out_of_run_auto_fix` input name no `drain_run_id`. When the panel
opens, it reads the pull request's state from GitHub
(`GET /api/tasks/<task>/auto-fix`), skipping the drain's five-minute recheck.
That reading is cached for 60 seconds; a dashboard refresh after the cache
interval reads it again, so a pull request whose checks or mergeability
changed is re-evaluated without reopening the task.
The button is enabled only when the pull request conflicts with its base or a
required check failed on its head. Otherwise it is disabled, and its title names
the reason: no blocker, mergeability still being calculated, budget spent, a
repair already running, and so on.

The action needs an operator session, governed as `auto_drain.auto_fix`. It
grants repair only. The resumed leaf keeps its own `completion`, so a pull
request whose leaf lacked `--complete` stays in `review` after the repair. A
leaf started with `--complete` is resumed only when the session also holds
`auto_drain.complete`; without it the reason is `authority`.

A refused start (`POST /api/tasks/<task>/auto-fix`) answers `409` with
`code: "task_auto_fix_<reason>"`, `reason` and `error`, and starts nothing. The
reasons are those listed above, except `no_capacity` and `checked_recently`,
which a single-task start never reports. These are added:

- `no_delivery_leaf`: the task has no `task_pr_pipeline` run of its own;
- `leaf_not_stopped`: the newest delivery finished or was cancelled;
- `live_run`: also when the newest delivery, or an earlier repair, is still
  running;
- `task_status` or `no_pull_request`: the task is not delivered, or records no
  pull request;
- `write_free`: the dashboard is read-only;
- `replica_checkout`: this checkout is a replica, so repair on its owner;
- `claimed_execution`: also when the task is bound to another machine's run (a
  follower);
- `resource_throttled`: host resource pressure holds new runs.

A session without the operator capability gets `403` `authorization_denied`
naming `auto_drain.auto_fix`. An unknown task gets `404`.

## A delivery pull request merged outside Orbit

When you merge a task's delivery pull request on GitHub yourself, no run observes
the merge, so the task stays `blocked`, `in-progress` or `review`. Orbit settles it
to `done` from the merge. Each pass of a running owner drain (`orbit run auto`) does
this after it reconciles stale runs, matching against the 100 most recently merged
pull requests. To settle a task now, or one whose pull request merged before
those 100, run this in the owning workspace's checkout:

```bash
orbit task settle-merged <task-id>   # read each pull request Orbit recorded for the task
orbit task settle-merged             # match every candidate against the recent merges
```

A task settles only when all of these hold:

- it is `blocked`, `in-progress` or `review`;
- no run or unsettled execution claim of its own is live, and no live `run auto`
  leaf carries it. That run lands or fails the task itself. A handed-off claim
  whose completion authority an operator revoked is the one exception (see below);
- one of its delivery pull requests is merged into the task's target branch. A
  delivery pull request has an `orbit/<task-id>-…` head branch and a body naming the
  task. Its author is a delivery identity (`pr.delivery_authors`, else the forge
  login), or it is a follower handoff the owner accepted, or a pull request kept
  with a released or failed claim's candidate (including a before-landing evidence
  hold). A claim's pull request must merge into its recorded landing branch: the
  accepted handoff's branch, or the original admission receipt's branch for a
  released or failed claim. A missing or expired admission receipt cannot prove
  that branch. Other delivery pull requests must merge into the base branch of
  the task's delivery run, else the workspace base branch;
- it owes no post-deploy acceptance criterion still awaiting an operator's met
  record. Merging the pull request does not discharge that measurement.

The settlement records an `external_merge_settled` history entry from the old
status to `done`. Its landing note names the pull request, the branch and the merge
commit. A task with no execution summary gets one naming the same landing. The
settlement starts no work, so another task's execution claim on the same files does
not refuse it. The task's other open Orbit pull requests close as on any landing.
`orbit task settle-merged <task-id>` names the reason a task does not settle, such
as its own live claim or no merge into its target branch.

### After revoking a claimed handoff

Revoking a claimed handoff's completion authority (the dashboard's **Revoke
authority**) cannot be undone, and nothing lands that handoff afterwards. Its
claim stays `handed_off` and keeps the task in `review`, so **Approve** and a plain
status write are both refused. The owner console's authority line names the way
out:

- If the handoff's pull request merged outside Orbit, `orbit task settle-merged
  <task-id>` (or the next `orbit run auto` pass) settles the task. It must be
  exactly the handoff's own pull request, merged into the handoff's landing
  branch and naming the task. One owner store write ends the claim as `revoked`
  and moves the task `review → done` with the usual landing note. The write grants
  no completion authority, records no merge intent or landing attempt, and leaves
  the revoked request revoked.
- Otherwise, recover the claim to `blocked` or `backlog` from the dashboard's
  claim panel.

Settlement is refused, with the reason, while an owner landing job still carries
the handoff (its attempt is `dispatched`) or a merge intent is unresolved. It is
also refused when the handoff's pull request is unmerged or merged into another
branch, or when only some other pull request merged. A handoff whose authority was
never revoked is refused too: approval or the owner's landing job completes it.

## Replay from the beginning

When checkpoint outputs are invalid or the run must start from step zero:

```sh
orbit job replay <run_id>
```

Replay runs in the foreground and exits nonzero unless the new run succeeds.
For `orbit run job` and `orbit job resume`, add `--wait` to wait for the terminal
outcome and exit nonzero unless it succeeds. A run that ends `held` exits nonzero
because delivery is still waiting on evidence or the forge.

## Verification

Use `orbit run show <new_run_id>` and `orbit run events <new_run_id>` to confirm the new
attempt is linked, expected completed steps were skipped on resume, and the run reached the
intended terminal state.

Related: [Inspect the audit trail](./audit-trail.md) ·
[Check Orbit health](./health-checks.md).
