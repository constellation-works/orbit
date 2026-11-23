# Recover a partial implementation or review

## Salvage an incomplete candidate

A run that stopped with a candidate is not a reason to start over. This
includes a failed or cancelled run, an interrupted review, or a blocked
claim. Orbit already keeps what it can. A claimed leaf's candidate stays with
its released claim. A reviewer stopped mid-review leaves its uncommitted
repair as a marked commit at `refs/orbit/candidates/<task>/<run>-review`.
The attempt's report stays on the task. The task's next claim continues from
those automatically.

An owner-local delivery leaf that died after opening its
pull request, with its task in `review`, is resumed on that pull request once by
the owner drain's next pass, when that drain holds the leaf's `--complete` and
`--auto-fix` grants. An `--auto-fix` drain also hands a conflicting or red pull
request of a task with no live run back to its stopped leaf's completion step,
unless an operator holds the task. When you
must decide what happens next, follow these steps:

1. **Diagnose the preserved worktree.** On the host that owns it (`orbit run
   show <run-id> --json` names it), read it yourself or run your provider's
   CLI read-only in it. Reach a verdict:
   - Is the candidate salvageable?
   - Which review findings still hold?
   - What work remains?
   - Is a design decision needed?

   Point it at the candidate head, the kept repair ref and the task's
   `review-report.json`. Record its answer on the task as a comment or as
   remaining work. The diagnosis is read-only. It must not commit, push,
   edit a pull request, or change the task. Say so to any agent you run, and
   check afterwards that it did not.
2. **Route by cause.**
   - **Transient** (host restart, provider outage, a killed worker, a forge
     refusal): resume the stopped run with `orbit job resume <run-id>` where
     it is resumable, or requeue the task. A completion held on unreadable or
     throttled check evidence can be resumed this way; it re-observes the
     existing pull request and checks its head before proceeding.
   - **Defects** (findings that hold, missing work): record them as remaining
     work on the task, then return it to `backlog` and re-ship it. The next
     run resumes the kept candidate and repairs it; it does not re-implement.
   - **Design decision** (approach, scope or acceptance criteria in doubt):
     escalate to the human who owns the decision, with the diagnosis attached.
     Do not dispatch until it is decided.
3. **Last resort.** Use operator subagents or a manual rebase only when the
   routes above cannot recover the candidate, for example because the kept
   ref cannot be fetched or the repair no longer applies. Record on the task
   what was done, by whom, and which commits resulted.

An owner's own run stopped mid-review keeps the same marked commit and ref,
and a task comment names them. Its next run does not apply that repair
automatically. To keep it, follow the steps above and fold the repair into
the remaining work. Discard a candidate only deliberately, with
`orbit task update <task-id> --discard-candidate --status backlog`.
