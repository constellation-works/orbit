# Diagnose hosted checks and red bases

## Contents

- [Hosted-check failures and inherited red bases](#hosted-check-failures-and-inherited-red-bases)
  - [Inspect the owner, task and provider evidence](#inspect-the-owner-task-and-provider-evidence)
  - [Interpret the classification before choosing recovery](#interpret-the-classification-before-choosing-recovery)
  - [Recognize the bounded wait and its outcome](#recognize-the-bounded-wait-and-its-outcome)

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
   orbit tool run orbit.task.show --input '{"id":"<task-id>","fields":["status","job_run_id","external_refs","history","comments"]}'
   ```

   - Copy actual step IDs when a job is customized. Run JSON uses `.run.state`;
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
   - `ci_base_suspect_reclassified`: `head`, `check`, `signature`, `from`
     (`base-suspect`), `to` (`candidate`), `base_run_id`, `base_sha`,
     `base_tip`, `pr_number`. Base observation ended with the base proven
     green, so the landing requeued the candidate instead of holding it.
   - `ci_check_rerun`: `head`, `run_id`, `pr_number`, `event`, and `jobs[]`
     containing `check`, `classification`, `signature`, `job_id`,
     `run_attempt`, `started_at` and `completed_at` from the earlier attempt.
   - `pr_base_update_reserved`: `execution`, `repository`, `pr_number`,
     `head_sha`, `base_sha`, `base_branch`, `deadline_ms` and `evidence`.
     Inherited handoff recovery names `evidence.cause: "inherited"` and
     `evidence.handoff_id`; count reservations for that PR, not polls.

     - A no-edit rerun's refresh uses `evidence.cause: "base_inherited_refresh"`
       with `trigger: "no_edit_rerun_failed"`, `rerun_failure`, `fork_point`,
       `landed_base` and `checks` ([no-edit refresh](run-debugging-no-edit-rerun.md#a-failed-no-edit-rerun-refresh-before-hold)).
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

   - Repeat the job/log reads for the relevant base runs. Record the workflow
     and job name, failed step, emitted diagnostic signature, head/base SHAs,
     event, PR association, run/attempt identities and timestamps. A shared job
     name or failed-step label alone is insufficient. For latest-base push
     evidence, landing can look back at most eight first-parent commits for the
     same job's success/failure: a skipped or absent job says nothing; a pending
     same job waits.

   An unreadable listing/log is a gap, never a passing base.

### Interpret the classification before choosing recovery

| Class | Decisive evidence and owner-landing behavior |
|---|---|
| **Inherited** | The same emitted signature fails on the accepted base, or a moved base tip passes the same workflow/job. Untouched-file failures need the base-suspect procedure below unless matching base failure evidence establishes inheritance. Landing updates the PR branch onto the current base when admitted, then reads checks again; a still-red or unproven base waits without a repair agent. |
| **Transient** | Emitted logs show runner or network failure. Landing reruns failed jobs within `workflow.ci_infrastructure_retry.max_attempts` (default 3), backing off exponentially from `workflow.ci_infrastructure_retry.backoff_seconds` (default 60). Exhaustion records a transient-class hold with the attempt count; no code repair runs. A set in which every failed job's provider-reported failed step matches `workflow.ci_infrastructure_retry.handoff_steps` (a dependency-handoff step, such as awaiting or downloading another job's artifact) is transient from the step names alone, with no log read, so a truncated or unreadable log cannot make it unknown or inconclusive; one failed job in any other step leaves the set to per-job classification. |
| **Base-suspect** | The log cites only repository files the candidate did not change and the base does not establish the same failure. Wait for a successful same-check push run on the accepted base ancestry; even a pass predating the PR failure only permits one unchanged-head PR rerun. A passing rerun clears it. Candidate attribution needs the same signature repeated on that rerun. A matching base failure remains inherited. When the handoff landing's `max_base_ci_cycles` observations end, a same-check pass on a base-branch push run at a commit containing the candidate's base, with no same-check failure there or on the landing tip, proves the base green: the failure is reclassified `candidate` (`ci_base_suspect_reclassified`) and takes the candidate requeue route. A missing pass, a red tip or an unreadable base keeps the `delivery_base_checks_pending` hold. |
| **Candidate** | A real diagnostic and readable base evidence do not explain the failure, or the unchanged-head PR rerun repeats the base-suspect signature after a same-check base pass. When the candidate set has no cause evidence (no aggregate, no changed-file citation, and no transient, inherited, unknown or inconclusive job), an eligible set on a head with no prior rerun first gets one unchanged-head classification rerun. It spends the shared once-per-head reservation before sending; a repeat failure returns the task to the backlog with the candidate kept, subject to the landing repair budget. Other candidate failures take the same immediate requeue route. |
| **Workflow failed before jobs** | A workflow run on the pinned head completed with a non-success conclusion, its complete job listing is empty, and a required context from that workflow remains absent. Completion stops waiting on that poll. If the candidate diff changes the workflow file, `--auto-fix` admits the ordinary candidate repair directly, with the workflow path, run, attempt and readable provider diagnosis; no classification rerun comes first. Without `--auto-fix`, completion stops in review with that failed-check evidence. If the candidate does not change the file, completion uses the existing single cumulative infrastructure rerun for the PR and holds if it fails again, spending no agent repair. A workflow with no jobs that is queued or running stays pending. An absent context never passes, and this evidence never authorizes a merge. Final recovery receives the candidate-owned classification and excludes `rerun_ci`; repair or requeue the candidate through its admitted lane. |
| **Unknown** | One failed job's log could not be read, including a truncated log without a selected diagnostic. The job is named in the evidence; it does not void the class decided by readable jobs. If no readable job decides, landing stays unclassified and stops in review without spending recovery. A cancelled or timed-out job is transient from its conclusion even without a log. |
| **Inconclusive** | A read log contains no diagnostic, so its signature is only the step name, or a read classification rests on fails, such as the base jobs or a base log. Stop in review without rerunning, updating or repairing. |

An unchanged-head rerun must be a verified `pull_request` execution for the
same PR and head, the same workflow run with a later `run_attempt`, starting
after the earlier attempt completed, with the same check and signature.
Compare provider evidence with `ci_base_suspect` or `ci_check_rerun`; a
suspicion note, another push/dispatch run or a green base alone cannot spend
the repair.

`ci_check_rerun.jobs[].classification` is `transient`, `flaky`
(including the rerunnable untouched-file case), or `unattributed` (the
pre-repair classification rerun). A flaky or unattributed rerun retains one
shared reservation; only a pure logged transient set can expand to the
configured infrastructure retry budget.

Every failed required check needs its own evidence. A required aggregate
check with no answering workflow job is candidate-owned only when its own
failure annotations all name files the candidate changed; otherwise it is
inconclusive. Passing analysis jobs do not clear a failed aggregate.

### Recognize the bounded wait and its outcome

Inherited handoff base updates share a persisted budget of at most three
cumulative updates, each onto a different base commit, across polls, retries
and restarts. After the first update, a newer tip where every inherited job
passes can permit another update. A red, pending or unproven tip keeps waiting
within the landing deadline, including when the update budget is spent.

Only
a head already containing that passing tip that still fails earns candidate
repair (one automatic repair, or requeue once that repair is spent). These are verified merges of base advances into the PR branch;
landing does not edit or rebase the candidate to bypass a check.

The `landing_waiting_for_base` note makes that wait visible. At the landing
deadline, an unresolved base wait stops in review with the reason `the base
never recovered before the landing deadline`. A hold at the landing deadline
never reclassifies. Base-suspect observations also have `max_base_ci_cycles`
(default 3) per unchanged head/check/signature.

At exhaustion, a same-check pass
on a base-branch push run at a commit containing the candidate's base, with no
same-check failure there or on the landing tip, proves the base green: the
failure is reclassified `candidate` (`ci_base_suspect_reclassified`) and takes
the candidate requeue route, returning the task to the backlog.

Without that
proof (no pass, a red tip or an unreadable base), exhaustion holds the candidate
in review as `delivery_base_checks_pending`, naming the check, signature and
missing evidence.
CI retry evidence that stays unreadable (plan authentication or log reads) for
the same cycle count escalates too: a same-check base pass sends a flaky
failure to candidate repair (or requeue once its repair is spent).

Candidate failures keep their
immediate backlog requeue route, and inherited failures keep their base update/wait route.
Other failure classes with unreadable retry evidence stop naming the read
failure.
Inspect the effective activity input and recorded deadline instead of
extending or restarting the wait by assumption.

A conclusive candidate failure ends the claim and returns the task to `backlog`
immediately, preserving the candidate
and failure evidence for its next run, subject to the landing repair budget.
That landing run is held as
`delivery_landing_repair_requeue`, not failed, and the hold is not resumed
or handed back to completion. Exhausting the task's landing repair
budget blocks it for an operator.

Unresolved base waits and evidence that
leaves classification undecided keep the candidate in review without ending the claim. Confirm the actual task and latest landing
attempt state before recommending any action.
