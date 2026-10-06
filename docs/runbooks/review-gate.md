---
type: runbook
summary: Inspect before-PR review verdicts, findings, and evidence holds, then decide how to resume a task.
tags: [operations, review-gate, delivery]
paths: ["crates/orbit-core/src/application/review/**", "crates/orbit-core/assets/jobs/task_pr_pipeline.yaml", "crates/orbit-engine/src/executor/automation/vcs/failure/handoff.rs"]
related_features: [review-gate]
related_artifacts: ["ORB-13989", "ORB-13992", "ORB-14194"]
last_validated: 2026-10-06
---

# Operate the Before-PR Review Gate

Use this runbook when `review.before_pr = true` and you need to read what a
review did to a delivery, or a task is `blocked` with a `review_gate_escalation`
event or `in-progress` while awaiting named external evidence.

## 1. What the gate does

One fresh reviewer, from `operation.review_crew`, examines the implementation
commit, fixes what it finds, and returns a verdict. There is no findings-driven
rework loop. If PR completion rebases a conflicting reviewed head, the pipeline
may admit one fresh re-review in the same lineage; a second conflict leaves the
task in review as `review_gate_stale`.

After a replacement push, GitHub's PR metadata can briefly report the previous
published head. Completion retries only that recorded SHA, checking with
`git ls-remote origin refs/pull/<number>/head` that the remote PR head already
names the new candidate. It makes at most three re-reads at the configured
poll interval, capped at 60 seconds and one quarter of the completion wait
budget. `merge.stale_head_observations` records the lag on successful delivery.
An unrelated SHA, changed or missing remote ref, or exhausted bound still
refuses with `delivery_evidence_stale` (or `review_gate_stale` when only the
review pin applies), leaving the task in review. Completion must observe the
exact candidate in PR metadata before sending the unchanged SHA-conditioned
merge request; waiting never extends the review certificate to another head.

| Verdict | Candidate branch | Delivery |
| --- | --- | --- |
| `accept` | the implementation commit(s) only | the PR opens on that head |
| `accept_with_fixes` | the implementation, then one `review: <summary>` commit authored by `<family>-reviewer` | owner validation reruns on the reviewer commit and its paths widen the task's selectors; the PR opens with a "Review fixes" section |
| `reject` (or `incomplete`) | whatever was committed, kept | the task is blocked, no PR is opened, and final recovery gets one look |
| Evidence-only `incomplete` (or legacy `changes_required`) | the reviewed candidate is kept unpublished | the run ends `held`, with no retry, step recovery, final recovery, or failure handoff; the task stays `in-progress` with `review_awaiting_evidence` |

The implementation commit is never amended. A failed revalidation of the
reviewer commit is reported as `reject` too, even though the certificate
records `accept_with_fixes`.

An evidence hold applies only when every remaining requirement is a named
unavailable external check and the report contains no open defect or failed
required check. `review-evidence-hold.json` pins the attempt, candidate,
task meaning, and each required artifact. Attach a passing `ReviewExternalEvidence`
result at each named path and its nonempty log artifact. Each result must match
the kind, exact command and candidate tree. Attempt, commit, display name and
artifact path changes do not expire a result on the same tree. Unrelated,
different-tree, failed or incomplete evidence leaves the hold in place.

Receipt of all matching evidence queues the task in `backlog` with
`review_evidence_received` for a fresh review. It does not approve the candidate
or resume the terminal held run. Fresh admission includes verified result/log
pairs in `satisfied_external_evidence`; settlement re-reads them on the final
tree and satisfies repeated unavailable checks without another hold. A repair
that changes the tree needs new evidence. An operator status decision or changed task
meaning prevents automatic receipt from overriding that decision. Held runs
are settled outcomes for pipeline waits and are excluded from reliability's
success/failure denominator.

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
  manifest), `stale_claim` (the owner released, failed, revoked or superseded
  the claim), `claimed_review_bridge_refused` (another activity, task, path or
  field), or "could not reach this run's coordinator" (no usable response).
  Preserve the exact call and inspect the task, run, claim and review ledger
  before recovery. Do not replay a stale, expired, settled or cancelled
  attempt. A transport loss has an unknown outcome and must use existing
  idempotent/reconciliation behavior; it does not authorize a new claim or
  report. Only after normal terminal settlement, no live owner and cause
  diagnosis may existing task authorization start a fresh attempt. A
  `capability_denied` naming a missing `ORBIT_PLUGIN_BROKER` is a launch or
  binary capability failure; confirm the installed executable hash and launch
  configuration. Never add SSH credentials to the sandbox or attach a report
  by hand. If report PUT is refused, the gate fails closed on its missing or
  invalid artifact. See
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

After the delivery run, read `review-gate.json` through the public artifact
tool and verify its `verdict`, `validation_complete` and `final_candidate`:

```bash
orbit tool run orbit.task.artifact.get --input \
  '{"id":"<task-id>","path":"review-gate.json"}'
orbit run show <leaf-run-id> --step handoff --json --no-reconcile
```

The gate artifact is authoritative; the report verdict alone is not. The
handoff output must show acceptance for the same final candidate. For
`accept_with_fixes`, inspect the accepted handoff/PR body for the
`## Review fixes` section and verify it names the reviewer commit.

## 5. Related references

- [Review gate design](../design/review-gate/2_design.md) — verdicts, the
  two-commit shape, revalidation, budgets, and coverage.
- [CONFIG.md](../CONFIG.md) — the `operation.review_*` keys.
- [Claimed-review artifacts](./claimed-review-artifacts.md) — the follower
  reviewer's coordinator route, its smoke procedure and rollout record.
- [Recover stuck job runs](./stuck-job-runs.md) — runs that never reached the
  gate's failure handoff.
