# Read pipeline delivery inputs

## The `completion` input

Every [shipped pipeline](workflows-shipped-pipelines.md) that ships a task takes a `completion` input, defaulting to
`review`. `orbit run ship --complete` / `orbit run auto --complete` set it to
`done` on the submitted run, and it propagates unchanged through
`workspace_auto_pipeline` → `task_auto_pipeline` → `task_gate_pipeline` → the
leaf pipelines. Because the workspace drain reads it
from its own input each iteration, work discovered mid-window inherits the same
authorization.

Under `completion: done`, the leaf pipelines gain a terminal step
(`task_complete`, or `pr_complete` for PR mode) that performs the guarded
`review -> done` transition — in local mode only after the merge and push steps
succeeded, and in PR mode only after the PR is verified merged. A run submitted
without the flag carries no `completion` key at all, so its persisted input is
identical to a pre-`--complete` submission.

See
[orchestration-completion.md](orchestration-completion.md) for the authorization semantics.

### The `auto_fix` input

`orbit run auto --auto-fix` sets `auto_fix: true` on the drain, which forwards it
to every leaf the same way it forwards `completion`; without the flag the key is
absent. In `task_pr_pipeline` it runs `pr_complete` even under
`completion: review`, as a watcher that never merges and never completes the
task: it returns once the PR is mergeable or merged elsewhere. A failed required
check fails the step with a marked hand-off that the executor routes to the
job's `step_failure_recovery`.

A conflict goes to the step's existing
`pr_conflict_recovery`. Both draw their crew from `workflow.purpose_crews.auto_fix`,
else `workflow.low_complexity_crews`.
The retried `pr_complete` validates and publishes the repair and then watches the
new head. A reviewed head is sent through the `re_review_*` round instead. See
[orchestration-auto-fix.md](orchestration-auto-fix.md#auto-fix-authorization).
