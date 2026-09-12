# Debugging a failed job run

Debug an Orbit job run without guessing. A failed run has multiple layers of evidence: the job-run bundle under `.orbit/state/job-runs/`, v2 audit events under `.orbit/state/audit/v2_loop/`, transcript blobs under `.orbit/state/audit/blobs/`, task records, Git state, and sometimes live processes. This gives a repeatable order of operations so you identify the first real failure, separate root cause from downstream fallout, and report a concrete next step.

## Quick Triage

Given a run id `<run_id>`, first read it through the authoritative
`orbit_workflow_run_show` (`id`, `workspace`) or the owning host's
`orbit run show`. Record the owner host and workspace before inspecting files.
The following raw-file sequence is a fallback when public readers omit evidence:

1. Locate the run bundle:

   ```bash
   find .orbit/state/job-runs -maxdepth 3 -type d -name '<run_id>' -print
   ```

2. Read the run manifest and state:

   ```bash
   sed -n '1,140p' .orbit/state/job-runs/<job_id>/<run_id>/jrun.yaml
   sed -n '1,220p' .orbit/state/job-runs/<job_id>/<run_id>/state.json
   find .orbit/state/job-runs/<job_id>/<run_id>/steps -maxdepth 1 -type f -print -exec sed -n '1,220p' {} \;
   ```

3. Record before drawing conclusions: `job_id`; `state`; `pid` and `pid_start_time`; `input.task_ids`, `input.base_branch`, `input.base_sync`, mode flags; `started_at`/`finished_at`/`duration_ms`; failing `step_id`, `activity_name`, `error_message`, and any recovery attempt.

4. If there are multiple candidate run ids, compare `input.task_ids` first — the fastest way to identify which run owns a task.

## Use Orbit Inspection Commands First

Prefer the public inspection surface before raw file spelunking:

```bash
orbit run show <run_id> --json
orbit run events <run_id> --json
orbit run trace <run_id>
orbit run logs <run_id> --json
```

### Inspect recovery evidence before falling back to audit files

The authoritative run-show response includes `recovery_attempts`, a bounded
projection of persisted `step.recovery_attempted` events. Its `state` is
`unavailable` for legacy runs with no v2 audit trail, `not_attempted` when a
trail exists but recovery did not run, or `recorded` when `items` contain
attempts. Each item names the durable run/event and failed-step identifiers,
the recovery activity, outcome, failure phase, and a redacted bounded
diagnostic. `limit` and `truncated` say when older attempts were omitted.

Keep `error_code` and `error_message` from the run itself as the original
workflow failure. A recovery attempt is secondary evidence: `succeeded` does
not rewrite that original failure, and a failed `authorization`, preparation,
`dispatch`, or `activity` attempt explains why recovery did not complete.

### Tell a working agent from an abandoned wrapper

For a run that is still `running`, `orbit_workflow_run_show` carries
`execution_progress`: the activity step that is open and the provider children
it spawned. Only `show` carries it — `orbit_workflow_run_list` pages many runs
and does not pay for an audit scan and a liveness probe per row.

Its `state` is `observed` when the run has a v2 audit trail and `unavailable`
when it has none, so an empty projection never has to be read as "nothing is
running". `active_step` names the open `step_id` and `step_index`, and each
`provider_processes.items` entry names the child's `pid`, `provider`, owning
step, and a `liveness` of `alive`, `exited`, or `unknown`.

`liveness` is judged against the identity token recorded with the PID, so a
recycled PID reads `exited` rather than a false `alive`, and a PID recorded in
another PID namespace reads `unknown` rather than a false `exited`. A child with
`finished: true` carries its `exit_code` instead. `limit` and `truncated` say
when a long retry history was bounded; open children are never the ones dropped.

A `running` run whose open step has no `alive` child is the signature of an
abandoned wrapper. `orbit run show <run_id> --json` reports the same
`provider_processes` for the owning host.

### Verify model routing before reading logs

`orbit run show <run_id> --json` separates three identities: `requested_crew`
is the submitted `crew` input, `resolved_run_crew` is the run-level routing
decision, and `activity_provenance` is durable provider/model evidence for
each agent activity. The latter is the source for actual model routing; it can
be mixed within one run. `actual_status: "not_started"` means no activity has
begun, while `"unavailable"` means it began but no invocation evidence is
available. Do not infer provider usage or token cost from the requested or
resolved crew, and do not charge deterministic workflow wrapper steps to a
model.

Activities with `system_crew: true` are routed through `[workflow].system_crew`
(which overwrites an activity `crew` during dispatch). Other activities use an
explicit activity `crew` when present, otherwise the run's resolved crew.
Check `activity_provenance` after execution to verify the effective route,
especially after changing crew configuration.

Step-scoped variants when the failing step is known: `orbit run show|logs|events <run_id> -s <step_id> --json`.

If these commands fail or omit needed detail, fall back to files under `.orbit/state/` and mention the fallback in your report.

## Read The V2 Audit Trail

```bash
tail -80 .orbit/state/audit/v2_loop/<run_id>.jsonl
rg -n 'failed|error|recovery|cli.invocation|step.started|step.finished|activity.started|activity.finished|run.finished' .orbit/state/audit/v2_loop/<run_id>.jsonl
```

Interpretation: `run.started`/`run.finished` define the overall lifecycle; `step.started`/`step.finished` are job step boundaries; `activity.started`/`activity.finished` identify activity execution and deterministic vs agent-loop type; `cli.invocation.started`/`.finished` identify provider command, model, cwd, timeout, exit code, stdout/stderr blob refs; `step.recovery_attempted` tells whether recovery ran and succeeded — a failed recovery can be a secondary problem, diagnose the original failed step first.

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
- **Parent orchestration failure:** a child run failed and a gate/auto/epic parent is still running or waiting — identify both run ids.

## Operator Agent Invocations

A run of `agent_invoke_pipeline` is not a delivery pipeline. It is one operator
invocation of an agent for exploration or debugging, submitted with
`orbit run agent` / `orbit_agent_invoke`, and it changes no task, branch, or
pull request — so do not look for a worktree, a task lifecycle, or a delivery
tail when triaging one.

`orbit run show <RUN_ID>` prints an `Invocation:` line for these runs, and
`--json` carries the same facts under `agent_invocation`:

- `outcome` is the run's own state, never the provider's exit code.
- `provider_sandbox` is the provider's own inner sandbox at submission
  (`codex:danger-full-access`, `claude:default`, …), distinct from Orbit's
  executor sandbox (`sandboxed: false` on the submit result).
- `envelope_completed: false` on an otherwise-clean exit means the agent stopped
  mid-turn: exit zero is not evidence the investigation succeeded.
- `timed_out: true` means the wall-clock bound killed it — resubmit with a
  longer `--timeout` (the maximum is 7200s) or a narrower prompt.
- `summary` and the bounded preview are the answer; `orbit run logs <RUN_ID>`
  has the complete captured output when the preview is truncated.

These runs execute their provider subprocess outside the executor sandbox by
explicit per-invocation operator admission, so a sandbox-denial diagnostic is
never the explanation for one failing. The run trail records the admission as a
`trusted_host.execution_admitted` audit event naming the authorizing operator
and the working directory. For remote admission it also records the
destination-resolved caller machine ID, remote invocation mode, and actual
identity proof. A cooperative grant is recorded as `cooperative` plus
`self-asserted`, never as key-bound. They are deliberately **not resumable**: the
admission covered one invocation, so submit a new one rather than resuming.

For recurring signatures and known remedies, read [common-failures.md](common-failures.md) after the initial classification — keep this file focused on investigation flow; add new patterns there.

## Check Task State

```bash
orbit tool run orbit.task.show --full --input '{"id":"<task_id>","model":"<agent-family>"}'
```

Check status/history, plan/execution_summary, comments, workspace_path, external_refs/PR metadata, dependencies/resolved_dependencies. If implementation succeeded but a later workflow step failed, the task may already have a useful execution summary — preserve that context.

## Check Parent And Child Runs

Parent gate/auto/epic runs can fail because a child failed, and children can keep working after a parent reports a gate failure:

```bash
rg -n '<run_id>|<task_id>' .orbit/state/job-runs .orbit/state/audit/v2_loop
orbit run history --json
```

Look for `input.task_ids` overlap between candidate runs, parent events that invoke/wait on another `jrun-*`, child run ids named in gate/auto/epic/`invoke_and_wait` step output, and parent runs still `pending`/`running` after a child failed. Report the run owning the first real failure as primary, then name parent/child fallout separately.

## Check Git State For Workflow Failures

```bash
git -C <workspace_path> status --short --branch
git -C <workspace_path> rev-parse --abbrev-ref HEAD
git -C <workspace_path> rev-list --left-right --count <base_ref>...HEAD
git -C <workspace_path> log --oneline --decorate --graph --max-count=12 --all
git -C <workspace_path> ls-remote origin refs/heads/<branch> refs/heads/<base_branch>
```

Use read-only `git merge-tree` to understand conflicts; Git rebase has no
general dry-run mode — don't resolve conflicts unless asked to fix the run, not merely investigate it.

## Check Live Processes

If `jrun.yaml` says `state: running`, verify the recorded process still exists and its start time matches:

```bash
ps -o pid,ppid,pgid,stat,etime,command -p <pid>
ps -axo pid,ppid,pgid,stat,etime,command | rg '<run_id>|<workspace_path>|<task_id>'
```

If cancellation is authorized, prefer `orbit run cancel <run_id>` on the owning
host and inspect its result. If process cleanup is still required: match run id → task id(s) → `pid` → `pgid` → command; prefer process-group termination (`kill -TERM -<pgid>`, wait, verify with `ps ... | awk '$3==<pgid>'`); escalate to `kill -KILL -<pgid>` only if children remain and the human clearly asked to kill it; if the killed child belongs to a parent gate/auto run for the same task, inspect the parent and kill it only after verifying it owns the same task(s); report whether the run record updated to `failed`/`cancelled` or still says `running` despite no live process.

## Report Format

```markdown
<run_id> failed in <step_id>/<activity_name>.

Primary cause: <one sentence>.

Evidence:
- Task(s): <ids>
- Run state: <state>, started <timestamp>, finished <timestamp or still running>
- First failed event: <event type / step / activity>
- Key error: <short error text>
- Relevant stdout/stderr/audit source: <command, blob ref, or file path>

Current state:
- Process: <not running | running pid/pgid | killed>
- Task: <status and important metadata>
- Branch/PR: <if relevant>

Next step: <specific recommended action>
```

Keep the report short unless the human asks for a full forensic trace.

## Validation Checklist

Before finalizing a diagnosis, verify: you matched the right run id and task id(s); you identified the first failed step, not just the last logged error; you checked recovery events when present; you checked stdout/stderr blobs for the failing invocation when available; you checked live process state for runs marked `running`; you separated root cause from downstream fallout; you recorded friction ([friction.md](friction.md)) if Orbit diagnostics or recovery behavior were misleading.
