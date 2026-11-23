# Triage a failed job run

## Evidence sources

Debug an Orbit job run without guessing. A failed run has multiple layers of evidence: structured run state and steps via `orbit run show`, audit events via `orbit run events` and `orbit run trace`, captured logs via `orbit run logs`, transcript blobs under `.orbit/state/audit/blobs/`, task records, Git state, and sometimes live processes.

This gives a repeatable order of operations so you identify the first real failure, separate root cause from downstream fallout, and report a concrete next step.

## Quick Triage

Given a run id `<run_id>`, first read it through the owning host's
`orbit run show`. The authoritative `orbit_run` action `show` (`id`,
`workspace`) requires operator authority and is denied for agent sessions.
Record the owner host and workspace before inspecting evidence:

1. Inspect run state, step summary, and process details:

   ```bash
   orbit run show <run_id> --json
   ```

2. Inspect audit events and failure trace:

   ```bash
   orbit run events <run_id> --json
   orbit run trace <run_id>
   ```

3. Inspect captured stdout/stderr logs:

   ```bash
   orbit run logs <run_id> --json
   orbit run logs <run_id> -s <step_id>
   ```

4. Record before drawing conclusions: `job_id`; `run.state`; `pid` and provider process `liveness`; `input.task_ids`, `input.base_branch`, `input.base_sync`, mode flags; `started_at`/`finished_at`/`duration_ms`; failing `step_id`, `activity_name`, `error_message`, and any recovery attempt.

5. If there are multiple candidate run ids, compare `input.task_ids` first (via `orbit run history --json` or `orbit run show`) — the fastest way to identify which run owns a task.

## Follow the evidence

| When | Read |
|---|---|
| The failing step has a recovery attempt | [Recovery evidence](run-debugging-recovery-evidence.md) |
| The summary omits the failure | [Audit trail, logs and blobs](run-debugging-audit-trail.md) |
| A run is still marked running | [Live-process checks](run-debugging-live-processes.md) |
| You have enough evidence for a handoff | [Report format and validation](run-debugging-report.md) |
