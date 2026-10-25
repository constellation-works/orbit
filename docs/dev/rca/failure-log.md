---
type: context
summary: Open causes of Orbit task-run failures and blocks, one entry per distinct cause, each with the task that will close it; resolved causes are removed.
incident_date: 2026-09-27
last_validated: 2026-10-12
tags: [incident, rca, operations, distributed-drain, sandbox]
paths: []
related_artifacts:
  - ORB-14777
  - ORB-15156
  - ORB-15692
  - ORB-15864
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

## 2026-10-24: Compiler-cache namespace guard fails a hosted check, and the green candidate has no recovery route

- **Where:** dk-server-2 (owner), `complete_pr` / `pr_complete` of
  `task_pr_pipeline`, the hosted `Check / Clippy / Test` job's
  `Run CI guardrails` step.
- **Symptom:** PR [#5855](https://github.com/constellation-works/orbit/pull/5855)
  at `0ad21022f16a71da569cac60fb5097d25eb6d662` failed
  `scripts/test-compiler-cache-namespaces.sh`, stage `client-side-shared-tcp`:
  participants C and D exited 2 with empty stdout and stderr (run 40079428623,
  job 120829734093). The candidate touched only
  `crates/orbit-web/src/tests/host_tunnels.rs`. All test shards passed. ORB-17882,
  the CI remediation task for the same failure, blocked on it as well.
- **Cause:** unconfirmed. The guard passed at `3ee5d569` and `35c5bfe88`, and a
  same-SHA rerun (attempt 2) passed on 2026-10-24, so the failure is
  intermittent. The owner host cannot create user namespaces, so it cannot
  reproduce the failure locally.
- **Rescue route:** none supported. `orbit task requeue` refused because the
  stopped run has no failure-handoff receipt. Source repair through
  `orbit.workflow.run.resume` refused because "failure-handoff ownership, failed
  step or task specification disagrees". A plain resume
  (`jrun-20261024-1814-t1`) moved the task to `review`, then failed at
  `complete_pr` because the run's kept worktree had been deleted while the task
  was blocked. `reconcile-review` applies only to follower deliveries. The PR
  was merged by hand after the green rerun (`gh pr merge 5855 --squash
  --match-head-commit 0ad21022…`, as `c8f8ed69`), and
  `orbit task settle-merged ORB-17852` closed the task.
- **Fix:** no task yet. F2026-10-556 tracks the guard failure, and F2026-10-561
  tracks the missing recovery route and the deleted worktree. Until those are
  fixed, rerun the failed job at the same SHA. If it passes, merge the PR by hand
  and run `orbit task settle-merged <task>`. Requeue a no-diff task such as
  ORB-17882 with `ORBIT_OPERATOR=1 orbit task requeue --reason …`.
- **Tasks:** ORB-17852 (`jrun-20261024-0837-c6`, PR #5855), ORB-17882
  (`jrun-20261024-1221-c3`).
- **Final recovery:** `escalate` on both runs. Blocked-task triage
  (`jrun-20261024-1806-t1`) reported both as needing a human.

## 2026-10-15: Supported Python lacks the mandatory fixture's PyYAML dependency

- **Where:** mac-mini-1, ORB-16198's candidate `make ci-fast`, with base
  `3943cbf406d1f61f93f5822d1bafac31730d6fa7` and clean candidate
  `b4f41f82b6e006fa897391ada768df1a6219d884`.
- **Symptom:** after 599 seconds in repository guards, the unchanged
  verified-download fixture failed at its unconditional `import yaml`.
- **Cause:** `/opt/homebrew/opt/python@3.14/bin/python3.14` was Python 3.14.7
  without PyYAML. The early preflight checked only Python's minimum version
  for `tomllib`; contributor setup omitted the mandatory module. Script
  changes selected the fixture on the candidate, while the baseline skipped
  it. That baseline pass did not exercise the failing fixture and therefore
  did not establish a causal source regression. The local workflow linter's
  optional warning/skip could not cover this mandatory test.
- **Host repair and proof:** the same exact interpreter now imports PyYAML
  6.0.3 from `/Users/daniel/Library/Python/3.14/lib/python/site-packages`,
  with user site enabled and no `PYTHONPATH` override. The supported repair
  used that interpreter's `-m pip install --only-binary=:all: --no-deps
  --no-cache-dir --require-hashes --target` with that user-site destination
  and a requirements file containing `PyYAML==6.0.3` pinned to SHA-256
  `34d5fcd24b8445fadc33f9cf348c1047101756fd760b4dacb5c3e99755703310`
  (the `pyyaml-6.0.3-cp314-cp314-macosx_11_0_arm64.whl` wheel). It changed
  no Homebrew/global packages, persistent environment, source or claim.
  On the unchanged clean candidate, that interpreter ran
  `scripts/test-ci-download-verified.py -v`: **9/9 passed in 23.049s**,
  exit 0. Full commands, install plan/result, import path, clean-environment
  proof and fixture output are preserved in ORB-16198 artifact
  `environment/pyyaml-host-repair-20261015.json`.
- **Fix:** ORB-16209 adds a selected-fixture PyYAML preflight before Cargo
  and repository guards, with the exact interpreter and manual environment
  setup guidance, plus isolated public-entrypoint regression controls and
  contributor documentation. Selection rules and Python version checks stay
  intact; no automatic installer or mandatory-test skip is introduced. This
  is candidate prevention, pending normal delivery and review; the host repair
  above is separate. Generic baseline-selection attribution is outside scope.
- **Tasks:** ORB-16198; incident F2026-10-378; prevention ORB-16209.
- **Final recovery:** none for this dependency repair. The original failed
  receipt and historical reliability counts remain unchanged; the later
  host fixture pass does not relabel the original attempt as successful.

## 2026-10-15: Native artifact puts lose genuine operator provenance

- **Where:** native `task artifact put` on dk-server-2, submitting the result
  and log for a held native review of ORB-16198.
- **Symptom:** the actual operator submitted the staged pair at
  2026-10-15T04:23:45Z. Both artifacts retained `created_by: human:daniel`
  and byte-identical hashes, but neither had an `ArtifactWriter` stamp.
  Exact receipt validation correctly kept the original run held, the task
  in progress and the incomplete certificate unchanged.
- **Cause:** the shared artifact route introduced by ORB-15939 dropped the
  authenticated operator decision at `TaskVerb::ArtifactPut`. The tool's
  screened owner write constructed a context with operator decision authority
  unset. The trusted-writer classifier introduced by ORB-14530 correctly
  refused to infer authority from the human attribution label.
- **Fix:** ORB-16220 forwards the authenticated decision and uses the existing
  screened lifecycle write for a local operator, preserving the agent/tool
  route for transport and worker calls. It changes no grants, writer policy,
  receipt validator, claim lineage or legacy artifacts. After deployment,
  only a genuine operator's supported resubmission of both files can provide
  trusted evidence; see [review evidence receipt](../../runbooks/review-gate.md).
  This implementation does not resubmit or rewrite the real receipts.
- **Prevention:** a public native CLI process regression checks both writer
  stamps, exact bytes, single-member hold retention, one fresh-review queue
  event and unchanged candidate, attempt and incomplete certificate. Negative
  controls retain agent provenance with omitted models, human-looking labels
  and operator overrides, and exercise MCP/worker and confinement boundaries.
- **Evidence limits:** the original native log reports 11 passed checks,
  six ignored helper entries and two `ps`-denied `DEFERRED` cleanup paths.
  Those cleanup paths remain unverified; provenance repair cannot relabel
  them as covered.
- **Tasks:** ORB-16198; diagnosed by ORB-16190.
- **Final recovery:** none; the evidence hold must await supported human
  resubmission and the ordinary validator, never a status or certificate bypass.

## 2026-10-15: Printed runner commands become deterministic failure causes

- **Where:** owner `complete_pr` / `pr_complete`, and the shared CI signature
  classifier used by reproduction, landing recovery and CI failure filing.
- **Symptom:** PR [#4823](https://github.com/constellation-works/orbit/pull/4823)
  at `1af607609feb79be1b7dd266d64731c5e09133fb` reported a disk-guard signature
  while naming `Install cargo-nextest`. The first Luna repair declared
  `external_blocker` and filed F2026-10-379 as insufficient disk. Provider
  [job 116218350235](https://github.com/constellation-works/orbit/actions/runs/38672403770/job/116218350235)
  instead records disk admission **passed**, then the missing
  `scripts/ci-download-verified.sh` diagnostic and install exit **127**.
- **Cause:** `classify_log_lines` recognized the `Run` header but lost provenance
  for its multi-line printed source. An error-bearing source line became a
  diagnostic marker and outranked the later unannotated runtime failure.
  `head_check_evidence` put that signature into the repair mandate before agent
  interpretation. Its tail-only excerpt could then hide the real diagnostic
  behind cleanup. The same wrong signature is retained in PR #4824's auto-fix
  attestation, although that repair independently found the actual missing file.
- **Fix:** ORB-16208 preserves command-display provenance, retains display
  boundaries in bounded selections, and anchors signatures and repair excerpts
  to emitted diagnostics. Public PR completion and CI-filing regressions cover
  printed bodies in successful and failed steps, genuine annotations, compiler
  and test identities, cleanup tails and incomplete provenance. This is a
  candidate prevention change, **not deployed prevention**; parent ORB-16190
  owns rollout and installed-source / after-landing verification. It does not
  prevent workflow/checkout generation skew (the separate cause below).
- **Tasks:** ORB-16192, ORB-16195; repair ORB-16208. F2026-10-379's original
  incorrect body remains preserved with a resolved, superseded disposition
  pointing to F2026-10-380 and ORB-16208; no disk repair occurred. Durable evidence:
  `evidence/pr-4823-diagnostic-provenance.json` on ORB-16208 and ORB-16190;
  ORB-16195 artifact
  `auto-fix/jrun-20261015-0225-c3/b8761e59323dcf17be5965e31b110b9a82c01cf5.json`.
- **Final recovery:** on the **same** `jrun-20261015-0211-c3`, verified `resume`
  was applied at 03:00:00Z from `prepare_branch` to refresh base evidence and
  synchronize the candidate. It did not change disk policy or runner capacity.
  The exact-head recovery outcomes are recorded under the generation cause.

## 2026-10-15: New workflow generation calls a helper absent from the PR checkout

- **Where:** hosted `Install cargo-nextest` in PR #4823 / ORB-16192 and
  [PR #4824](https://github.com/constellation-works/orbit/pull/4824) / ORB-16195.
- **Symptom:** candidate heads `1af607609feb79be1b7dd266d64731c5e09133fb` and
  `ea8b8483cc44d11e33f1352daa2acb99999436ee` failed install with
  `scripts/ci-download-verified.sh: No such file or directory`, exit 127, after
  successful disk admission. Original hosted jobs were
  [116218350235](https://github.com/constellation-works/orbit/actions/runs/38672403770/job/116218350235)
  and [116219627401](https://github.com/constellation-works/orbit/actions/runs/38672806630/job/116219627401).
- **Cause:** both candidates branched from
  `637f95e05bb8dc1c15d5e9b6d350cf61d569bd0a`. The live workflow generation
  invoked the verified-download helper introduced by
  `14156e650c21a53bf191d9d6774f2bd3327a219e`, while explicit checkout used the
  older candidate without that file. Local green gates on those old heads did
  not prove compatibility with the newer hosted workflow.
- **Fix:** bounded recovery restored a compatible candidate on each **same
  run, branch and PR**. ORB-16192's normal base-sync resume produced
  `b0a5e0dd8a4b9f69bf121659fd2a0b04b8256eca`; its four final local gates and
  hosted checks **passed**, and PR #4823 merged normally at 03:23:46Z as
  `10627cc948218ca8ba6be8124c1f5dbc8f548a94`. ORB-16195's auto-fix attempt
  **1 of 3**, Gemini Flash, admitted retry at 03:10:55Z and added only the
  missing executable helper as `b8761e59323dcf17be5965e31b110b9a82c01cf5`.
  All four final local gates **passed** on that head; the affected-test gate
  selected **no Rust packages** for the script-only repair.
  Fresh durable provider evidence now records its hosted run
  [38675154825](https://github.com/constellation-works/orbit/actions/runs/38675154825)
  **passed** and PR #4824 **merged** at 03:31:51Z as
  `ab854b40578511953774f162a51929e246a2780c`. Main Check 116226985792 ran
  03:13:48–03:31:46Z (**1,078 seconds**): functional repair passed, but the
  under-900-second CI budget was **unmet by 178 seconds**. The earlier
  03:28:35Z observation was pending and is not credited as a pass.
  Prevention requires compatible workflow and checkout generations, plus
  exact-head hosted validation after recovery; parent ORB-16190 retains that
  rollout/verification obligation. This classifier leaf changes no workflow,
  settings, deployment or release. Installed/deployed source and owner
  after-landing review are **not verified by this leaf** or implied by DONE.
- **Tasks:** ORB-16192 (`jrun-20261015-0211-c3`), ORB-16195
  (`jrun-20261015-0225-c3`); prevention closure remains with ORB-16190.
  ORB-16195 retains `delivery/hosted-check-116219627401.log.gz`
  (gzip SHA-256 `9161918a0b0e27df80fd97618b34dc5508e3b1cd45988433fdfb9656e6372727`),
  the exact repair attestation above, `delivery/final-provider-receipt.json`,
  and `delivery/final-check-116226985792.log.gz`. Original failed evidence
  remains preserved. Remove this open prevention entry once its parent fix
  lands, following this log's lifecycle.
- **Final recovery:** ORB-16192's verified same-run `prepare_branch` resume
  was applied; ORB-16195 used bounded same-run auto-fix retry rather than a
  final-recovery restart. Neither recovery repaired disk capacity.

## 2026-10-12: Loaded-host wall-clock bounds in orbit-web tests fail unrelated Mac gates

- **Where:** Mac follower, the before-landing affected-test gate of
  `task_claimed_pr_pipeline` (`landing_review_gate_settle`).
- **Symptom:** the reviewer returns verdict `incomplete` with "Resolve the
  failing affected-test gate", naming
  `host_tunnels::a_paced_identity_body_cannot_outlive_its_deadline` (elapsed
  over `identity_budget + 4s`) and `host_forward::*` tests that time out in
  `wait_until(.., 10s)`. The candidate diff touches none of that code.
- **Cause:** the tests bound liveness with fixed wall-clock limits. With five
  Mac leaves at test-threads 4 (load 12-14 on 14 cores) the limits are
  exceeded on the candidate and the base alike.
- **Fix:** ORB-15864 (backlog). Until it lands, land the candidate by hand
  when the diff does not touch orbit-web and hosted checks are green.
- **Tasks:** ORB-15793 (comment-only, PR #4554).
- **Final recovery:** `escalate` (`jrun-20261012-0946-c4`).

## 2026-10-12: A Mac claimed gate with both Seatbelt and `ps` notices cannot be deferred

- **Where:** Mac follower, `implement_one` in `task_claimed_pr_pipeline`. The
  implementer runs `make ci-test-affected` inside its own sandbox-exec
  profile.
- **Symptom:** the implementer finishes the change, then declares
  `validation_environment` ("the affected-test gate's mixed sandbox notices
  make its deferral ineligible"). Final recovery escalates and the claim
  fails with the diff uncommitted in the Mac worktree.
- **Cause:** ORB-15409 lets a Mac gate whose only gaps are nested-Seatbelt
  `SKIP:` lines defer to the host replay. A broad gate also prints `ps`
  refusals from process-inspection tests inside the agent sandbox, and a gate
  with both kinds of notice does not qualify, so broad Mac gates never defer.
- **Fix:** ORB-15692 (backlog, critical). Until it lands, rescue by hand:
  save `git diff <merge-base>` from the Mac worktree to
  `~/.orbit/state/rescue/`, apply it to a scratch worktree on current
  `agent-main`, run the affected tests natively on the Mac (outside the agent
  sandbox) or on dk-server-2 for portable code, open a PR and merge.
- **Tasks:** ORB-15782 (`jrun-20261012-0744-c2`), ORB-15816
  (`jrun-20261012-0946-c1`), ORB-15817 (`jrun-20261012-0946-c2`, PR #4549).
- **Final recovery:** `escalate` on each run named above.

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
  [build budget](../../runbooks/build-budget.md). On 2026-10-11 the Mac was
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
