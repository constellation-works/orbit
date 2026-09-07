---
type: runbook
summary: Diagnose, cancel, resume, or replay pending and running Orbit job runs.
tags: [operations, jobs, runs, recovery, debugging]
paths: ["crates/orbit-core/src/application/job/**", "crates/orbit-cli/src/command/run/**", "crates/orbit-core/src/runtime/run_audit.rs"]
related_features: [activity-job, auditability]
related_artifacts: [ORB-10070, ORB-10496, ORB-10801]
last_validated: 2026-08-22
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

Before changing run state, confirm the recorded owner PID and whether that exact process
is still alive. A long-running run is not necessarily stuck.

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
has not reported an exit, and the same records come back as `provider_processes` from
`GET /api/runs/:id` — which is what bridge `workflow_run_status` returns:

```text
$ orbit run show jrun-20260727-0241-2
Agent: provider=codex pid=154953 step=agent_implement liveness=alive started_at=2026-07-27T02:41:02+00:00
```

- `liveness=alive` — the child is still there and is the same process that was recorded.
  A step that has been running a long time with a live agent is working, not stuck.
- `liveness=exited` — the child is gone (or its PID was recycled by an unrelated process)
  while the step never recorded an exit. This is the lost-child case worth acting on.
- `liveness=unknown` — the host cannot probe liveness. Never read this as dead.

Liveness is probed when you ask, against the local process table, so it is only meaningful on
the host that ran the child; a historical run inspected elsewhere reports `exited`. Use
`orbit run show --json` for the full records (`pid`, `pid_start_time`, `step_id`, `finished`),
or `orbit run events <run_id> --type cli.invocation.process` for the raw audit events.

## A submitted run outlives its command

`orbit run job <job_id>` / `orbit job run <job_id>` submit the run to a detached worker
and return as soon as it is durable [ORB-10801]. A zero exit means validation,
persistence, and worker startup succeeded — it says nothing about the run's eventual
outcome, which is why every submission prints the run id and the inspection commands
above. Add `--wait` to block on the run instead; that mode exits nonzero for a `failed`,
`timeout`, `cancelled`, or `interrupted` terminal state and prints the diagnostic.

A submission that reports `queued` is durable but has not started: the job is already at
its `max_active_runs` limit and its worker is polling for a slot.

If the submission itself fails (unknown job, invalid asset, worker that could not spawn),
no run is left behind in a startable state — the persisted run is terminalized as
`interrupted` with a startup diagnostic and worker-log path.

## Interpret run states

`pending → running → success | failed | timeout | cancelled | interrupted`
(plus transient `retrying` and step-level `skipped`).

`interrupted` means the run was orphaned: its owner process died through a crash, SIGKILL,
or reboot without finalizing the run. A job that genuinely failed is `failed`;
`interrupted` means the worker died.

## Understand orphan reconciliation

Every run records its owner `pid` plus a pid-start-time token. Pipeline workers claim their
queued run at startup, so `pending` runs carry an owner too [ORB-10070]. A reconcile pass
probes liveness and finalizes conclusively orphaned runs to `interrupted`, releasing their
task reservations:

- `running` runs with a dead owner;
- `pending` runs whose claimed worker died; and
- `pending` runs never claimed within a 30-minute grace window, such as queued children
  stranded when their parent run was interrupted by a reboot.

Reconciliation runs best-effort at workspace open and lazily on
`orbit run history` / `orbit run show`. `orbit doctor` reports orphans read-only. A run
whose PID is alive but unverifiable is deliberately left alone.

Nested sandboxed activity commands are not a liveness authority for their host worker. When an
activity child carries truthy `ORBIT_MANAGED_RUN_CONTEXT` and a non-blank `ORBIT_RUN_ID`, its
workspace open deliberately skips the opportunistic orphan scan: Bubblewrap may place it in a
private PID namespace where the live host worker appears absent. Investigate and reconcile from
the host's top-level Orbit process instead. Do not remove the private PID namespace, enable a
bare fallback, or treat the child's inability to see the worker as evidence of an orphan.

Example after a worker was SIGKILLed mid-step:

```text
$ orbit run history --limit 1
│ RUN_ID                 JOB_ID            ATTEMPT   STATE         ERROR_MESSAGE                              │
│ jrun-20260704-0927-2   demo_sleep_long   1         interrupted   job run marked interrupted because         │
│                                                                  recorded worker process is no longer alive │
│                                                                  (reason=process_not_found, pid=154953, …)  │
```

## Cancel a conclusively stuck run

After verifying that the owner is gone or that the run should no longer continue:

```sh
orbit run cancel <run_id>
```

This terminalizes the run on demand. Do not cancel solely because a legitimate step has
been `running` longer than expected.

Cancellation is truthful about the race with completion: a run that reached a terminal
state before the signal landed reports `already_terminal` and keeps its real outcome
rather than being overwritten with `cancelled`.

## Operator agent invocations

A run of `agent_invoke_pipeline` is one operator invocation of an agent for exploration
or debugging (`orbit run agent <prompt>`, or the `orbit.agent.invoke` tool). It is a
single-step run with no worktree, no task ownership, and no delivery tail, so most of
this runbook's task-recovery steps do not apply to one.

Three differences matter when triaging one:

- **It is unsandboxed by design.** The provider subprocess runs on the host as the same
  operating-system user as Orbit, admitted per invocation by an operator. A sandbox
  denial is never the explanation for one failing, and the admission is recorded as a
  `trusted_host.execution_admitted` audit event naming the authorizing operator, the
  workspace, and the working directory. Remote admissions also retain the
  destination-resolved caller machine ID, invocation mode, and actual identity proof:
  strict grants are `key-bound`; explicitly cooperative same-OS-account SSH grants are
  `cooperative` and `self-asserted`.
- **Cancel it the ordinary way.** `orbit run cancel <run_id>` signals the owner process
  (TERM then KILL) and terminates the process tree, exactly as for any other run.
- **It cannot be resumed.** `orbit job resume` and `submit_resume_run` refuse a run that
  carries an admission, because that admission covered one invocation and a resume would
  reuse it without a new authorization. Submit a fresh invocation instead. `orbit job
  replay` is likewise not a workaround: the replayed input carries no admission, so the
  activity fails closed.

Read the outcome with `orbit run show <run_id>` — its `Invocation:` line distinguishes a
completed answer from a mid-turn stop, a timeout, and a cancellation — and
`orbit run logs <run_id>` for the full captured output.

## Resume from checkpoints

The v2 executor checkpoints every completed top-level step into
`job_runs.pipeline_state_json` in `~/.orbit/orbit.db`; there is no separate checkpoint
file. Resume accepts runs in `interrupted`, `failed`, or `timeout`. Any other state errors
with `resume requires an interrupted, failed, or timed-out run`.

```sh
orbit job resume <run_id>
```

Resume starts a new linked run with `attempt + 1` and `retry_source_run_id` set.
Checkpointed steps are skipped and their outputs are replayed into the pipeline:

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

Resume needs the job present in the catalog (`orbit job list --all`). A run started from
a raw YAML path can be resumed only after that YAML is registered under `resources/jobs/`.
A run with no successful checkpoints degrades to a full replay.

## Replay from the beginning

When checkpoint outputs are invalid or the run must start from step zero:

```sh
orbit job replay <run_id>
```

## Verification

Use `orbit run show <new_run_id>` and `orbit run events <new_run_id>` to confirm the new
attempt is linked, expected completed steps were skipped on resume, and the run reached the
intended terminal state.

Related: [Inspect the audit trail](./audit-trail.md) ·
[Check Orbit health](./health-checks.md).
