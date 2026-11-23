# Executing a task: refusals and special handoffs

Contents:

- [Upgrade admission refusal](#upgrade-admission-refusal)
- [Verified no-change implementation](#verified-no-change-implementation)
- [Invalid review finding](#invalid-review-finding)
- [Sanctioned sandbox deferral](#sanctioned-sandbox-deferral)
- [Read-only delegation](#read-only-delegation)

## Upgrade admission refusal

An `orbit` command run inside a managed activity can be refused at once with
`upgrade admission refused: [upgrade_pending] a generation switch is pending: …`
while an Orbit upgrade holds admission. This is not a task blocker and not a
transient error to work around. Do not retry the command, wait for the upgrade
to finish, or reach the same result another way.

Stop the step and leave the
worktree as it is: keep every uncommitted change, and do not commit, revert or
clean up. Return `result.blocker` with kind `upgrade_pending` and the refusal
text as `evidence`, saying in that evidence what is done and what is not run.
Use this kind only for this refusal; a failing check or a missing prerequisite
is a different outcome. A reviewer whose step is refused the same way follows
this too.

The step then ends on `upgrade_pending`. Whether the task returns to `backlog` or
blocks depends on who ran it and the task's claim release budget:

- **Owner run:** the task is never blocked. Run finalization requeues it to
  `backlog` (`upgrade_pending_requeued`) and keeps the candidate — it commits what
  you left and pushes that commit to a durable ref on `origin` — and the next run
  resumes it once the upgrade settles.
- **Claimed leaf:** the leaf releases its claim as `transient`, which counts
  against the task's 2-in-24h release budget (an ordinary `upgrade_pending` refusal
  carries neither `provider_limit` nor `owner_busy`). Within budget
  (`claim_released`), the task returns to the owner's `backlog` and its next claim
  resumes the kept candidate.
  - On the 3rd+ budgeted release within 24 hours, the
    owner instead fails the claim and moves the task to `blocked`, so no next claim
    can start until an operator moves it back to `backlog` (any committed candidate
    is still kept).
  - Identify that blocked state on the owner via
    `orbit task show <task-id>` (`orbit.task.show`) and
    `orbit run settlements --json`:
    - `status: blocked`,
    - the
      `claim_release_budget_exhausted` history event (`note` starting
      `release budget exhausted (2 in 24h): …`),
    - the `Blocked: follower drains released
      this claim …` comment listing each counted release, and
    - on the settlement/claim
      `phase` `failed` and `last_event` `claim_release_budget_exhausted`
      (`release.budget_exhausted: true` on the stored claim record).

A claimed PR leaf that already published its head reuses it and needs no extra
ref. A run on another host resumes the candidate only when it was pushed or
published. If `origin` refuses the push, the candidate stays on the host that held
it: a run there still resumes it, and a run on any other host starts the task
afresh.

For an owner run, the task's `candidate_held` history entry records which
case applied; a refused push reads `host-local` there and in the `## Orbit
upgrade` comment. For a claimed leaf, read the owner's `claim_candidate_kept`
entry instead: a refused push reads `only on <machine>` there, and no
`candidate_held` entry or comment exists. Do not push or repair the candidate ref
yourself.

## Verified no-change implementation

This section applies only to tasks without `no-diff-expected`. When the
implementation correctly needs no file changes, and all required
validation passes, attach `no-diff.json` and one captured validation log per
check with `orbit.task.artifact` action `put`. This is only for a genuine validated
no-op; never use it to bypass failed validation or pending worktree changes.
The evidence must match the pinned task, current run (or its recorded retry
lineage), and tested HEAD:

```json
{
  "schema_version": 1,
  "task_id": "<current-task-id>",
  "run_id": "<current-run-id>",
  "tested_head": "<pinned-head-sha>",
  "reason": "The requested behavior already holds; no change is required.",
  "validation": [
    {
      "command": "<required validation command>",
      "exit_code": 0,
      "log_artifact": "validation.json"
    }
  ]
}
```

Each referenced validation log is JSON with `run_id`, `tested_head`,
`command`, `exit_code` set to `0`, and captured `output`. Commands must be
non-empty and unique. The commit verifier also requires a clean worktree and
the current HEAD to equal `tested_head`; otherwise reconcile the changes and
rerun validation before attaching the evidence.

## Invalid review finding

A review-filed bug (type `bug` tagged `code-review`, `delivery-code-review`,
`security-review` or `qa-sweep`) can be wrong. Confirm the defect at your
base before editing.

When it does not exist there (the code does not do what
the finding claims, or the claimed consequence cannot happen), change
nothing, run no workspace gates, and attach `finding-invalid.json` with
`orbit.task.artifact` action `put`. It is not for a defect already fixed (use
already-landed or no-diff) or one you judge not worth fixing (that is the
operator's call):

```json
{
  "schema_version": 1,
  "task_id": "<current-task-id>",
  "run_id": "<current-run-id>",
  "tested_head": "<pinned-head-sha>",
  "finding_fingerprint": "<finding_fingerprint from your task context>",
  "reasoning": "Why the defect as described does not exist at tested_head.",
  "evidence": [
    {"location": "src/path.rs:42", "note": "What this line shows."}
  ]
}
```

Each `location` is a repository-relative `path:line` naming an existing
line at `tested_head`; at least one is required, and every `note` is
non-empty. The commit step requires a clean worktree at `tested_head` and a
report from this run or its retry lineage. It then records a REJECTED
finding verdict (source `implementer`), moves the task to `rejected`, and
lands nothing.

The fingerprint binds the report to the finding you judged: if
the task was edited or left `in-progress` since your prompt, the commit step
refuses the report and the task keeps its state. Still write the execution summary with `Outcome: success`,
saying the finding is invalid; a failed outcome refuses the handoff. A
claimed run cannot reject the owner's task: return `result.blocker` with
kind `finding_invalid` and the same evidence instead.

## Sanctioned sandbox deferral

Before attempting or claiming a sandbox deferral, read and follow this
subsection. Only a required affected-test gate may qualify, and it must exit 0
while printing only notices for tests whose nested sandbox the agent lane
refuses: `DEFERRED: bubblewrap unavailable:` on Linux,
`SKIP: sandbox-exec cannot apply a profile:` on macOS, or `DEFERRED: ps denied:`
on a claimed macOS leaf.

A single gate may report multiple notice kinds only
when every corresponding sandbox probe refuses in the implementer's lane.
For an eligible case, use this route instead of failing the step for the
deferred test coverage.

Run the gate with its comparison base pinned to `input.base_sha` and set
`ORBIT_VALIDATION_SUMMARY` to a new file under `.orbit/tmp/`. The summary must
record the gate command, exit status, executed-test count, pinned base and
notices. Return only the summary path in this handoff:

```json
{"deferred_sandbox_validation":{"summary":".orbit/tmp/<file>.json"}}
```

Do not add other output keys. Orbit derives and checks the record from the
summary, including that the command is a required gate, the exit status is 0,
tests ran, the base matches, and every notice is eligible for this lane. Orbit
runs one refusal probe for each notice kind; every probe must refuse.

It then
replays the gate outside the agent sandbox before delivery, and the replay must
run tests without deferral. In the execution summary, name the gate, summary
path and notices, and mark the deferred coverage `not_run`, never passed.

A required `orbit plugin test` gate in a macOS worker must also set
`ORBIT_VALIDATION_GATE` to the gate exactly as configured and
`ORBIT_VALIDATION_BASE` to `input.base_sha`. Without both values, the route
fails with `validation_route_unavailable` and runs no case.

No other check can be deferred. This includes a nonzero exit, any other `SKIP:`
or `skipping` notice, a gate that ran or selected no tests, a notice for a
sandbox this lane does not refuse, `DEFERRED: ps denied:` outside a claimed
macOS leaf, a host-required criterion, a `baseline_red` command, and every
other required command such as formatting, lint, goldens or static analysis.

## Read-only delegation

When the provider supports read-only subagents, use them only for focused
reading questions. Never delegate edits, validation, commits or dispatch.
Read the cited files yourself before relying on an answer. Orbit no longer
provides a separate explorer tool or activity.
