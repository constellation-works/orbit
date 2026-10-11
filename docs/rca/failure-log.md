---
type: context
summary: Open causes of Orbit task-run failures and blocks, one entry per distinct cause, each with the task that will close it; resolved causes are removed.
incident_date: 2026-09-27
last_validated: 2026-10-11
tags: [incident, rca, operations, distributed-drain, sandbox]
paths: []
related_artifacts:
  - ORB-14777
  - ORB-15156
  - ORB-15409
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

## 2026-10-11: A Mac claimed implementer cannot pass a gate that reaches Seatbelt tests

- **Where:** Mac follower, `implement_one` in `task_claimed_pr_pipeline`. The
  implementer runs `make ci-test-affected` inside its own sandbox-exec
  profile.
- **Symptom:** the implementer finishes the change, then declares
  `validation_environment` or `validation_incomplete` ("macOS refuses nested
  sandbox profiles"). Step recovery returns `not_recovered`, final recovery
  escalates, and the claim fails with the diff uncommitted in the Mac worktree.
- **Cause:** nested sandbox-exec cannot apply a profile, so every
  Seatbelt-gated test prints `SKIP: sandbox-exec cannot apply a profile: …`
  (39 lines for an orbit-core change). `judge_host_test_output` refuses any
  `SKIP:` line. The implementer prompt says a Seatbelt denial is not
  deferrable, and macOS has no counterpart of the Bubblewrap deferral
  (ORB-15287). Whether a claim fails depends on how strictly the model reads
  that rule, not on the change. In the same context three `orbit-cli::process`
  tests also fail on the base, and they pass on dk-server-2.
- **Fix:** ORB-15409 (backlog). Daniel chose macOS deferral parity on
  2026-10-11: the Mac implementer may hand off a gate whose only gaps are
  these `SKIP:` lines, and the claimed `validate` step replays
  `make ci-test-affected` outside the agent sandbox before push. Until it lands:
  - Rescue by hand. Copy the worktree diff (`git diff HEAD`) to a box
    worktree on current `agent-main`, run ci-fast, ci-test-affected and
    ci-lint there, and open a PR.
  - Only crew sol has failed this way. Claude crews hand off anyway (9/9 on
    2026-10-11), so their Seatbelt tests never run before landing. Tag sol
    tasks that reach Seatbelt-gated crates `os:linux`.
- **Tasks:** ORB-15399 (luna, PR #4260), ORB-15395 (sol, PR #4262), and in
  part ORB-15365 (PR #4253).
- **Final recovery:** `escalate` (`jrun-20261011-0356-c1`,
  `jrun-20261011-0406-c1`).

## Operational causes (no code defect)

- **Editing the primary checkout while a drain runs.** A leaf compares the
  registered primary checkout before and after each step and tolerates only
  `.orbit/` record dirt that misses the run's paths. Any other uncommitted edit
  there fails the leaf `primary_checkout_drift`, even when the run wrote only
  in its own worktree, and final recovery records an `external_blocker`
  instead of discarding someone else's work. On 2026-10-10 another session
  rebuilding the README hero GIF (#4212) in the Mac primary checkout blocked
  ORB-15345 after its implementation had finished (`jrun-20261010-2233-c1`);
  the change was landed by hand from the retained worktree. Edit in a
  separate worktree while a drain runs.
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
- **Test threads uncapped on a host that runs several gates at once.**
  `scripts/build-budget.py` caps compile jobs per slot, but test threads only
  when `ORBIT_TEST_THREADS` or `<budget-dir>/test-threads` is set (ORB-15385).
  On a host where slots × cores oversubscribes the CPU (three gates on the
  14-core Mac), timing-bound tests go red on the candidate and the base alike,
  and implementers spend their wall clock proving base failures. Set
  `test-threads` so that slots × threads fits the cores; see
  [build budget](../runbooks/build-budget.md). On 2026-10-11 the Mac was
  capped instead by starting pull drains with `NEXTEST_TEST_THREADS=4` and
  `RUST_TEST_THREADS=4`, which its ws_orbit `execution.env.pass` lists.
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
