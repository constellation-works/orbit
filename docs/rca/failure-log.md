---
type: context
summary: Open causes of Orbit task-run failures and blocks, one entry per distinct cause, each with the task that will close it; resolved causes are removed.
incident_date: 2026-09-27
last_validated: 2026-10-09
tags: [incident, rca, operations, distributed-drain, sandbox]
paths: []
related_artifacts:
  - ORB-14777
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

## 2026-10-09: An owner task-lock timeout fails a follower's final recovery

- **Where:** Mac pull follower, `final_recovery` after `landing_review_gate_settle`.
- **Symptom:** `remote tool failed (internal_error): ... timed out after 30000ms
  acquiring task commit boundary lock`. On the box, `orbit-sweep.service` failed on
  the same lock at the same instant (18:37:24Z).
- **Cause:** under drain load the workspace partition lock
  (`tasks/workspaces/<ws>/.task-commit.lock`) is held for more than 30 s. Its
  holder is never recorded (`holder=unknown`), and a follower's owner call reports
  the timeout as `internal_error` with no retry.
- **Fix:** ORB-15088 (open): record the holder and how long it held the lock, find
  and bound the slow sections, and return a typed retryable error that final
  recovery retries.
- **Tasks:** ORB-15029 (`jrun-20261009-1653-c1`). The operator pushed the reviewer's
  fix commit to PR #3963, ran the SIGTERM test on the box (Linux runs the path the
  macOS guard skips), and landed it.
- **Final recovery:** none applied (the invocation failed to load the task).

## 2026-10-09: Before-landing review holds correct candidates on report-shape or base-state claims

- **Where:** `landing_review_gate_settle` (`review.before_landing`, trial ORB-14849).
- **Symptom:** `review_gate_blocked: verdict incomplete` with 0 open candidate
  findings, as one of:
  (a) `validation_contradicted: <cmd> was recorded as diagnostic but is not_run`;
  (b) the reviewer claims a red base and the host's `baseline_claim_refused`
  contradicts it;
  (c) `review_timeout_incomplete` at exactly 3600 s.
- **Cause:** (a) a skipped baseline or CodeQL command labelled `diagnostic`
  instead of `excluded`. `RoleContradicted` is not in `correctable()`, so the
  reviewer never gets its correction pass. (b) Reviewer environment failures
  inside the managed run: the proc_spawn fixture inherited `ORBIT_ACTIVITY_*`
  (fixed by ORB-15052). A host baseline that ran zero Rust tests counted as
  passing (friction F2026-10-211). (c) The reviewer process is bounded by
  `min(review.minutes, agent_review_repair wall_clock_timeout_seconds 3600)`,
  while its manifest advertises the full 120 minutes (F2026-10-214).
- **Fix:** ORB-15083 (open) for (a), and ORB-15094 (open) for (c).
  Friction curation will file a task for the zero-test baseline.
- **Tasks:** ORB-15021 (PR #3957), ORB-15038 (PR #3967), ORB-15078 (PR #3977),
  ORB-15050 (PR #3962), ORB-15043 (PR #3969), ORB-14916 (PR #3966, timeout),
  ORB-14934 (PR #3972, timeout). All landed by operator decision after their red checks
  matched agent-main's.
- **Final recovery:** escalated (human_action: operator decision).

## 2026-10-09: Final recovery escalates a forward resume after an agent yields mid-implement

- **Where:** box leaf `implement_bundle` (crew gemini-flash), then final recovery.
- **Symptom:** `the provider exited 0 but stdout carried no valid terminating Orbit
  response envelope`. Final recovery returns `resume` from `commit` and is refused
  ("neither the failed step … nor an earlier step of its phase"), so the task goes
  `blocked` even though a complete candidate is in the worktree.
- **Cause:** `admit_resume_step` turns any forward resume into `escalate`, and the
  recovery prompt does not list the allowed resume steps.
- **Fix:** ORB-15079 (open): list the allowed steps in the prompt, and downgrade a
  forward resume to the failed step.
- **Tasks:** ORB-15018 (`jrun-20261009-1613-c7`; partial candidate as failure-handoff
  PR #3960), requeued on sol by the operator.
- **Final recovery:** escalated (invalid resume step).

## 2026-10-09: Git protection refuses git's own temp leftovers in `.git/objects`

- **Where:** Mac pull-drain leaves at sandbox resolution, right after deploying
  b19309df2 (ORB-14896 extended the Git metadata protection to macOS).
- **Symptom:** Every leaf and its final recovery fail within seconds with
  `Git protection refuses symlink, special-file or hard-linked metadata entry
  .git/objects/…`, and the task goes `blocked`.
- **Cause:** `scan_git_tree` (`runtime/git_sandbox.rs`) refuses any file with
  more than one link. When a git object write is interrupted after the temp
  file is linked to its final name, both names remain. The Mac checkout had 506
  `tmp_obj_*` and 6 `tmp_pack_*`/`tmp_idx_*` leftovers, dating back to July.
  Linux hosts can collect them the same way.
- **Fix:** ORB-14928 (open): accept hard links within the object store, give a
  remedy in the error, add a doctor check, and preflight drains before they
  claim. Until then, clear them on the host with
  `find .git/objects -type f -name 'tmp_*' -links +1 -delete`, then confirm
  `find .git -type f -links +1` is empty.
- **Tasks:** ORB-14912 (`jrun-20261009-1319-c1`) and ORB-14916
  (`jrun-20261009-1320-c1`), both requeued after the cleanup.
- **Final recovery:** could not run: its own invocation was refused by the same
  check, so it escalated.

## Operational causes (no code defect)

- **A task that offers alternative fixes without choosing one.** An implementer
  that meets two fixes in the description, where the code supports both,
  blocks with `contradictory_requirements` instead of guessing.
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
