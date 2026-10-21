# Routing findings into repairs

## CI and QA: verify freshness before filing

Use `ci_failure_sweep_pipeline` for CI discovery. Its current admission path
files proposed repairs, runs pilot, and can promote its own warning-free,
selector-backed tasks; it does not implement them. Inspect the resulting task
and admission evidence rather than duplicating that work. See
[workflows.md](workflows.md).

Before filing or dispatching a repair, compare the failing run/job and SHA
with the current landing branch and prior fix PRs. An old CI failure can arrive
after the repair merged; the sweep holds such failures in
`pending_supersession` rather than filing them, so read that list in the
sweep's step output before filing by hand. A red run is held there for a newer
push run still in flight at a descendant commit only while it is a lone
failure: once the previous completed run failed the same job with the same
normalized error signature, the sweep files it (`reproduced_on`), and a hold older than
`pending_supersession_window_minutes` (default 30) is filed with the pending
run named in the task (`held_past_window`). A failure that sits in
`pending_supersession` across sweeps on a busy branch is therefore not
waiting on you; one that never gets filed is a sweep defect. Attach the exact failing command/log excerpt and
current reproducibility evidence to one bounded task. Search open and closed
history; reject proven duplicates with a link to the delivered fix. Cancel a
duplicate's active child only within authorization and after inspecting its
state; do not cancel the whole drain.

When a hand fix is open, archive the sweep finding with a `covered_by`
relation in the same task update. Preserve all existing relations and append
`{"type":"covered_by","target":"<covering task id>"}` or
`{"type":"covered_by","target":"github-pr:https://github.com/OWNER/REPO/pull/NUMBER"}`.
`github-pr:NUMBER` uses the checkout's GitHub repository. This is durable
operator coverage of the exact failure key: later sweeps report `covered`
with the archived owner and cover, and create no pilot candidate. A compiler
error's key includes the checkout, so the hold also follows the same complete
diagnostic set (paths and messages, ignoring line and column) to later
checkouts; an owner filed before that set was recorded in its description
holds only at its own checkout. A rejected finding can carry the same
relation. A missing or unreadable cover remains withheld with
`operator_cover_unavailable`; inspect that reason rather than assuming the PR
is open. A closed, unmerged PR (or archived/rejected cover task)
releases the key. After a merged cover's commit is in the failing checkout,
the sweep files a new repair and names that cover as a fix that did not hold;
older checkouts stay covered while waiting for the fix.

A plain archive or rejection without `covered_by` suppresses the exact key
(and, for compiler errors, the same diagnostic set at later checkouts) for
`ci_failure.operator_suppression_hours` (default 6) from the operator's
status decision. Its report entry is `withheld`, reason `operator_archived`,
with the owner and expiry. After expiry a current failure can file again.
Matching normalized failing-test signatures across jobs share one task with
all job names and source identities; different signatures remain separate.
Read `file.withheld` and its matching audit entries alongside
`skipped_existing` and `pending_supersession` when assessing CI relief.

Post-merge code review and QA follow the same loop. Exercise real user paths
and report concrete defects; do not replace verification with an agent's
claim that the change is correct. Pilot and promptly promote authorized
repairs while independent work continues.

Run an authorized sweep on the owning workspace with:

```bash
orbit run job ci_failure_sweep_pipeline
```

Check whether its routine or a manual invocation is already active first.

## Diagnose failed runs before retrying

```bash
orbit task show <task-id>
orbit run show <run-id> --json
orbit run logs <run-id> --step <step-id> --json
```

A failed run can leave its task `blocked` with the failure attached. The owner's
clock can dispatch final recovery for an eligible block; inspect its decision
and current task state first. See [automation.md](../../orbit-setup/references/automation.md#built-in-final-recovery-of-blocked-tasks).
If it remains blocked, read the evidence yourself, then
make the transition deliberately — return the task to `backlog` only once you
know why it failed and that a rerun can succeed. A sandbox denial or provider
failure is not inherently transient; repeated identical failures need a repair
or configuration correction before another attempt.

When a run stops on a rebase conflict, final recovery can repair the conflict
files and resume. The next conflict recovery adopts those repairs and continues
the stopped rebase, so don't abort it or stage files by hand. Adoption still
refuses leftover conflict markers, whitespace errors, an untouched conflict, and
a moved HEAD or index.

A live process is not stopped merely because a tool observation timed out.
Re-poll the same run and inspect current process liveness. Conversely, a stale
lock file alone does not prove a worker is alive. Follow
[run-debugging.md](run-debugging.md) before process-level
intervention, and never weaken protected-path policies to make a retry pass.

## Salvage an incomplete candidate

A run that stopped with a candidate is not a reason to start over. This
includes a failed or cancelled run, an interrupted review, or a blocked
claim. Orbit already keeps what it can. A claimed leaf's candidate stays with
its released claim. A reviewer stopped mid-review leaves its uncommitted
repair as a marked commit at `refs/orbit/candidates/<task>/<run>-review`.
The attempt's report stays on the task. The task's next claim continues from
those automatically. An owner-local delivery leaf that died after opening its
pull request, with its task in `review`, is resumed on that pull request once by
the owner drain's next pass, when that drain holds the leaf's `--complete` and
`--auto-fix` grants. An `--auto-fix` drain also hands a conflicting or red pull
request of a task with no live run back to its stopped leaf's completion step,
unless an operator holds the task. When you
must decide what happens next, follow these steps:

1. **Diagnose with `orbit_agent_invoke`.** Run it on the host that owns the
   preserved worktree, with `cwd` set to that worktree (`orbit run show
   <run-id> --json` names it), or `orbit run agent --cwd <worktree>
   "<prompt>"`. Ask for a verdict:
   - Is the candidate salvageable?
   - Which review findings still hold?
   - What work remains?
   - Is a design decision needed?

   Point it at the candidate head, the kept repair ref and the task's
   `review-report.json`. Record its answer on the task as a comment or as
   remaining work. The invocation is diagnosis only. It must not commit,
   push, edit a pull request, or change the task. Say so in the prompt, and
   check afterwards that it did not.
2. **Route by cause.**
   - **Transient** (host restart, provider outage, a killed worker, a forge
     refusal): resume the stopped run with `orbit job resume <run-id>` where
     it is resumable, or requeue the task.
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

## Close a rescued blocked task

When a task's delivery PR was merged by hand, Orbit settles the task to `done`
from the merge. Each `orbit run auto` pass settles a `blocked`, `in-progress` or
`review` task whose delivery PR is among the recent merges into its target
branch, when no run or claim of its own is live. To settle one task now, run
`orbit task settle-merged <task-id>` in the owning workspace's checkout. It reads
each PR Orbit recorded for the task, including a held or kept PR from a released
or failed claim even without an accepted handoff. That PR must name the task and
be merged into the landing branch its claim's admission receipt recorded; a
missing or expired receipt cannot prove it. An owed post-deploy measurement still
requires the operator's met record.

A claimed handoff whose completion authority an operator revoked keeps its
claim `handed_off` and its task in `review`; Approve is refused and the
revocation is final. When exactly that handoff's PR merged into its landing
branch outside Orbit, `orbit task settle-merged <task-id>` settles it: one
owner store write ends the claim as `revoked` and moves the task to `done`.
It is refused while an owner landing job still carries the handoff or a merge
intent is unresolved. When the PR will not merge, recover the claim to
`blocked` or `backlog` instead. The owner console's authority line names both
steps. The command names the reason when the task
does not settle. The settlement starts no work, so another task's claim on the
same files does not refuse it. History records it as `external_merge_settled`, with a
landing note naming the PR and merge commit
([stuck job runs](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/stuck-job-runs.md#a-delivery-pull-request-merged-outside-orbit)).

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
invocation has no terminal, so it needs the variable. Don't pass `model`,
because naming an agent makes the write an agent's. Without the summary, from
an agent, or without operator capability, the write starts work. While a
claim overlaps, that start is refused with the claim's task and run named.

The CLI subcommand also closes without `--force`: run
`orbit task update <task-id> --status in-progress --execution-summary "<…>"`,
then `--status review` and `--status done`. From an agent shell (`ORBIT_AGENT_*`
or a managed run), the CLI's move into `in-progress` starts work like the tool
does and is refused with the claim's task and run named while another run's
claim overlaps. Keep `--force` for an edge the lifecycle refuses. It records an
override in task history.

## A PR exists but completion failed

Inspect the failed step and GitHub state independently. A `complete_pr` error
can be a repository merge-method or auto-merge setting mismatch even when
there are no rebase conflicts. A failure-handoff PR is preserved work, not
proof that it is safe to merge or that the task is done.

Check the candidate head/base, mergeability, checks, and repository settings.
Use an allowed merge method through the supported recovery path. Change a
repository setting only when the user's authorization covers that change;
opening a PR does not grant that permission. Verify actual merge and reconcile
task state with the evidence. Do not rerun implementation solely because the
completion step failed.

## Explicit source repair of stopped owner publications

An operator on the owning host can inspect a published candidate without
resuming its failed attempt:

```bash
orbit tool run orbit.workflow.run.resume --input '{"id":"<source-run>","source_repair":{"action":"inspect","task_id":"<task-id>"}}'
```

Submit the returned `binding` unchanged with `action: "submit"`, the same
`task_id`, a new bounded `request_key`, and a `diagnosis` naming the observed
failure and remaining source work. This is an explicit source decision. It
creates a new attempt with current required commands, configured review,
`completion: "review"` and `auto_fix: false`. It grants no merge authority.
Keep the identical key, binding and diagnosis when a submission reply is lost:
the durable reservation/link returns the same successor, even after it stops.
Each newly published head needs another explicit inspection and fresh key;
automatic fixes and explicit generations share the PR repair budget. Generic
resume still cannot replay an admitted source-repair attempt.

In addition to cancelled publications and failed publications with an applied
requeue, inspection supports two conclusively stopped failed owner shapes
after final recovery escalated:

- **An admitted source repair pushed before `pr_open` failed.** Its original
  system reservation and unique run link, successful branch adoption, host
  commit, prepare, synchronization, review/publication chain and push must
  agree. The original admission supplies the existing PR identity when no
  successful `pr_open` checkpoint exists. The old source evidence remains
  immutable. Host-recorded selector additions from that attempt are accounted
  for when comparing its original task scope; other meaning/scope changes
  refuse admission.
- **The terminal failure hook published a repair.** The durable
  `pr_failure_handoff` checkpoint must name the exact task, run, failed step,
  setup owner/base, preservation commit, push and PR. Its commit must have an
  authenticated host candidate or immutable setup base as its preservation
  parent. The handoff's recorded task specification must still match. That
  persisted specification covers description and acceptance criteria; it does
  not certify historical selector hints or review approval. Inspection binds
  the full current meaning/scope for submission and source application. The
  retained handoff commit is selected even when it advances past the failed
  validation candidate. If its worktree and local branch are gone, existing
  exact commit objects and the unchanged remote branch lease permit
  reconstruction. A local branch present at another head still refuses.

Neither route treats publication as validation. The new worktree integrates
the exact retained candidate onto the current remote base, preserves its PR
and branch under the exact lease, and requires fresh complete host gates and
normal configured review. A handoff repair was not certified by the old
candidate's checks. Failure receipts and historical checkpoints are retained;
there is no requeue/history rewrite or claimed held-evidence amendment here.

Both inspection and submission freshly observe the origin repository,
remote branch and provider PR number, URL, head, branch and base branch. The
PR must remain open and unmerged with automatic merge explicitly disabled;
a draft remains a draft. Missing, contradictory or changed evidence refuses
before reservation. A live/foreign/claimed source or a newer attempt that
may have written a candidate cannot be bypassed by selecting an older head.
Newer owner attempts between the source and the task's current binding are
bound together in `binding.superseded`, and a change to any of them voids
the binding. Each must be conclusively stopped and either cancelled, or failed
at or before candidate adoption with a settled (escalated or requeued) final
recovery, no failure handoff other than one that published nothing
(`no_candidate`), and a clean retained worktree. A failed attempt that reached
implementation or left work behind is a resume candidate, not a successor to
set aside
([stuck job runs](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/stuck-job-runs.md#a-cancelled-published-source-repair)).
Diagnose the named missing receipt or identity mismatch, then inspect again;
do not invent a checkpoint, copy another run's admission, or weaken gates.

## Repair tasks and activity boundaries

Prefer a bounded task for actionable tooling, documentation, or operational
friction. A separate friction artifact is optional, not a prerequisite; honor
a user's preference to file tasks only. Avoid duplicate records that describe
the same fix without adding useful evidence.

Agent output is advisory, not an authoritative activity success contract. Do
not add generic output-schema enforcement or retries that force a model to
produce a particular report shape. Deterministic operations validate the
inputs they consume and the state they change at their own boundary. Verify
actual files, persisted selectors, tests, and delivery outcomes. Improve a
misleading prompt or diagnostic narrowly when evidence supports it.

## Verify deployment separately from merge

When installation or service recovery is in scope, a merged source fix is
only an intermediate result. On the owning host:

1. Verify the executable actually invoked, service command, config, source
   revision, and checkout state. Preserve operator configuration overrides.
2. Build from the intended revision in an appropriate checkout, install to the
   actual executable location, and synchronize managed assets through the
   supported mechanism. Do not assume matching version strings mean matching
   binaries, or overwrite operator changes to make a source checkout clean.
3. Restart only the intended services. Verify their process/executable identity,
   sustained health and the repaired behavior; an immediate successful health
   response can miss a server that exits shortly afterward.
4. Record installed revision/hash, service result, and any remaining mismatch.
   Restore temporarily changed routines/settings unless the user made the
   change permanent.

See [maintenance.md](../../orbit-setup/references/maintenance.md) for supported
sync/upgrade mechanics. If capability or authority is missing, report it;
never create a shadow store or edit runtime state directly to bypass the gap.
