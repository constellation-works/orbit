---
type: context
summary: Running log of why Orbit task runs failed or got blocked, one entry per distinct cause, with the fix that closed it.
incident_date: 2026-09-27
last_validated: 2026-10-07
tags: [incident, rca, operations, distributed-drain, sandbox]
paths: ["scripts/test-validate-codex-plugin.sh", "scripts/test-validate-agent-plugin.sh", "crates/orbit-exec/src/macos_sandbox/**", "crates/orbit-core/src/adapter/engine_host/v2_host/pull/**", "crates/orbit-core/assets/activities/**", "crates/orbit-core/assets/executors/claude.yaml", "crates/orbit-agent/src/providers/claude/**"]
related_artifacts:
  - ORB-13463
  - ORB-13486
  - ORB-13491
  - ORB-13492
  - ORB-13501
  - ORB-13604
  - ORB-13605
  - ORB-13606
  - ORB-13612
  - ORB-13639
  - ORB-13642
  - ORB-13649
  - ORB-13663
  - ORB-13664
  - ORB-13843
  - ORB-13851
  - ORB-13852
  - ORB-13902
  - ORB-13915
  - ORB-13921
  - ORB-13949
  - ORB-13958
  - ORB-13964
  - ORB-13983
  - ORB-14027
  - ORB-14079
  - ORB-14151
  - ORB-14259
  - ORB-14260
  - ORB-14262
  - ORB-14266
  - ORB-14272
  - ORB-14295
  - ORB-14301
  - ORB-14312
  - ORB-14313
  - ORB-14320
  - ORB-14321
  - ORB-14322
  - ORB-14328
  - ORB-14331
  - ORB-14334
  - ORB-14367
  - ORB-14369
  - ORB-14370
  - ORB-14376
  - ORB-14392
  - ORB-14393
  - ORB-14394
  - ORB-14396
  - ORB-14398
  - ORB-14399
  - ORB-14400
  - ORB-14402
  - ORB-14414
  - ORB-14417
  - ORB-14434
  - ORB-14435
  - ORB-14436
  - ORB-14437
  - ORB-14441
  - ORB-14450
  - ORB-14455
  - ORB-14461
  - ORB-14462
  - ORB-14463
  - ORB-14464
  - ORB-14465
  - ORB-14466
  - ORB-14467
  - ORB-14468
  - ORB-14469
  - ORB-14470
  - ORB-14471
  - ORB-14474
  - ORB-14475
  - ORB-14476
  - ORB-14477
  - ORB-14478
---

# Run failure log

A running record of why task runs failed or ended `blocked`. It has one entry per
distinct cause, not one per task. A task that fails for a cause already listed
goes into that entry's **Tasks** line. Full incident reviews still get their own
dated file in this folder. This log is for the everyday failures that don't need
one.

Each entry records:

- **Where**: the host (owner or follower) and the pipeline step.
- **Symptom**: what the run reported.
- **Cause**: the actual mechanism.
- **Fix**: the task or PR that closed it, or `open`.
- **Tasks**: the tasks that hit it.
- **Final recovery**: the decision final recovery returned and its run id, whether the applier
  applied or refused it, or `none` when final recovery did not run.

Newest entries go first. When you rescue a blocked task, add its cause here before
you close it out.

## 2026-10-07: Pull admission still excludes no-diff tasks from followers

- **Where:** Owner pull admission (`coordination/admission.rs:356-367`).
- **Symptom:** Mac drain `jrun-20261007-0628-t1` sat idle while ORB-14461 through
  ORB-14471 waited on the CPU-throttled owner.
- **Cause:** The ORB-14259 stopgap that excluded no-diff work from follower
  admission remained after the NoDiff handoff shipped.
- **Fix:** ORB-14474 (reported open by ORB-14441).
- **Tasks:** ORB-14259 (the earlier Mac no-diff claims, superseded by this
  admission cause), ORB-14461 through ORB-14471, ORB-14474.
- **Final recovery:** none.

## 2026-10-07: Follower pull passes discard owner admission diagnostics

- **Where:** Follower `pull_refill` → `record_pull_pass` (`follower.rs:64-110`).
- **Symptom:** `drain_last_pass` reports `queued 0` and `excluded_total 0` while
  the receipt lists 20 waiting tasks.
- **Cause:** The follower records its local admission counters instead of the
  owner's admission diagnostics carried by the pull receipt.
- **Fix:** ORB-14475 (reported open by ORB-14441).
- **Tasks:** ORB-14475.
- **Final recovery:** none.

## 2026-10-07: Task-pilot retries replay a stale claim

- **Where:** Owner `task_pilot_pipeline`, at apply and then prepare.
- **Symptom:** `stale preparation: state-trigger source changed from X to Y`
  repeats with the same pair. The RCA lists runs `jrun-20261004-1929/1946`,
  `-2308/2322`, `jrun-20261006-0343/0404`, and
  `jrun-20261007-0606/0615`.
- **Cause:** Pull or deploy advances local `agent-main` after the claim freezes
  its source; retry replays that source. After two attempts the fingerprint is
  retired and the task is shelved from piloting. Nine hits cost about 95
  agent-minutes.
- **Fix:** ORB-14476 (reported open by ORB-14441).
- **Tasks:** ORB-14476.
- **Final recovery:** none.

## 2026-10-07: CI-sweep pilot children failed on benign task races

- **Where:** Owner `ci_failure_sweep_pipeline`.
- **Symptom:** `CI-sweep task changed to rejected before admission` on
  `jrun-20261004-1400-t1`, `jrun-20261006-0200-t1`, and
  `jrun-20261006-0220-t1`; `requires exactly one prepared task` on
  `jrun-20261004-2240-t1`.
- **Cause:** The pilot child raced with task status and prepared-task changes
  between selection and admission.
- **Fix:** ORB-14477 (reported open by ORB-14441).
- **Tasks:** ORB-14477.
- **Final recovery:** not recorded in ORB-14441.

## 2026-10-07: macOS claimed leaves cannot nest Seatbelt

- **Where:** Mac claimed leaf, `implement_one`.
- **Symptom:** `sandbox_apply: Operation not permitted`, exit 71, so the test
  self-skips (`jrun-20261004-0849-c1` for ORB-13902 and
  `jrun-20261007-0121-c1` for ORB-14414).
- **Cause:** The claimed leaf cannot apply the nested Seatbelt sandbox on the
  Mac host, leaving the sandbox-dependent test without evidence.
- **Fix:** ORB-14478 (reported open by ORB-14441), with the shared contract in
  ORB-14334 (also reported open).
- **Tasks:** ORB-13902, ORB-14334, ORB-14414, ORB-14478.
- **Final recovery:** escalate.

## 2026-10-07: Evidence-held candidates were re-implemented after receipt

- **Where:** Owner `task_pr_pipeline`, `candidate_resume`.
- **Symptom:** Nine held runs affected five tasks. ORB-14331 and ORB-14328 each
  looped three times; ORB-14396, ORB-14398, and ORB-14400 were also held.
- **Cause:** `candidate_resume` did not reuse the received candidate after an
  evidence hold, so another attempt re-implemented it and repeated the hold.
- **Fix:** ORB-14450 (reported open by ORB-14441); earlier partial fixes were
  ORB-14313 and ORB-14376.
- **Tasks:** ORB-14313, ORB-14328, ORB-14331, ORB-14376, ORB-14396,
  ORB-14398, ORB-14400, ORB-14450.
- **Final recovery:** none; evidence holds skip final recovery.

## 2026-10-07: Before-PR review rejected candidates against a red base

- **Where:** Owner and Mac `review_gate_settle`, and implementer
  `validation_blocked`.
- **Symptom:** About 12 leaves, including `jrun-20261005-0730-c2`,
  `jrun-20261005-0840-c5`, `jrun-20261006-1747-c20/c21`, and
  `jrun-20261006-2022-c17`.
- **Cause:** The base had `replace_box` clippy failures on 10-05 and
  `orbit-core` test failures on 10-06; candidates were rejected for failures
  outside their changes.
- **Fix:** ORB-14434 (reported open by ORB-14441), sequenced after ORB-14450.
- **Tasks:** ORB-14434, ORB-14450.
- **Final recovery:** escalate on each affected leaf.

## 2026-10-07: Nested Bubblewrap could not provide sandbox evidence

- **Where:** Owner implement and review lanes.
- **Symptom:** `this host kernel denies unprivileged user namespace creation`
  on `jrun-20261006-0504-c3`, `jrun-20261006-0736-c3`, and
  `jrun-20261006-1153-c3`.
- **Cause:** The owner kernel refused the nested unprivileged user namespace
  needed for Bubblewrap, so those runs could not produce sandbox evidence.
- **Fix:** ORB-14331 added owner-side CodeQL fulfilment for held claimed
  candidates; ORB-14334 carries the shared sandbox contract (reported open by
  ORB-14441).
- **Tasks:** ORB-14331, ORB-14334.
- **Final recovery:** escalate.

## 2026-10-07: Delivery-code-review consumer stopped draining deliveries

- **Where:** Owner auto-task consumer.
- **Symptom:** Doctor reported `review ERROR`; state was `definition_changed`,
  execution was refused with `active_execution`, 133 deliveries were pending,
  and the last batch covered 2026-10-06 06:20Z.
- **Cause:** The consumer's active execution kept it from reconciling the
  changed definition and processing its pending batch.
- **Fix:** ORB-14455 (reported open by ORB-14441).
- **Tasks:** ORB-14455.
- **Final recovery:** not recorded in ORB-14441.

## 2026-10-07: Activity `proc.spawn` timeout was clamped to 60 seconds

- **Where:** Owner implement step.
- **Symptom:** `jrun-20261006-0434-c25` stopped at the 60-second limit.
- **Cause:** Activity `proc.spawn` clamped the requested duration to 60 seconds.
- **Fix:** ORB-14437 (reported open by ORB-14441).
- **Tasks:** ORB-14437.
- **Final recovery:** requeue.

## 2026-10-07: `git_push` did not retry a transient GitHub rejection

- **Where:** Owner push step.
- **Symptom:** `remote rejected … (failed)` on `jrun-20261006-0707-c13`.
- **Cause:** `git_push` treated a transient GitHub rejection as final instead of
  retrying it.
- **Fix:** ORB-14436 (reported open by ORB-14441).
- **Tasks:** ORB-14436.
- **Final recovery:** escalate.

The fixed-cause entries below follow ORB-14441, which records them against `9fcb45e63` unless a specific earlier fix is named. That source does not establish production verification for every fix.

The fixed-cause entries below follow ORB-14441, which records them against `9fcb45e63` unless a specific earlier fix is named. That source does not establish production verification for every fix.

## 2026-10-07: Review settlement rejected stable checks by report revision

- **Where:** Owner review settlement.
- **Symptom:** `validation_incomplete` because a required check was recorded by
  an earlier report revision and omitted from the final report, or a superseded
  attempt had no related required check. Thirteen owner leaves hit this from
  10-04 09:33Z to 10-06 20:22Z; examples include
  `jrun-20261006-1432-c5`, `-1553-c22`, and `-2022-c18`.
- **Cause:** Settlement matched validation to report revisions and command
  strings instead of stable required-check record identities.
- **Fix:** ORB-14312 and ORB-14322 were partial; ORB-14370 (commit
  `ca336ff74`) uses stable record IDs. Production verification remains pending
  because before-PR review is off.
- **Tasks:** ORB-14312, ORB-14322, ORB-14370.
- **Final recovery:** escalate.

## 2026-10-07: The validation environment on the new owner host was incomplete

- **Where:** Owner required-validation step and Mac validation.
- **Symptom:** `ci-guardrails: ripgrep (rg) is required` in ten runs on 10-04
  (including `jrun-20261004-1727-c5` and `jrun-20261004-1957-t1`); npm 12
  changed `npm pack --json` output shape on `jrun-20261005-0325-c3` and
  `jrun-20261005-0336-c7/c8`; Mac Python 3.9 lacked `tomllib` on
  `jrun-20261005-0026-c1`.
- **Cause:** Required validation ran with the systemd `PATH`; host tool versions
  also differed from what the checks expected. The failures began after
  ORB-13915 enabled required validation on the owner path.
- **Fix:** ORB-14027 uses an interactive login-shell `PATH`; ORB-14079 handles
  npm 12's output shape.
- **Tasks:** ORB-13915, ORB-14027, ORB-14079.
- **Final recovery:** escalate.

## 2026-10-07: Provider capacity and content-filter refusals were treated as work failures

- **Where:** Owner step recovery and full-review provider invocation.
- **Symptom:** Codex `Selected model is at capacity` on
  `jrun-20261005-0456-c5/c6`; cybersecurity content-filter refusals on
  `jrun-20261006-0104-c24/c29`. Recovery retried and then escalated at about
  one million tokens on each.
- **Cause:** Capacity and content-filter refusals were handled as task failures,
  triggering another attempt against an unavailable or refusing provider.
- **Fix:** ORB-14266 and commit `f283e3d85` stop spending recovery attempts on
  provider capacity exhaustion.
- **Tasks:** ORB-14266.
- **Final recovery:** escalate.

## 2026-10-07: macOS OAuth and keychain provider authentication fail

- **Where:** Mac claimed leaves using Claude or Antigravity.
- **Symptom:** Ten claimed leaves from 10-04 15:55Z to 10-07 01:21Z lost Claude
  OAuth (examples `jrun-20261006-0933-c1` and `jrun-20261007-0121-c2`).
  Antigravity keychain authentication failed three times.
- **Cause:** The Mac's Claude OAuth credential had been revoked; Antigravity's
  separate keychain authentication also failed.
- **Fix:** Use a dedicated `claude setup-token` as
  `CLAUDE_CODE_OAUTH_TOKEN`; ORB-14262 excludes the Claude crew and ORB-14435
  re-probes it (reported open by ORB-14441). ORB-14414 fixed Antigravity auth.
- **Tasks:** ORB-14262, ORB-14414, ORB-14435.
- **Final recovery:** not recorded in ORB-14441.

## 2026-10-07: `pr_complete` failed on transient GitHub errors

- **Where:** Owner `pr_complete` step.
- **Symptom:** GraphQL errors or TLS handshake timeouts on
  `jrun-20261006-1847-c5/c6`.
- **Cause:** The step treated transient GitHub errors as terminal.
- **Fix:** ORB-14392 adds recovery for the transient completion failure.
- **Tasks:** ORB-14392.
- **Final recovery:** `complete_no_diff`, applied; final-recovery run ID not
  recorded in ORB-14441.

## 2026-10-07: A repair commit moved HEAD before the next commit step

- **Where:** Owner `task_pr_pipeline`, after review rejection and final recovery.
- **Symptom:** `worktree_head_changed` on `jrun-20261006-2234-c3` for ORB-14399.
- **Cause:** Final recovery chose `resume commit`; its repair commit moved HEAD,
  invalidating the next commit step's pinned worktree head.
- **Fix:** ORB-14402.
- **Tasks:** ORB-14399, ORB-14402.
- **Final recovery:** `resume commit`; the repair commit applied and moved HEAD.

## 2026-10-07: Rebase recovery provenance broke after `agent-main` advanced

- **Where:** Owner rebase recovery.
- **Symptom:** `jrun-20261006-1815-c11` failed after `agent-main` advanced.
- **Cause:** Recovery provenance no longer matched the updated base. Plain
  rebase conflicts in `orbit-types/src/workflow/mod.rs` and `pull/*.rs` were
  normal concurrency and are not part of this defect.
- **Fix:** ORB-14393.
- **Tasks:** ORB-14393.
- **Final recovery:** not recorded in ORB-14441.

## 2026-10-07: Reviewer wall-clock and review-budget limits were exhausted

- **Where:** Owner and Mac review workers.
- **Symptom:** 1,800-second timeouts on `jrun-20261004-1034-c3` and
  `jrun-20261004-1212-c6`; review budgets reached 10,353 and 19,385 seconds
  against a 7,200-second limit on `jrun-20261004-1506-c3` and
  `jrun-20261004-1540-c6`.
- **Cause:** Reviews could time out or spend far beyond their configured budget
  without a useful partial result.
- **Fix:** PR #3372 settles timeouts as incomplete with a partial report;
  ORB-14394 keeps CodeQL out of review.
- **Tasks:** ORB-14394.
- **Final recovery:** not recorded in ORB-14441.

## 2026-10-07: `denyModify` matched `.env`-like paths unexpectedly

- **Where:** Owner validation worktree and Rust toolchain installation.
- **Symptom:** A fixture probe named `.env` failed on
  `jrun-20261005-0709-c12`; rustup's `macro.env.html` failed on
  `jrun-20261006-1135-c3`.
- **Cause:** The broad deny pattern matched both the environment-file fixture
  and the Rust documentation filename.
- **Fix:** ORB-14151 and PR #3476 (`3103233826`).
- **Tasks:** ORB-14151.
- **Final recovery:** not recorded in ORB-14441.

## 2026-10-07: A final-recovery crew draw had no persisted source state

- **Where:** Owner blocked-task recovery after a follower claim failed.
- **Symptom:** `no persisted state to freeze its workflow.final_recovery_crews
  draw` on `jrun-20261004-1653-t2`, `jrun-20261004-1655-t1`, and
  `jrun-20261004-1926-t2`.
- **Cause:** Recovery tried to freeze its crew draw in the failed follower run's
  state, which was not persisted on the owner.
- **Fix:** ORB-13964 uses the recovery run's own persisted state (commit
  `a0e689e6`).
- **Tasks:** ORB-13964.
- **Final recovery:** no recovery run could start before the fix; later outcome
  not recorded in ORB-14441.

## 2026-10-07: Recovery checkouts lacked Bubblewrap's `.orbit` deny root

- **Where:** Owner blocked-task recovery checkout under
  `recovery-checkouts/`.
- **Symptom:** `linux-bwrap cannot enforce absent denyModify` for the checkout's
  `.orbit/**` on `jrun-20261004-2005-t1` and `jrun-20261004-2108-t1`.
- **Cause:** The detached recovery checkout had no `.orbit` inode for
  Bubblewrap's read-only deny mount.
- **Fix:** ORB-13983 prepares an empty ignored `.orbit` root before launch
  (commit `e22b4def0`, merged by 2026-10-05 08:00Z).
- **Tasks:** ORB-13983.
- **Final recovery:** not recorded in ORB-14441.

## 2026-10-07: Task-pilot raced a drain while editing a task

- **Where:** Owner task-pilot and drain pipelines.
- **Symptom:** Fifteen runs failed with `material_changed: plan`, a
  claim-scoped mutation, or `status_changed`.
- **Cause:** Pilot mutation and drain admission raced on the same task state.
- **Fix:** ORB-14272 serializes the competing changes.
- **Tasks:** ORB-14272.
- **Final recovery:** not recorded in ORB-14441.

## 2026-10-07: Two task-pilot runs took the same task

- **Where:** Owner task-pilot admission.
- **Symptom:** Six runs selected one task concurrently.
- **Cause:** The pilot admission path did not prevent two pilots from claiming
  the same task.
- **Fix:** ORB-13949.
- **Tasks:** ORB-13949.
- **Final recovery:** not recorded in ORB-14441.

## 2026-10-07: `run auto` piloted mixed-crew bundles

- **Where:** Owner `run auto` pilot selection.
- **Symptom:** A bundle containing tasks for different crews entered the same
  pilot pass.
- **Cause:** Automatic piloting did not keep each bundle within one crew.
- **Fix:** ORB-14367.
- **Tasks:** ORB-14367.
- **Final recovery:** not recorded in ORB-14441.

## 2026-10-07: Codex task-pilot inspection tools were unavailable

- **Where:** Owner Codex task-pilot inspection.
- **Symptom:** The pilot could not inspect the task before deciding whether to
  prepare it.
- **Cause:** The Codex inspection tools were not available to the pilot.
- **Fix:** ORB-14295.
- **Tasks:** ORB-14295.
- **Final recovery:** not recorded in ORB-14441.

## 2026-10-07: Mac claimed leaves wrote outside their frozen footprint

- **Where:** Mac claimed-leaf implementation.
- **Symptom:** Five runs created new files outside the claim's frozen footprint.
- **Cause:** The footprint did not widen to include implementation-created
  paths, so the handoff rejected those candidates.
- **Fix:** ORB-13921.
- **Tasks:** ORB-13921.
- **Final recovery:** not recorded in ORB-14441.

## 2026-10-07: Claimed reviewers could not read owner-held artifacts

- **Where:** Mac claimed-review leaves.
- **Symptom:** Eight runs could not reach evidence stored on the owner, including
  the work tracked by ORB-14301, ORB-14260, and ORB-14321.
- **Cause:** The claimed reviewer lacked the owner artifact-read route needed
  to inspect that evidence.
- **Fix:** ORB-14301, ORB-14260, and ORB-14321.
- **Tasks:** ORB-14301, ORB-14260, ORB-14321.
- **Final recovery:** not recorded in ORB-14441.

## 2026-10-07: A redacted session field caused false protocol skew

- **Where:** Owner-to-follower claimed-leaf protocol fingerprint.
- **Symptom:** `jrun-20261007-0040-t1` was killed with `protocol_skew`.
- **Cause:** The owner redacted `XDG_SESSION_ID` inside the fingerprint, so
  equivalent protocol state compared unequal.
- **Fix:** ORB-14417.
- **Tasks:** ORB-14417.
- **Final recovery:** not recorded in ORB-14441.

## 2026-10-07: The owner rejected a newer follower's `crews` field

- **Where:** Owner-to-follower drain protocol.
- **Symptom:** The owner rejected the newer follower's `crews` field for 14
  minutes on 10-04.
- **Cause:** The owner protocol parser had not yet accepted the follower's
  newer `crews` field.
- **Fix:** ORB-13958.
- **Tasks:** ORB-13958.
- **Final recovery:** not recorded in ORB-14441.

## 2026-10-07: Upgrade-interrupted routine runs resumed and failed

- **Where:** Owner routine-run recovery after upgrade.
- **Symptom:** Runs interrupted by an upgrade were resumed and then failed.
- **Cause:** Resume reused a run interrupted across the upgrade rather than
  starting from a compatible state.
- **Fix:** ORB-14320.
- **Tasks:** ORB-14320.
- **Final recovery:** not recorded in ORB-14441.

## 2026-10-07: The CI sweep failed on incomplete cancelled-run logs

- **Where:** Owner `ci_failure_sweep_pipeline`.
- **Symptom:** The sweep received incomplete logs from PR runs cancelled by
  concurrency.
- **Cause:** The sweep treated partial cancellation logs as a complete CI
  failure record.
- **Fix:** ORB-14369 distinguishes incomplete cancelled-run logs. Transient
  `gh pr list` TLS failures and exhausted investigation budgets remain
  `retryable_error` by design; the next slot retries them.
- **Tasks:** ORB-14369.
- **Final recovery:** the next sweep slot recovers transient retryable errors;
  the incomplete-log incidents' recovery was not recorded in ORB-14441.

## 2026-10-03: Owner moved to a new host without Bubblewrap

- **Where:** Owner (`hm_9ca6004473492f06`, newly on dk-server-2, Ubuntu 26.04),
  every agent step (`implement_one`, `pilot`) under `linux-bwrap`.
- **Symptom:** `cli invocation failed (permanent): trusted Bubblewrap not
  available at /usr/bin/bwrap; declare allow_fallback: true to permit bare exec`.
  The first sweep's drain (`jrun-20261004-0123-t1`) failed all three leaves, and
  both task-pilot runs failed.
- **Cause:** The owner was migrated by copying `~/.orbit` and the checkouts to a
  fresh OS install, so `orbit init` never ran there and nothing installed the
  `bubblewrap` package or loaded the `bwrap-userns-restrict` AppArmor profile.
  `orbit doctor` (without `providers`) did not flag it. Running `orbit init` would
  not have fixed it either: automatic host preparation covers Ubuntu 24.04, not
  26.04.
- **Fix:** No code defect. The operator installed the package and loaded the
  profile (`sudo apt install bubblewrap && sudo apparmor_parser -r
  /etc/apparmor.d/bwrap-userns-restrict`). `orbit doctor providers` then reported
  every provider `sandbox_ready`. When you move a host by copying it, run
  `orbit doctor providers` before you enable the clock.
- **Rescue:** stopped the drain with `orbit run auto --stop`, then moved the
  blocked tasks back to `backlog`.
- **Tasks:** ORB-13843, ORB-13851, ORB-13852.

## 2026-10-03: `release_locks` rejected its own input after a gate failure

- **Where:** Owner, `task_gate_pipeline` failure cleanup (`gate_invoke` →
  deterministic `release_locks`).
- **Symptom:** `deterministic action release_locks failed: invalid input: unknown
  fields 'run_id', ...` on `jrun-20261004-0123-c5` (and its parent
  `task_auto_pipeline` run `-c1`), after the child `task_pr_pipeline` failed on
  the Bubblewrap cause above.
- **Cause:** The v2 activity dispatcher injects `run_id` (or `job_run_id`
  when the input already names a run) and `step_id` into core deterministic
  action input on both success and failure paths. `release_locks` forwarded
  that object unchanged to `orbit.task.locks.release`, whose strict tool-input
  validation rejects those context fields before releasing anything. This
  was not specific to child failures. Terminal-run finalization independently
  releases run-owned reservations, which can explain why the lock listing was
  empty afterwards; the historical release reason has not been verified.
- **Fix:** ORB-13854. Strip only the dispatcher context fields at the
  `release_locks` tool boundary, preserving strict validation of all other
  fields. A CLI boundary regression drives the shipped gate with a real failed
  child and a successful child, checks explicit release before terminal
  cleanup, and checks `orbit task locks list` for remaining reservations.
  `task_claimed_local_pipeline` has no lock-release activity: claim settlement
  owns its reservation release.
- **Tasks:** the same three as the entry above.

## 2026-09-28: Stopping a follower drain strands its live leaves

- **Where:** Follower (macOS) drain coordinator (`workspace_pull_pipeline`) and
  the leaves it admitted.
- **Symptom:** Drain `jrun-20260928-0242-t1` was stopped (it ended `cancelled`)
  at 04:14Z. Four leaves that later succeeded stay in local phase `settling` with
  an AcceptHandoff that was never delivered. Two that failed stay in `launched`
  with no settlement at all. On the owner, the claims stay `running` and the tasks
  stay `in-progress`, so neither the PRs nor the tasks reach completion.
- **Cause:** Only the coordinator that admitted a leaf records and delivers its
  settlement (`pull_refill` → `reconcile_pending`). Cancelling the coordinator
  removes the only settlement path. Live leaves keep running, but nothing reports
  their result to the owner.
- **Fix:** ORB-13663 (#2932). Settlement no longer belongs to the
  admitting coordinator: the admission record is the outbox, each leaf's worker
  records and delivers its own handoff or failure as it terminalizes, and
  `orbit run cancel` / `orbit run auto --stop` (and their dashboard buttons) run
  a settle-only pass that delivers anything still recorded and ends a dead
  drain's unlaunched claims as failures. Cancelling a drain no longer kills or
  strands its live leaves. See the design decision "Settlement belongs to the
  admission record, not to the drain that admitted it".
- **Rescue:** after the follower runs a build with the fix, run
  `orbit run auto --stop` in its replica checkout. It delivers the four recorded
  handoffs (tasks to `review`) and records and delivers the two failures (tasks
  to `blocked`). Claims the owner already revoked close locally as
  `closed_obsolete`.
- **Tasks:** the six leaves of drain `jrun-20260928-0242-t1`: ORB-13636,
  ORB-13658, ORB-13655, ORB-13657 (handed off), ORB-13271 and ORB-13622 (failed).

## 2026-09-27: Headless claude crew hands off while its validation gates run in the background

- **Where:** Follower (macOS) distributed leaf, claude crew (resolved opus), in the
  `implement_one` step.
- **Symptom:** `cli subprocess reported declared envelope status="failed" despite
  exit 0: error.code=validation_incomplete` ("make goldens was still running at
  handoff"). The task goes to `blocked` although the code is complete, and
  `step_failure_recovery` declines.
- **Cause:** The agent starts `make ci-fast`, `ci-lint` and `goldens` with Bash
  `run_in_background`, waits on a `Monitor`, and ends its turn. In
  `claude -p --json-schema` mode, Claude Code immediately forces the
  StructuredOutput call, so the agent must report before the gates finish. The
  executor (`crates/orbit-core/assets/executors/claude.yaml`) passes
  `--tools default`, which includes background Bash and Monitor.
- **Fix:** ORB-13664. The claude provider pins
  `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1` on every child it spawns. Verified on
  Claude Code 2.1.283 in `-p --json-schema` mode: Bash then rejects
  `run_in_background`, and `Monitor` is no longer offered, so gates run in the
  foreground.
- **Tasks:** ORB-13612 (rescue PR #2895), ORB-13463 (rescue PR #2896), ORB-13271 (rescue PR #2929).

## 2026-09-27: Owner and follower minted the same child run ID

- **Where:** Follower (macOS) distributed leaf and the owner's own local drain,
  in the `commit` step (`git_commit`).
- **Symptom:** `commit_batch_changes expected exactly one task for job_run_id
  'jrun-20260928-0230-c1', got 2`.
- **Cause:** Run IDs (`jrun-<YYYYmmdd-HHMM>-c<n>`) are unique only within one
  machine's store. The owner's drain and a Mac leaf both created
  `jrun-20260928-0230-c1` in the same minute, so the owner's task store bound two
  tasks to that ID. `commit_batch_changes` looks tasks up by run ID alone, not by
  run ID plus the machine that ran them, so both runs failed at commit.
  ORB-13599 (#2871) stops reuse within one store only.
- **Fix:** ORB-13649. Every run-keyed task lookup (commit, merge, blocking on
  run failure, resume) now matches the run ID plus the machine that executed the
  run, so a shared ID resolves to each machine's own task. Recorded in
  [distributed-drain decisions](../design/distributed-drain/4_decisions.md#a-run-is-its-id-plus-the-machine-that-executes-it).
- **Tasks:** ORB-13605 (rescue PR #2879), ORB-13604.

## 2026-09-27: Codex plugin validator test raced on fixed temp paths

- **Where:** Follower (macOS) distributed leaf, in the `claim_validate` step
  (`make ci-fast` → `scripts/test-validate-codex-plugin.sh`).
- **Symptom:** `shutil.Error: [... "[Errno 17] File exists:
  '/var/folders/.../T/codex-stale-latest-pin/plugin'"]`, then
  `make: *** [ci-fast] Error 1`.
- **Cause:** `clone_fixture()` writes each test case to
  `base_fixture.parent / name`, a fixed name in the shared `$TMPDIR`, outside the
  per-run `mktemp` directory. It checks whether the folder exists, deletes it,
  then copies, and never cleans the folder up afterwards. Concurrent leaves on one
  host raced on the same path, and the leftover folders stay behind.
- **Fix:** ORB-13650. The base fixture and every case folder now live under the
  per-run `fixture_root`, so the existing cleanup removes them.
  `scripts/test-validate-agent-plugin.sh` had the same pattern and got the same fix.
- **Tasks:** ORB-13606. Its change had already landed via #2867 before validation
  failed.

## 2026-09-27: Claimed Mac leaves could not reach the owner's task tools

- **Where:** Follower (macOS) distributed leaf, in the `implement` step
  (`agent_implement`).
- **Symptom:** The agent reported `task_store_unreachable`. The leaf settled as a
  failure and the owner task went to `blocked`. The finished code was left behind
  in `.orbit/state/worktrees/orbit-<run>`.
- **Cause:** In a claimed run, `agent_implement` told the agent to re-read the task
  and to persist `execution_summary` with `orbit.task.update`. On a follower those
  tools federate to the owner over `ssh <owner>`. The macOS worker sandbox denies
  reads of `~/.ssh`, which is deliberate (`credential_read_denies` in
  `crates/orbit-exec/src/macos_sandbox/compile.rs`). The owner was unreachable
  from inside the leaf by construction, so every claimed Mac leaf failed after
  doing its work.
- **Fix:** ORB-13642 (#2825). In claimed mode the agent returns `execution_summary`
  in its step output. `claim_handoff` delivers it with the settlement, and the
  agent never calls owner task tools. The sandbox deny stays as it is.
  `step_failure_recovery` reads the step output in claimed runs.
- **Tasks:** ORB-13501, ORB-13492, ORB-13491, ORB-13486, and the other Mac leaves
  rescued on 2026-09-27 (PRs #2806–#2844).
- **Rescue:** First recover the stranded owner claim to `backlog` or `blocked`
  (see [distributed-drain runbook](../runbooks/distributed-drain.md)). Then open a
  PR from the leaf's worktree.

## 2026-09-27: Owner-revoked claims stalled every follower drain pass

- **Where:** Follower drain, in `pull_refill` reconciliation.
- **Symptom:** The drain claimed nothing new, and every pass aborted.
- **Cause:** After an operator revoked or recovered a stranded claim on the owner,
  the follower's settle was answered with `stale_claim`. The follower treated that
  as a hard error, so the pass aborted before it could refill.
- **Fix:** ORB-13639 (#2797). The follower closes its local settlement when the
  owner has already ended the claim.
- **Tasks:** Every drain that ran after the ORB-13642 rescues, until the follower
  binary included #2797.

## 2026-09-27: Sandboxed macOS workers could not resolve their runtime binding

- **Where:** Follower (macOS) leaf, at worker start.
- **Symptom:** "managed worker runtime binding unavailable".
- **Cause:** The binding lookup ran the setuid `/bin/ps` to read process start
  identity, and the macOS worker sandbox cannot exec setuid binaries.
- **Fix:** #2792 reads the start identity from libproc instead.
- **Tasks:** The first Mac distributed leaves.

## 2026-09-27: Sandboxed workers could not read the recovery authority DB

- **Where:** Follower (macOS) leaf.
- **Symptom:** Reads of the recovery authority database failed inside the sandbox.
- **Cause:** The authority DB's WAL sidecar files were not kept, so the sandboxed
  reader could not open the database consistently.
- **Fix:** #2786 keeps the WAL sidecars.
- **Tasks:** The first Mac distributed leaves.

## Operational causes (no code defect)

- **Stale follower binary.** Follower-side fixes only take effect after the
  follower binary is rebuilt from `agent-main`. A leaf that fails for an already
  fixed cause usually means an old binary.
- **Release bump without the follower.** Drain admission requires the exact owner
  version and protocol schema. After a release bump the follower is refused until
  it is upgraded.
