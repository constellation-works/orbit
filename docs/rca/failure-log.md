---
type: context
summary: Open causes of Orbit task-run failures and blocks, one entry per distinct cause, each with the task that will close it; resolved causes are removed.
incident_date: 2026-09-27
last_validated: 2026-10-09
tags: [incident, rca, operations, distributed-drain, sandbox]
paths: ["crates/orbit-core/assets/jobs/task_pr_pipeline.yaml", "crates/orbit-store/src/driver/sqlite/job_run_store/queries.rs", "crates/orbit-core/assets/jobs/task_claimed_pr_pipeline.yaml", "crates/orbit-engine/src/activity_job/job_executor/recovery.rs", "crates/orbit-engine/src/executor/automation/vcs/commit/actions.rs", "crates/orbit-exec/src/macos_sandbox/**"]
related_artifacts:
  - ORB-14722
  - ORB-14727
  - ORB-14731
  - ORB-14740
  - ORB-14696
  - ORB-14812
  - ORB-14822
  - ORB-14827
  - ORB-14828
  - ORB-14837
---

# Run failure log

The causes of task-run failures and `blocked` tasks that are still open. It has
one entry per distinct cause, not one per task. A task that fails for a cause
already listed goes into that entry's **Tasks** line. Full incident reviews
still get their own dated file in this folder.

Each entry records:

- **Where**: the host (owner or follower) and the pipeline step.
- **Symptom**: what the run reported.
- **Cause**: the actual mechanism.
- **Fix**: the open task that will close it.
- **Tasks**: the tasks that hit it.
- **Final recovery**: the decision final recovery returned and its run id, whether the applier
  applied or refused it, or `none` when final recovery did not run.

Newest entries go first. When you rescue a blocked task, add its cause here
before you close it out, or extend the entry that already names the cause.
Delete an entry once its fix has landed. Git history keeps the resolved
entries.

## 2026-10-09: The code disagrees on whether a singular `task_id` binds a run to a task

- **Where:** Owner `task_pr_pipeline`, `implement_bundle` step.
- **Symptom:** The implementer stopped with `contradictory_requirements`
  (`jrun-20261009-0244-c3`). The store filter, the `JobRunQuery::task_id`
  contract and the `run_history` test treat only the `task_ids` array as
  ownership. The run projection, resume, step recovery and crew resolution also
  read the singular `task_id`.
- **Cause:** The two readings grew apart in the code. The QA-sweep task offered
  both fixes ("match `$.task_id` too, or drop it from the projection") without
  choosing one, and the agent rightly declined to choose. `blocked_task_recovery_pipeline`
  and `review_evidence_fulfilment_pipeline` submit the singular key, so
  `run history --task` hides those runs.
- **Fix:** ORB-14828 (open). The operator decided that the singular `task_id` is a
  binding and wrote that into the task. The store filter matches it.
- **Tasks:** ORB-14828.
- **Final recovery:** none. The agent blocked itself, the operator recorded the
  decision, and the run was resumed as `jrun-20261009-0317-t1`.

## 2026-10-09: A finished review batch without an execution summary fails the no-diff guard

- **Where:** Owner `task_pr_pipeline`, `git_commit` step, for a
  `delivery-code-review` batch.
- **Symptom:** "task 'ORB-14827' requires a meaningful persisted
  execution_summary before delivery; the implementing agent recorded none and
  the worktree holds no uncommitted change to derive one from"
  (`jrun-20261009-0150-c3`).
- **Cause:** The reviewer (sonnet) examined the whole batch, wrote
  `automation-coverage.json` with `examination_complete: true` and filed four
  findings, then exited without persisting a summary. A review batch never has
  a diff, so the guard had nothing to derive a summary from, and final recovery
  could not read the coverage artifact. The task blocked and the after-landing
  review consumer stalled behind it.
- **Fix:** ORB-14837 (open): the guard accepts a complete coverage artifact bound
  to the current batch as no-diff evidence and derives the summary from it.
- **Tasks:** ORB-14827.
- **Final recovery:** `escalate` (`jrun-20261009-0207-t2`). The operator wrote the
  summary and resumed the run.

## 2026-10-08: A validate-step recovery's repair cannot be committed

- **Where:** Owner `task_pr_pipeline`, `validate` step and its
  `step_failure_recovery`.
- **Symptom:** `make ci-fast` failed on the candidate. Step recovery repaired it,
  and in ORB-14727 the repair passed `ci-fast`. The run still ended `blocked`.
  ORB-14731's recovery declared `git_metadata_read_only` ("git add/commit fails
  with index.lock Read-only file system"). ORB-14727's final recovery escalated
  because "both repairs remain uncommitted".
- **Cause:** The pipeline expects validate-step recovery to commit its fix
  before the retry. The recovery sandbox mounts the worktree's Git metadata
  read-only by design, and no host step commits a recovery's working-tree
  changes before the retry, so a validated repair is lost. In both runs the
  failure being repaired was the unit-test ratchet.
- **Fix:** ORB-14822 (open): host code commits the recovery's repair under the
  `commit` step's rules. Recovery agents stay read-only on `.git`.
- **Tasks:** ORB-14731 (`jrun-20261008-1426-c3`), ORB-14727
  (`jrun-20261008-1421-c6`, blocked handoff PR #3825), ORB-14696
  (`jrun-20261009-0105-c24`: a rebase onto ORB-14731's four-argument
  `add_column_if_missing` broke the build; the uncommitted repair reached
  handoff PR #3846).
- **Final recovery:** ORB-14727 `escalate` (`jrun-20261008-1421-c6`); ORB-14696
  `escalate` (`jrun-20261009-0105-c24`); ORB-14731 none, because the agent
  blocker stopped the run first.

## 2026-10-08: macOS claimed executors cannot apply Seatbelt in affected tests

- **Where:** Mac claimed leaves, `make ci-test-affected` inside the executor's
  sandbox.
- **Symptom:** About 27 tests fail on an unmodified base with
  `sandbox-exec: sandbox_apply: Operation not permitted` (exit 71). Each leaf
  spends 10+ minutes on a baseline replay
  (ORB-14655, `jrun-20261008-1130-c1`).
- **Cause:** macOS refuses a second `sandbox_apply` inside the executor's
  profile. The test guards check only that `sandbox-exec` is executable, and the
  plugin-backend tests have no guard at all.
- **Fix:** ORB-14812 (open): one cached apply-probe that every sandbox-gated test
  checks, skipping with a `SKIP:` notice. Coverage of the real paths stays with
  macOS CI and host-run sandbox evidence.
- **Tasks:** ORB-14649, ORB-14655, ORB-14740.
- **Final recovery:** ORB-14740's final recovery confirmed that even
  `sandbox-exec -p '(version 1) (allow default)' /usr/bin/true` exits 71.

## Operational causes (no code defect)

- **A task that offers alternative fixes without choosing one.** An implementer
  that meets two fixes in the description, where the code supports both,
  blocks with `contradictory_requirements` instead of guessing (ORB-14828).
  Whoever files the task, a QA sweep included, names the chosen fix. When a
  task blocks this way, the operator writes the decision into its description,
  then resumes the run.
- **Stale follower binary.** Follower-side fixes only take effect after the
  follower binary is rebuilt from `agent-main`. A leaf that fails for an already
  fixed cause usually means an old binary.
- **Mac drain started without a login shell.** Mac Claude workers need the
  dedicated `CLAUDE_CODE_OAUTH_TOKEN` from a login shell (`zsh -l -c '…'`);
  without it they fall back to the Desktop login and get 401s when Desktop
  refreshes it. Drains and `orbit doctor` now warn when a pass-listed variable
  is unset (ORB-14777).
- **Release bump without the follower.** Drain admission requires the exact owner
  version and protocol schema. After a release bump the follower is refused until
  it is upgraded.
