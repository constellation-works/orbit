---
type: runbook
summary: Read a before-PR review's verdict, reviewer commit and findings comment, and decide what to do with a task the gate blocked.
tags: [operations, review-gate, delivery]
paths: ["crates/orbit-core/src/application/review/**", "crates/orbit-core/assets/jobs/task_pr_pipeline.yaml", "crates/orbit-engine/src/executor/automation/vcs/failure.rs"]
related_features: [review-gate]
related_artifacts: ["ORB-13989", "ORB-13992", "ORB-14194"]
last_validated: 2026-10-04
---

# Operate the Before-PR Review Gate

Use this runbook when `review.before_pr = true` and you need to
read what a review did to a delivery, or a task is `blocked` with a
`review_gate_escalation` event.

## 1. What the gate does

One fresh reviewer, from `operation.review_crew`, examines the implementation
commit, fixes what it finds, and returns a verdict. There is no findings-driven
rework loop. If PR completion rebases a conflicting reviewed head, the pipeline
may admit one fresh re-review in the same lineage; a second conflict leaves the
task in review as `review_gate_stale`.

| Verdict | Candidate branch | Delivery |
| --- | --- | --- |
| `accept` | the implementation commit(s) only | the PR opens on that head |
| `accept_with_fixes` | the implementation, then one `review: <summary>` commit authored by `<family>-reviewer` | owner validation reruns on the reviewer commit and its paths widen the task's selectors; the PR opens with a "Review fixes" section |
| `reject` (or `incomplete`) | whatever was committed, kept | the task is blocked, no PR is opened, and final recovery gets one look |

The implementation commit is never amended. A failed revalidation of the
reviewer commit is reported as `reject` too, even though the certificate
records `accept_with_fixes`.

## 2. Inspect

Placeholders: `<task-id>` is the blocked task, `<run-id>` the delivery run in
its status note, and `<branch>` the candidate branch the handoff names.

```bash
orbit task show <task-id>                 # status, the escalation note, comments
orbit task show <task-id> --json | jq .review   # verdict, commits, findings, budget
orbit run show <run-id>                   # which step failed
git log --format='%h %an %s' <base>..<branch>   # the implementation and reviewer commits
```

The settlement posts one comment per task. For example (excerpt):

```text
before-PR review settled attempt `rvw-…`: verdict **accept_with_fixes** (assurance: …).

Findings:
- `F1` [high, fixed] Missing bounds check on the reader
  - Changed: added the length check and a test (src/reader.rs, tests/reader.rs)
- `F2` [low, disposed: intended behaviour] Log level

- Reviewer: crew `reviewers` (…)
- Implementation: `<sha>` on base `<sha>` (1 commit(s), unchanged by review)
- Reviewer commit: `<sha>` `review: …` by <family>-reviewer
…
```

The full evidence is in the task artifacts `review-manifest.json`,
`review-report.json`, and `review-gate.json`:

```bash
orbit tool run orbit.task.artifact.get --input '{"id":"<task-id>","path":"review-gate.json"}'
```

## 3. Decide a blocked review

Identify the failed step from `orbit run show`, then act:

- **`review_gate_settle` refused with `review_gate_blocked`**: the reviewer
  left an open finding (`reject`) or could not finish (`incomplete`). Read the
  open findings and the `Escalation:` line in the comment. Fix or re-scope the
  task, then re-queue it for a fresh run with
  `orbit task update <task-id> --status backlog`. A fresh run starts a new
  review lineage with a full budget.
- **`review_validate` failed**: the reviewer's fixes broke a required
  command. The handoff pushed both commits, and the failure text names the
  command. A reviewer path outside the task's selectors does not fail the
  step; it widens the selectors, recorded as a `context_files_widened` entry
  in task history. If a widened path is not in intent, record that on the
  task. Then re-queue it.
- **`review_gate_admit` refused with `review_budget_exhausted`**: the
  candidate already had its one review (`review_candidate_reviewed`) or its
  reviewer spent `review.minutes` (`review_minutes_exhausted`). Renew it only
  with a recorded decision:
  `orbit task review-reset <task-id> --lineage '<exact-lineage-key>' --reason '<decision>'`.

- **A claimed reviewer reported `incomplete` because its manifest or report
  call was refused** (distributed drain follower): the reviewer's
  `orbit.task.artifact.get`/`put` reaches the owner only through the run's
  coordinator, the step runner outside the sandbox, which carries those two
  calls and nothing else. The refusal names the cause:
  `review_attempt_stale` (the reviewer ran past its attempt or the attempt
  was settled), `review_manifest_stale` (the owner holds another attempt's
  manifest), `claimed_review_bridge_refused` (another activity, task, path or
  field), or "could not reach this run's coordinator" (the step runner
  stopped). Each is safe to retry with a fresh run once the cause is gone; a
  `capability_denied` naming a missing `ORBIT_PLUGIN_BROKER` means the
  follower's binary predates the route and must be upgraded first. Never add
  SSH credentials to the sandbox or attach a report by hand. See
  [claimed-review artifacts](./claimed-review-artifacts.md).

If the run's final recovery already settled the task (for example archived or
requeued it), the failure handoff did not run; read that decision on the task
before acting.

A follower delivery whose pull request already merged at a head other than its
reviewed candidate cannot be re-gated: the merged head is immutable. Reconcile
it with `orbit task reconcile-review`, as described in the
[distributed drain runbook](./distributed-drain.md). A reconciliation is a
separate owner record, not a review-gate certificate.

## 4. Verify

After the next delivery run, `orbit task show <task-id> --json | jq .review.verdict`
reads `accept` or `accept_with_fixes`, and the PR body of an
`accept_with_fixes` run ends with a `## Review fixes` section naming the
reviewer commit.

## 5. Related references

- [Review gate design](../design/review-gate/2_design.md) — verdicts, the
  two-commit shape, revalidation, budgets, and coverage.
- [CONFIG.md](../CONFIG.md) — the `operation.review_*` keys.
- [Claimed-review artifacts](./claimed-review-artifacts.md) — the follower
  reviewer's coordinator route, its smoke procedure and rollout record.
- [Recover stuck job runs](./stuck-job-runs.md) — runs that never reached the
  gate's failure handoff.
