# Scope automatic check and conflict repairs

## Contents

- [Auto-fix authorization](#auto-fix-authorization)

## Auto-fix authorization

`orbit run auto --auto-fix` is the explicit authorization for one drain to
repair the pull requests its own runs opened. It is default-off, never enabled
by configuration, and independent of `--complete`. It authorizes repair only:
`--auto-fix` alone never merges, and only `--complete` takes a task to `done`.
It cannot be combined with `--pull` or `--stop`.

- The completion watcher detects the blockers; no second watcher exists. Pending
  checks wait. Failed required checks need conclusive candidate evidence before
  the job's `step_failure_recovery` repairs them, with the failed head's error
  signature and log excerpt as evidence.

  - An inherited red base or a
    base-suspect failure can keep waiting without spending a repair; see
    [hosted-check diagnosis](run-debugging-hosted-checks.md#hosted-check-failures-and-inherited-red-bases)
    for the evidence and the separate owner-handoff landing limits. Conflicts are
    repaired by `pr_conflict_recovery` against the current base.
- A missing protected check may trigger one infrastructure rerun without an
  agent: the forge must read effective classic and ruleset policy with integration
  identity, bind the PR, repository, base, head, workflow, event and latest attempt,
  and read complete workflow source, jobs and logs proving a direct dependency on
  a failed API-read prerequisite with an explicit HTTP 502, 503 or 504.

  - Literal
    names and a bounded single-axis literal matrix are supported; ambiguous or
    dynamic dependencies hold. The workflow blob must match the event base and
    exact PR head, proving the merge-ref DAG; changed definitions hold.
    Credential/permission diagnostics veto recovery.
- The owner reserves that rerun in the primary task's durable history under its
  write lock before mutation, separately from the agent-repair budget. One
  infrastructure rerun is allowed per repository/PR across heads, resumes and
  siblings. Original evidence, the reply and current-attempt observations are
  retained under `auto-fix/<run>/infrastructure-<workflow-run>-*.json`. A lost reply
  is reconciled by observation, never another mutation.

  - A second failure, missing
    protected result, superseded attempt, changed policy or identity, or completion
    deadline expiry holds the PR. Repair-only authority still cannot merge.
- Approvals, permissions, administrative rules, unknown or unreadable evidence,
  infrastructure failures outside that narrow prerequisite case, and optional
  failures alone launch no agent; the leaf stops in `review` with the reason.
- The repair goes through the ordinary commit, required final validation and
  lease-checked push. The watcher then re-checks the new head, and a reviewed
  head goes back through re-review before it can merge.
- The crew comes from `workflow.purpose_crews.auto_fix`, else
  `workflow.low_complexity_crews`, through the run's frozen pool draw,
  restricted by `--allow-crew`. Conflict recovery uses this pool too
  under `--auto-fix`. Task complexity and crew are never changed, and nothing
  escalates.
- Every attempt is written to the primary task's history with the PR, head,
  blocker, attempt number, crew and draw source before any agent runs. Only one
  attempt for a head and blocker may be live at a time. If its run is still
  live or its end cannot be confirmed, that pair remains reserved. Once the
  run is confirmed ended without publishing a new head, its admitted attempt
  still counts toward the PR's three-attempt budget, but another run may try
  the same head and blocker while budget remains. A blocker hand-back whose
  resumed run never admits an attempt is not repeated for that head and
  blocker; blocker hand-backs are also capped at three per PR. When the attempt
  budget is spent, the leaf stops with `auto_fix_budget_exhausted` and lists
  the watcher attempts and counts the source-repair reservations that share the budget.
- Closed or merged PRs and superseded heads get no repair.
- A blocked Orbit PR whose task no run is delivering any more is repaired out
  of run. Each `--auto-fix` drain pass hands it back to the task's newest
  failed, timed-out or interrupted leaf, rewound to completion, when the task
  is in `review` or was blocked or requeued by that leaf's own delivery.

  - The same ledger bounds it, each hand-back is recorded as `pr_auto_fix_out_of_run`,
    and `orbit run readiness` lists live repairs and the latest drain pass's
    skipped candidates under `out_of_run_auto_fix`. Skips carry `state: "skipped"`,
    the PR, reason, detail, observation time and whether the drain is still live;
    a blocker is null when the refusal preceded a forge read.

  - Read the full persisted pass with
    `orbit run show <drain-run> --step adopt_orphans --json`
    under `step_output.auto_fix.skipped`. Capacity and resource holds name the
    candidates without forge reads; the scan covers every task in `review`,
    `blocked` or `backlog` that records a PR, not a window of recent leaves. A write-free pass scans and persists nothing.

  Operator holds, `no-auto-approve` tasks, PRs not opened by Orbit and a leaf
  started with `--complete` under a drain without it are left alone.
- A `review` task whose newest leaf ended `held` with its PR open is handed
  back the same way when its hold is one a fresh forge read can clear:
  `delivery_check_evidence_inconclusive`, `delivery_completion_budget_expired`,
  `delivery_evidence_stale`, `delivery_requirements_unsatisfied`,
  `delivery_base_checks_pending`, `delivery_forge_observation_unavailable`, or
  `delivery_check_runs_absent` once the unchanged head reports a pending or
  passed required check or the forge positively confirms the base branch
  configures no required status checks (neither classic branch protection nor
  rulesets require checks).

  - Until that check exists or zero required checks are confirmed — missing or
    unreadable policy stays unconfirmed — the pass lists
    `delivery_check_runs_absent` under its code and spends no hand-back.
    Completion reruns with a fresh budget and repairs only what its own watcher
    then finds; the record and started entry carry `hold_code`.
  - An `auto_fix_stopped:` held leaf (`delivery_head_changed` or
    `delivery_repair_dispatched`) is likewise handed back once the unchanged
    published head has no conflict or pending/failed required checks and either
    at least one passed required check or a forge-confirmed zero-required-check
    base (missing or unreadable policy stays skipped under `auto_fix_stopped`).

  - A blocker on the
    PR goes through the ledger as above; otherwise the hand-back spends no
    repair budget and is bounded on its own: the pass waits 5 minutes after
    the leaf held, doubled for each earlier hand-back of that PR for that code
    (dashboard Fix PR skips the wait), and stops after 3 hand-backs per PR,
    head and code. A waiting or spent lineage is listed under
    `auto_fix.skipped` with the hold code as its `reason`.

  - A task a
    `completion.hold` rule keeps in review is skipped as `completion_held`.
    An admitted check repair whose `step_failure_recovery` failed without
    publishing (`delivery_repair_dispatched` with `pr_auto_fix_check_repair:`)
    is likewise observed without held-leaf backoff and retried through the
    repair ledger while budget remains (or handed back once checks turn green
    on the published head, or reported with its remaining budget when held or
    exhausted). Other holds keep their own next step (see the
    [delivery-refusal table](run-debugging-delivery-holds.md#codes-and-next-steps)
    in run debugging), and the source leaf still counts as `held`.
- The dashboard's **Fix PR** action on a `review` task starts the same
  hand-back for that one task without a drain (operator session, governed as
  `auto_drain.auto_fix`); a refusal is a `409` coded `task_auto_fix_<reason>`.

Submission stays asynchronous, so a `--complete` run's eventual outcome is not
known when the command returns — confirm with `orbit run show <run_id>` and
`orbit.task.show` rather than assuming it completed.
