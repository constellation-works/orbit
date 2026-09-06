# Common Job-Run Failures

Use this reference after the first failed or suspicious step is identified. Match symptoms conservatively, confirm with the listed checks, then apply the smallest remedy that preserves audit history.

Keep this file portable. Do not add local task IDs, run IDs, reservation IDs, personal paths, or incident-specific names. Use placeholders such as `<run_id>`, `<task_id>`, `<reservation_id>`, and `<workspace_path>`.

## Stale Task Lock Reservation With No Owner Run

Symptoms:

- A gate, auto, or task run repeatedly emits `task.locks.reserve.denied`.
- Conflicts name `held_by: "reservation"` with the same `held_by_id` on each retry.
- CLI-only `orbit task locks list` or reservation-store inspection shows the blocking reservation belongs to a different task that is already `done`, `rejected`, `archived`, or otherwise no longer active.
  `failed` is a run state, not a task lifecycle status.
- The reservation has `owner_run_id: null`, so terminal run cleanup cannot release it by owner.

Confirm:

```bash
orbit task locks list --json
orbit tool run orbit.task.show --full --input '{"id":"<blocking_task_id>","model":"<agent-family>"}'
rg -n '<reservation_id>|task.locks.reserve.denied|<blocked_task_id>' .orbit/state/audit/v2_loop
```

If the public lock output omits needed details, inspect the global Orbit audit DB only as evidence:

```bash
sqlite3 -header -column ~/.orbit/orbit.db \
  "SELECT reservation_id, task_ids_json, files_json, created_at, expires_at, released_at, release_reason, owner_run_id
   FROM task_reservations
   WHERE reservation_id='<reservation_id>';"
```

Solution:

1. Verify the blocking task is not actively running and the reservation is stale.
2. Release through the CLI-only lock admin surface, not by editing SQLite:

   ```bash
   orbit task locks release <reservation_id>
   ```

3. Re-run or re-check the blocked run. The next reserve attempt should either acquire a fresh reservation with `owner_run_id` set or reveal a different conflict.
4. Record friction when the stale reservation required manual diagnosis or release.

## Stale Installed Job Or Workspace Catalog Drift

Symptoms:

- A workflow fails in a deterministic step with a template error such as `missing input value for <field>`.
- Explicit CLI inputs are present in the run bundle but ignored by the activity.
- The expected released behavior is absent from the effective global or workspace job definition.

Confirm:

```bash
orbit run show <run_id> --json
orbit job show <job> --json
rg -n '<missing_field>|<activity_name>|<job_name>' .orbit/state/audit/v2_loop/<run_id>.jsonl
diff -u .orbit/resources/jobs/job_<job>.yaml ~/.orbit/resources/jobs/job_<job>.yaml
find .orbit/resources/jobs ~/.orbit/resources/jobs -name '*<job>*' -print
```

Solution:

- Start with `orbit workspace sync --check`; use `orbit workspace sync` for
  managed defaults. Locally edited overrides are preserved and require deliberate
  reconciliation. See [maintenance.md](setup/maintenance.md).
- If a workspace `.orbit/resources` job shadows the global job, remove or update the workspace override.
- If the already-loaded run cannot recover because it captured the stale definition, start a fresh run after the catalog is corrected.

## No Repo Diff For PR Creation

Symptoms:

- Implementation reports success, but `pr_open` fails or returns no PR because the branch has no commits relative to the base.
- Errors include `No commits between <base> and <branch>`, `commits_ahead: 0`, or `pr_created:false`.
- The task changed global files, external artifacts, task metadata, or another non-repo surface.

Confirm:

```bash
orbit run show <run_id> --json
git -C <workspace_path> status --short --branch
git -C <workspace_path> rev-list --left-right --count <base_ref>...HEAD
orbit tool run orbit.task.show --full --input '{"id":"<task_id>","model":"<agent-family>"}'
```

Solution:

- Treat this as a handoff-shape problem, not necessarily failed implementation.
- Preserve the execution summary and artifacts.
- If the work is intentionally outside repo diff, verify the installed pipeline's no-diff/artifact contract and choose an
  authorized handoff that supports it. Switching to local mode alone does not
  establish that a no-diff task succeeded.

## Sandboxed Child Tool Cannot Write Required Orbit Store

Symptoms:

- Agent activity runs under macOS sandbox and a nested Orbit tool fails with `Operation not permitted`.
- Failures mention an Orbit store that is not exposed to the activity sandbox.
- The root workspace may have the file or command, but the generated job worktree cannot access the needed path.

Confirm:

```bash
orbit run logs <run_id> --json
rg -n 'Operation not permitted|sandbox' \
  .orbit/state/audit/v2_loop/<run_id>.jsonl .orbit/state/audit/blobs
```

Solution:

- Verify whether the blocked store is intentionally denied to activity agents.
- For optional checkpoint writes, preserve the primary run result and file friction rather than failing the task.
- For required writes, rerun with a crew/profile that can access the store or add a narrow sandbox carve-out in a dedicated fix.

## Recovery Rebuilds The Wrong Store

Symptoms:

- `step_failure_recovery` reports success, but the retried step fails with the same data/index error.
- Recovery action touches one DB, but the failing runtime path reads a different global or workspace store.

Confirm:

```bash
orbit run show <run_id> --json
rg -n 'step_failure_recovery|semantic.db|orbit.db' .orbit/state/audit/v2_loop/<run_id>.jsonl
```

Solution:

- Identify the store actually used by the failing runtime path before accepting recovery success.
- Reindex or repair the actual store, then rerun.
- Report recovery success as misleading if it repaired a non-participating DB.

## Transient SQLite Database Lock

Symptoms:

- A validation or indexing command fails with `store error: database is locked`.
- A retry completes more rows and a subsequent run is idempotent.
- Other Orbit MCP/CLI processes are active against the same DB.

Confirm:

```bash
ps -axo pid,ppid,stat,etime,command | rg 'orbit|mcp|semantic|docs index'
rg -n 'database is locked|embedded_chunks|skipped_fields' ~/.orbit/state/logs .orbit/state/audit
```

Solution:

- Retry once after active Orbit processes settle.
- If the command is resumable/idempotent, confirm with a follow-up run that reports no new work.
- If locks recur, identify the long-lived writer before broadening scope.
