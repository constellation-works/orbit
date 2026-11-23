# Inspect recovery after a failed no-edit rerun

## A failed no-edit rerun: refresh before hold

An auto-fix check repair that made no edit gets one rerun of its failed checks
on the unchanged head. If that rerun fails again, completion decides once
whether the failure is inherited from the base.

**Refresh.** The refresh applies only when all of these hold:

- Each failed required check passed on a newer base commit where the head's
  fork point conclusively failed the same job. The fork point's result is the
  newest success or failure of that job on its first-parent history, searching
  at most eight commits including the fork point and reading past commits whose
  job was skipped. If the search finds no decisive result within those eight
  commits, or the result is pending, ambiguous or unreadable, the refresh is
  held.
- No failed required check also fails on the base the same way.
- No failed required job's log cites a file the candidate changed.
- The base tip is not already in the head.

The run then reserves admission for the pull request's base update in task
history and asks the host to merge the base tip into the head as one
conflict-free merge commit. It never rebases or force-pushes. Completion adopts
the refreshed head only after checking that it carries base commits alone, and
the forge reruns the required checks on it. The refresh runs at most once per
head. A refreshed head that fails again goes through the ordinary check path
and is never refreshed a second time.

**Evidence.** Inspect these records:

- `pr_base_update_reserved` in task history with
  `evidence.cause: "base_inherited_refresh"`, `trigger: "no_edit_rerun_failed"`,
  the `rerun_failure`, `fork_point`, `landed_base` and per-check `checks`. It is
  written before the host call and consumes the pull request's one refresh
  admission.

  - A reservation whose head did not move does not prove that a forge
    write was sent: the host can refuse before calling the forge writer (for
    example, because the pull request is closed), leaving no outgoing write and
    no `pr_refresh` row. The reserved admission is still spent, so do not send
    the refresh again for that head.
- `pr_refresh` audit rows: `requested`, then `updated`, `already_up_to_date`,
  `refused` or `failed`, naming the run as actor. An early host refusal can
  precede the `requested` row; absence of that row does not prove that the
  reservation or its admission was unused.

**Pending automatic recovery: do not republish.** No operator action is needed
when either of these holds:

- The refresh row is `updated` and the run is still `running`. Completion is
  waiting for the required checks on the refreshed head within its normal
  completion budget.
- A throttled failed-job log read was recorded as `pr_auto_fix_read_deferred`.
  Completion repolls after the forge's reset.

If the refreshed checks fail again, the ordinary check path takes over. If they
do not settle within the completion budget, the run holds as
`delivery_completion_budget_expired`.

**Confirmed hold: needs intervention.** The run is `held` with
`delivery_repair_dispatched`. Every refresh hold's reason contains
`the head is not refreshed to its base tip:` followed by the cause, so search
for that text. The only exception is the spent rerun, which holds before any
refresh. Use the reason to find the case:

- *The rerun was already spent in CI history.* The refresh was never reached.
  Repair and publish a new head, then start a new delivery attempt.
- *Not base-inherited, or the base tip is already in the head.* The base does
  not fix this failure for this head. Repair the candidate, publish a new head,
  then start a new delivery attempt.
- *Conflicts with the base.* The reason names the conflicting paths, and the
  head is unchanged. Inspect them with read-only `git merge-tree`, resolve on
  the branch, revalidate, publish a new head, then start a new delivery attempt.
- *The host or forge refused the refresh.* A host-side refusal can happen
  before the forge writer is called, such as when the pull request is closed;
  in that case there may be no `requested` `pr_refresh` row and no outgoing
  write, though the reservation still spent the admission.

  Inspect the live
  pull request head SHA and the available task-history and `pr_refresh`
  evidence, fix what the refusal names, then decide whether a new head needs
  publishing before starting a new delivery attempt.
- *The refreshed head failed base-only verification.* The forge may already
  have moved the pull request head before verification failed, including when
  fetching or checking the new SHA failed. The engine does not roll back that
  update. Inspect the live pull request head SHA and the available task-history
  and `pr_refresh` evidence before deciding what to publish; then start a new
  delivery attempt with the appropriate head.
- *A refresh was already reserved for this head, or the pull request's base
  update is spent.* The refresh is not sent again. Check the pull request head
  and its `pr_refresh` rows, then publish a new head and start a new delivery
  attempt. The base-update budget is shared, so a later head may also get no
  refresh.
- *Missing pull-request fields, an unnamed origin, an unresolved base, or
  unreadable check evidence.* Fix the input the reason names, then start a new
  delivery attempt. Do not resume the held run.
