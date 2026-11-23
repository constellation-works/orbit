# Match live processes before cancelling a run

## Check Live Processes

If `orbit run show <run_id> --json` reports `state: "running"`, verify the recorded process (`pid`) and provider processes (`provider_processes`) still exist and match:

```bash
orbit run show <run_id> --json
ps -o pid,ppid,pgid,stat,etime,command -p <pid>
ps -axo pid,ppid,pgid,stat,etime,command | rg '<run_id>|<workspace_path>|<task_id>'
```

If cancellation is authorized, prefer `orbit run cancel <run_id> --confirm` on the owning
host and inspect its result.

| Process cleanup |
|---|
| If process cleanup is still required: match run id → task id(s) → `pid` → `pgid` → command; prefer process-group termination (`kill -TERM -<pgid>`, wait, verify with `ps ... \| awk '$3==<pgid>'`); escalate to `kill -KILL -<pgid>` only if children remain and the human clearly asked to kill it; if the killed child belongs to a parent gate/auto run for the same task, inspect the parent and kill it only after verifying it owns the same task(s); report whether the run record updated to `failed`/`cancelled` or still says `running` despite no live process. |
