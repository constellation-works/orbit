---
type: context
summary: Open causes of Orbit task-run failures and blocks, one entry per distinct cause, each with the task that will close it; resolved causes are removed.
incident_date: 2026-09-27
last_validated: 2026-10-10
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

## 2026-10-09: Before-landing review holds correct candidates on base-state claims, timeouts or a placeholder report

- **Where:** `landing_review_gate_settle` (`review.before_landing`, trial ORB-14849).
- **Symptom:** `review_gate_blocked: verdict incomplete` with 0 open candidate
  findings, as one of:
  (a) the reviewer claims a red base and the host's `baseline_claim_refused`
  contradicts it;
  (b) `review_timeout_incomplete` at exactly 3600 s;
  (c) the reviewer exits 0 well inside its budget, and its only report is the
  initial placeholder ("Review still running; validation not yet complete.").
- **Cause:** (a) Reviewer environment failures inside the managed run: Mac load
  flakes in orbit-cli process/mcp targets, or a Python 3.9 login PATH on the Mac
  (fixed in `~/.zprofile` on 2026-10-10). The host's base rerun of
  `make ci-test-affected` selects no crates on the base, runs zero tests and
  still counts as a pass (F2026-10-211, F2026-10-222). (b) The reviewer process
  is bounded by `min(review.minutes, agent_review_repair
  wall_clock_timeout_seconds 3600)`, while its manifest advertises the full
  120 minutes (F2026-10-214). (c) Settlement counts the placeholder as the
  candidate's one review, so re-admission needs an operator `review-reset`
  (F2026-10-224).
- **Fix:** ORB-15122 (open) for (a), by rerunning the disputed command on the
  candidate. ORB-15131 (open) for (a), by making the base rerun comparable.
  ORB-15094 (open) for (b). ORB-15130 (open) for (c).
- **Tasks:** ORB-15050 (PR #3962), ORB-15043 (PR #3969), ORB-15057 (PR #4006),
  ORB-15058 (PR #4009), ORB-14916 (PR #3966, timeout), ORB-14934 (PR #3972, timeout),
  ORB-14929 (PR #4016, timeout), ORB-15119 (PR #4030, placeholder report). All landed by
  operator decision after their red checks matched agent-main's, or after an
  operator review of the diff.
- **Final recovery:** escalated (human_action: operator decision).

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
