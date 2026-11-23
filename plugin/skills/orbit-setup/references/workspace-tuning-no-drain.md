# Workspace tuning: keeping a task out of the drain

## Keeping a task out of the drain (`no-drain`)

Use the reserved `no-drain` task tag when an interactive session owns a
task and no automatic drain should pick it up. It is a tag, not a policy
key: there is no exclusion list to configure.

- **Apply it** by including `no-drain` in the task's tag list at filing
  time, or with `orbit.task.update` sending the full tag list including
  `no-drain` (a tag update replaces the stored list, so resend the tags
  to keep alongside it).
- **Automatic admission skips it.** `orbit run auto` drains, pull-drain
  followers and any other automatic admission pass never admit a backlog
  task carrying the tag.
- **Automatic approval skips it too.** Drain-window auto-approval never
  approves a proposed task carrying the tag, even with a clean
  assessment; the hold is recorded under the tag's reason. An assessment
  pass may still assess the task.
- **Explicit dispatch still works.** Claiming the task to `in-progress`
  by hand and running it with an explicit single-task dispatch
  (`orbit run ship <task-id>`) both still work.
- **Remove it** with `orbit.task.update` sending the full tag list
  without `no-drain`. The task becomes eligible again on the next
  admission pass.
- **Verify the exclusion** with `orbit run readiness` and the
  backlog-exclusion listing: an excluded task is reported with the
  tag's reason (`no_drain_tag`). Re-run the same check after removing
  the tag to confirm the task is eligible again.
