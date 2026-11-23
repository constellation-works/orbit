# Settle a rescued blocked task

## Contents

- [Close a rescued blocked task](#close-a-rescued-blocked-task)

## Close a rescued blocked task

When a task's delivery PR was merged by hand, Orbit settles the task to `done`
from the merge. Each `orbit run auto` pass settles a `blocked`, `in-progress` or
`review` task whose delivery PR is among the recent merges into its target
branch, when no run or claim of its own is live. To settle one task now, run
`orbit task settle-merged <task-id>` in the owning workspace's checkout.

It reads
each PR Orbit recorded for the task, including a held or kept PR from a released
or failed claim even without an accepted handoff. That PR must name the task and
be merged into the landing branch its claim's admission receipt recorded; a
missing or expired receipt cannot prove it.

A held local delivery's PR that was
brought up to its base before merging (such as with `gh pr update-branch`) settles
too, when the commits added after the published head are only clean merges of
target-branch commits; a merged head with any other commit does not. An owed
post-deploy measurement still requires the operator's met record.

A claimed handoff whose completion authority an operator revoked keeps its
claim `handed_off` and its task in `review`; Approve is refused and the
revocation is final. When exactly that handoff's PR merged into its landing
branch outside Orbit, `orbit task settle-merged <task-id>` settles it: one
owner store write ends the claim as `revoked` and moves the task to `done`.
It is refused while an owner landing job still carries the handoff or a merge
intent is unresolved.

When the PR will not merge, recover the claim instead,
choosing any status the task lifecycle allows from `review` (`blocked`,
`backlog`, `archived`, `rejected`, or `review` to keep it there). Recovering a
handed-off claim to `done` is also one write, and keeps the ordinary
completion gates: the task's execution summary, any owed post-deploy
measurement and applied mutation proposals; a refused target names the rule.

The dashboard's Recovery panel offers one **Recover claim** action with a
status picker of exactly those targets, beside the required reason and a
confirmation. The owner console's authority line names both
steps. The command names the reason when the task
does not settle. The settlement starts no work, so another task's claim on the
same files does not refuse it.

History records it as `external_merge_settled`, with a
landing note naming the PR and merge commit
([stuck job runs](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/stuck-job-runs.md#a-delivery-pull-request-merged-outside-orbit)).

Settling to `done` also closes the task's other open Orbit-authored delivery
and preservation PRs. Each gets a `superseded by #N` comment naming the landing
PR and a `pull_request_closed` task-history entry. The branches are kept;
human PRs and other tasks' PRs stay open. Closure is best effort: if the forge
fails, settlement still stands and a warning is recorded. Inspect the task
history and any closure warnings before closing a remaining stale PR by hand.

If a follower's handoff was refused before its PR was recorded, and an operator
rebased and merged that PR, recover the stopped claim to `blocked` first. Then,
in the owning checkout, use one audited settlement command:

```bash
orbit task settle-merged <task-id> --recovered-claim <claim-id> --pull-request <number> --published-head <full-original-commit>
```

The original head is the follower's published candidate before the rebase. It is
an operator attestation when the refused handoff left no owner-held candidate;
existing candidate evidence must agree. The owner verifies the PR's delivery
branch, task identity and delivery author, its merged head and merge commit,
and the original admission's landing branch.

Missing or expired receipts, later
claims, live runs, dispatched landing attempts, unresolved merge intents and owed
post-deploy criteria refuse settlement. History records `operator-rescued`, the
operator-reported original head and the forge's merged head and merge commit.
The task reaches `done` without force and the claim stays `revoked`. The follower
closes its pending settlement on the next delivery, logs that once and stops
retrying; the obsolete PR head is not observed again.

When the work landed some other way (a commit, or a PR that is not the task's
delivery PR), close the task rather than rerunning it. First confirm that no
run is live on it and that the merged commit meets its acceptance criteria.
Then, as an operator in the owning workspace's checkout, run:

```bash
export ORBIT_OPERATOR=1
orbit tool run orbit.task.update --input '{"id":"<task-id>","status":"in-progress","execution_summary":"<what landed: PR, commit, evidence>"}'
orbit tool run orbit.task.update --input '{"id":"<task-id>","status":"review"}'
orbit tool run orbit.task.update --input '{"id":"<task-id>","status":"done"}'
```

The `execution_summary` on the `blocked → in-progress` write makes it a close
rather than a start. No work starts, so another run's execution claim on the
same files does not refuse the write, and you don't need `--force`. Add `plan`
to that write if the task has none. Operator capability comes from an
interactive terminal or `ORBIT_OPERATOR=1`; an `ssh host '<command>'`
invocation has no terminal, so it needs the variable.

Do not set
`ORBIT_AGENT_*` or pass CLI `--model`: either names an agent and makes the
write an agent's. Without the summary, from
an agent, or without operator capability, the write starts work. While a
claim overlaps, that start is refused with the claim's task and run named.
The refusal covers the whole write: field edits sent with the start (a
title, a plan, artifacts) do not persist, so there is nothing to inspect
or revert afterwards.

Send an edit on its own when it should land whether
or not the start does.

The CLI subcommand also closes without `--force`: run
`orbit task update <task-id> --status in-progress --execution-summary "<…>"`,
then `--status review` and `--status done`. From an agent shell (`ORBIT_AGENT_*`
or a managed run), the CLI's move into `in-progress` starts work like the tool
does and is refused with the claim's task and run named while another run's
claim overlaps. Keep `--force` for an edge the lifecycle refuses. It records an
override in task history.
