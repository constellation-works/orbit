---
type: runbook
summary: Inspect before-PR and before-landing review verdicts, findings, and evidence holds, then decide how to resume a task.
tags: [operations, review-gate, delivery]
paths: ["crates/orbit-core/src/application/review/**", "crates/orbit-core/assets/jobs/task_pr_pipeline.yaml", "crates/orbit-core/assets/jobs/task_claimed_pr_pipeline.yaml", "crates/orbit-engine/src/executor/automation/vcs/failure/handoff.rs"]
related_features: [review-gate]
related_artifacts: ["ORB-13989", "ORB-13992", "ORB-14194", "ORB-14849"]
last_validated: 2026-10-09
---

# Operate the Review Gate

Use this runbook when `review.before_pr = true` or `review.before_landing =
true` and you need to read what a review did to a delivery, or a task is
`blocked` with a `review_gate_escalation` or
`review_timeout_requeue_exhausted` event, requeued after a reviewer timeout,
`in-progress` while awaiting named external evidence, or in `review` with an
open PR after a before-landing review did not approve it.

Review has three timings. Before-PR review (`review.before_pr`) holds PR
creation for the reviewer. Before-landing review (`review.before_landing`)
opens the PR first and reviews it while hosted CI runs. After-landing review is
the `delivery-code-review` auto-task. There is one review layer before
landing: config load fails, naming both keys, while `review.before_pr` and
`review.before_landing` are both on. Both share `review.minutes` and
`operation.review_crew`, and neither runs on a workspace that ships locally:
readiness holds that backlog as `local_route_before_pr` or
`local_route_before_landing`, and `orbit doctor` fails its `review` check.

## 1. What the gate does

One fresh reviewer, from `operation.review_crew`, examines the implementation
commit, fixes what it finds, and returns a verdict. There is no findings-driven
rework loop. If PR completion rebases a conflicting reviewed head, the pipeline
may admit one fresh re-review in the same lineage; a second conflict leaves the
task in review as `review_gate_stale`.

After a replacement push, GitHub's PR metadata (`headRefOid`) and the remote
PR head ref (`refs/pull/<number>/head`) can briefly report the previous
published head. Completion retries only that recorded SHA, checking each read
with `git ls-remote origin refs/pull/<number>/head`. A PR head ref already at
the new candidate lets stale metadata settle. A PR head ref still at the
recorded previous head also settles, but only while `git ls-remote origin
refs/heads/<task branch>` independently names the new candidate; a ref seen at
the candidate that later regresses is refused. It makes at most three re-reads
at the configured poll interval, capped at 60 seconds and one quarter of the
completion wait budget. `merge.stale_head_observations` records the lag on
successful delivery. An unrelated SHA, a missing or moved remote ref, an absent
or different task branch ref, an unknown, empty or candidate-equal previous
head, a failed `ls-remote`, or an exhausted bound (including a PR head ref
still at the previous head) still refuses with `delivery_evidence_stale` (or
`review_gate_stale` when only the review pin applies), leaving the task in
review. Completion must observe the exact candidate in PR metadata before
sending the unchanged SHA-conditioned merge request; waiting never extends the
review certificate to another head.

| Verdict | Candidate branch | Delivery |
| --- | --- | --- |
| `accept` | the implementation commit(s) only | the PR opens on that head |
| `accept_with_fixes` | the implementation, then one `review: <summary>` commit authored by `<family>-reviewer` | owner validation reruns on the reviewer commit and its paths widen the task's selectors; the PR opens with a "Review fixes" section |
| `reject` or other substantive `incomplete` | whatever was committed, kept | the task is blocked, no PR is opened, and final recovery gets one look |
| Abandoned review: the reviewer exited cleanly but its only report is the initial placeholder (`incomplete`, no escalation, finding or validation record, never revised) | the implementation is pushed; nothing was reviewed | settlement releases the attempt and refuses with `review_abandoned:`, so the candidate's one review is not spent; the task is blocked with a comment saying so, and resuming the run or requeueing it admits a reviewer again within the remaining minutes; no PR opens |
| Reviewer wall-clock timeout | the implementation and any partial reviewer repairs are pushed; the partial report is retained | the first timeout on a task's implementation tree requeues it to `backlog` with `review_timeout_incomplete`; another timeout on that tree blocks it with `review_timeout_requeue_exhausted`; no PR opens |
| Red base: every failed required check carries a `baseline` claim settlement confirms, and nothing else is open | the reviewed candidate is kept unpublished | the step fails typed `[baseline_red]` and the run ends `held`; the task goes to `backlog` under `baseline_red_hold` until the base passes, and the next run resumes the candidate for a fresh review |
| Evidence-only `incomplete` (or legacy `changes_required`) | the reviewed candidate is kept unpublished; a claimed leaf also pushes it to `orbit-evidence/<branch>` on `origin` | the run ends `held`, with no retry, step recovery, final recovery, or failure handoff; the task stays `in-progress` with `review_awaiting_evidence` (for a claimed leaf, its settlement releases the claim with the hold) |

The implementation commit is never amended. A failed revalidation of the
reviewer commit is reported as `reject` too, even though the certificate
records `accept_with_fixes`.

### Before-landing review

With `review.before_landing` captured, the table above applies to the
`landing_review*` steps after `pr_open`, with these differences:

| Outcome | Pull request | Task |
| --- | --- | --- |
| `accept` | completion merges the reviewed head | `done` with `--complete`, else `review` |
| `accept_with_fixes` | owner validation reruns on the reviewer commit, which `landing_push` pushes onto the published head under a lease; hosted CI restarts and completion merges that head | `done` with `--complete`, else `review` |
| `reject`, any `incomplete` (evidence-only included), reviewer timeout, failed revalidation, `push_lease_lost` | open and unmerged; only a fix the review settled was pushed | stays `review`; a comment starting "Before-landing review did not approve PR #…" names the typed reason, and the failure handoff decision is `landing_review_failure` |

A refusal that leaves the PR open for a recorded decision (`reject`, any
`incomplete`, or an abandoned review below) ends the run `held` with code
`review_decision_pending`, not `failed`: nothing went wrong, and the gate and
auto parents waiting on it pass it as held. A reviewer timeout, failed
revalidation or `push_lease_lost` still ends the run `failed`.

A reviewer that exits cleanly with only its initial placeholder report is
released, not settled: the refusal leads with `review_abandoned:`, the failure
handoff takes the same `landing_review_failure` path, and the candidate's one
review stays unspent, so `review-reset` is not needed. A reviewer that wrote a
reason, a validation record, or a second report revision settled a genuine
`incomplete`, which counts. The settlement comment names the PR the task
carries and the status the store holds when it is posted.

No outcome closes the PR or requeues the task. A later DIRTY rebase of the
reviewed head routes through the `re_review*` steps as before. A claimed leaf
runs the same review after its `pr_open` and hands off the settled verdict as
before-landing evidence; the owner refuses a handoff without it or for another
head, and a leaf whose review does not approve blocks the task on the owner
with its PR open.

The timeout bound permits one automatic requeue per task and implementation
tree, recorded in task history without a time window. The implementation tree
is the tree of the candidate head after stepping back over the gate's own
`review: partial reviewer repairs preserved` commits, so the reviewer's partial
work, which changes the candidate tree on every timeout, never renews the
allowance. A fresh run starts a new review lineage with its captured minute
budget; resuming the same lineage uses its remaining budget. A new commit
containing the same tree does not renew the requeue allowance, and changing the
tree then restoring it does not erase its earlier requeue. An implementer
change to the tree has its own allowance.

Final recovery may repair a rejected candidate once. If it appends a commit,
the engine records the exact HEAD advance around that invocation and resumes
at commit or an earlier requested step. Required validation and before-PR
review run again for the new candidate; the rejected attempt cannot approve
it. The commit guard accepts only the recorded repair HEAD in the same
worktree, descended from the pinned base. Any later unrecorded HEAD change
still fails with `worktree_head_changed`.

An evidence hold applies only when every remaining requirement is a named
unavailable external check and the report contains no open defect or failed
required check. `review-evidence-hold.json` pins the attempt, candidate,
task meaning, and each required artifact. Attach a passing `ReviewExternalEvidence`
result at each named path and its nonempty log artifact with
`orbit task artifact put` from an operator shell: no agent identity, outside
any managed run. A result or log an agent attached never counts
([review gate design §4](../design/review-gate/2_design.md)). Each result must match
the kind, exact command and candidate tree. Attempt, commit, display name and
artifact path changes do not expire a result on the same tree. Unrelated,
different-tree, failed or incomplete evidence leaves the hold in place.

A hold whose every requirement is kind `codeql` or a `linux`
`host_sandbox_test` is fulfilled by a Linux owner without an operator. The
owner runs each named command at the held commit and attaches the result and
its log. A failed or incomplete run attaches only the log, with a typed
reason. Each attempt is audited as `review.evidence_fulfilment`. See
[owner fulfilment](codeql-local.md#owner-fulfilment) for CodeQL and
[host sandbox tests](#host-sandbox-tests) below.

Receipt of all matching evidence queues the task in `backlog` with
`review_evidence_received` for a fresh review, unless the hold names only owed
evidence (below). It does not approve the candidate
or resume the terminal held run. The next delivery run resumes the held
candidate instead of implementing again: its `resume_candidate` step reports
`resumed_held` (see the orchestrate skill's preserved-candidate reference).
Fresh admission includes verified result/log
pairs in `satisfied_external_evidence`; settlement re-reads them on the final
tree and satisfies repeated unavailable checks without another hold. A repair
that changes the tree needs new evidence. A candidate rebased onto a moved
base, by that resume or by completion before landing, keeps the evidence only
while its whole patch over the base is unchanged (`git patch-id --stable`):
the manifest and certificate record `evidence_carried` (`from_tree`,
`to_tree`, `patch_id`). Otherwise the admission output's `evidence_carry` is
`rerequested` with the reason (`patch_changed`, `source_unavailable`) and the
review holds again for evidence on the new tree. An operator status decision or changed task
meaning prevents automatic receipt from overriding that decision. Held runs
are settled outcomes for pipeline waits, pass the parent's success guard
(reported as `held_count`), and are excluded from reliability's
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

A required check can fail on the candidate only because the base it was
pinned to already fails it. The reviewer then records a `baseline` claim on
that failed record: the base commit, the outcome there, and the failures both
share, with `sources` outside the candidate's scope. Settlement reruns the
check on the host, on the final candidate and on the base. It shares the base
result cache with required validation, and the runs land in the
`review-baseline.json` task artifact. Only a `workflow.required_validation_commands`
or `review.baseline_commands` entry captured when the run was admitted is
rerun, never the reviewer's own command text. A failure of one of those
commands cannot be filed as a `diagnostic` instead: the review settles
`incomplete` naming the command. A claim holds the task only when the base fails
with the same exit status, every claimed failure appears in the base output,
and the candidate adds no failing test or lint location. A candidate that
fails beyond the base keeps its verdict (`reject`, escalated
`baseline_exceeded`). When the host's run refutes the claim, its run on the
final candidate decides the check:

- The candidate passes, and the review has no open finding and no pending
  external evidence. The reviewer's failure was its own environment's. The
  record counts as passed, with a note naming the host's run, and the
  certificate's `host_overrides` records the command, the reviewer's outcome
  and the run. A review whose claims all resolve this way settles `accept`
  (`accept_with_fixes` over a reviewer repair) on the host's evidence. A
  pass whose validation summary shows no counted test ran is not evidence
  and is refused.
- The candidate fails and the base passes a comparable run. The failure is
  the candidate's own, so the review settles `reject` with a
  `baseline_refuted` escalation naming the command.

A host pass never overrides an open finding. Any other claim the host
contradicts or cannot check, including a pass under an open finding, settles
`incomplete` with a `baseline_claim_refused` escalation. A command that
selects its own tests, such as `make ci-test-affected`, reports its selection
and executed-test count, and the base rerun is handed the candidate's
selection. A base run that tests another selection, or passes without
executing a counted test, is not comparable: the claim is neither refuted nor
confirmed, and the review settles `incomplete` with a
`baseline_not_comparable` escalation (see the
[validation summary](../DEVELOPMENT.md#validation-summary-and-base-reruns)). The certificate's
`baseline_red` names the holds. When the base ref moves to a commit where the
command passes, admission lifts the hold. The next delivery resumes the
preserved candidate (`resumed_validated`) without the implementer and admits
a fresh review. A claimed leaf's resume runs the implementer again.

### Host sandbox tests

Orbit's own sandbox tests cannot run inside an agent lane, whose sandbox
refuses a nested one. Inside the lane they print a `DEFERRED: …` notice and
pass without executing their confined path. The reviewer copies each notice
into the record's `deferred`, and such a pass never counts as running that
path: settlement reads it as `not_run` (`validation_incomplete`).

The same holds earlier, for the implementer [ORB-15287]. An affected-test
gate that exits 0 with only `DEFERRED: bubblewrap unavailable:` notices does
not fail the implementation step. The implementer returns a
`deferred_sandbox_validation` record (the gate's command, exit code, executed
tests, pinned base, notices and a failing
`bwrap --unshare-user --ro-bind / / -- /bin/true` probe), and the implement
step refuses any record that is not exactly that case before commit. The gate
must be one of the owner's `workflow.required_validation_commands` or
`review.baseline_commands`. The owner's `candidate_validate` (and a claim's
`claim_validate`) then runs the gate after the required commands, outside the
agent sandbox. A pass that still prints a `DEFERRED:` line is a
`validation_environment` failure, and a run that reports no executed tests is
refused, so the path runs natively before delivery or the candidate is held.
Real failures, `SKIP:` or `skipping` notices, empty gates, notices with no
failing probe and the other required commands are not deferrable
([development guide](../DEVELOPMENT.md#test-process-environment)).

A reviewer whose sandbox cannot run a sandbox-gated test names it as
`host_sandbox_test` evidence for an OS. A claimed leaf's host runs it outside
the agent sandbox when it settles
([design](../design/review-gate/2_design.md#4-what-the-validation-records-establish-orb-11528-orb-11545)). The certificate's
`host_evidence` lists each attempt with its typed `reason`. The run's output
and validation environment are in the log artifact beside the named result
(`<artifact>.log.json`):

```bash
orbit tool run orbit.task.artifact.get --input '{"id":"<task-id>","path":"review-gate.json"}' \
  | jq '.content | fromjson | .host_evidence'
orbit tool run orbit.task.artifact.get --input '{"id":"<task-id>","path":"<artifact>.log.json"}'
```

A workspace `[[review.host_evidence]]` rule makes this deterministic for a
claimed leaf. A Rust change on a macOS leaf owes the Linux CodeQL run, and a
sandbox-gated test on a host of the rule's OS owes a run outside the agent
sandbox. Orbit adds each owed requirement whatever the reviewer reported:
`owed_external_evidence` in the manifest and `owed_evidence` in the certificate
list them. A verdict whose only gaps are owed checks holds rather than blocking.
A reviewer's claimed pass of an owed check also holds, with an `owed_evidence`
escalation. Once the evidence arrives, the owner's next run resumes the held
candidate and its admission decides `evidence_received`. Settlement then
reuses the held certificate without a reviewer, and the certificate's
`resumed_hold_attempt` names it
([design](../design/review-gate/2_design.md#4-what-the-validation-records-establish-orb-11528-orb-11545)).

```bash
orbit tool run orbit.task.artifact.get --input '{"id":"<task-id>","path":"review-gate.json"}' \
  | jq '.content | fromjson | {owed_evidence, resumed_hold_attempt}'
```

A `linux` requirement that is still held — the review ran locally, or the
claimed leaf's host could not run it — is fulfilled by a Linux owner whose
host can create Bubblewrap namespaces. The owner's clock sweep dispatches
`review_evidence_fulfilment_pipeline` for it, one run at a time, and stands
down on a follower, a worker, another platform or a host without namespaces.
The run admits only an exact owner-required validation command or
`cargo test -p <crate> --test <target> [<filter>]`, written in
`[A-Za-z0-9._/:@+=-]` and spaces with no other option. A command outside
that allowlist refuses the whole hold before anything runs. An admitted
command runs without a shell at the held commit, in a detached worktree with
the validation environment and a run-local build target, outside any sandbox
because the namespaces it tests cannot nest. The worktree and target are
removed afterwards. A test that executed and passed attaches the result as
`system` and requeues review. Anything else attaches only the log.

Inspect a fulfilment run:

```bash
# Every attempt, with its run id, candidate, commands, exit codes and reason.
orbit audit list --tool review.evidence_fulfilment --json
# The step's output: fulfilled, reason, detail, retryable, requeued.
orbit run show <run-id>
# The command line run, its output, tests passed and validation environment.
orbit tool run orbit.task.artifact.get --input '{"id":"<task-id>","path":"<artifact>.log.json"}'
```

The run also comments its outcome on the task.

| Reason | Cause |
| --- | --- |
| `shell_metacharacter` | The command has a character outside the allowlist. Never ran. |
| `command_not_allowed` | The command is neither `cargo test` nor an owner-required command. Never ran. |
| `argument_not_allowed` | An option or extra argument outside the `cargo test` form. Never ran. |
| `sandbox_unavailable` | The output reports the sandbox could not apply. |
| `self_skipped` | The test passed but printed `SKIP:`, `DEFERRED:` or `skipping`. |
| `no_tests_ran` | No libtest summary reports a passed test. |
| `test_failed` | The test ran and failed. |
| `tool_missing` | `cargo` is missing from the validation PATH. |
| `command_failed` | The worktree or the command could not be started. |
| `timed_out` | The run exceeded its time limit. |

`sandbox_unavailable` means the host itself cannot apply the sandbox: run the
test there by hand, outside any sandbox. `self_skipped` and `no_tests_ran`
mean the command proved nothing: correct the requirement's command or filter.
A refused command (`shell_metacharacter`, `command_not_allowed`,
`argument_not_allowed`) never ran. The hold then waits for an operator's
result, as for any other evidence; a `macos` requirement that no claimed leaf
fulfilled always does.

## 3. Decide a blocked review

Identify the failed step from `orbit run show`, then act:

- **A `landing_review*` or `landing_push` step failed** (before-landing
  review): the PR is open and unmerged and the task is in `review`. Read the
  findings comment and the "Before-landing review did not approve" comment.
  To keep the change, fix it on the PR branch and land it with an operator
  decision, or re-queue the task for a fresh run (a new lineage reviews the
  new candidate). To drop it, reject the task; `pr.close_on_terminal` closes
  the PR then. `push_lease_lost` means the PR branch moved after the review
  settled: inspect who pushed before deciding.

- **`review` timed out**: read the retained partial report and timeout
  handoff. The first timeout requeues automatically. If
  `review_timeout_requeue_exhausted` blocked the task, repair the candidate or
  record an operator decision before continuing. Requeueing the same
  implementation tree does not renew its automatic timeout allowance, even
  when the reviewer left different partial repairs each time.
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
  calls and a claimed worker's other owner calls, and nothing else. The
  refusal names the cause:
  `review_attempt_stale` (the reviewer ran past its attempt or the attempt
  was settled), `review_manifest_stale` (the owner holds another attempt's
  manifest), `stale_claim` (the owner released, failed, revoked or superseded
  the claim), `claimed_review_bridge_refused` (another activity, task, path or
  field), or `owner_route_unavailable` "could not reach this run's
  coordinator" (no usable response; the run skips recovery and releases the
  claim).
  Preserve the exact call and inspect the task, run, claim and review ledger
  before recovery. Do not replay a stale, expired, settled or cancelled
  attempt. A transport loss has an unknown outcome and must use existing
  idempotent/reconciliation behavior; it does not authorize a new claim or
  report. Only after normal terminal settlement, no live owner and cause
  diagnosis may existing task authorization start a fresh attempt. An
  `owner_route_unavailable` naming a missing `ORBIT_PLUGIN_BROKER` is a launch or
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
- [CONFIG.md](../CONFIG.md) — the `[review]` keys and `operation.review_crew`.
- [Claimed-review artifacts](./claimed-review-artifacts.md) — the follower
  reviewer's coordinator route, its smoke procedure and rollout record.
- [Recover stuck job runs](./stuck-job-runs.md) — runs that never reached the
  gate's failure handoff.
