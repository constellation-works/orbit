# Cancel or diagnose a job run

## Cancelling

```bash
orbit run cancel <run_id> --confirm
```

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
  [operational-logs.md](../../orbit-setup/references/operational-logs.md).

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
