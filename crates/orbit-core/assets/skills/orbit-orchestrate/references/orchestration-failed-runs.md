# Requeue failed runs

## Failed runs

A failed run can park its task in `blocked` with the failure attached. The
owner's clock can dispatch final recovery for an eligible block; inspect its
decision before intervening. See [automation-built-in.md](../../orbit-setup/references/automation-built-in.md#built-in-final-recovery-of-blocked-tasks).
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
reports whether the condition cleared. Without `--confirm` it only reports.
`--confirm` requeues only the cleared blocks and leaves a task blocked by its
own failure blocked. A human's comment or write after the block, or a
publication that did not authenticate, stays for `orbit task requeue`.

The launcher is resolved with dispatch's lookup from that shell's PATH and
HOME. A transient block is requeued only once its run has stopped, when the
task store takes a write transaction and the task's own lock is free; the
probe cannot take the lock that timed out. The next run resumes the candidate
the failure handoff published. Each published candidate is requeued this way
at most twice; after that use `orbit task requeue`.
