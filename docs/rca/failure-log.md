---
type: context
summary: Open causes of Orbit task-run failures and blocks, one entry per distinct cause, each with the task that will close it; resolved causes are removed.
incident_date: 2026-09-27
last_validated: 2026-10-09
tags: [incident, rca, operations, distributed-drain, sandbox]
paths: ["crates/orbit-core/assets/jobs/task_pr_pipeline.yaml", "crates/orbit-core/assets/jobs/task_claimed_pr_pipeline.yaml", "crates/orbit-engine/src/activity_job/job_executor/recovery.rs", "crates/orbit-engine/src/activity_job/cli_runner/inspection.rs", "crates/orbit-exec/src/macos_sandbox/**"]
related_artifacts:
  - ORB-14722
  - ORB-14727
  - ORB-14731
  - ORB-14740
  - ORB-14777
  - ORB-14806
  - ORB-14812
  - ORB-14822
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
  (`jrun-20261008-1421-c6`, blocked handoff PR #3825).
- **Final recovery:** ORB-14727 `escalate` (`jrun-20261008-1421-c6`); ORB-14731
  none, because the agent blocker stopped the run first.

## 2026-10-08: Task-pilot source inspection's full-history fetch exceeds its 10-second bound

- **Where:** Owner `task_pilot_pipeline`, source inspection
  (`cli_runner/inspection.rs`).
- **Symptom:** `process timed out after 10000ms: git … fetch --quiet --no-tags
  <repo>/.git <sha>` in `.orbit/state/source-inspections-v1/0/checkout`
  (`jrun-20261008-1506-c1`).
- **Cause:** A newly initialised inspection slot fetches the pinned revision with
  its full history from the local repository. ORB-14728 bounded every
  production git subprocess, and this bulk copy got the 10-second local
  default. It overruns under drain load (box load 59 on 32 cores at 15:09Z).
- **Fix:** ORB-14806 (open): an explicit, measured bound for bulk object copies.
- **Tasks:** the pilot batch of `jrun-20261008-1506-c1`.
- **Final recovery:** none.

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

## 2026-10-07: Mac Claude workers lose OAuth

- **Where:** Mac claimed leaves using the Claude crew.
- **Symptom:** "Claude HTTP 401, OAuth token revoked", often in step recovery
  after hours of work, which loses the candidate. Ten leaves were hit from
  10-04 to 10-07, and again on 10-08 (`jrun-20261008-0858-c1`).
- **Cause:** Workers fell back to the shared Claude Desktop login, which a
  Desktop credential refresh revokes. The dedicated worker token
  (`CLAUDE_CODE_OAUTH_TOKEN`, listed in `execution.env.pass`) prevents this, but
  only when the launching shell has it. On 10-08 the drain was started from a
  non-login shell without it, and Orbit passed nothing, silently.
- **Fix:** ORB-14777 (open): drains, ship, `run job` and `orbit doctor` warn when
  a pass-listed variable is unset. Until it lands, start Mac drains from a login
  shell (`zsh -l -c '…'`).
- **Tasks:** ORB-14722, ORB-14740.
- **Final recovery:** none recorded.

## Operational causes (no code defect)

- **Stale follower binary.** Follower-side fixes only take effect after the
  follower binary is rebuilt from `agent-main`. A leaf that fails for an already
  fixed cause usually means an old binary.
- **Release bump without the follower.** Drain admission requires the exact owner
  version and protocol schema. After a release bump the follower is refused until
  it is upgraded.
