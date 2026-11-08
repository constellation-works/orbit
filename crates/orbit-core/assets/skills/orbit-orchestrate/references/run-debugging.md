# Debugging a failed job run

Debug an Orbit job run without guessing. A failed run has multiple layers of evidence: structured run state and steps via `orbit run show`, audit events via `orbit run events` and `orbit run trace`, captured logs via `orbit run logs`, transcript blobs under `.orbit/state/audit/blobs/`, task records, Git state, and sometimes live processes. This gives a repeatable order of operations so you identify the first real failure, separate root cause from downstream fallout, and report a concrete next step.

## Quick Triage

Given a run id `<run_id>`, first read it through the owning host's
`orbit run show`. The authoritative `orbit_workflow_run_show` (`id`,
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

## Use Orbit Inspection Commands First

Use the public inspection surface to inspect the run:

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

An item's `outcome` says only whether the recovery activity completed. Whether
the failed step was then retried is `retry_admitted`, decided by `decision`:
the decision file `step_failure_recovery` writes under the worktree's
`.orbit/tmp/step-recovery/`, never the `recovered` field of its response.
`decision.status` is `verified` (with `verdict` `retry` or `not_recovered`),
`absent` (no file: the legacy retry-on-success), `invalid` (malformed, stale,
for another run/step/attempt, or an unsafe path) or `unavailable` (the host
could not read it); the last two never admit a retry and `detail` names why.
Older runs and other recovery activities carry no `decision`. The retried
step's own result is the `step.post_recovery_attempt` audit event and the
`orbit.job.step_post_recovery_attempt` log line.

Keep `error_code` and `error_message` from the run itself as the original
workflow failure. A recovery attempt is secondary evidence: `succeeded` does
not rewrite that original failure, and a failed `authorization`, `decision`,
preparation, `dispatch`, or `activity` attempt explains why recovery did not
complete.

### Tell a working agent from an abandoned wrapper

For a run that is still `running`, the operator-only `orbit_workflow_run_show`
(operator authority; denied for agent sessions) carries
`execution_progress`: the activity step that is open and the provider children
it spawned. Only `show` carries it — `orbit_workflow_run_list` (also
operator-only) pages many runs and does not pay for an audit scan and a
liveness probe per row. A session with a shell on the owning host reads the
same liveness with `orbit run show <run_id> --json` (`provider_processes`).

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

With `view: "bounded"`, progress and the log page come from a fixed audit
budget rather than a replay of the whole trail: a prefix of step lifecycle
rows, the newest provider spawn and completion rows, the newest samples since
them, the parents that place them in a step, and the requested log page.
`audit_metadata` gives `rows_read` and its `row_budget`. `logs.total` is still
a full store count, so pagination stays exact. When `audit_metadata.complete`
is `false`, the history outgrew a budget: children spawned before the window,
their samples, step positions, or the active step may be missing, and
`provider_processes.truncated` is `true`. Omit `view` for the full replay.

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

Provider token counters take precedence over usage declared in an answer;
declared usage is only a fallback when provider counters are absent. Codex
prompt-tier evidence belongs to the latest started rollout turn, so a resumed
session does not re-charge earlier turns. Missing rollout or turn-boundary
evidence leaves per-request usage unknown; large invocation totals alone cannot
prove which requests incurred a premium.

An explicit activity `crew` always wins. Otherwise activities with a
`crew_config_key` draw from that pool once per run: the task pilot from
`workflow.purpose_crews.task_pilot`, step-failure and PR conflict recovery from
`workflow.purpose_crews.failure_recovery`, auto-fix repairs from
`workflow.purpose_crews.auto_fix`, final recovery from
`workflow.purpose_crews.final_recovery`. An unset purpose pool falls back to
the pool it replaced (`low` for the pilot and auto-fix, `medium` for recovery). The run
state's `activity_crew_draws` records each draw, its source and the eligible
members; `orbit run show` prints them as `Activity Crew:` lines, and `--json`
names each draw's `rule`. An empty complexity pool falls back to
`workflow.default_crew` and the source says so. Other activities use the run's
resolved crew.
Check `activity_provenance` after execution to verify the effective route,
especially after changing crew configuration.

Step-scoped variants when the failing step is known: `orbit run show|logs|events <run_id> -s <step_id> --json`.

If these commands fail or omit needed detail, inspect raw payload blobs under `.orbit/state/audit/blobs/` or definition snapshots under `.orbit/state/job-runs/` and mention the fallback in your report.

## Read The V2 Audit Trail

Audit events are persisted in SQLite; use `orbit run events` and `orbit run trace` to inspect them:

```bash
orbit run events <run_id> --json
orbit run trace <run_id>
orbit run events <run_id> -s <step_id> --json
orbit run events <run_id> --type cli.invocation.finished --json
orbit run events <run_id> --json | rg 'failed|error|recovery|cli.invocation|step.started|step.finished|activity.started|activity.finished|run.finished'
```

Interpretation: `run.started`/`run.finished` define the overall lifecycle; `step.started`/`step.finished` are job step boundaries; `activity.started`/`activity.finished` identify activity execution and deterministic vs agent-loop type; `cli.invocation.started`/`.finished` identify provider command, model, cwd, timeout, exit code, stdout/stderr blob refs; `step.recovery_attempted` tells whether recovery ran and completed and, in `decision`, whether its written decision admitted the retry; `step.post_recovery_attempt` is that retry's outcome — a failed recovery can be a secondary problem, diagnose the original failed step first.

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

## Bounded response-correction rounds

A read-only activity that declares a `response_correction` policy gets a bounded second chance when its agent reply is rejected for formatting alone. The parser stays the authority: the rejected reply never becomes a result. A fresh invocation with the same crew, tools and sandbox runs inside the unspent part of the activity's wall-clock budget, and only a reply that itself parses succeeds. This is not step recovery and never extends the budget.

Only read-only activities declare it, such as the task pilot, so a correction never repeats a mutation.

The fresh invocation receives one `rejection`:

- `contradictory_reply`: the reply said success and also carried an error object.
- `missing_envelope`: a clean turn ended in prose with no completion envelope, so nothing in it counts yet.
- `structured_output_retry_exhausted`: the provider exhausted its formatting retries; the request carries only the terminal diagnostic, not an answer.

A requested correction that cannot start records one `refusal`:

- `rounds_exhausted`: the step already spent its `max_rounds`.
- `wall_clock_exhausted`: less than the policy's minimum remaining budget was unspent.

### Read a correction without re-running

Do not re-run the activity to see whether a correction happened. Inspect the finished step:

```bash
orbit run show <run_id> --json
orbit run show <run_id> -s <step_id> --json
```

The step output carries `response_correction` with `rounds`, `max_rounds`, `refusal` (null when the last request ran), `original_error` and `original_stdout_blob_ref`. `rounds: 0` with a `refusal` means the first rejection was never corrected; `rounds >= 1` means at least one fresh invocation ran. A still-failing step appends `after N response correction(s)` to its message, plus `(correction refused: <code>)` when a refusal stopped the loop and the original error after `original error before correction:`. The rejected reply's trace and stdout blob stay in the run's captured output, so compare `original_stdout_blob_ref` against the latest invocation before concluding what the agent said.

## Local required-validation replay

A failed local required command can receive one bounded, isolated replay of
the selected failing tests on the same candidate. This is an automatic
validation evidence path, not permission to repeat checks manually or to add
retries.

The replay is eligible only when the synchronized base has a comparable
passing result without a host-coverage refusal, the original candidate command
did not pass, time out, lack a required tool, fail from infrastructure or
network trouble, defer execution, or refuse host output, and the runner can
identify one to three failing tests. If these conditions do not hold, no
candidate replay can establish a pass. When eligible, the system runs the
original command once with the exact failing-test selection on the same
candidate.

The replay resolves the original command only if it passes without a timeout,
missing tool, infrastructure failure, deferred outcome, or host-output
refusal; reports exactly the handed failing-test selection; and executes at
least as many tests as that selection contains. This ties the passing result
to every named original failure. A qualifying replay is recorded in task
history as `flaky_validation`, and delivery may continue with the original
failure and successful replay preserved as evidence. An attempted replay that
fails or cannot prove the required selection and coverage remains a required
command failure. Its replay log is kept, while the original log has no
accepted `flaky` record. Ineligible failures remain failures as well.

### Inspect the local replay evidence

List a task's artifact paths and history first:

```bash
orbit tool run orbit.task.show --input '{"id":"<task-id>","model":"<agent-family>","fields":["artifacts","history"]}'
```

For the relevant command index, fetch the original candidate log, its base
comparison log, and the replay log when present:

```bash
orbit tool run orbit.task.artifact.get --input '{"id":"<task-id>","path":"validation/<run-id>/<index>.json","model":"<agent-family>"}'
orbit tool run orbit.task.artifact.get --input '{"id":"<task-id>","path":"validation/<run-id>/<index>.baseline.json","model":"<agent-family>"}'
orbit tool run orbit.task.artifact.get --input '{"id":"<task-id>","path":"validation/<run-id>/<index>.rerun.json","model":"<agent-family>"}'
```

The original log's `candidate_rerun` field links to the last path. The replay
log identifies its `role` as `candidate_rerun`; the base log is named by the
original log's baseline record. Cross-check `command` and `run_id` across all
three logs. The original log's `base_sha` should match the base log and the
accepted history record; its `tested_head` should match the replay log and
history record. Compare each log's `validation_env` and captured outputs. The
typed `flaky_validation` history record binds `command`, `run_id`, `base_sha`,
and `tested_head`, and carries the selected failing tests, original/base/replay
outputs, replay environment, and `rerun_tests_run`. Confirm the event's record
matches the artifacts before treating delivery as resolved. If the replay
path or accepted history event is absent, the command has no accepted local
replay evidence.

This mechanism is separate from [hosted-check diagnosis](#hosted-check-failures-and-inherited-red-bases), which classifies provider jobs and their permitted landing reruns. A red or unproven base follows the separate [red-base hold procedure](../../orbit-setup/references/automation.md#built-in-re-checking-tasks-held-for-a-red-base). A workspace-local `flaky-test-hunt` auto-task, where one is defined, analyzes hosted CI history and files tasks; it does not explain this local command replay.

## Whole-run replay from step zero

Use `orbit job resume <run_id>` when the run's successful top-level
checkpoints are valid and the remaining work should continue from them. Resume
creates a linked run that reuses those outputs and skips the completed steps.

Use `orbit job replay <run_id>` when checkpoint outputs cannot be trusted or
the job must restart from the beginning. Replay creates a new run linked to the
source, uses its persisted input with the current job definition, and executes
from step zero; it leaves the source run unchanged. Replay runs in the
foreground and exits nonzero unless the new run succeeds. If you hold the
workspace claim, pass its token with `--claim-token <token>`.

Replay is refused for a replica checkout or when a task is held by a live
execution claim. After replay, use the new run ID with
`orbit run show <new_run_id>` and `orbit run events <new_run_id>` to confirm it
links to the source, starts its steps from the beginning, and reaches the
intended terminal state.

## Hosted-check failures and inherited red bases

Use this procedure when owner landing is waiting on or stopped by a failed
hosted check. It classifies required checks and checks explicitly owed by
review on the pinned PR head before spending candidate recovery. A red rollup
alone does not establish a code defect or an abandoned run.

### Inspect the owner, task and provider evidence

1. On the owning host and workspace, find the landing run as well as the
   delivery leaf and drain. Inspect each relevant run; the shipped
   `task_landing_pipeline` uses step `land` for activity `handoff_land`:

   ```bash
   orbit run show <landing-run-id> --no-reconcile --json
   orbit run show <landing-run-id> --step land --no-reconcile --json
   orbit run events <landing-run-id> --no-reconcile --json
   orbit run logs <landing-run-id> --step land --no-reconcile --json
   orbit tool run orbit.task.show --input '{"id":"<task-id>","model":"<agent-family>","fields":["status","job_run_id","external_refs","history","comments"]}'
   ```

   Copy actual step IDs when a job is customized. Run JSON uses `.run.state`;
   a step read exposes `step_output`. Read the run's error and task history
   even if a failed deterministic step has no successful output or agent log.
   These run commands inspect the host where they execute; an agent without
   owner access reports that gap rather than substituting a follower's store.

2. In task history, inspect `event` and decode its JSON `note` where present:
   - `landing_waiting_for_base`: `state: "waiting_for_base"`, `head`, `reason`
     and absolute `deadline_ms`. A recorded wait is evidence of a bounded
     landing poll, not an instruction to restart it.
   - `ci_base_suspect`: `base`, `head`, `check`, `signature`, `run_id`, `job_id`,
     `run_attempt`, `pr_number`, `event`, `started_at`, `completed_at`, `reason`.
   - `ci_check_rerun`: `head`, `run_id`, `pr_number`, `event`, and `jobs[]`
     containing `check`, `classification`, `signature`, `job_id`,
     `run_attempt`, `started_at` and `completed_at` from the earlier attempt.
   - `pr_base_update_reserved`: `execution`, `repository`, `pr_number`,
     `head_sha`, `base_sha`, `base_branch`, `deadline_ms` and `evidence`.
     Inherited handoff recovery names `evidence.cause: "inherited"` and
     `evidence.handoff_id`; count reservations for that PR, not polls.
   - `landing_stopped`: the stop reason, including classification, diagnostic
     signature/log excerpt or the evidence that could not be read. The owner
     claim's stopped landing attempt retains the full evidence: structured PR
     refusals have `kind: "owner_landing_refusal"`, `reason`, `repairable`,
     `accepted`, `observation` and, for a requeue stop, `requeue: true`.

   Completed landing output has `phase: "landed"` and `evidence`; when check
   recovery acted, `evidence.check_recovery` lists its actions. Recovery
   activity success alone does not prove a check passed or a PR merged.

3. Independently inspect the PR and the exact failed job and base push runs
   using the installed GitHub CLI in the owning repository:

   ```bash
   gh pr view <pr-number> --repo <owner/repo> --json number,url,state,headRefOid,baseRefName,mergeStateStatus,statusCheckRollup
   gh pr checks <pr-number> --repo <owner/repo> --required
   gh run view <workflow-run-id> --repo <owner/repo> --attempt <attempt> --verbose
   gh run view <workflow-run-id> --repo <owner/repo> --attempt <attempt> --job <job-id> --log
   gh run list --repo <owner/repo> --branch <base-branch> --event push --commit <base-sha>
   ```

   Repeat the job/log reads for the relevant base runs. Record the workflow
   and job name, failed step, emitted diagnostic signature, head/base SHAs,
   event, PR association, run/attempt identities and timestamps. A shared job
   name or failed-step label alone is insufficient. For latest-base push
   evidence, landing can look back at most eight first-parent commits for the
   same job's success/failure: a skipped or absent job says nothing; a pending
   same job waits. An unreadable listing/log is a gap, never a passing base.

### Interpret the classification before choosing recovery

| Class | Decisive evidence and owner-landing behavior |
|---|---|
| **Inherited** | The same emitted signature fails on the accepted base, or a moved base tip passes the same workflow/job. Untouched-file failures need the base-suspect procedure below unless matching base failure evidence establishes inheritance. Landing updates the PR branch onto the current base when admitted, then reads checks again; a still-red or unproven base waits without a repair agent. |
| **Transient** | Emitted logs show runner or network failure. Landing reruns failed jobs once and waits again. A repeated transient failure stops in review, without a code repair. |
| **Base-suspect** | The log cites only repository files the candidate did not change and the base does not establish the same failure. Wait for a successful same-check push run on the accepted base ancestry; even a pass predating the PR failure only permits one unchanged-head PR rerun. A passing rerun clears it. Candidate attribution needs the same signature repeated on that rerun. A matching base failure remains inherited. |
| **Candidate** | A real diagnostic and readable base evidence do not explain the failure, or the unchanged-head PR rerun repeats the base-suspect signature after a same-check base pass. Landing stops for its one automatic repair, or the bounded backlog return when that repair is spent. |
| **Unknown** | One failed job's log could not be read, including a truncated log without a selected diagnostic. The job is named in the evidence; it does not void the class decided by readable jobs. If no readable job decides, landing stays unclassified and stops in review without spending recovery. A cancelled or timed-out job is transient from its conclusion even without a log. |
| **Inconclusive** | A read log contains no diagnostic, so its signature is only the step name, or a read classification rests on fails, such as the base jobs or a base log. Stop in review without rerunning, updating or repairing. |

An unchanged-head rerun must be a verified `pull_request` execution for the
same PR and head, the same workflow run with a later `run_attempt`, starting
after the earlier attempt completed, with the same check and signature.
Compare provider evidence with `ci_base_suspect` or `ci_check_rerun`; a
suspicion note, another push/dispatch run or a green base alone cannot spend
the repair. The rerunnable untouched-file case can appear as `flaky` in
`ci_check_rerun.jobs[].classification`. It shares landing's one-rerun budget
with transient recovery.

Every failed required check needs its own evidence. A required aggregate
check with no answering workflow job is candidate-owned only when its own
failure annotations all name files the candidate changed; otherwise it is
inconclusive. Passing analysis jobs do not clear a failed aggregate.

### Recognize the bounded wait and its outcome

Inherited handoff base updates share a persisted budget of at most three
cumulative updates, each onto a different base commit, across polls, retries
and restarts. After the first update, a newer tip where every inherited job
passes can permit another update. A red, pending or unproven tip keeps waiting
within the landing deadline, including when the update budget is spent. Only
a head already containing that passing tip that still fails earns candidate
repair. These are verified merges of base advances into the PR branch;
landing does not edit or rebase the candidate to bypass a check.

The `landing_waiting_for_base` note makes that wait visible. At the landing
deadline, an unresolved base wait stops in review with the reason `the base
never recovered before the landing deadline`. Base-suspect observations also
have `max_base_ci_cycles` (default 3) per unchanged head/check/signature;
exhaustion stops in review naming the check, signature and missing evidence.
CI retry evidence that stays unreadable (plan authentication or log reads) for
the same cycle count escalates too: a same-check base pass sends a flaky
failure to candidate repair. Candidate failures keep their candidate
repair/requeue route, and inherited failures keep their base update/wait route.
Other failure classes with unreadable retry evidence stop naming the read
failure.
Inspect the effective activity input and recorded deadline instead of
extending or restarting the wait by assumption.

A conclusive candidate failure stops for one automatic repair leaf with
fresh validation and a new handoff. Once that repair is spent, the owner
ends the claim and returns the task to `backlog`, preserving the candidate
and failure evidence for its next run. Exhausting the task's landing repair
budget blocks it for an operator. Unresolved base waits and evidence that
leaves classification undecided spend no candidate repair. Confirm the actual task and latest landing
attempt state before recommending any action.

### Review deliveries without a handoff

A dashboard ship, `orbit run ship` and an owner drain leaf all deliver through
the same `task_pr_pipeline`, so they behave the same. Without `--complete` or
`--auto-fix`, the leaf ends at `review` with its pull request open, and no
owner landing follows: that runs only for a follower's handoff. Instead the
leaf submits a detached `task_pr_watch_pipeline` (step `watch`, activity
`pr_checks_watch`). It records one `landing_checks_observed` entry in the task
history. Its JSON note holds `state` (`passed`, `failed`, `conflicting`,
`blocked` or `pending`), `pr_number` and `head_sha`. A `failed` note adds
`failed_checks`, plus a `classification` and `detail` that use the classes in
the table above. A watch whose job listing read is rate-limited by the provider
records classification `deferred`; a non-throttle listing failure or no
deciding job after log reads records `inconclusive`. An individual unreadable
failed-job log does not void a readable job's classification. The watch only observes. To repair a red or conflicting pull
request, use the dashboard's **Fix PR** or an `--auto-fix` drain under
[auto-fix authorization](orchestration.md#auto-fix-authorization). Fix PR
resumes the succeeded leaf at its completion step under the leaf's own
`completion`. A drain pass still resumes only stopped leaves. A watch that
ended without an entry (the task left review, or the pull request merged or
closed) shows `recorded: false` and its `ended` reason in
`orbit run show <watch-run-id> --step watch --json`.

These outcomes describe existing owner landing, not new operator authority.
For completion-watcher repairs use [auto-fix authorization](orchestration.md#auto-fix-authorization);
its repair and single base-update limits remain separate. A **missing**
protected result uses that reference's narrowly proved HTTP-prerequisite
infrastructure rerun, not this failed-job classification. Local required
validation failures use [red-base validation holds](../../orbit-setup/references/automation.md#built-in-re-checking-tasks-held-for-a-red-base),
not hosted landing waits. None of these diagnoses authorizes manual requeue,
a second drain, weaker checks or a merge. A stopped healthy handoff follows
the existing operator-only [retry landing](orchestration.md#retry-a-stopped-owner-landing)
procedure only when its authority and evidence qualify.

## Historical agent invocations

Operator invocations and delegated explorers are no longer submitted. Stored
runs retain their answers and captured output through `orbit run show <RUN_ID>`
and `orbit run logs <RUN_ID>`. An old trusted-host admission cannot authorize
a new run or a resume; use a task workflow for new work.

## Check Task State

```bash
orbit tool run orbit.task.show --full --input '{"id":"<task_id>","model":"<agent-family>"}'
```

Check status/history, plan/execution_summary, comments, workspace_path, external_refs/PR metadata, dependencies/resolved_dependencies. If implementation succeeded but a later workflow step failed, the task may already have a useful execution summary — preserve that context.

### Replay task-pilot assessment (operator only)

When an acceptance criterion depends on task-pilot behavior, replay it through
the operator path with disposable fixture tasks. File the fixtures through
the normal task flow so they land in `proposed`; tag each `no-drain` and
`no-auto-approve`. Include a fixture expected to produce the promotion hold
and a control whose complete, selectable request should not be held. These
tags keep automatic drains and proposed-task approval away from the fixtures
while still allowing pilot assessment.

For each fixture, request a full assessment with operator authority, then
inspect both the task and the task-level attention view:

```bash
ORBIT_OPERATOR=1 orbit task repilot <id> --full
orbit task show <id>
orbit attention --kind pilot_warning --task <id> --json
```

Compare the assessment and any `pilot_warning` promotion hold with the
expected fixture outcome; the control should have no pilot warning. Archive
all fixtures with `orbit task archive <id>` after recording the results.

## Decision requests on held work

Some held work waits for a human judgment rather than a retry. Orbit then
records a **decision request** on the task. The request names its gate
(`block_triage`, `pilot_admission`, `proposed_approval` or `review_approval`),
the allowed answers with each one's consequence, the binding it was raised
against, and an expiry. It is a task-history entry, so it stays with the task.

A request reads in one of four effective states:

- `open`: waiting for an answer.
- `answered`: an allowed answer was applied. Terminal.
- `superseded`: the hold changed before an answer arrived, such as a new block
  episode, a newer pilot assessment, a newer request on the same task, a task
  that moved, or an answer given on another surface. Terminal.
- `expired`: `expires_at` passed while the request was open. Terminal.

Supersession and expiry are derived when a request is read, so a request can
read `superseded` before any sweeper has run.

### Find requests

```bash
orbit decision list                          # open requests in the active workspace
orbit decision list --gate pilot_admission   # one gate only
orbit decision list --task <task-id> --all   # one task, including closed requests
orbit decision show <request-id>             # allowed answers, binding, expiry, resolution
```

The task lists its open requests in `open_decisions`:

```bash
orbit tool run orbit.task.show --input '{"id":"<task-id>","fields":["status","open_decisions"]}'
```

An agent session reads the same requests through the registered read-only tools
`orbit.decision.list` (`gate`, `task_id`, `include_closed`) and
`orbit.decision.show` (`id`).

The dashboard's **Why stuck** card links each blocked or held task to the run or
finding that stopped it, with one-click requeue, settle-merged and
pilot-admission actions. Use the card to find what stopped a task, and answer
its request as described below.

### Answer requests (operator only)

An operator answers a request from the CLI or the dashboard:

```bash
orbit decision answer <request-id> <answer-id> --note "<evidence>"
```

`<answer-id>` must be one of the request's allowed answers, as
`orbit decision show` lists them. The answer is applied as the human operator.
For a pilot-admission verdict the note is the verdict's evidence, so give one.
A superseded or expired request is refused and nothing changes.

### Agents read, never answer

An agent session may read a request: seeing it never answers it. It may not
answer one. `orbit.decision.answer` is registered inactive, so only the operator
CLI and the dashboard answer. It may not submit one either: `orbit.decision.submit`
is governed and never callable from an agent session. Submissions come from a
registered decision channel's plugin, which presents a per-request token;
`orbit decision submit` is the CLI form of that operation.

When a request waits on an answer, report its id, gate and allowed answers to
the operator and leave the answer to them. Do not route around the request with
a task update.

### Verify an answer applied

1. `orbit decision show <request-id>` reads `answered`, and its `resolution`
   names the chosen answer, the answering human, the note and when it was applied.
2. `orbit decision list --task <task-id>` no longer lists the request as open,
   and `open_decisions` no longer names it.
3. The task history holds a `decision_answered` entry for the request, beside the
   entry the answer itself produced (a pilot verdict, a triage application or an
   approval):

   ```bash
   orbit tool run orbit.task.show --input '{"id":"<task-id>","fields":["history"]}'
   ```

4. Check the outcome the answer asked for in the task's own state, such as its
   status.

If the command errored or the request still reads `open`, do not assume the
answer was applied. Read the request again before answering it a second time.

## Check Parent And Child Runs

Parent gate/auto runs can fail because a child failed, and children can keep working after a parent reports a gate failure:

```bash
orbit run history --json
orbit run show <run_id> --json
orbit run events <run_id> --json | rg '<run_id>|<task_id>'
```

Look for `input.task_ids` overlap between candidate runs, parent events that invoke/wait on another `jrun-*`, child run ids named in gate/auto/`invoke_and_wait` step output, and parent runs still `pending`/`running` after a child failed. Report the run owning the first real failure as primary, then name parent/child fallout separately.

A gate or auto run that failed only because its child did records that leaf in its `--json` `root_cause` (`leaf_run_id`, `task_id`, `step`, `code`; null on a run that failed on its own), and `orbit run history --state failed --incidents` shows one row per leaf with the folded parents in `cascaded_run_ids`. `orbit run history --failures` shows one row per task and root cause instead: the root-cause run, its step and crew, the cause's first line in full, and `orbit run show` for that run (`--json`: `failures`). It cannot be combined with `--incidents`. On a drain, `orbit run show` names that same root-cause run in the failed-leaf warning, states each task's current status, and omits the `orbit job resume` line once the task has left `blocked`. A red base or a before-landing review awaiting a decision ends the run `held`, and a cancelled child ends its waiting parents `cancelled`; neither is a failure. A claimed leaf stopped by its provider's usage limit also ends `held`: its step code is `provider_limit`, its message names the provider and the reset, and the claim returns to the owner. A CI sweep whose only gaps are run-scoped retryable collection errors succeeds with outcome `deferred` and a `deferred_count`. Neither lists under `--state failed`; Incidents lists both under the `deferred` class.

A run an Orbit upgrade stopped at a step boundary ends `interrupted` with step code `upgrade_quiesce`, and the clock resumes it once the generation settles. That is a designed pause, not a failure: `--failures` and `--incidents` leave it and any parent echoing it out of their rows and count them in a top-level `upgrade_paused`, present only when nonzero (one per root run, also printed as the `Upgrade-paused, not failures:` line), a plain history row carries `upgrade_paused: true`, and the reliability counts report it as `upgrade_paused` outside the failure rate. A claimed leaf's settlement reports `accounting.outcome: upgrade_paused` with reason `upgrade_quiesce`. A run stopped any other way, such as `process_not_found` for a lost or killed worker, stays a failure. A run that already held an error when the upgrade stopped it keeps that error as its cause and still counts as failed.

`orbit run history --task <id>` lists runs whose `input.task_ids` array
contains the task or whose top-level `input.task_id` matches it.
`orbit run history --json` prints `{"runs":[<job-run>]}`; `--failures` prints
`{"failures":[{task_id, run_id, step, crew, cause, show}]}` instead; both it and `--incidents` add `upgrade_paused` when an upgrade paused any listed run. ROLE says
how a run was submitted: top-level directly, child by a parent run; run ids
minted before role markers existed read as unmarked. `--incidents` folds every
listed run that echoes the same leaf into one row, the leaf's own when it is
listed: CASCADED counts the folded runs and `.cascaded_run_ids` names them.
`--failures` folds the same runs into one row per task and root cause: RUN_ID,
STEP and CREW are the root-cause run's, and CAUSE is that run's error on one
line without the plain ERROR column's 60-character cap. `--no-reconcile`
reports stored records as-is; without it, stale-run reconciliation finalizes
an orphaned pending or running run as interrupted, releases its task
reservations, and settles with their owner the pull claims an ended drain or
leaf left.

## Delivery-refusal holds

A fail-closed delivery decision ends the run `held`, never `failed`. The
code comes from a typed refusal, never from matching diagnostic wording, so
read the code rather than the message text. Retry, step recovery, final
recovery and the failure handoff do not run for these decisions. The refusing
action keeps its existing task transition and landing evidence; run
finalization does not change the task's status. Gate and auto parents pass a
held child. Run history lists the run as `held`, and reliability counts it
under held, outside the failure rate.

### Read the code on a run

```bash
orbit run show <run_id> --json
orbit run history --json
```

In the run-show response the code is the `run` object's `error_code`, with
the human-readable detail in `error_message` and the `held` outcome in
`run.state`. History lists the same run with a `held` state.

### Codes and next steps

Resolve the named refusal before starting a new delivery attempt. Apart from
`delivery_forge_observation_unavailable`, these holds do not enable a generic
`orbit job resume`.

| Code | Decision | Next step |
|---|---|---|
| `delivery_head_changed` | The pull request reports a head different from the one this run published. | Compare the pull-request head against the published candidate, then start a new delivery attempt on the current head. |
| `delivery_evidence_stale` | Pinned delivery identity is missing or cannot be confirmed, or merge evidence is contradictory. | Re-establish the pinned identity and confirm the merge evidence, then start a new delivery attempt. |
| `delivery_repair_dispatched` | A repair was already dispatched for the blocked head. | Leave the dispatched repair to finish; do not dispatch another one. |
| `delivery_pr_draft` | The pull request is still a draft. | Mark the pull request ready for review, then start a new delivery attempt. |
| `delivery_pr_closed` | The pull request closed without merging. | Reopen the pull request or open a new one, then start a new delivery attempt. |
| `delivery_task_not_promotable` | The task's current status refuses promotion. | Move the task to a promotable status, then start a new delivery attempt. |
| `delivery_landing_conflict` | Landing requires a conflict repair with fresh validation and a new handoff. | Repair the conflict, revalidate, and hand off again. |
| `delivery_requirements_unsatisfied` | Required reviews or checks are unsatisfied, without a failed check. Pending-only owner landings keep waiting within their check-wait budget instead of stopping on this hold; non-auto-fix completion with readable policy stops on this hold even when only a non-required check is still running, and never arms auto-merge on that head. When current policy is unreadable, completion without auto-fix instead re-polls up to four polls, and an unpinned, ungated run may arm auto-merge while that grace reports pending. | Satisfy the named gate; for a stopped authorized owner landing, use **Retry landing** or `orbit task retry-handoff`. Other deliveries need a new attempt. |
| `delivery_base_checks_pending` | Failed checks are inherited from, or suspected to affect, the base; the base's own result comes before candidate recovery. | Wait for the base's own check result; do not run candidate recovery first. |
| `delivery_completion_budget_expired` | The completion budget expired before completion could be confirmed; the last observed pull-request state was unmerged and the task stays in review. A hold does not cancel auto-merge, and another actor may have merged since, so the forge is authoritative. | Check the forge for the current pull-request state, then start a new delivery attempt. |
| `delivery_check_evidence_inconclusive` | An auto-fix stop whose check evidence could not be read conclusively; no repair agent runs. | Let the next auto-fix pass reread the pull-request status; do not dispatch a repair on that evidence. |
| `delivery_forge_observation_unavailable` | Read-only forge reads kept failing with a transport error before any completion mutation. Nothing changed on the forge and the task stays in review. | Run `orbit job resume <run_id>`: it reads the forge again and continues completion from its checkpoints. |
| `delivery_origin_unavailable` | The origin could not be reached while admitting backlog tasks; the run is held without changing those tasks. | Wait for the origin to recover; a later normal admission retries the work. A task already in progress follows ordinary run failure handling instead. |
| `delivery_check_repair_activity_absent` | An in-budget auto-fix check repair was admitted, and the job has no `step_failure_recovery` to run it. The repair stays uncommitted. | Give the job a `step_failure_recovery` activity and start a new delivery attempt. An exhausted repair budget remains a failure, not this hold. |

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

If `orbit run show <run_id> --json` reports `state: "running"`, verify the recorded process (`pid`) and provider processes (`provider_processes`) still exist and match:

```bash
orbit run show <run_id> --json
ps -o pid,ppid,pgid,stat,etime,command -p <pid>
ps -axo pid,ppid,pgid,stat,etime,command | rg '<run_id>|<workspace_path>|<task_id>'
```

If cancellation is authorized, prefer `orbit run cancel <run_id> --confirm` on the owning
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

Before finalizing a diagnosis, verify: you matched the right run id and task id(s); you identified the first failed step, not just the last logged error; you checked recovery events when present; you checked stdout/stderr blobs for the failing invocation when available; you checked live process state for runs marked `running`; you separated root cause from downstream fallout; you recorded friction ([friction.md](../../orbit/references/friction.md)) if Orbit diagnostics or recovery behavior were misleading.

Settlement hydration reads follower runs through the operator-only `orbit.workflow.run.observe` route, which reads stored records without reconciliation or mutation. The follower opens that route without stale-run reconciliation, including discovery that precedes the read. A nonterminal leaf is a mismatch. Ordinary run inspection is an operator surface: `orbit.workflow.run.show` requires operator capability and refuses agent sessions. A shell session on the owning host can use `orbit run show <RUN_ID>`; the MCP route requires operator capability. Both reconcile on open. Followers must support the observe route; older followers cannot be hydrated safely.
