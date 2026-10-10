---
type: context
summary: Open causes of Orbit task-run failures and blocks, one entry per distinct cause, each with the task that will close it; resolved causes are removed.
incident_date: 2026-09-27
last_validated: 2026-10-10
tags: [incident, rca, operations, distributed-drain, sandbox]
paths: []
related_artifacts:
  - ORB-14777
  - ORB-15156
  - ORB-15162
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

## 2026-10-10: Final recovery cannot start for a task whose required tools it disallows

- **Where:** owner, `final_recovery` admission after a failed step.
- **Symptom:** ``final recovery activity `final_recovery` did not return a
  decision: task `ORB-15156` required tool `orbit.task.add` failed admission:
  tool 'orbit.task.add' is in the activity disallow list (final_recovery)``.
- **Cause:** Admission checks a task's `required_tools` for deny-mode
  activities too, and refuses the task when the activity's disallow list covers
  one. Every `delivery-code-review` batch task requires `orbit.task.add` and
  `orbit.task.artifact.put`, so none of them can get final recovery, and the
  run parks with no decision.
- **Fix:** ORB-15162 (open). Activities in the named non-implementer set drop a
  covered requirement from `requested_tools` and record a note; the tool stays
  out of the callable set. `agent_implement` still fails closed.
- **Tasks:** ORB-15156 (closed by hand).
- **Final recovery:** none; it failed admission (`jrun-20261010-0544-c3`).

## Operational causes (no code defect)

- **A task that offers alternative fixes without choosing one.** An implementer
  that meets two fixes in the description, where the code supports both,
  blocks with `contradictory_requirements` instead of guessing.
  Whoever files the task, a QA sweep included, names the chosen fix. When a
  task blocks this way, the operator writes the decision into its description,
  then resumes the run.
- **Stale follower binary.** Follower-side fixes only take effect after the
  follower binary is rebuilt from `agent-main`. A leaf that fails for an already
  fixed cause usually means an old binary. On 2026-10-10 the Mac binary, built
  at 01:11Z, predated ORB-15131 and ORB-15122. Its claimed reviews of ORB-15117
  and ORB-15072 settled against a base rerun that selected no crates, and both
  were held. Both were landed by operator decision.
- **Mac drain started without a login shell.** Mac Claude workers need the
  dedicated `CLAUDE_CODE_OAUTH_TOKEN` from a login shell (`zsh -l -c '…'`), and
  every workspace's effective `execution.env.pass` must list it. Drains and
  `orbit doctor` warn when a pass-listed variable is unset (ORB-14777).
  Clock-started runs read it from `~/.orbit/clock.env`. A Claude activity
  without a token is refused before `claude` starts, instead of 401ing
  mid-run on the Desktop login (ORB-15154).
- **Release bump without the follower.** Drain admission requires the exact owner
  version and protocol schema. After a release bump the follower is refused until
  it is upgraded.
- **Reviewer fixtures named like secrets.** The default policy denies creating
  `**/.env`, `**/*.env` and similar paths, and before commit the Linux sandbox
  fails the step for any such path the child created, including scratch under
  the worktree's `.orbit/tmp`. A reviewer that builds a probe `clock.env`
  there and leaves it fails permanently (ORB-15156). Remove such fixtures
  before the step ends, or name them so the deny does not match.
- **CodeQL name heuristics.** `rust/cleartext-logging` treats a function whose
  name contains `trusted` as a secret source. A code-scanning sweep task for
  such an alert has nothing to fix (ORB-15140, `trusted_wrapper` returns a fixed
  Bubblewrap path). Dismiss the alert on GitHub as a false positive with the
  reason, then reject the task.
