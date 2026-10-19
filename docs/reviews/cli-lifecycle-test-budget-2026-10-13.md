# CLI process and workspace test budget candidate

This candidate retires standalone entry points into contract tables. Retained behaviors keep their original assertions and platform guards. Mutable runtime bodies run in an exact isolated child once per table. Grouping reduces runner entries; the separate setup changes below remove actual repeated work.

One duplicated replay-policy matrix retires against core integration coverage. The hung candidate-probe case moves to the public updater and uses the existing supervision-clock seam after the real candidate signals readiness, preserving timeout, untouched executable/generation, staging/backup cleanup and free admission during and after the probe.

The original macOS-only 60-second wall wait is eliminated without shortening a startup deadline. Existing binary update argument, refusal and output cases remain. The Linux-only probe module is guarded at its declaration, removing an unused-import warning on macOS.

## Accepted delivery scope, 2026-10-14

Daniel accepts the measured incremental improvements below: about 17% process
and 11% workspace summed time, with the transferred core cost accounted. The
original 50% goal is superseded for this delivery and belongs to the next round.
Count caps remain 85 process and 75 workspace tests. Historical failed pairs
and their qualifications remain unchanged. Delivery still requires the four
pipeline final gates and hosted Linux, Windows compile and native macOS checks
on the updated head before the coordinator lands the existing PR #4749.

## Current production measurement, 2026-10-14

The managed implementation resume recovered the validated fixture changes and
candidate-preservation assertion through `95c158184` onto `ab075609`. Every
injected candidate path matched the original checkpoint exactly; the carried
increment applied without overlap. Production, Cargo and newer upstream
regression code remain identical to the current base, including the landed
image-cache and Windows repairs.

One matched six-leg comparison ran on dk-server-2. Both portfolios used the
same actual CLI SHA-256
81cbb1ff3a3cdc59ede1fd651bdc5b28f3840434d460020be7f5bcdd27d4110a,
normal 10-slot/4-job/4-thread host budgets, default nextest profile, zero retries
and required native sandbox checks. Supported nextest archives, extraction and
lists were prepared outside timing. Each side's exact source was restored
before its full process, workspace and existing core resume-selector runs.
The order was A process/workspace/core, then B process/workspace/core; all six
commands exited zero with no sandbox deferral notices. This is one sample with
ordinary host load, not an isolated causal estimate or a best-of selection.

| Binary | Current unreduced A | Reduced B | Historical half-A limit | Accepted result |
| --- | --- | --- | --- | --- |
| process | 160 executed, 166 listed / 149.218 s summed | 74 executed, 80 listed / 124.003 s summed | 74.609 s | Count passes; about 17% summed-time reduction |
| workspace | 143 executed/listed / 80.662 s summed | 71 executed/listed / 71.964 s summed | 40.331 s | Count passes; about 11% summed-time reduction |
| Existing core resume selector | 1 executed / 0.246 s | 1 executed / 0.650 s | Accounted separately | Added 0.404 s |

The same six pre-existing ignored process child helpers remain ignored; no
identity was newly ignored. CLI plus selected core summed time is
230.126 -> 196.617 seconds. Command walls, including nextest startup and budget
admission but excluding preparation, are process 37.473 -> 32.042 seconds,
workspace 21.007 -> 19.022, and core 0.316 -> 0.730. The newer shared production
startup improvement applies to both A and B and cannot be credited solely to
these test reductions. The historical attempts and their misses below remain
preserved. The original half-time target was missed by 49.394 seconds for
process and 31.633 seconds for workspace; that target is now explicitly
superseded for this delivery. No new timing cycle or additional saving is claimed.

Run evidence is stored in task artifacts under
performance/current-production-resume/. It includes the exact commands,
source/image/config hashes, complete identity lists, original logs and JUnit,
CPU/load/process snapshots and the six-leg verdict. An extraction preparation
failure (missing destination directory, exit 96) is retained; it was corrected
before timing, with no test retry. The candidate source was restored exactly
before this documentation update.

The retirement ledger below names every removed standalone identity: 123
process and 103 workspace identities from the current A lists. The final
candidate-quality fix also compares the post-refusal candidate digest against
the already-produced immutable manifest digest; the former same-path comparison
was vacuous. This adds no test identity or speed credit.

Direct CLI QA verified job help, an unknown resume flag (exit 2), a local update
missing its install target (exit 2), and the JSON admission contract. These
checks used an isolated scratch HOME. Real worker, updater, authority, cleanup
and routing assertions also passed in the full fixture suites. Native macOS
behavior was not exercised in this Linux run. The pipeline owns the final
`make ci-fast`, `make ci-lint`, `make goldens` and `make ci-test-affected`
commands; they were not run in this implementation session.

## Original attempt measurements

At this historical checkpoint the count limits passed and the original
summed-time limits failed, so that attempt was incomplete and repository gates
were not run. The accepted delivery scope above supersedes the half-time gate
without changing any historical result.

| Binary | Historical Linux baseline | Native macOS baseline | Candidate |
| --- | --- | --- | --- |
| process | 164 tests / 330 s summed | 157 tests / 529.497 s summed | 74 tests / 488.475 s summed |
| workspace | 143 tests / 236 s summed | 136 tests / 190.213 s summed | 64 tests / 190.688 s summed |

Baseline: `env -u ORBIT_WORKER_CONTEXT_REQUIRED ./scripts/build-budget.py -- cargo nextest run -p orbit-cli --test process --test workspace --no-fail-fast --success-output immediate --tool-config-file test-budget:<baseline-config>` at `872c7d2b58b009e1502b358df5a6c795f0958084`. The two JUnit suites supply independent counts and summed testcase durations. Candidate commands use `--test process` and `--test workspace` separately on this same macOS host, with the same build-budget admission and repository profile. Build time is excluded from JUnit testcase totals.

The native process baseline reports Seatbelt and ps-denied notices. Restricted sandbox coverage is not a completed pass; these notices must remain visible in any final affected-test gate. Native platform guards account for the difference from the historical Linux counts. Existing ignored helper entry points remain ignored; no test was newly ignored.

Process test wall time is 124.636 s; workspace test wall time is 67.526 s. These are not summed testcase times. The historical limits are 165 s and 118 s; half of this host's measured baseline would be 264.749 s and 95.107 s. The candidate exceeds both comparisons. Native benchmarks used this checkout's ordinary nextest profile and build-budget admission. The discarded four-thread and single-thread scheduling experiments did not meet the goals; the profile is unchanged.

Final commands: `cargo nextest run -p orbit-cli --test process --no-fail-fast --success-output immediate --tool-config-file test-budget:<process-config>` and the same command with `--test workspace` / `<workspace-config>`, each under `env -u ORBIT_WORKER_CONTEXT_REQUIRED ./scripts/build-budget.py --`. Logs and JUnit reports are under `.orbit/tmp/test-budget/`; `measurements.json` records the current host, head, counts, summed times and sandbox notices.

Original-attempt validation: both full binary measurements exited 0; `cargo fmt --all -- --check` and `git diff --check` passed. `make ci-fast`, `make ci-test-affected`, `make ci-lint`, `make goldens` and `make check-ci-macos` were not run. Native CI filters still name the retained `generation_root::` and `run_observation::` modules, but their selection has not been validated by the gate. Local CodeQL does not apply to this test-only change.

## Native salvage setup changes

The original candidate and failed/skipped evidence are preserved under `.orbit/tmp/salvage-15953-20261013/`. The source checkpoint was rebased onto `c2681b53e396d7f5e754691028788bb8b804f353` before profiling. These focused measurements are exploratory; they do not establish either full-binary runtime limit or the native-half criterion.

- Read-only updater JSON, human-output and checkout-build refusal checks reuse one actual copied executable. Unknown, cargo and managed channel checks rename the same assessed file only after each child exits, so each next CLI still classifies its actual executable path. Replacement, handover, concurrency and quiescence fixtures retain distinct images and their deadlines.
- GC's five eligible-state fixtures use the public registered-runtime composition and public task/archive/run lifecycle operations. Registry composition synchronizes the machine-owned task prefix and workspace binding. Each run records a real guarded child owner, finalizes, then releases and reaps that child before collection; a still-live owner remains ineligible. The existing CLI/routine GC assertions still execute.
- Scratch overrides still submit the shipped job through the real CLI, then observe that actual run through the public wait API at its admitted one-second cadence. The default positive and negative `--wait`/output/exit controls remain through the real CLI. Retention, symlink, outside-target preservation and input-refusal assertions remain.

The first profile isolated five setup-only zero-second GC jobs at 5.028–5.039 seconds each. After the setup replacement, the same existing GC table passed all three behaviors in 5.182 seconds (previous focused table 30.759 seconds), with each eligible-state seed taking 0.109–0.212 seconds. The existing scratch table passed in 14.393 seconds, with its override jobs taking 0.217 and 1.057 seconds; its default and negative CLI waits remained about five seconds each. The two updater tables passed in 13.973 seconds summed / 8.759 seconds wall. Native image assessment varied with concurrent fixtures, so these exploratory samples are not a controlled full-suite speedup claim.

RSA key generation/export measured only 0.022–0.055 seconds, so the genuine cryptographic fixtures retain their current key generation and signing. Temporary profiling hooks were removed, and profile-only local-candidate, integrity and generation-helper source files were restored byte-for-byte before full measurements. The failed private-module import and still-live-owner seed experiment remain in their logs; neither is a passing baseline or accepted control. Formatting and diff checks passed after the fixes. The first clean salvage checkpoint `b6a43fba9eaf1d5723e7ec7c4f59bd7c10c5dfb3` then ran both full native binaries together through the build-budget wrapper with `ORBIT_REQUIRE_SANDBOX_EXEC=1`, `CARGO_BUILD_JOBS=4` and the private salvage target. All 138 selected tests passed; six existing helper entry points remained ignored, with zero `DEFERRED:` or sandbox-skip notices. Process: 74 tests / 465.681 seconds summed. Workspace: 64 tests / 145.636 seconds summed. Combined nextest wall: 154.393 seconds. The environment/background context and exact command are retained in `native-candidate-context.json`, and the JUnit totals in `native-candidate-measurements.json`. This is positive native behavior evidence, but both runtime targets remain unmet against the original same-host baseline. Required repository gates have not run; further bounded setup work is in progress.

The second bounded cohort used identical default-profile filters and native settings before and after: `run_observation_contracts_0`, `update_flow_contracts_0` and `update_command_contracts_0`. All three existing entries passed both times. Observation fell from 27.577 to 15.663 seconds; the four equal-version/no-swap updater rows fell from 14.942 to 7.151 seconds. The untouched flow table varied from 6.717 to 5.502 seconds, which is not an optimization claim. The selected sums were 49.236 and 28.316 seconds; walls were 27.577 and 15.663 seconds.

The security-floor matrix originally retained all four real CLI submissions/input parsing and subsequent CLI `run show` assertions, including the input override followed by restoration of the workspace floor. The later retirement ledger removes the duplicate pre-override workspace-high tuple and keeps its stronger post-override counterpart. The built-in row retains the actual CLI `--wait`; the remaining precedence rows observe their actual workers through the public one-second wait cadence with terminal-success checks. The four updater root/admission rows share only an unchanged native image; each keeps its independent HOME/configuration/workspace/generation state and verifies image bytes after its row. Actual swap, TOCTOU, compatibility and recovery fixtures retain independent images. Oversized fake-installation setup measured below a millisecond, so it was left unchanged, along with all four actual source-limit refusals. No test entry was added or skipped. Full-binary acceptance remains unmet until measured on the completed candidate.

The third paired cohort selected the existing live workspace sweep boundary and GC table with the same filter before and after. Both entries passed: live sweep 13.960 → 3.910 seconds; GC 5.264 → 5.300 seconds (variation only). Selected sums: 19.224 → 9.210 seconds; walls: 13.960 → 5.301 seconds. Sweep's two setup-only seed calls fell from 5.108/5.118 to 0.158/0.082 seconds by reusing GC's public registered-runtime/task/run lifecycle helper with its real reaped owner. The actual filtered and unfiltered scheduler passes, CLI projections, eligibility attribution and `RoutineRuns` owner-exit/unwind cleanup remain. Profiling was removed before the source checkpoint. No new full-binary claim follows from this focused pair.

A stripped-executable experiment was rejected. The private debug binary is 221,164,392 bytes, including 121,434,984 bytes of Mach-O `__LINKEDIT`. APFS fixture copies measured below a millisecond and local-candidate manifest production about 0.108 seconds; fresh-image assessment took seconds. A standard native `strip -u -r` copy was 127,612,464 bytes, retained byte-identical loaded text/data/unwind sections, verified its ad-hoc signature and reported the identical full update contract/version. Paired fresh-copy launches measured 1.369 seconds original versus 0.923–0.976 seconds stripped. However, substituting this genuine same-source image into the existing two local-candidate tables caused the TOCTOU table's live-client control to fail opening its isolated store (`local_candidate.rs`, client-while-validating assertion). The other table passed, but the failed cohort is not accepted performance or compatibility evidence. The original cohort passed both entries (27.238/23.217 seconds). No stripping fixture helper was retained; the cause of the experimental failure has not been established. Probe JSON, signatures/source digests, paired JUnit/logs and the temporary substitution patch remain in `strip-probe-*` and `profile-strip-*` artifacts. Original source was restored byte-for-byte.

A separate existing scratch-GC row diagnostic passed and captured its real default CLI wait: `worktree_gc_pipeline` reported 263 ms execution, terminal at `2026-10-13T13:27:06.297735Z`, while the command returned after 5.040826 seconds. CLI `command/run/support.rs` supplies the core default five-second status-poll interval; `application/job/pipeline/wait.rs` reconciles and reads settled states before sleeping only when the run remains active. This is observation latency, not a mandatory ownership-cleanup dwell. The public runtime and `pipeline.wait` tool already accept a one-second poll; agent invocation uses one second, while CLI `WaitArgs` exposes only wait/deadline controls. A production latency change belongs in a separate scope and must retain reconciliation, deadlines, durable-state and exit-status contracts. No production change was made here. The output-only probe was restored byte-for-byte, and `profile-wait-*` retains the command, JUnit and actual CLI JSON.

The preserved clean checkpoint `2f8121ab9f408a164348fe71032955b6a386ab90` was rebased without conflicts onto fresh `origin/agent-main` `cd641b30720c413321762f4d21d923fc385741d2`, which includes ORB-15988 / PR4688's CLI one-second wait polling and ORB-15982 / PR4687's documentation reconciliation. The resulting source checkpoint `ddf1685bd0cbc48d37d00a895c450d196400aa03` ran one full native process/workspace measurement with the same default nextest profile, build-budget wrapper, private target, `CARGO_BUILD_JOBS=4` and `ORBIT_REQUIRE_SANDBOX_EXEC=1`. All 138 selected tests passed; six existing helpers remained ignored, with zero deferral or sandbox-skip notices. Process: 74 tests / **445.438 seconds summed**. Workspace: 64 tests / **119.694 seconds summed**. Combined nextest wall: **142.725 seconds**; compile plus run: 179.068 seconds. Source was clean before and after. `native-post-wait-context.json`, `native-post-wait.xml`, `native-post-wait.log` and `native-post-wait-measurements.json` retain exact source/base/binary identity, settings, background load and native stdout. The original checkpoint/evidence remain preserved.

Both native-half budgets still fail: process 445.438 > 264.7485 seconds; workspace 119.694 > 95.1065 seconds. Counts meet their separate goals, but there is no runtime waiver or completion claim. Relative to the original same-host baseline (529.497/190.213 seconds), these totals include **both fixture changes and the separately landed product wait fix**, plus other upstream changes; they are not fixture-only causal savings. The earlier controlled fixture cohorts remain separately reported above. Native Mac engine measurements were paused and no competing build was active at launch; normal Orbit services remained, and host load rose from 1.87/2.52/2.64 to 8.07/5.66/3.96 during compilation/tests. The original baseline's contained-worker safety deferrals remain a limitation of that historical comparison, while this candidate completed the native assertions positively. No full run was repeated to seek a pass, and repository gates remain not run.

The latest remaining costs are real updater images and lifecycle work: update 201.742 seconds (local-candidate tables 48.486/38.965), run observation 94.336, detached workers 56.280 and the two real dashboard handovers 49.079. Workspace host registry/routing total 17.316/16.069, and generation-root controls 9.491. Store-GC and scratch-GC tables now take 4.091/3.902 seconds following the product polling fix. The rejected stripped-image experiment and all genuine swap/TOCTOU/client/refusal/recovery/descendant cleanup controls remain recorded; no additional fixture mechanism or test retirement was justified by this measurement.

Three narrow admission reductions follow the preserved pre-change full run. Native tampered-manifest crypto duplicates the existing direct public RSA tamper test; the retained native revoked-key row still authenticates a genuinely signed manifest, refuses before archive download, and proves no installation/generation mutation, backup or staging. The separate successful 2400-second agent invocation duplicates work the existing successful 30-second answer already performs; its API and engine dispatch-bound assertions move onto that actual answer run. Default/maximum acceptance, zero/over-maximum refusal, provider failure/timeouts, redaction and log-follow controls remain. The security matrix's pre-override workspace-high run duplicates its stronger post-override run; built-in floor, explicit input parsing and subsequent workspace floor/sticky-reset assertions remain through the real shipped job. No other updater, TOCTOU, quiescence, compatibility, security or dashboard case is retired. The recursive trace collector is the existing collector moved to shared local scope, not a new fixture mechanism. Focused validation passed: the unchanged public `security::tampered_manifest_fails_signature_verification` selected exactly one test and passed; the existing revoked-key/checksum table, agent invocation table (including relocated interior transmission, default/ceiling and refusal controls), and security/observation table all passed, exactly three entries selected. Commands, source patch hash, stdout and JUnit remain in `wave4-focused-*`; the public crypto case took 0.013 seconds wall and the CLI cohort 15.307 seconds wall. No native deferral/skip notices appeared. The 445.438/119.694-second totals above remain the preserved pre-change comparison; the final full result follows.

The final authorized full native run used clean source `d9c725402b694e676a84048c30535a2be953c7ac` on the same actual production base `cd641b30720c413321762f4d21d923fc385741d2`, matching default profile, build-budget/private target, jobs=4 and required native sandbox settings. All 138 selected tests passed; six existing helper identities remained ignored, with zero deferral/sandbox-skip notices. **Process: 74 tests / 435.024 seconds summed. Workspace: 64 tests / 118.964 seconds summed. Combined test wall: 139.978 seconds.** Compile plus run: 140.304 seconds. Source was clean before and after. `native-final-admission-{context.json,measurements.json,xml,log,toml}` preserves the exact source, command, native stdout and original-host thresholds. The shared `origin/agent-main` ref had advanced to `0c9fb39d` via another checkout before launch; this is preserved as `origin_ref_at_start`, not falsely used as the candidate's ancestry base. No extra rebase or timing retry followed. The production executable SHA-256 is identical to the pre-change full run: `d27c84f18fbc23a3ad37a075fdc8ca5d3469679a6b2acc95114454d85f912439`.

The three changed entries measured pre-change → final: native integrity 10.635 → 5.287 seconds; agent invocation 13.392 → 11.798; security/observation 9.803 → 8.032 (33.830 → 25.117 summed). Whole-binary process moved 445.438 → 435.024 and workspace 119.694 → 118.964; workspace source was unchanged in this cohort, so its variation is not an optimization claim. These are real same-source-production observations with normal background load, not an isolated causal estimate: load averages were 2.75/2.96/3.30 at launch and 5.76/4.05/3.68 after, with normal Orbit services present and no competing Mac benchmark/compile. **Both summed-time goals remain unmet** (435.024 > 264.7485; 118.964 > 95.1065). Counts alone do not satisfy the task, and no threshold is relaxed. Comparisons to the original 529.497/190.213-second native baseline still include the separately landed product polling fix and other upstream changes. Repository gates remain not run; native behavior is positively exercised, but runtime acceptance is not achieved.

A final small quoted-recovery reduction preserves the actual reported shell retry and adds only `--json` for its output view. That retry itself must now report `already_current`, `replaced=false`, all original successful convergence steps and unchanged installed bytes. The subsequent independent local-update invocation retires; the bootstrap row still proves separate idempotent replay and backup preservation. The existing `local_candidate_contracts_1` entry passed once with the change, also retaining its FIFO/accepted-inode/client controls. `quoted-retry-focused-*` records exactly one selected entry, source patch, command and stdout. This was a correctness control during a sibling Mac lint gate, not a new timing comparison. All completed Mac full-run misses above remain unchanged. One first Linux baseline/candidate pair is authorized on the original task's owner host, against actual ancestry base `cd641b307`, with identical 10-slot/4-job/4-thread budget; no Linux result is claimed before the pair completes.

## Retirement map for the PR body

Each line maps one old standalone identity to its surviving table and same-named row, or gives the redundancy reason. Both the function and the row retain their original platform guards.

### process

| Previous entry point | Surviving coverage / reason |
| --- | --- |
| `run_observation::successful_security_sweep_shows_filing_floor_source_and_excluded_alerts` | `run_observation::run_observation_contracts_0` / `successful_security_sweep_shows_filing_floor_source_and_excluded_alerts` |
| `run_observation::shipped_security_job_uses_config_unless_run_input_overrides_it` | `run_observation::run_observation_contracts_0` / same row, retaining built-in moderate, explicit critical override and subsequent workspace-high/sticky reset. The pre-override workspace-high tuple retires as identical to the stronger post-override workspace-high tuple. |
| `run_observation::readiness_bootstrap_preserves_stale_runs_and_held_reservations` | `run_observation::run_observation_contracts_0` / `readiness_bootstrap_preserves_stale_runs_and_held_reservations` |
| `run_observation::readiness_reports_total_and_notifies_only_when_backlog_is_truncated` | `run_observation::run_observation_contracts_0` / `readiness_reports_total_and_notifies_only_when_backlog_is_truncated` |
| `run_observation::job_replay_cli_reexecutes_deterministic_fixture_with_persisted_lineage` | `run_observation::run_observation_contracts_1` / `job_replay_cli_reexecutes_deterministic_fixture_with_persisted_lineage` |
| `run_observation::no_reconcile_run_reads_leave_stale_runs_and_reservations_unchanged` | `run_observation::run_observation_contracts_1` / `no_reconcile_run_reads_leave_stale_runs_and_reservations_unchanged` |
| `run_observation::default_run_reads_still_reconcile_stale_runs` | `run_observation::run_observation_contracts_1` / `default_run_reads_still_reconcile_stale_runs` |
| `run_observation::explicit_run_lookup_preserves_store_errors` | `run_observation::run_observation_contracts_1` / `explicit_run_lookup_preserves_store_errors` |
| `run_observation::job_run_alias_produces_a_completed_trace_and_terminal_cancel_is_stable` | `run_observation::run_observation_contracts_2` / `job_run_alias_produces_a_completed_trace_and_terminal_cancel_is_stable` |
| `run_observation::cancel_task_leaf_requeues_by_default_and_block_is_explicit` | `run_observation::run_observation_contracts_2` / `cancel_task_leaf_requeues_by_default_and_block_is_explicit` |
| `run_observation::cancelling_a_live_run_keeps_the_signalled_worker_exit_as_its_outcome` | `run_observation::run_observation_contracts_2` / `cancelling_a_live_run_keeps_the_signalled_worker_exit_as_its_outcome` |
| `run_observation::replacement_drain_counts_and_reports_inherited_workers_until_they_finish` | `run_observation::run_observation_contracts_2` / `replacement_drain_counts_and_reports_inherited_workers_until_they_finish` |
| `run_observation::run_concurrency_updates_persisted_revision_and_refuses_stale_writes` | `run_observation::run_observation_contracts_3` / `run_concurrency_updates_persisted_revision_and_refuses_stale_writes` |
| `run_observation::run_extend_lengthens_a_live_drain_and_a_repeat_start_still_queues` | `run_observation::run_observation_contracts_3` / `run_extend_lengthens_a_live_drain_and_a_repeat_start_still_queues` |
| `run_observation::a_drain_at_concurrency_twenty_runs_twenty_delivery_wrappers_at_once` | `run_observation::run_observation_contracts_3` / `a_drain_at_concurrency_twenty_runs_twenty_delivery_wrappers_at_once` |
| `run_observation::run_concurrency_retunes_a_pull_drain_past_the_former_leaf_ceiling` | `run_observation::run_observation_contracts_3` / `run_concurrency_retunes_a_pull_drain_past_the_former_leaf_ceiling` |
| `run_observation::a_live_drains_recorded_throttle_reaches_readiness_run_show_and_ship` | `run_observation::run_observation_contracts_4` / `a_live_drains_recorded_throttle_reaches_readiness_run_show_and_ship` |
| `run_observation::run_show_counts_local_deferred_tasks_once` | `run_observation::run_observation_contracts_4` / `run_show_counts_local_deferred_tasks_once` |
| `run_observation::run_show_names_the_tasks_a_pull_drains_owner_kept_off_this_host` | `run_observation::run_observation_contracts_4` / `run_show_names_the_tasks_a_pull_drains_owner_kept_off_this_host` |
| `run_observation::run_show_reports_deferred_tasks_beyond_the_listed_ones` | `run_observation::run_observation_contracts_4` / `run_show_reports_deferred_tasks_beyond_the_listed_ones` |
| `run_observation::run_show_names_the_pull_breaker` | `run_observation::run_observation_contracts_5` / `run_show_names_the_pull_breaker` |
| `run_observation::run_show_exposes_degraded_pull_pass_health` | `run_observation::run_observation_contracts_5` / `run_show_exposes_degraded_pull_pass_health` |
| `run_observation::doctor_warns_when_the_replicas_gates_differ_from_the_owners` | `run_observation::run_observation_contracts_5` / `doctor_warns_when_the_replicas_gates_differ_from_the_owners` |
| `run_observation::force_cancel_reports_unstopped_local_children_and_exits_one` | `run_observation::run_observation_contracts_5` / `force_cancel_reports_unstopped_local_children_and_exits_one` |
| `run_observation::a_surface_reservation_reaches_readiness_and_drain_run_show` | `run_observation::run_observation_contracts_6` / `a_surface_reservation_reaches_readiness_and_drain_run_show` |
| `run_observation::run_show_marks_an_agent_blocked_on_a_stopped_descendant` | `run_observation::run_observation_contracts_6` / `run_show_marks_an_agent_blocked_on_a_stopped_descendant` |
| `run_observation::build_budget::readiness_and_doctor_warn_for_local_and_pull_drain_capacity` | `run_observation::build_budget::build_capacity_contracts` / `readiness_and_doctor_warn_for_local_and_pull_drain_capacity` |
| `run_observation::build_budget::readiness_and_doctor_stay_silent_for_a_checkout_outside_the_admission_wrapper` | `run_observation::build_budget::build_capacity_contracts` / `readiness_and_doctor_stay_silent_for_a_checkout_outside_the_admission_wrapper` |
| `run_observation::build_budget::readiness_stays_silent_when_the_wrapper_link_resolves_outside_the_checkout` | `run_observation::build_budget::build_capacity_contracts` / `readiness_stays_silent_when_the_wrapper_link_resolves_outside_the_checkout` |
| `run_observation::build_budget::invalid_build_slots_setting_surfaces_as_advisory_in_readiness_and_doctor` | `run_observation::build_budget::build_capacity_contracts` / `invalid_build_slots_setting_surfaces_as_advisory_in_readiness_and_doctor` |
| `run_observation::build_budget::leaf_show_retains_failed_attempt_and_live_wait_statistics` | `run_observation::build_budget::build_capacity_contracts` / `leaf_show_retains_failed_attempt_and_live_wait_statistics` |
| `run_observation::run_show_display::run_show_prints_human_durations_and_one_based_steps_in_every_human_mode` | `run_observation::run_show_display::run_show_contracts` / `run_show_prints_human_durations_and_one_based_steps_in_every_human_mode` |
| `run_observation::run_show_display::run_show_hides_shipped_catalog_layers_unless_verbose` | `run_observation::run_show_display::run_show_contracts` / `run_show_hides_shipped_catalog_layers_unless_verbose` |
| `run_observation::run_show_display::run_show_lists_audit_steps_for_an_interrupted_run_with_only_a_run_level_record` | `run_observation::run_show_display::run_show_contracts` / `run_show_lists_audit_steps_for_an_interrupted_run_with_only_a_run_level_record` |
| `run_observation::run_show_display::every_run_view_reads_the_step_an_interrupted_run_died_in_as_interrupted` | `run_observation::run_show_display::run_show_contracts` / `every_run_view_reads_the_step_an_interrupted_run_died_in_as_interrupted` |
| `run_observation::run_show_display::run_show_lists_a_final_recovery_resume_with_its_rationale` | `run_observation::run_show_display::run_show_contracts` / `run_show_lists_a_final_recovery_resume_with_its_rationale` |
| `run_observation::agent_invoke::agent_wait_redacts_sensitive_warning_content_in_progress_and_output` | `run_observation::agent_invoke::agent_invocation_contracts_0` / `agent_wait_redacts_sensitive_warning_content_in_progress_and_output` |
| `run_observation::agent_invoke::agent_wait_prints_the_answer_and_exits_nonzero_for_failed_invocations` | `run_observation::agent_invoke::agent_invocation_contracts_0` / `agent_wait_prints_the_answer_and_exits_nonzero_for_failed_invocations` |
| `run_observation::agent_invoke::agent_timeout_admission_and_engine_use_the_same_ceiling` | `run_observation::agent_invoke::agent_invocation_contracts_0` retains default/ceiling acceptance, zero/over-ceiling refusal and loaded-activity comparison. Its repeated 2400-second successful invocation retires; explicit interior timeout/engine trace checks now use the already-executed 30-second answer invocation in `agent_wait_prints_the_answer_and_exits_nonzero_for_failed_invocations`. |
| `run_observation::agent_invoke::agent_submissions_report_saturation_and_their_queue_position` | `run_observation::agent_invoke::agent_invocation_contracts_1` / `agent_submissions_report_saturation_and_their_queue_position` |
| `run_observation::agent_invoke::run_logs_follow_emits_live_output_once_and_stops_at_terminal` | `run_observation::agent_invoke::agent_invocation_contracts_1` / `run_logs_follow_emits_live_output_once_and_stops_at_terminal` |
| `run_observation::agent_invoke::run_logs_follow_exits_cleanly_when_merged_output_reader_closes_early` | `run_observation::agent_invoke::agent_invocation_contracts_1` / `run_logs_follow_exits_cleanly_when_merged_output_reader_closes_early` |
| `run_observation::job_exit_status::failing_replay_exits_nonzero_and_preserves_its_result` | `run_observation::job_exit_status::job_exit_contracts` / `failing_replay_exits_nonzero_and_preserves_its_result` |
| `run_observation::job_exit_status::held_run_resume_and_replay_exit_nonzero` | `run_observation::job_exit_status::job_exit_contracts` / `held_run_resume_and_replay_exit_nonzero` |
| `run_observation::wait::wait_expiry_reports_active_state_without_timing_out_the_run` | `run_observation::wait::wait_contracts` / `wait_expiry_reports_active_state_without_timing_out_the_run` |
| `run_observation::wait::invalid_wait_deadline_does_not_submit_work` | `run_observation::wait::wait_contracts` / `invalid_wait_deadline_does_not_submit_work` |
| `job_resume_detached::unix::ship_and_auto_cli_commands_launch_workers` | `job_resume_detached::unix::worker_command_contracts_0` / `ship_and_auto_cli_commands_launch_workers` |
| `job_resume_detached::unix::auto_refuses_approve_proposed_with_pull` | `job_resume_detached::unix::worker_command_contracts_0` / `auto_refuses_approve_proposed_with_pull` |
| `job_resume_detached::unix::auto_pull_validates_its_crew_restriction_before_submitting` | `job_resume_detached::unix::worker_command_contracts_0` / `auto_pull_validates_its_crew_restriction_before_submitting` |
| `job_resume_detached::unix::auto_warns_when_required_validation_cannot_use_the_login_shell` | `job_resume_detached::unix::worker_command_contracts_1` / `auto_warns_when_required_validation_cannot_use_the_login_shell` |
| `job_resume_detached::unix::gate_releases_reservation_after_child_failure_or_success` | `job_resume_detached::unix::worker_command_contracts_1` / `gate_releases_reservation_after_child_failure_or_success` |
| `job_resume_detached::unix::run_show_step_output_survives_an_earlier_when_skipped_step` | `job_resume_detached::unix::worker_command_contracts_1` / `run_show_step_output_survives_an_earlier_when_skipped_step` |
| `job_resume_detached::unix::resume_returns_while_detached_worker_continues_after_cli_exit` | `job_resume_detached::unix::worker_command_contracts_2` / `resume_returns_while_detached_worker_continues_after_cli_exit` |
| `job_resume_detached::unix::resume_wait_joins_detached_run_and_reports_success` | `job_resume_detached::unix::worker_command_contracts_2` / `resume_wait_joins_detached_run_and_reports_success` |
| `job_resume_detached::unix::resume_wait_exits_nonzero_for_a_failed_detached_run` | `job_resume_detached::unix::worker_command_contracts_2` / `resume_wait_exits_nonzero_for_a_failed_detached_run` |
| `job_resume_detached::lifecycle::completion_resume_restores_review_and_completes_merged_pr` | `job_resume_detached::lifecycle::resume_lifecycle_contracts_0` / `completion_resume_restores_review_and_completes_merged_pr` |
| `job_resume_detached::lifecycle::resumed_terminal_runs_block_readmitted_tasks` | `job_resume_detached::lifecycle::resume_lifecycle_contracts_0` / `resumed_terminal_runs_block_readmitted_tasks` |
| `job_resume_detached::lifecycle::completion_resume_cannot_invent_review_or_reverse_withdrawal` | `job_resume_detached::lifecycle::resume_lifecycle_contracts_0` / `completion_resume_cannot_invent_review_or_reverse_withdrawal` |
| `job_resume_detached::lifecycle::resume_preserves_promotion_across_non_completion_tails` | `job_resume_detached::lifecycle::resume_lifecycle_contracts_1` / `resume_preserves_promotion_across_non_completion_tails` |
| `job_resume_detached::lifecycle::resume_readmits_lineage_failure_handoff_blocks` | Seven provenance decisions survive in existing core `resume_ownership::resume_of_a_lineage_binding_keeps_the_checkpoint_batch_id`; CLI `resume_lifecycle_contracts_1` / same row retains unpromoted and promoted real-worker terminal/history controls. See current-source increment correspondence below. |
| `update::update_preserves_explicit_and_environment_roots_from_another_checkout` | `update::update_command_contracts_0` / `update_preserves_explicit_and_environment_roots_from_another_checkout` |
| `update::update_without_a_root_override_retains_default_workspace_routing` | `update::update_command_contracts_0` / `update_without_a_root_override_retains_default_workspace_routing` |
| `update::update_and_preflight_admit_against_the_same_overridden_root` | `update::update_command_contracts_0` / `update_and_preflight_admit_against_the_same_overridden_root` |
| `update::a_live_host_global_pin_refuses_an_overridden_root_update_in_either_spelling` | `update::update_command_contracts_0` / `a_live_host_global_pin_refuses_an_overridden_root_update_in_either_spelling` |
| `update::help_documents_both_latest_and_explicit_version_selection` | `update::update_command_contracts_1` / `help_documents_both_latest_and_explicit_version_selection` |
| `update::check_reports_the_available_release_and_exits_three` | `update::update_command_contracts_1` / `check_reports_the_available_release_and_exits_three` |
| `update::check_renders_a_prefix_free_remediation_sentence` | `update::update_command_contracts_1` / `check_reports_the_available_release_and_exits_three` calls the original human-output helper on the same assessed read-only executable |
| `update::check_is_quiet_and_succeeds_when_the_installed_release_is_current` | `update::update_command_contracts_1` / `check_is_quiet_and_succeeds_when_the_installed_release_is_current` |
| `update::check_succeeds_without_recommending_an_update_when_the_published_release_is_older` | `update::update_command_contracts_2` / `check_succeeds_without_recommending_an_update_when_the_published_release_is_older` |
| `update::applying_an_update_to_a_checkout_build_is_refused_with_the_command_that_works` | `update::update_command_contracts_1` / `check_reports_the_available_release_and_exits_three` calls the original refusal helper on the same assessed read-only executable |
| `update::installed_and_unknown_location_binaries_are_not_checkout_builds` | `update::update_command_contracts_2` / `installed_and_unknown_location_binaries_are_not_checkout_builds` |
| `update::an_unparseable_requested_version_is_rejected_before_any_network_or_disk_work` | `update::update_command_contracts_2` / `an_unparseable_requested_version_is_rejected_before_any_network_or_disk_work` |
| `update::integrity::a_checksum_mismatch_never_reaches_the_install_path_and_staging_is_cleaned` | `update::integrity::release_integrity_contracts_0` / `a_checksum_mismatch_never_reaches_the_install_path_and_staging_is_cleaned` |
| `update::integrity::a_manifest_signed_over_different_bytes_is_rejected_before_download` | Retired duplicate: `orbit-common/tests/mechanisms/security.rs::tampered_manifest_fails_signature_verification` retains real RSA rejection and its diagnostic; `update::integrity::a_revoked_key_fails_closed_before_expiry` retains native authenticated-release refusal before download with untouched installation/generation, no backup and clean staging. |
| `update::integrity::a_revoked_key_fails_closed_before_expiry` | `update::integrity::release_integrity_contracts_0` / `a_revoked_key_fails_closed_before_expiry` |
| `update::integrity::an_archive_with_extra_members_including_traversal_is_rejected` | `update::integrity::release_integrity_contracts_1` / `an_archive_with_extra_members_including_traversal_is_rejected` |
| `update::integrity::a_mislabeled_release_is_rejected_without_attempting_the_swap` | `update::integrity::release_integrity_contracts_1` / `a_mislabeled_release_is_rejected_without_attempting_the_swap` |
| `update::integrity::a_downgrade_is_refused_unless_explicitly_requested` | `update::integrity::release_integrity_contracts_1` / `a_downgrade_is_refused_unless_explicitly_requested` |
| `update::integrity::a_second_concurrent_update_is_refused` | `update::integrity::release_integrity_contracts_2` / `a_second_concurrent_update_is_refused` |
| `update::integrity::a_stale_writer_and_an_older_binary_never_displace_a_newer_install` | `update::integrity::release_integrity_contracts_2` / `a_stale_writer_and_an_older_binary_never_displace_a_newer_install` |
| `update::integrity::an_update_waits_for_an_in_flight_clock_tick_then_installs` | `update::integrity::release_integrity_contracts_2` / `an_update_waits_for_an_in_flight_clock_tick_then_installs` |
| `update::integrity::an_update_refuses_a_clock_tick_that_outlasts_the_quiesce_bound` | `update::integrity::release_integrity_contracts_3` / `an_update_refuses_a_clock_tick_that_outlasts_the_quiesce_bound` |
| `update::integrity::a_release_update_admits_a_session_that_hands_over_and_pins_after_it_does` | `update::integrity::release_integrity_contracts_3` / `a_release_update_admits_a_session_that_hands_over_and_pins_after_it_does` |
| `update::integrity::a_hanging_candidate_probe_times_out_without_holding_an_authority` | `update::flow::update_flow_contracts_0` / `a_hanging_candidate_probe_times_out_without_holding_an_authority`: Moved to the public updater with readiness-triggered supervision clock; retains timeout, no replacement, staging/backup cleanup and free admission during/after the probe. |
| `update::local_candidate::an_older_installation_is_bootstrapped_to_an_equal_version_candidate_and_replays_idempotently` | `update::local_candidate::local_candidate_contracts_0` / `an_older_installation_is_bootstrapped_to_an_equal_version_candidate_and_replays_idempotently` |
| `update::local_candidate::wrong_provenance_digest_target_or_ownership_refuses_before_replacement` | `update::local_candidate::local_candidate_contracts_0` / `wrong_provenance_digest_target_or_ownership_refuses_before_replacement` |
| `update::local_candidate::a_live_discovered_workspace_client_blocks_a_local_candidate_update` | `update::local_candidate::local_candidate_contracts_0` / `a_live_discovered_workspace_client_blocks_a_local_candidate_update` |
| `update::local_candidate::a_candidate_without_the_admission_contract_is_refused_before_replacement` | `update::local_candidate::local_candidate_contracts_1` / `a_candidate_without_the_admission_contract_is_refused_before_replacement` |
| `update::local_candidate::clients_run_but_no_update_enters_mid_validation_and_the_accepted_bytes_are_installed` | `update::local_candidate::local_candidate_contracts_1` / `clients_run_but_no_update_enters_mid_validation_and_the_accepted_bytes_are_installed` |
| `update::local_candidate::a_post_swap_failure_recovers_from_a_quoted_install_dir` | `update::local_candidate::local_candidate_contracts_1` / same row. The real reported shell retry now carries the JSON outcome/convergence/replaced=false and unchanged-byte assertions; its subsequent independent replay retires as redundant with this stronger retry and the bootstrap row’s idempotent replay/backup-preservation control. |
| `update::flow::oversized_mirror_inputs_fail_before_executable_replacement` | `update::flow::update_flow_contracts_0` / `oversized_mirror_inputs_fail_before_executable_replacement` |
| `update::flow::successful_read_only_inspection_cannot_authorize_a_downgrade` | `update::flow::update_flow_contracts_0` / `successful_read_only_inspection_cannot_authorize_a_downgrade` |
| `update::flow::a_replacement_without_admission_protocol_is_refused_before_installation` | `update::flow::update_flow_contracts_0` / `a_replacement_without_admission_protocol_is_refused_before_installation` |
| `update::flow::an_update_refreshes_an_installed_bundled_bubblewrap_with_the_new_release` | `update::flow::update_flow_contracts_1` / `an_update_refreshes_an_installed_bundled_bubblewrap_with_the_new_release` |
| `update::flow::recovery_hint_targets_requested_version_when_newer_release_is_published` | `update::flow::update_flow_contracts_1` / `recovery_hint_targets_requested_version_when_newer_release_is_published` |
| `update::flow::recovery_hint_preserves_allow_downgrade_when_retrying_older_release` | `update::flow::update_flow_contracts_1` / `recovery_hint_preserves_allow_downgrade_when_retrying_older_release` |
| `update::bundled_bwrap::a_signed_listed_binary_is_staged_executable_with_its_digest` | `update::bundled_bwrap::bundled_bwrap_contracts` / `a_signed_listed_binary_is_staged_executable_with_its_digest` |
| `update::bundled_bwrap::unsigned_tampered_or_unlisted_binaries_are_refused` | `update::bundled_bwrap::bundled_bwrap_contracts` / `unsigned_tampered_or_unlisted_binaries_are_refused` |
| `update::local_candidate_inputs::executable_headers_map_to_release_targets_or_refuse` | `update::local_candidate_inputs::candidate_input_contracts` / `executable_headers_map_to_release_targets_or_refuse` |
| `update::local_candidate_inputs::manifests_are_v1_operator_attested_documents_only` | `update::local_candidate_inputs::candidate_input_contracts` / `manifests_are_v1_operator_attested_documents_only` |
| `worktree_resolution::tool_run_explicit_workspace_resolves_from_home_and_temp_directory` | `worktree_resolution::checkout_routing_contracts_0` / `tool_run_explicit_workspace_resolves_from_home_and_temp_directory` |
| `worktree_resolution::config_show_reports_shared_and_local_roots_for_git_worktrees_and_overrides` | `worktree_resolution::checkout_routing_contracts_0` / `config_show_reports_shared_and_local_roots_for_git_worktrees_and_overrides` |
| `worktree_resolution::submodule_bootstrap_and_runtime_keep_the_submodule_workspace` | `worktree_resolution::checkout_routing_contracts_0` / `submodule_bootstrap_and_runtime_keep_the_submodule_workspace` |
| `worktree_resolution::submodule_roots_remain_independent_when_git_is_unavailable` | `worktree_resolution::checkout_routing_contracts_0` / `submodule_roots_remain_independent_when_git_is_unavailable` |
| `worktree_resolution::submodule_runtime_respects_its_registered_data_directory` | `worktree_resolution::checkout_routing_contracts_1` / `submodule_runtime_respects_its_registered_data_directory` |
| `worktree_resolution::linked_worktree_gitfile_fallback_checks_the_common_directory` | `worktree_resolution::checkout_routing_contracts_1` / `linked_worktree_gitfile_fallback_checks_the_common_directory` |
| `worktree_resolution::workspace_list_skips_unchanged_registry_lock_and_refuses_unpersisted_validation` | `worktree_resolution::checkout_routing_contracts_1` / `workspace_list_skips_unchanged_registry_lock_and_refuses_unpersisted_validation` |
| `worktree_resolution::auto_task_show_reports_and_honors_the_selected_definition_source` | `worktree_resolution::checkout_routing_contracts_1` / `auto_task_show_reports_and_honors_the_selected_definition_source` |
| `worktree_resolution::auto_task_crud_from_linked_worktree_writes_definition_yaml_to_local_root` | `worktree_resolution::checkout_routing_contracts_2` / `auto_task_crud_from_linked_worktree_writes_definition_yaml_to_local_root` |
| `worktree_resolution::detached_worker_bootstraps_and_claims_while_registry_writer_is_held` | `worktree_resolution::checkout_routing_contracts_2` / `detached_worker_bootstraps_and_claims_while_registry_writer_is_held` |
| `worktree_resolution::doctor_graph_cleanup_uses_split_roots_and_keeps_json_stdout_clean` | `worktree_resolution::checkout_routing_contracts_2` / `doctor_graph_cleanup_uses_split_roots_and_keeps_json_stdout_clean` |
| `worktree_resolution::doctor_stale_lock_repair_keeps_locks_a_live_process_holds` | `worktree_resolution::checkout_routing_contracts_2` / `doctor_stale_lock_repair_keeps_locks_a_live_process_holds` |
| `worktree_resolution::doctor_graph_cleanup_never_deletes_through_symlinks` | `worktree_resolution::checkout_routing_contracts_3` / `doctor_graph_cleanup_never_deletes_through_symlinks` |
| `worktree_resolution::root_resolution_precedence` | `worktree_resolution::checkout_routing_contracts_3` / `root_resolution_precedence` |
| `worktree_resolution::doctor_providers_with_explicit_root_does_not_bind_parent_of_data_dir` | `worktree_resolution::checkout_routing_contracts_3` / `doctor_providers_with_explicit_root_does_not_bind_parent_of_data_dir` |
| `worktree_resolution::clean_root_workspace_init_then_auto_task_list_succeeds` | `worktree_resolution::checkout_routing_contracts_3` / `clean_root_workspace_init_then_auto_task_list_succeeds` |
| `worktree_resolution::workspace_init_force_rebinds_synthetic_data_dir_checkout` | `worktree_resolution::checkout_routing_contracts_4` / `workspace_init_force_rebinds_synthetic_data_dir_checkout` |
| `env_pass_warning::job_start_warns_once_naming_only_the_unset_variable_and_records_it` | `env_pass_warning::pass_list_warning_contracts` / `job_start_warns_once_naming_only_the_unset_variable_and_records_it` |
| `env_pass_warning::drain_and_ship_starts_report_the_unset_variable_and_record_it` | `env_pass_warning::pass_list_warning_contracts` / `drain_and_ship_starts_report_the_unset_variable_and_record_it` |
| `env_pass_warning::start_with_every_pass_listed_variable_set_is_silent_and_records_nothing` | `env_pass_warning::pass_list_warning_contracts` / `start_with_every_pass_listed_variable_set_is_silent_and_records_nothing` |
| `env_pass_warning::doctor_warns_for_an_unset_pass_listed_variable_and_is_ok_when_all_are_set` | `env_pass_warning::pass_list_warning_contracts` / `doctor_warns_for_an_unset_pass_listed_variable_and_is_ok_when_all_are_set` |
| `run_observation::replay_crew::replay_recaptures_automatic_crews_and_preserves_explicit_choices` | `orbit-core::runtime::dispatch_admission::replay_crew::detached_replay_recaptures_automatic_crews_and_preserves_explicit_choices`: Redundant five-row crew-policy recapture matrix; core asserts the same automatic/default/explicit/legacy/provider-hold choices and preserved input. CLI replay success, lineage and nonzero exits remain in run_observation_contracts_1 and job_exit_contracts. |

### workspace

| Previous entry point | Surviving coverage / reason |
| --- | --- |
| `generation_root::explicit_root_spellings_select_the_same_generation_host_sweep_and_admission_roots` | `generation_root::generation_admission_contracts_0` / `explicit_root_spellings_select_the_same_generation_host_sweep_and_admission_roots` |
| `generation_root::task_list_with_tilde_orbit_root_registers_its_participant_under_home` | `generation_root::generation_admission_contracts_0` / `task_list_with_tilde_orbit_root_registers_its_participant_under_home` |
| `generation_root::clock_ticks_during_generation_hold_emit_one_dated_summary_on_resume` | `generation_root::generation_admission_contracts_0` / `clock_ticks_during_generation_hold_emit_one_dated_summary_on_resume` |
| `generation_root::clock_ticks_refused_by_a_breaking_migration_are_reported_by_doctor` | `generation_root::generation_admission_contracts_0` / `clock_ticks_refused_by_a_breaking_migration_are_reported_by_doctor` |
| `generation_root::init_with_root_succeeds_when_home_orbit_is_readonly_and_unpinned` | `generation_root::generation_admission_contracts_1` / `init_with_root_succeeds_when_home_orbit_is_readonly_and_unpinned` |
| `generation_root::init_with_isolated_home_writes_generation_lock_under_home_orbit` | `generation_root::generation_admission_contracts_1` / `init_with_isolated_home_writes_generation_lock_under_home_orbit` |
| `generation_root::preflight_with_root_refuses_an_unwritable_host_global_authority` | `generation_root::generation_admission_contracts_1` / `preflight_with_root_refuses_an_unwritable_host_global_authority` |
| `generation_root::preflight_with_root_reports_both_authorities_when_home_orbit_is_writable` | `generation_root::generation_admission_contracts_1` / `preflight_with_root_reports_both_authorities_when_home_orbit_is_writable` |
| `generation_root::preflight_on_isolated_home_is_blocked_by_a_live_exclusive_holder` | `generation_root::generation_admission_contracts_2` / `preflight_on_isolated_home_is_blocked_by_a_live_exclusive_holder` |
| `generation_root::isolated_home_refuses_a_different_executable_digest_while_a_pin_is_held` | `generation_root::generation_admission_contracts_2` / `isolated_home_refuses_a_different_executable_digest_while_a_pin_is_held` |
| `generation_root::init_with_root_ignores_a_foreign_pin_on_home_orbit` | `generation_root::generation_admission_contracts_2` / `init_with_root_ignores_a_foreign_pin_on_home_orbit` |
| `generation_root::preflight_with_root_admits_uninitialized_override_cwd_and_host_global` | `generation_root::generation_admission_contracts_2` / `preflight_with_root_admits_uninitialized_override_cwd_and_host_global` |
| `generation_root::preflight_with_orbit_root_admits_uninitialized_override_cwd_and_host_global` | `generation_root::generation_admission_contracts_3` / `preflight_with_orbit_root_admits_uninitialized_override_cwd_and_host_global` |
| `generation_root::preflight_refuses_a_live_cwd_client_for_root_and_orbit_root` | `generation_root::generation_admission_contracts_3` / `preflight_refuses_a_live_cwd_client_for_root_and_orbit_root` |
| `generation_root::preflight_with_initialized_root_does_not_add_a_different_cwd_workspace` | `generation_root::generation_admission_contracts_3` / `preflight_with_initialized_root_does_not_add_a_different_cwd_workspace` |
| `generation_root::preflight_deduplicates_a_host_global_alias_and_keeps_the_cwd_workspace` | `generation_root::generation_admission_contracts_3` / `preflight_deduplicates_a_host_global_alias_and_keeps_the_cwd_workspace` |
| `generation_root::preflight_reports_a_broken_cwd_workspace_config` | `generation_root::generation_admission_contracts_4` / `preflight_reports_a_broken_cwd_workspace_config` |
| `generation_root::update_with_uninitialized_root_still_requires_a_workspace` | `generation_root::generation_admission_contracts_4` / `update_with_uninitialized_root_still_requires_a_workspace` |
| `host_registry::tilde_root_host_list_matches_the_absolute_root_with_a_registered_remote` | `host_registry::host_registry_contracts_0` / `tilde_root_host_list_matches_the_absolute_root_with_a_registered_remote` |
| `host_registry::host_add_reads_identity_from_the_remote_and_refusals_leave_the_file_untouched` | `host_registry::host_registry_contracts_0` / `host_add_reads_identity_from_the_remote_and_refusals_leave_the_file_untouched` |
| `host_registry::host_list_show_rename_and_remove_report_live_state_and_dependents` | `host_registry::host_registry_contracts_0` / `host_list_show_rename_and_remove_report_live_state_and_dependents` |
| `host_registry::doctor_never_reports_a_match_for_a_host_that_omits_its_version_and_protocol` | `host_registry::host_registry_contracts_0` / `doctor_never_reports_a_match_for_a_host_that_omits_its_version_and_protocol` |
| `host_registry::doctor_migration_command_adds_an_existing_legacy_host_and_clears_the_warning` | `host_registry::host_registry_contracts_1` / `doctor_migration_command_adds_an_existing_legacy_host_and_clears_the_warning` |
| `host_registry::legacy_removal_never_probes_the_removed_host_and_requires_retained_hosts` | `host_registry::host_registry_contracts_1` / `legacy_removal_never_probes_the_removed_host_and_requires_retained_hosts` |
| `host_registry::legacy_destinations_migrate_on_first_mutation_and_both_files_are_refused` | `host_registry::host_registry_contracts_1` / `legacy_destinations_migrate_on_first_mutation_and_both_files_are_refused` |
| `host_registry::legacy_rows_whose_machine_name_and_ssh_alias_collide_migrate_to_unique_names` | `host_registry::host_registry_contracts_1` / `legacy_rows_whose_machine_name_and_ssh_alias_collide_migrate_to_unique_names` |
| `host_routing::task_show_projections_match_the_owning_host` | `host_routing::host_routing_contracts_0` / `task_show_projections_match_the_owning_host` |
| `host_routing::cli_task_ids_route_to_the_host_their_prefix_names` | `host_routing::host_routing_contracts_0` / `cli_task_ids_route_to_the_host_their_prefix_names` |
| `host_routing::federated_mcp_routes_an_id_only_call_by_prefix` | `host_routing::host_routing_contracts_0` / `federated_mcp_routes_an_id_only_call_by_prefix` |
| `host_routing::a_routed_call_carries_the_callers_authority_and_no_more` | `host_routing::host_routing_contracts_0` / `a_routed_call_carries_the_callers_authority_and_no_more` |
| `host_routing::a_write_through_a_mirror_selector_is_refused_by_that_hosts_sole_writer_rule` | `host_routing::host_routing_contracts_1` / `a_write_through_a_mirror_selector_is_refused_by_that_hosts_sole_writer_rule` |
| `host_routing::local_task_commands_refuse_remote_selection_without_delivering_or_mutating` | `host_routing::host_routing_contracts_1` / `local_task_commands_refuse_remote_selection_without_delivering_or_mutating` |
| `host_routing::routed_task_update_refusals_offer_a_command_that_works` | `host_routing::host_routing_contracts_1` / `routed_task_update_refusals_offer_a_command_that_works` |
| `host_routing::host_flag_resolves_to_the_selector_that_host_lists` | `host_routing::host_routing_contracts_1` / `host_flag_resolves_to_the_selector_that_host_lists` |
| `host_routing::host_ssh_command_hint_quotes_metacharacters_and_roundtrips_posix_argv` | `host_routing::host_routing_contracts_2` / `host_ssh_command_hint_quotes_metacharacters_and_roundtrips_posix_argv` |
| `routine_root::routine_list_preserves_unchanged_registry_modification_time` | `routine_root::routine_command_contracts_0` / `routine_list_preserves_unchanged_registry_modification_time` |
| `routine_root::routine_list_prints_next_due_and_last_fire_in_one_zone` | `routine_root::routine_command_contracts_0` / `routine_list_prints_next_due_and_last_fire_in_one_zone` |
| `routine_root::routine_list_honors_explicit_root_over_uninitialized_home_and_environment` | `routine_root::routine_command_contracts_0` / `routine_list_honors_explicit_root_over_uninitialized_home_and_environment` |
| `routine_root::routine_list_workspace_selector_scopes_results_and_reports_empty_workspaces` | `routine_root::routine_command_contracts_0` / `routine_list_workspace_selector_scopes_results_and_reports_empty_workspaces` |
| `routine_root::routine_commands_honor_orbit_root_and_mutate_only_the_selected_root` | `routine_root::routine_command_contracts_1` / `routine_commands_honor_orbit_root_and_mutate_only_the_selected_root` |
| `routine_root::routine_pause_rejects_unknown_names_and_reports_json_for_known_ones` | `routine_root::routine_command_contracts_1` / `routine_pause_rejects_unknown_names_and_reports_json_for_known_ones` |
| `routine_root::plugin_seeded_definitions_whose_plugin_is_off_are_hidden_but_still_shown_by_name` | `routine_root::routine_command_contracts_1` / `plugin_seeded_definitions_whose_plugin_is_off_are_hidden_but_still_shown_by_name` |
| `init_force_identity::force_refuses_missing_or_invalid_identity_flags_before_deleting_the_root` | `init_force_identity::init_identity_contracts` / `force_refuses_missing_or_invalid_identity_flags_before_deleting_the_root` |
| `init_force_identity::force_with_valid_identity_flags_resets_the_root_and_creates_a_new_identity` | `init_force_identity::init_identity_contracts` / `force_with_valid_identity_flags_resets_the_root_and_creates_a_new_identity` |
| `init_force_identity::ordinary_reinit_still_ignores_identity_flags_and_preserves_the_identity` | `init_force_identity::init_identity_contracts` / `ordinary_reinit_still_ignores_identity_flags_and_preserves_the_identity` |
| `init_force_identity::force_reset_may_choose_a_new_prefix_even_after_the_old_one_minted_task_ids` | `init_force_identity::init_identity_contracts` / `force_reset_may_choose_a_new_prefix_even_after_the_old_one_minted_task_ids` |
| `init_interactive_stdin::silent_open_stdin_pipe_exits_instead_of_hanging` | `init_interactive_stdin::init_stdin_contracts_0` / `silent_open_stdin_pipe_exits_instead_of_hanging` |
| `init_interactive_stdin::drip_writing_without_newline_expires_one_prompt_budget` | `init_interactive_stdin::init_stdin_contracts_0` / `drip_writing_without_newline_expires_one_prompt_budget` |
| `init_interactive_stdin::invalid_piped_machine_name_is_asked_again_before_the_root_is_seeded` | `init_interactive_stdin::init_stdin_contracts_0` / `invalid_piped_machine_name_is_asked_again_before_the_root_is_seeded` |
| `init_interactive_stdin::oversized_unterminated_pipe_answer_is_rejected` | `init_interactive_stdin::init_stdin_contracts_0` / `oversized_unterminated_pipe_answer_is_rejected` |
| `init_interactive_stdin::closed_stdin_keeps_the_existing_message` | `init_interactive_stdin::init_stdin_contracts_1` / `closed_stdin_keeps_the_existing_message` |
| `init_interactive_stdin::piped_machine_name_and_task_prefix_still_complete_interactive_init` | `init_interactive_stdin::init_stdin_contracts_1` / `piped_machine_name_and_task_prefix_still_complete_interactive_init` |
| `workspace_selector::workspace_remove_deregisters_deleted_checkout_by_name_id_and_path` | `workspace_selector::workspace_selector_contracts_0` / `workspace_remove_deregisters_deleted_checkout_by_name_id_and_path` |
| `workspace_selector::global_workspace_flag_selects_by_name_and_id_from_a_foreign_checkout` | `workspace_selector::workspace_selector_contracts_0` / `global_workspace_flag_selects_by_name_and_id_from_a_foreign_checkout` |
| `workspace_selector::global_workspace_flag_fails_closed_on_unknown_selector` | `workspace_selector::workspace_selector_contracts_0` / `global_workspace_flag_fails_closed_on_unknown_selector` |
| `workspace_selector::global_workspace_flag_on_deleted_checkout_reports_inactive_status_and_recorded_path` | `workspace_selector::workspace_selector_contracts_0` / `global_workspace_flag_on_deleted_checkout_reports_inactive_status_and_recorded_path` |
| `workspace_selector::migrate_dry_run_honors_selected_checkout_and_confirm_uses_the_same_one` | `workspace_selector::workspace_selector_contracts_1` / `migrate_dry_run_honors_selected_checkout_and_confirm_uses_the_same_one` |
| `workspace_selector::tool_run_workspace_selection_uses_global_flag_and_input_precedence` | `workspace_selector::workspace_selector_contracts_1` / `tool_run_workspace_selection_uses_global_flag_and_input_precedence` |
| `workspace_selector::checkout_path_and_cwd_resolve_an_id_that_collides_with_another_workspace_name` | `workspace_selector::workspace_selector_contracts_1` / `checkout_path_and_cwd_resolve_an_id_that_collides_with_another_workspace_name` |
| `workspace_selector::publication_lifecycle_survives_id_collision_when_selected_by_checkout_or_name` | `workspace_selector::workspace_selector_contracts_1` / `publication_lifecycle_survives_id_collision_when_selected_by_checkout_or_name` |
| `workspace_selector::task_show_follows_the_global_task_id_and_explicit_workspace_stays_a_filter` | `workspace_selector::workspace_selector_contracts_2` / `task_show_follows_the_global_task_id_and_explicit_workspace_stays_a_filter` |
| `workspace_selector::tool_run_task_show_follows_the_global_task_id_and_explicit_workspace_stays_a_filter` | `workspace_selector::workspace_selector_contracts_2` / `tool_run_task_show_follows_the_global_task_id_and_explicit_workspace_stays_a_filter` |
| `workspace_selector::workspace_role_cli_reasserts_owner_and_replica_and_preserves_registry_on_refusal` | `workspace_selector::workspace_selector_contracts_2` / `workspace_role_cli_reasserts_owner_and_replica_and_preserves_registry_on_refusal` |
| `workspace_source_remote::force_init_binds_first_origin_and_preserves_task_partition_and_publication` | `workspace_source_remote::source_remote_contracts_0` / `force_init_binds_first_origin_and_preserves_task_partition_and_publication` |
| `workspace_source_remote::force_init_recovers_identity_with_an_unchanged_non_portable_origin` | `workspace_source_remote::source_remote_contracts_0` / `force_init_recovers_identity_with_an_unchanged_non_portable_origin` |
| `workspace_source_remote::force_init_rejects_invalid_origins_before_first_binding_and_on_retry` | `workspace_source_remote::source_remote_contracts_0` / `force_init_rejects_invalid_origins_before_first_binding_and_on_retry` |
| `workspace_source_remote::force_init_cannot_bind_first_origin_from_a_replica_or_foreign_owner` | `workspace_source_remote::source_remote_contracts_0` / `force_init_cannot_bind_first_origin_from_a_replica_or_foreign_owner` |
| `workspace_source_remote::owner_can_dry_run_apply_read_back_and_retry_source_remote_rebind` | `workspace_source_remote::source_remote_contracts_1` / `owner_can_dry_run_apply_read_back_and_retry_source_remote_rebind` |
| `workspace_source_remote::credential_bearing_rebind_is_redacted_and_does_not_mutate` | `workspace_source_remote::source_remote_contracts_1` / `credential_bearing_rebind_is_redacted_and_does_not_mutate` |
| `workspace_source_remote::source_remote_help_names_inspection_dry_run_and_rebind` | `workspace_source_remote::source_remote_contracts_1` / `source_remote_help_names_inspection_dry_run_and_rebind` |
| `workspace_sync::workspace_sync_upgrades_previous_release_automation_with_provenance_intact` | `workspace_sync::workspace_sync_contracts_0` / `workspace_sync_upgrades_previous_release_automation_with_provenance_intact` |
| `workspace_sync::retired_delivery_qa_is_absent_after_init_and_a_modified_copy_is_kept` | `workspace_sync::workspace_sync_contracts_0` / `retired_delivery_qa_is_absent_after_init_and_a_modified_copy_is_kept` |
| `workspace_sync::workspace_sync_creates_missing_defaults_preserves_operator_content_and_is_idempotent` | `workspace_sync::workspace_sync_contracts_0` / `workspace_sync_creates_missing_defaults_preserves_operator_content_and_is_idempotent` |
| `workspace_sync::a_predating_workspace_definition_overrides_a_new_default_across_sync_and_update` | `workspace_sync::workspace_sync_contracts_0` / `a_predating_workspace_definition_overrides_a_new_default_across_sync_and_update` |
| `workspace_sync::workspace_sync_outside_registered_workspace_fails_before_writing` | `workspace_sync::workspace_sync_contracts_1` / `workspace_sync_outside_registered_workspace_fails_before_writing` |
| `workspace_sync::workspace_sync_reports_a_denied_manifest_write_without_claiming_convergence` | `workspace_sync::workspace_sync_contracts_1` / `workspace_sync_reports_a_denied_manifest_write_without_claiming_convergence` |
| `workspace_sync::workspace_init_migrates_legacy_gitignore_and_reinit_is_byte_idempotent` | `workspace_sync::workspace_sync_contracts_1` / `workspace_init_migrates_legacy_gitignore_and_reinit_is_byte_idempotent` |
| `workspace_sync::relocated_root_checkout_ignores_its_delivery_worktrees` | `workspace_sync::workspace_sync_contracts_1` / `relocated_root_checkout_ignores_its_delivery_worktrees` |
| `workspace_sync::workspace_init_refuses_checkout_and_name_collisions_without_writing` | `workspace_sync::workspace_sync_contracts_2` / `workspace_init_refuses_checkout_and_name_collisions_without_writing` |
| `ship_sweep_root::ship_sweep_selects_explicit_environment_and_default_registries` | `ship_sweep_root::ship_sweep_contracts` / `ship_sweep_selects_explicit_environment_and_default_registries` |
| `ship_sweep_root::ship_sweep_preserves_unchanged_registry_modification_time` | `ship_sweep_root::ship_sweep_contracts` / `ship_sweep_preserves_unchanged_registry_modification_time` |
| `ship_sweep_root::ship_sweep_dry_run_does_not_persist_invalid_status_flip_for_missing_checkout` | `ship_sweep_root::ship_sweep_contracts` / `ship_sweep_dry_run_does_not_persist_invalid_status_flip_for_missing_checkout` |
| `sweep_root::sweep_honors_explicit_root_over_uninitialized_home` | `sweep_root::sweep_command_contracts_0` / `sweep_honors_explicit_root_over_uninitialized_home` |
| `sweep_root::sweep_honors_orbit_root_over_uninitialized_home` | `sweep_root::sweep_command_contracts_0` / `sweep_honors_orbit_root_over_uninitialized_home` |
| `sweep_root::sweep_alias_and_clock_tick_have_identical_json_output` | `sweep_root::sweep_command_contracts_0` / `sweep_alias_and_clock_tick_have_identical_json_output` |
| `sweep_root::routine_clock_is_not_a_compatibility_alias` | `sweep_root::sweep_command_contracts_0` / `routine_clock_is_not_a_compatibility_alias` |
| `sweep_root::doctor_flags_an_installed_clock_without_deadline_or_descendant_cleanup` | `sweep_root::sweep_command_contracts_1` / `doctor_flags_an_installed_clock_without_deadline_or_descendant_cleanup` |
| `worktree_gc_routing::reclaim_dry_run_reports_pattern_bytes_doctor_warns_and_confirm_preserves_the_checkout` | `worktree_gc_routing::worktree_gc_contracts` / `reclaim_dry_run_reports_pattern_bytes_doctor_warns_and_confirm_preserves_the_checkout` |
| `worktree_gc_routing::routine_dispatch_ignores_ambient_orbit_root_and_reaps_only_the_owning_workspaces_worktree` | `worktree_gc_routing::worktree_gc_contracts` / `routine_dispatch_ignores_ambient_orbit_root_and_reaps_only_the_owning_workspaces_worktree` |
| `worktree_gc_routing::direct_gc_cli_previews_refuses_dirty_and_reclaims_only_the_selected_worktree` | `worktree_gc_routing::worktree_gc_contracts` / `direct_gc_cli_previews_refuses_dirty_and_reclaims_only_the_selected_worktree` |
| `doctor_runtime::task_relations_reports_a_canonical_dangling_edge_the_index_lost` | `doctor_runtime::doctor_runtime_contracts` / `task_relations_reports_a_canonical_dangling_edge_the_index_lost` |
| `doctor_runtime::retired_graph_cleanup_unlinks_boundaries_without_following_them` | `doctor_runtime::doctor_runtime_contracts` / `retired_graph_cleanup_unlinks_boundaries_without_following_them` |
| `doctor_runtime::cleanup_preserves_a_lock_held_by_a_live_process` | `doctor_runtime::doctor_runtime_contracts` / `cleanup_preserves_a_lock_held_by_a_live_process` |
| `store_gc::audit_retention_plans_then_prunes_rows_and_unreferenced_blobs` | `store_gc::store_retention_contracts` / `audit_retention_plans_then_prunes_rows_and_unreferenced_blobs` |
| `store_gc::run_retention_drops_terminal_state_and_keeps_run_history` | `store_gc::store_retention_contracts` / `run_retention_drops_terminal_state_and_keeps_run_history` |
| `store_gc::run_retention_prunes_resource_minutes_past_thirty_days` | `store_gc::store_retention_contracts` / `run_retention_prunes_resource_minutes_past_thirty_days` |
| `store_gc::retention_job_runs_when_the_default_crew_is_disabled` | Retired exact duplicate: `store_gc::store_retention_contracts` / `audit_retention_plans_then_prunes_rows_and_unreferenced_blobs` retains the identical real job/crew/refusal contracts with stricter null-crew and sweep/blob assertions. |
| `tmp_gc::previews_and_confirmation_report_bytes_and_preserve_the_directory` | `tmp_gc::tmp_gc_contracts` / `previews_and_confirmation_report_bytes_and_preserve_the_directory` |
| `tmp_gc::symlinks_are_unlinked_without_traversal_and_a_linked_tmp_root_refuses` | `tmp_gc::tmp_gc_contracts` / `symlinks_are_unlinked_without_traversal_and_a_linked_tmp_root_refuses` |
| `tmp_gc::symlink_gc_still_checks_symlinks_when_non_utf8_name_is_unsupported` | `tmp_gc::tmp_gc_contracts` / `symlink_gc_still_checks_symlinks_when_non_utf8_name_is_unsupported` |
| `tmp_gc::a_checkout_without_orbit_directory_has_nothing_to_reclaim` | `tmp_gc::tmp_gc_contracts` / `a_checkout_without_orbit_directory_has_nothing_to_reclaim` |
| `scratch_gc::prunes_entries_older_than_the_window_and_never_follows_symlinks` | `scratch_gc::scratch_gc_contracts` / `prunes_entries_older_than_the_window_and_never_follows_symlinks` |
| `scratch_gc::a_negative_window_is_refused_before_anything_is_removed` | `scratch_gc::scratch_gc_contracts` / `a_negative_window_is_refused_before_anything_is_removed` |


## Current-source coverage audit increment

The source audit at `9a58793f778fee8f9f587c8a0d8b4603f02196f4` found one exact duplicated fixture and five repeated immutable CLI observations. The larger handoff decision matrix relocates into an existing core test identity; it is not an already-covered deletion. No production source or test identity is added.

| Retired repeated work | Surviving assertions and boundary |
| --- | --- |
| `store_gc::retention_job_runs_when_the_default_crew_is_disabled` | `store_gc::store_retention_contracts` / `audit_retention_plans_then_prunes_rows_and_unreferenced_blobs` already uses the identical disabled-crew config and real CLI shipped retention job, requires successful JSON and strictly null resolved crew, then refuses the identical missing crew. The stronger carrier also checks both applied sweeps and preserved blob references. |
| Four repeated local single-field shows plus one repeated local readiness show in `host_routing::host_routing_contracts_0` / `task_show_projections_match_the_owning_host` | Reuse the first actual immutable local CLI results. All individual/multi-field shape assertions, three route spellings for every JSON field set, three human readiness views, real remote/MCP routing and authority/refusal rows remain. |
| Five full worker repetitions in `job_resume_detached::lifecycle::resume_readmits_lineage_failure_handoff_blocks` | Existing `orbit-core::runtime resume_ownership::resume_of_a_lineage_binding_keeps_the_checkpoint_batch_id` now contains all seven original provenance permutations: four event forms, outside-lineage and missing-run negatives, and promoted restoration. Public `submit_resume_run` reaches ownership reconciliation while seeding, before the worker executes. Each isolated fresh fixture asserts exact admission stage/history transition, unchanged history prefix/source checkpoints, seeded checkpoint and batch/retry identity. Both negative rows require unchanged whole task/history. The real CLI retains two handoff workers, unpromoted and promoted, including terminal Blocked/Review outcomes and restoration history. |

The unpromoted core rows assert Blocked-to-InProgress admission; terminal failure-to-Blocked remains through the retained CLI handoff carrier and failed/SIGKILL/superseding bundle-cleanup matrix. The promoted row asserts Blocked-to-Review; real CLI completion still includes failed forge, repeated retry and successful merged delivery. The core substitute worker only keeps an admitted run alive; it replaces no real process, signal, cleanup or completion assertion.

Keep the original full CLI security sweep template precedence, read-only/default run-bootstrap, error/output, update generation/handover, scratch/live-process, worktree capability/remediation and host registry/routing controls. The inspected lower-layer tests do not prove those same boundaries.

Focused validation is evidence for this increment, not the original half-runtime acceptance. The preceding exact matched Linux pair remains process222.286 ->188.401s summed and workspace123.976 ->105.365s; half limits111.143/61.988s remain unmet. The added core row execution must be included in eventual aggregate performance accounting; moving work between binaries is not itself a saving. No new full pair was run for this increment.

## Combined fixture and observation slate

This increment starts at `de4463d99e8eb8f88f99f548d54724b6ed3d4763`.
It removes incidental setup and observations within existing identities. It
adds no production behavior, test identity, timeout override, or shared mutable
state across rows. Its performance is unmeasured; focused checks do not establish
the original half-time acceptance.

| Removed repeated work | Retained behavior and assertions |
| --- | --- |
| The unused `local-home/repo` workspace in `Fleet::new` | The local machine still gets the same locked public identity/private config and the real CLI registers `local-repo/local-ws`. All actual remote workspaces, fake SSH commands, identity/protocol/old-host/collision/authority controls remain. |
| Incidental `Routed` task-add CLI setup and comment/list state probes | Fresh explicitly registered owner runtimes create the same low-complexity tasks and read full task/comment state. BR/LB ID assertions remain, and state baselines require both tasks. All routed CLI/MCP requests, five actual local projections, every route/projection cell, human output, no-dial, caller capability and sole-writer refusals remain. Existing child isolation clears inherited authority before in-process APIs run. |
| Identical second host-add refusals for `echo` and `beta --name ALPHA` | Their machine-ID/different-machine and `--name` message assertions move into the first identical refusal calls, which still assert error codes and unchanged host-file bytes. |
| Teardown `skills/name` and `skills/id` | `skills/path` retains the deletion fault and every byte/leftover/binding/retry assertion; `partition/name` and `partition/id` retain the actual selector routes. |
| Teardown `orbit/name` and `orbit/id` | `orbit/path` retains the deletion fault and every byte/leftover/binding/retry assertion; `partition/name` and `partition/id` retain the actual selector routes. |
| Crash retry `partition_deleted/name` and `partition_deleted/id` | `partition_deleted/path` retains this crash point. `local_data_deleted/name` and `local_data_deleted/id` retain both actual selector spellings at the stronger boundary where all local data is gone. |
| Repeated terminal CLI polling in the worker-command matrices | Public `show_job_run_observed` watches real persisted success/failed outcomes with unchanged real elapsed bounds and 100ms cadence; one final actual default CLI show supplies the same JSON and downstream assertions. Running/interrupted and SIGKILL reconciliation stay on the original CLI polling path. Real submissions/resume/--wait, detached PID ownership, child dispatch, gate release/locks and step output remain. |
| Unrelated stale runs and held reservations in the three display fixtures | Each fresh terminal fixture keeps FAILED plus its own interrupted/audit/recovery state. All 14 CLI/MCP operations and human/default/verbose/JSON/step/catalog/audit/provenance/recovery assertions remain. Full stale/held fixtures stay in readiness, no-reconcile, default reconciliation, authority/refusal and all other consumers. |

Selector factorization is backed by the production teardown resolving its
selector from the saved registry before the common deletion pipeline. Every
failure/crash row still verifies name, ID and path registration, and all three
actual CLI spellings remain after local data deletion. The separate symlink
discovery guard remains unchanged.

The last matched source pair remains process194.993 ->162.098s summed and
workspace107.014 ->96.431s. Their half limits97.4965/53.507s are unmet by
64.6015/42.924s; the added existing core ownership row costs0.358s. This increment
does not replace that measurement or claim those gaps closed. Real worker
binding grace, dashboard handover checks, caller expiry, quiescence and stalled
IO deadlines remain intact.


## Conditional cross-layer and projection increment

This increment starts at a9ecd974bdaf075d3772eba1549b84df95d016ec.
It retains the original count and half-runtime criteria. The matched pair at
that source remains process 202.689 ->165.108s and workspace 114.463 ->99.478s
summed, missing their half limits by 63.7635/42.2465 seconds. Core relocation adds 0.362 seconds.
These are contextual whole-suite measurements, not savings attributed to the
following changes.

| Retired repeated work | Surviving exact coverage |
| --- | --- |
| web_serve_handover::a_handed_over_dashboard_is_reaped_when_an_assertion_panics | server_readiness::inherited_pipes::drop_and_assertion_unwind_finalize_inherited_pipes_without_readiness now probes the original parent PID and requires ESRCH after the existing real assertion unwind, retaining watchdog, deadline, reader/descendant EOF and panic result. web_serve_handover::a_replaced_dashboard_execs_the_installed_executable_and_keeps_serving retains real executable adoption, in-place PID, address, health and SIGTERM and now asserts actual listening-port refusal after cleanup. Both own the same BoundServer/ChildGuard path; cleanup does not branch on executable generation. |
| Schema-v2 real CLI invocation inside update::local_candidate::local_candidate_contracts_0 / wrong_provenance_digest_target_or_ownership_refuses_before_replacement | Existing update::local_candidate_inputs::candidate_input_contracts / manifests_are_v1_operator_attested_documents_only calls public CandidateManifest::parse and rejects schema 2 with its exact diagnostic. The native consumer retains missing/malformed manifest refusals and unchanged installed bytes, staging and generation; production propagates parse failure before replacement without error-variant-specific effects. Every other provenance/digest/target/ownership/update control remains. |
| Graph-leaf success invocation inside worktree_resolution::checkout_routing_contracts_3 / doctor_graph_cleanup_never_deletes_through_symlinks | doctor_runtime::doctor_runtime_contracts / retired_graph_cleanup_unlinks_boundaries_without_following_them calls public remove_retired_graph_state, requires both real graph symlinks removed, count 2 and unchanged outside sentinel. The CLI carrier keeps actual symlinked-knowledge ancestor refusal, both outside sentinels and preserved ancestor link. worktree_resolution::checkout_routing_contracts_2 retains real split-root --remove-graph dispatch and clean JSON stdout. |
| Repeated projection field x route cells and human renderings in host_routing::host_routing_contracts_0 / task_show_projections_match_the_owning_host | Five actual owning-host CLI reference projections remain. Prefix route retains scalar title, comment array, readiness object, terminal object and complete four-field object; raw selector and --host/workspace each compare the full four-field payload against the actual owner. One real human readiness call parses and compares the full object. Total projection calls 23 -> 13 (five owner, eight remote), not name-only checks. Real MCP/v1, caller authority, mirror/no-dial/refusals and routed writes stay unchanged. |
| Incidental routed comment insertion used only to seed the immutable projection fixture | The already-open owning remote runtime uses public update_task_with_identity with the same comment and no agent/model override. Actual routed CLI write and MCP mutation carriers remain in their existing rows; projection payloads are still read through real local/remote CLI calls. |

No test identity, production seam, retry, ignore, timeout override or new
fixture engine is added. The single removed identity is the duplicated
handover panic fixture, after its required assertions transfer. Every other
retirement removes only the specified redundant invocation. CI's existing
web_serve_handover:: filter still selects the real surviving handover.


## Preserved-candidate CI repair

The repair starts from PR #4749 head `bddf17eb1a392cc0dd77723b862eae7d1a2ba026`,
restored exactly onto its `ab075609` base. Four one-row contract carriers now
log and call their existing assertion body directly: checkout routing, host
SSH hint quoting, workspace collision refusal and Linux clock diagnostics.
The last case is the analogous Linux-only single-element-loop diagnostic
reproduced during focused Clippy validation. Test identities, isolation, row
labels and assertions are unchanged. Host routing compares existing string
IDs directly rather than allocating copies.

The workspace binary now declares the shared portable `ChildGuard` on every
compile target, matching the process binary. Its Unix signal and process-group
implementation remains guarded inside the support module; other targets
compile the existing kill/wait fallback. No GC or sweep assertion is disabled,
no shell behavior is invented for native Windows, and Windows remains a
compile-only target with actual execution supported through WSL2.

The immutable produced-manifest digest comparison remains intact. This CI
repair adds no tests, ignores, retries, policy overrides or performance credit.
Final validation and exact-head hosted checks remain delivery responsibilities.

Focused repair validation passed: warnings-denied Clippy for both CLI binaries,
eight existing CLI identities covering all repaired carriers, routing refusal,
real GC/sweep ownership and immutable candidate preservation, plus the existing
core resume-ownership identity. Compiled lists remain 80 process entries
(74 runnable, the same six ignored helpers) and 71 workspace entries. Formatting,
diff whitespace, doc links, Windows portability inspection and the compiled
macOS workflow-filter check passed. Direct CLI QA initialized a workspace,
refused both checkout-renaming and durable-name collisions with registry and
identity bytes preserved, and parsed the update admission contract. These
focused checks do not replace pipeline final gates or native hosted checks.

The updated PR title and complete retirement body are stored in task artifacts
`ci-repair/pr-title.txt` and `ci-repair/pr-body.md`. The coordinator applied
those bytes to PR #4749 when publishing `94a41ae7`, after all four final gates
passed on its validated source tree. The original branch and PR remain
authoritative; replacement PR #4751 was closed unmerged.

### Follow-up macOS carrier repair

Native macOS Clippy on `94a41ae7` found that
`run_observation_contracts_6` becomes a single-element loop when its Linux-only
stopped-descendant row is omitted. The carrier now logs and calls the existing
surface-reservation assertion body directly, followed on Linux by the existing
stopped-descendant assertion body. Both row labels, order, assertion bodies and
the isolated-child guard remain unchanged. No test, platform coverage, retry
or deadline is removed or added.

The other conditional carriers retain at least two rows on Linux and macOS;
the four previously repaired unary carriers retain their direct calls.
This repair claims no additional performance improvement and repeats no timing
experiment or manual QA. Focused warnings-denied Clippy and the existing
real-CLI carrier check provide local evidence. The pipeline must rerun all four
final commands, then the coordinator must verify Linux CI, Windows compilation
and native macOS checks on the newly published head before landing PR #4749.

## Local-candidate setup follow-up, 2026-10-14

The two local-candidate tables retain all six row names, platform guards,
independent installations/HOMEs/workspaces, native candidate bytes and assertions.
Each candidate's required public manifest producer now performs its initial
launch instead of a separate setup-only `update --contract --json` launch.
The existing generation fixture accepts that probe; all other consumers still
use its original contract assessment. Mach-O UUID renewal and ad-hoc signing
remain unchanged. Git roots remain real repositories, but no longer seed an
unrelated README commit: these rows attest provenance explicitly with the
operator's commit, rather than deriving it from Git HEAD.

The matched owner-host pair used base `2296455fcd01c3143311438ef710337ba472965a`
on dk-server-2, native x86_64 Linux, 32 CPUs, Rust 1.98.1 and nextest 0.9.136.
The normal host budget was eight build slots, four Cargo jobs and four test
threads, with default lower CPU/IO priority. No setting or profile changed;
the current default profile has a five-minute slow period, so the requested
15-second limit is evaluated from actual durations, not inferred from PASS.
The actual CLI SHA-256 was identical in both arms:
`e93943ec003bc3b4986de9ecc32eb9ea0df301012be79f752f44429b731280e7`
(376,136,432 bytes). No stripping or production change was made.

Both focused commands selected/executed exactly two tests; the other 78
identities were filtered or already ignored. They ran under
`./scripts/build-budget.py -- cargo nextest run -p orbit-cli --test process
--no-fail-fast --retries 0 --success-output immediate -E
'test(=update::local_candidate::local_candidate_contracts_0) |
test(=update::local_candidate::local_candidate_contracts_1)'`.

| Measurement | Table 0 | Table 1 | Summed test time | Command wall |
| --- | --- | --- | --- | --- |
| Unchanged baseline | 12.329 s | 8.442 s | 20.771 s | 116.539 s, including 103 s initial compilation |
| Candidate | 11.856 s | 8.082 s | 19.938 s | 15.310 s, including test-target recompilation |
| Baseline with process tracing | 14.410 s | 10.104 s | 24.514 s | 14.996 s |
| Candidate with identical process tracing | 14.602 s | 10.114 s | 24.716 s | 15.162 s |

Tracing confirms contract launches across the cohort fell from 14 to eight,
while Git seed commits fell from six to zero. The uninstrumented pair is a
modest observed reduction; the instrumented pair is slightly slower. These
are single samples on a shared host, not a causal speedup estimate or a
best-of selection. Load averages were 11.72/8.95/11.60 to 11.19/9.89/11.65 for
the baseline and 10.37/10.42/11.48 to 9.69/10.26/11.41 for the candidate.
No load was synthesized or cache conditions artificially controlled.

| Surviving row | Observable guarantee |
| --- | --- |
| Table 0 / older-installation bootstrap and idempotent replay | Equal-version native replacement, attested commit and digests, unsigned-local provenance, both admitted roots, completed convergence and preserved older backup on replay |
| Table 0 / wrong provenance, digest, target or ownership | Full-commit validation; mismatched, missing and malformed manifests; changed bytes and foreign architecture; unmanaged/symlink targets; required/conflicting flags; producer preservation; host-global live-client refusal even with a workspace root override; unchanged bytes, generation, backup and staging |
| Table 0 / discovered-workspace live client | A live workspace pin blocks replacement without changing installed bytes, backup or the workspace generation record |
| Table 1 / unsupported admission contract, Linux | A genuine native executable lacking the contract is refused before replacement, with installation/generation/backup/staging preservation |
| Table 1 / validation FIFO and accepted inode | Real client access during validation, exclusion of a concurrent update, unchanged installation before acceptance, accepted bytes installed despite pathname replacement, successful convergence and client access afterward |
| Table 1 / quoted post-swap recovery | Actual failed migration reports `needs_recovery`; the reported quoted shell retry converges without another swap or changing accepted bytes |

The retired setup-only contract assertion survives at the real native staged
candidate admission boundary in the successful bootstrap/FIFO rows, with its
negative control in the unsupported-contract row. No behavioral row or test
identity was retired. Real staged/installed probes, FIFO synchronization,
deadlines, existing executable-busy handling, cleanup and rollback coverage
remain. No retry, sleep, visibility change to production code, mock or
speculative test was added.

The full process target then selected/executed 74 tests, all passed, with only
the six existing ignored child helpers. Its local tables took 11.991/8.283 s;
total summed time was 126.286 s, nextest wall 32.922 s and command wall
33.283 s. Load moved 12.07/10.68/11.49 to 12.35/10.92/11.54. The independently
landed flow callback repair is present in the base and its two unchanged
tables passed in 0.226/0.146 s. This full target is not the full affected-test
gate and establishes no full-CI 15-minute claim. The pipeline owns final
`make ci-fast`, `make ci-lint`, `make goldens` and `make ci-test-affected`.

Task artifacts under `performance/local-candidate/` retain every timed attempt,
raw logs, process traces, exact baseline/candidate test sources and patch,
host/settings/configuration hashes and the surviving guarantee map. Parent
ORB-15949's original macOS failure remains 60.017 s timeout for table 0 and
49.836 s for table 1; its failure-only baseline replay ran 20 tests and table 0
took 20.812 s. Those are not matched full-suite or cross-host speedup evidence.
The parent baseline local-candidate source and settings match this checkout,
but its failed candidate Git object and host budget/load details are unavailable
locally. Native macOS candidate timing is not measured in this Linux session;
no macOS runtime result is claimed, and the earlier rejected stripping
experiment remains rejected.

The shared helper's other integration target, MCP, also returned exit zero:
61 selected/executed runner entries, 73.293 s summed, 22.850 s nextest wall and
26.440 s command wall. All five upgrade/handover entries completed, including
the actual replaced-server and drain handovers. Two unrelated readonly
Bubblewrap scenarios printed `DEFERRED: bubblewrap unavailable` because this
lane cannot create a namespace; their sandbox coverage is **not run**, despite
the runner's PASS results. Their exact notices remain in the raw log, and the
pipeline's final affected-test gate owns its coverage enforcement.

Focused process-target Clippy passed in 39.126 s with the repository's
test-target flags, `-D warnings -A clippy::disallowed_methods`. An initial
diagnostic omitted the test-fixture exception and failed with 231 disallowed
filesystem/process calls; the identical command on unmodified baseline source
failed with the same 231 errors. Both logs are retained rather than reported
as passing checks. Focused rustfmt and the documentation-index check passed;
the generated index row is unchanged because the first heading and summary
were not edited. Final repository gates remain pipeline-owned and not run in
this session. The real native CLI scenarios supply the meaningful positive,
refusal, accepted-inode and recovery QA; no additional manual QA is claimed.

## Later changes

Checked against HEAD on 2026-10-19. The candidate landed as PR #4749 (merge 9e5114917). Three names in the retirement map no longer match the landed suite:

- `run_show_hides_shipped_catalog_layers_unless_verbose` and `run_show_lists_audit_steps_for_an_interrupted_run_with_only_a_run_level_record` are listed as surviving in `run_show_contracts`, but neither is defined at HEAD. Commit 5951deb5a dropped them when the candidate landed.
- Both `preflight_with_root_admits_uninitialized_override_cwd_and_host_global` and `preflight_with_orbit_root_admits_uninitialized_override_cwd_and_host_global` still existed as separate tests at 5951deb5a. Commit 7182fadd3 (ORB-16847) deleted the `ORBIT_ROOT` test; the surviving `preflight_with_root_admits_uninitialized_override_cwd_and_host_global` is the separate `--root` admission test, not a rename.
