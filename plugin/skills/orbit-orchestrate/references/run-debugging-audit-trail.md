# Trace a failure through audit files and logs

## Read The V2 Audit Trail

Audit events are persisted in SQLite; use `orbit run events` and `orbit run trace` to inspect them:

```bash
orbit run events <run_id> --json
orbit run trace <run_id>
orbit run events <run_id> -s <step_id> --json
orbit run events <run_id> --type cli.invocation.finished --json
orbit run events <run_id> --json | rg 'failed|error|recovery|cli.invocation|step.started|step.finished|activity.started|activity.finished|run.finished'
```

| Event interpretation |
|---|
| Interpretation: `run.started`/`run.finished` define the overall lifecycle; `step.started`/`step.finished` are job step boundaries; `activity.started`/`activity.finished` identify activity execution and deterministic vs agent-loop type; `cli.invocation.started`/`.finished` identify provider command, model, cwd, timeout, exit code, stdout/stderr blob refs; `step.recovery_attempted` tells whether recovery ran and completed; `decision` is the decision-file verdict, and `retry_admitted` is the final host admission (the host can still refuse a verified `retry`); `step.post_recovery_attempt` is the separately recorded retry outcome, absent when no attempt ran — a failed recovery can be a secondary problem, diagnose the original failed step first. |

## Read Logs And Blobs

`orbit run logs` is the preferred way to read captured stdout/stderr. For raw blobs, map blob refs through `.orbit/state/audit/blobs/<first-two-hex>/<full-hash>`:

```bash
blob=<blob_ref>
sed -n '1,220p' ".orbit/state/audit/blobs/${blob:0:2}/$blob"
rg -n 'error|failed|panic|conflict|Validation|Outcome|execution_summary|git push|pr_open|rebase|ModelNotFound' .orbit/state/audit/blobs/<hh>/<blob>
```

Do not paste huge transcripts back to the human — summarize the decisive lines and identify the blob/command source.

For CI-failure sweeps, checkout identity is separate runner-log evidence, not
the API event or pull-request head SHA. The collector streams the full log,
keeps only a bounded human excerpt, and scans at most 8 MiB for checkout
evidence. Treat `checkout_identity.state: incomplete`, `missing`, or
`ambiguous` as a diagnostic; do not fill it from another SHA field.

## Distinguish Failure Classes

- **Implementation failure:** the agent loop exited nonzero or reported a failed envelope during `implement_one`.
- **Validation failure:** implementation completed but the repo's build, format, test, or a task-specific validation command failed.
- **Git/branch failure:** `git_push`, `pr_open`, `git_merge`, freshness checks, rebase, or conflicts failed after implementation.
- **Provider/tooling failure:** provider command failed before useful work, model unavailable, timeout, sandbox denial, tool surface mismatch.
- **Recovery failure:** the original step failed and `step_failure_recovery` also failed — report both, keep the original step as primary unless recovery caused additional damage.
- **Parent orchestration failure:** a child run failed and a gate/auto parent is still running or waiting — identify both run ids.
