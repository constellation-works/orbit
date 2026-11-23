# Requeue failed runs

## Failed runs

A failed run can park its task in `blocked` with the failure attached. The
owner's clock can dispatch final recovery for an eligible block; inspect its
decision before intervening. See [automation.md](../../orbit-setup/references/automation.md#built-in-final-recovery-of-blocked-tasks).
If it remains blocked, read the run and decide whether a rerun can succeed
before deliberately returning it to backlog.

```bash
orbit task list --status blocked
orbit run show <run-id> --json
orbit task update <task-id> --status backlog   # only once you know a rerun can succeed
```

Every blocked-to-backlog return, including a plain
`orbit task update <task-id> --status backlog`, runs the same publication
pre-flight as `orbit task requeue`. Once the task's run has stopped, an
operator can use `orbit task requeue <task-id> --reason '<why>'` (MCP
`orbit.task.recover` action `requeue`) to resume that candidate; while the run is live the
requeue is refused, since its failure handoff may still be publishing.

Add
`--discard-candidate` to close the task's undelivered Orbit PRs and rerun fresh,
which a changed task specification requires; `orbit task update <task-id>
--status backlog --discard-candidate` is the same discard and closure without a
recorded requeue decision.

A claimed leaf that failed or escalated with a
committed candidate publishes no `[BLOCKED]` PR; the owner keeps its candidate
(the task's `claim_candidate_kept` history event), and the same requeue reports
and resumes it (`resumes.source: claim`) under the same specification and
discard rules.

For blocks caused by a missing provider launcher or by transient
infrastructure (a busy lock, a locked task store), `orbit task recheck-blocked`
reports whether the condition cleared. Its `--confirm` option requeues only
the cleared blocks, a transient one onto its published candidate once its run
has stopped; it leaves implementation failures blocked, and a transient block
a human commented on, or whose publication did not authenticate, for
`orbit task requeue`.
