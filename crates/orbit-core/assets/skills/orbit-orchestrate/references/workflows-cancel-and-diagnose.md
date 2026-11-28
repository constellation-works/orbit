# Cancel or diagnose a job run

## Cancelling

```bash
orbit run cancel <run_id> --confirm
```

The command stops a pending or running run, releases its task reservations and
marks it `cancelled`. Without `--confirm` it changes nothing. A finished run
reports `already_terminal`. A cancelled task leaf returns its task to the
backlog unless `--block`. A pull drain cancels gracefully and waits for
launched leaves unless `--force`; `--force` also stops a local drain's child
runs and returns a pull drain's claims to the owner's backlog. The command
exits 1 when a stop cannot be confirmed.

For a run that is stuck rather than merely slow, diagnose before killing:
[run-debugging-live-processes.md](run-debugging-live-processes.md) covers matching a run id to its process
group and the safe termination order.

## Diagnosis

- A `jrun-*` id that failed, stuck, or was cancelled →
  [run-debugging-triage.md](run-debugging-triage.md) for the full flow: run bundle, audit
  trail, logs and blobs, failure classification, task and git state, kill
  procedure, report format.
- A known failure signature, once the failing step is identified →
  [common-failures.md](common-failures.md).
- Host-level incident, service warning, or missing run output →
  [operational-logs-triage.md](../../orbit-setup/references/operational-logs-triage.md).

**Safety, up front:**

- Files under `.orbit/state/job-runs/` and `.orbit/state/audit/` are evidence.
  Never edit them to "fix" a run.
- Never kill a process before matching run id → `pid`/`pgid`/task id(s)/command;
  terminate the process group for that run id only. Never kill a parent auto or
  gate run without verifying it owns the same task(s).
- Top-level `state: failed` is not a diagnosis — find the first failed step or
  activity.
- Task state and run state are the durable handoff. Never parse agent prose in
  their place.
- If Orbit's own tooling or diagnostics mislead you, record friction
  ([friction.md](../../orbit/references/friction.md)).
