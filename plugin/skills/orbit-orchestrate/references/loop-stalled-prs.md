# Reconcile a stalled pull request

## Contents

- [Reconcile stalled pull requests](#reconcile-stalled-pull-requests)
- [Refresh a held or review pull request](#refresh-a-held-or-review-pull-request)

## Reconcile stalled pull requests

A live drain claims new backlog work; it does not adopt older open PRs by
itself. Reconcile each open head independently from durable evidence before
assigning the next action. The forge is authoritative for open versus merged;
the task and run stores are authoritative for ownership and holds.

Read PR head to task, attempt, checks, and hold with supported commands:

1. List open heads on the owning repository:

   ```bash
   gh pr list --state open --repo <owner/repo> --json number,title,headRefOid,headRefName,baseRefName,updatedAt
   ```

2. Bind each head to its task. Orbit delivery branches read
   `orbit/<task>-…` and the PR body carries the task; confirm the binding on
   the task itself rather than trusting the branch name alone:

   ```bash
   gh pr view <pr-number> --repo <owner/repo> --json number,title,headRefName,headRefOid,baseRefName,body
   orbit tool run orbit.task.show --input '{"id":"<task-id>","fields":["status","job_run_id","external_refs","pr_status","open_decisions","completion_hold","final_recovery","history","comments"]}'
   ```

   `external_refs`, delivery evidence, and history bind the PR to the task.
   When the head, branch, or specification changed since the attempt published,
   the old attempt's evidence no longer covers the head.

3. Read the task's last completion attempt and whether it stopped:

   ```bash
   orbit run history --task <task-id> --json
   orbit run show <run-id> --json
   ```

   `run.state` with its `error_code` names the outcome; the task history
   carries the landing and watch entries (`landing_checks_observed`,
   `landing_waiting_for_base`, `landing_stopped`). A stopped attempt reads a
   terminal state with no live run or claim of its own; a live process is not
   stopped merely because an observation timed out. Never restart, requeue, or
   bypass an older attempt while its run is still live.

4. Read checks on the pinned head independently of the run record:

   ```bash
   gh pr view <pr-number> --repo <owner/repo> --json number,url,state,headRefOid,baseRefName,mergeStateStatus,statusCheckRollup
   gh pr checks <pr-number> --repo <owner/repo> --required
   ```

   Classify a red head before choosing recovery — inherited, transient,
   base-suspect, candidate, unknown, or inconclusive — following
   [hosted-check diagnosis](run-debugging-hosted-checks.md#hosted-check-failures-and-inherited-red-bases),
   not the rollup color alone. A red check alone grants no retry or merge
   authority, and owner landing may already be waiting on base recovery with a
   recorded wait and deadline.

5. Read the review or decision hold on the same head:

   ```bash
   orbit decision list --task <task-id> --all
   orbit decision show <request-id>
   ```

   The task's `open_decisions` and `completion_hold` are distinct holds; read each separately. A
   green head held for review or a `completion.hold` rule is a review hold,
   not a passing delivery: the next action belongs to whoever owns the gate.

Attribute the next action to its actor once the attempt has stopped:

| Head state | Owner of the next action |
|---|---|
| Red required checks, classified candidate-owned | Orchestrator routes a repair through [recovery-ci-findings.md](recovery-ci-findings.md); the next run repairs the kept candidate rather than re-implementing. |
| Red required checks, classified inherited, transient, or base-suspect | Owner landing or the base owns the wait or bounded rerun per the diagnosis; do not run candidate recovery first. |
| Green checks held by an open decision request | The human or operator who owns the gate; report the request's ID, gate, and allowed answers, and never answer it. If a completion hold is also current, report that hold separately. |
| Green checks held by a current `completion.hold` rule | The human or operator who owns the review; report the current PR number, URL and head together with the task's `completion_hold` rule, matched field, configured pattern, matched value and run. The operator reviews it by merging the open PR (or landing the local candidate) and approving the task with `orbit task update <task-id> --approve`; agents do not merge or approve. |
| Held on a delivery-refusal code (draft, closed, moved head, stale pins, conflict) | Whoever the code names in [delivery-refusal holds](run-debugging-delivery-holds.md#delivery-refusal-holds); start the named new attempt rather than resuming the held run. |
| Failure-handoff or preserved candidate with work remaining | The salvage route in [recovery-salvage-candidate.md](recovery-salvage-candidate.md#salvage-an-incomplete-candidate); resume or requeue the kept candidate, discarding it only deliberately. |
| Merged delivery | Settlement via `orbit task settle-merged <task-id>` or the drain's own pass, per [recovery-settle-rescued-task.md](recovery-settle-rescued-task.md#close-a-rescued-blocked-task). |

A policy answer arrives as a delivered human decision, never as a notice. A
fleet notice (`orbit notice list`, `orbit notice show`) grants no authority
and no delivery gate reads it; see
[fleet notices](../../orbit/references/tool-surface-notices.md#fleet-notices).

Verify a
claimed decision by its exact request id — `orbit decision show <request-id>`
reads `answered` with its resolution (answer, human, note, time), the request
leaves the open list, the task history holds `decision_answered`, and the
task's own state shows the outcome the answer asked for — following
[decision requests](run-debugging-decisions.md#decision-requests-on-held-work). If the
request still reads `open`, the answer was not applied.

Distinguish pending action from verified recovery. Pending is an open PR, a
red or pending check, an open decision, or a task still in `review` or
`blocked`. Verified is the decision read above plus the task state it moved,
or a merge observed on the forge with the task settled to `done` and its
landing note recorded. A preserved failure-handoff PR is preserved work, not
proof it is safe to merge or that the task is done.

A stopped landing that
can go again keeps its guards: same-handoff retry is operator-only through
`orbit task retry-handoff` under
[retry a stopped owner landing](orchestration-retry-landing.md#retry-a-stopped-owner-landing),
and rework of a published candidate takes the explicit source-repair route in
[recovery-source-repair.md](recovery-source-repair.md#explicit-source-repair-of-stopped-owner-publications).

## Refresh a held or review pull request

Use this when a held (`blocked`) or `review` task's open delivery pull request
is red on a failure inherited from its base branch, the base has since fixed
it, and the head is behind the base tip. Classify the red head first with
[hosted-check diagnosis](run-debugging-hosted-checks.md#hosted-check-failures-and-inherited-red-bases);
a candidate-owned failure stays red after the refresh.

A refresh merges the base tip into the head as one conflict-free merge commit
and starts the required checks on the new head. It never rebases or
force-pushes, edits no code, and lands nothing.

Call it with the task ID, using MCP `orbit.workflow` with action `refresh`, or:

```bash
orbit tool run orbit.workflow --input '{"action":"refresh","task_id":"<task-id>"}'
```

Add the global `--workspace <selector>` option when the task is not in the
active workspace. `orbit run` has no `refresh` subcommand, so the tool call is
the only CLI route. The call needs operator capability (an interactive terminal
or `ORBIT_OPERATOR=1`) or a resident cycle holding the `refresh` right; see
[the resident cycle](../../orbit/references/resident-cycle-act.md#3-act-within-authority).

The response names `outcome`, `previous_head`, `head_sha` and `checks_started`:

- `updated`: the base tip is merged into the head and `checks_started` is
  `true`. Verify the new head and its checks before anything else.
- `already_up_to_date`: the head already contains the base tip. Nothing moves,
  `checks_started` is `false`, and the existing checks stand. A refresh cannot
  change that head's checks, so diagnose the failure on it. If the failure is
  candidate-owned, repair the candidate, publish a new head and start a new
  delivery attempt.
- `task_refresh_refused`: the error reads `task_refresh_refused: task '<id>'
  did not refresh: <reason>`, and the head is unchanged. Act on the reason:
  - The task is not `blocked` or `review`.
  - A live run owns the pull request, or a live `run auto` leaf carries the
    task. Let that run finish, or stop it, before refreshing.
  - There is no open Orbit delivery pull request, there are several, or the
    pull request is not open or its base differs from the one Orbit recorded.
  - The base conflicts with the head. The reason names the conflicting paths
    and nothing was merged. Resolve them on the branch, revalidate, publish a
    new head and start a new delivery attempt; see
    [a failed no-edit rerun](run-debugging-no-edit-rerun.md#a-failed-no-edit-rerun-refresh-before-hold).
  - The forge refused. The reason carries its message.

Verify the result before landing:

1. Confirm the new head and its checks on the pull request. Its `headRefOid`
   should equal the response's `head_sha`, and the required checks should be
   running or complete on it:

   ```bash
   gh pr view <pr-number> --repo <owner/repo> --json headRefOid,baseRefName,mergeStateStatus
   gh pr checks <pr-number> --repo <owner/repo> --required
   ```

2. Read the `pr_refresh` audit rows for the task. `orbit audit list` has no
   command or task filter, so list the pull-request events and keep the rows
   whose `command` is `pr_refresh` and whose `task_id` is the task. Read
   `subcommand` (`requested`, then `updated`, `already_up_to_date`, `refused`
   or `failed`) and `arguments_json` (`new_head_sha` on a move, or `reason` and
   `conflicting_paths` on a refusal):

   ```bash
   orbit audit list --kind pull_request --since 1d --limit 500 --json
   ```

   A refusal before the forge write (wrong status, a live run, no open pull
   request) writes no `pr_refresh` row, so the error message is its only
   record. For the pipeline's own base refresh, see
   [a failed no-edit rerun](run-debugging-no-edit-rerun.md#a-failed-no-edit-rerun-refresh-before-hold).

`land` is separate. It still refuses while any required check on the new head
is failed or pending, so wait for green required checks on the new head before
landing. For the resident right and the review rule on a refreshed head, see
[the resident cycle](../../orbit/references/resident-cycle-act.md#3-act-within-authority).
