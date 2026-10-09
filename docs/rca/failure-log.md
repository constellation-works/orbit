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
