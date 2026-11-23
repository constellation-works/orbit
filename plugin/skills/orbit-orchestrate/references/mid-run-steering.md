# Mid-run steering

Use a steering comment to correct the agent's course while the admitted task
scope and acceptance criteria remain right. Do not cancel a run just to add
guidance: steering is delivered at the next prompt that includes task context,
not injected into a step that is already running.

Post an operator comment on the task while the run is in flight:

```bash
orbit task update <task-id> --comment "Please prioritize the existing acceptance criteria for the retry path."
```

This also works for a claimed task. Write to the owner's task record; the owner
admits an operator's comment-only update while the execution claim is active,
and the claimed leaf reads and records delivery through that owner. Other
unscoped task edits remain refused during the claim. Automation and agent
comments are not steering.

The run carries outstanding operator comments, oldest first, in its next safe
task-context prompt. Each comment is delivered at most once per run. Verify
delivery with `orbit task show <task-id>`: find the `steering_delivered` event
for the run and step, then confirm the comment is listed under `delivered`. A
comment under `truncated` was not carried in full. The host maintains the
delivery watermark as internal pipeline state; the event is the operator-facing
receipt.

Agent narrative in the execution summary may help diagnose a run, but
is not required to confirm delivery.

When steering is enabled, `task_pr_pipeline` and `task_local_pipeline`, plus
`task_claimed_pr_pipeline` and `task_claimed_local_pipeline`, check for
undelivered comments before `commit`. An outstanding comment sends the
uncommitted candidate through a bounded steering round, up to `max_rounds`.
If the limit is spent, or the last round delivers none of the remaining
comments, the task is held as `steering_unconsumed` and the run stops without
committing.

Inspect the task comments, then requeue, reply with clarification,
or accept the candidate through the normal review record.

Configure steering for the workspace:

```toml
[workflow.mid_run_steering]
enabled = true
max_rounds = 2
```

`enabled` defaults to `false`; `max_rounds` defaults to `2` and accepts
`1` through `10`. Settings refresh at prompt and gate checkpoints. Enabling
while runs are already in flight leaves missing watermarks absent, so operator
comments since dispatch are outstanding at the next prompt or gate unless an
inherited candidate or claim watermark provides the baseline; prefer enabling
between drains. Disabling takes effect at the next checkpoint and leaves
existing watermark and round state inert.

Steering cannot change the task's actual specification. A changed title,
description, or acceptance criteria changes the task meaning digest. If the
old-scope candidate must not continue, cancel the live child run if needed and
wait for it and its claim to settle. Then update the task and requeue for fresh
implementation with the old candidate discarded. See
[failed runs](orchestration-failed-runs.md#failed-runs) for requeue mechanics.
