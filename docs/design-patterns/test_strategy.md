---
type: pattern
summary: "Boundary-First Testing: integration, golden and e2e by default; unit tests by exception"
last_validated: 2026-11-10
---
# Boundary-First Testing

Test behaviour where callers meet it. A new test is written when a bug or defect is fixed: one regression test that fails before the fix, preferably as a row in an existing table-driven test. Features, refactors and chores add no new tests unless an acceptance criterion asks for one; the existing suite gates them, and a change updates the tests it breaks. When a regression test is written, it goes at a stable boundary by default: a crate-root integration test, a golden, or an e2e flow. A unit test is the exception, and it must earn its place by meeting one of the admission criteria below.

Gate time is a finite budget. Optimize the suite for the critical failures it can detect and the time it costs, rather than its test count.

## Quality and the 400-second budget

The complete **Check / Clippy / Test** CI job must finish in **at most 400 seconds of elapsed wall time at p90 over PR runs**. Measure from the job's start to completion, including setup, compilation, checks, linting, tests and required capability checks. Report queue wait separately: the wait before the Check job starts, and the part of its elapsed time the awaited jobs spent queued after it started. The workspace tests run in separate jobs (a test build and parallel nextest shards, [DEVELOPMENT.md](../DEVELOPMENT.md#hosted-ci)) that the Check job waits for, so its elapsed time covers them, including their runners' queue wait. Its `ci-await:` lines give each awaited job's start and completion. A pull request run tests only the affected crates and the always-run security and recovery crates ([PR test selection](../DEVELOPMENT.md#pr-test-selection)); the `agent-main` push run after the merge tests every crate. A local filtered run, a smaller test count or a sum of parallel testcase durations does not establish that the hosted job meets this budget. This is an acceptance requirement; documenting it does not claim the current suite already meets it.

Every test must protect a named observable guarantee. Prioritize severity and likelihood of failure, the uniqueness of the test's coverage, and its diagnostic value relative to runtime. Prove behavior through the public boundary callers use, retain meaningful negative cases, and use deterministic fault injection where a failure or race cannot be reached reliably otherwise. The current agent guide governs admission of new tests; the historical unit-test admission records below do not authorize new tests of private implementation functions.

Critical coverage takes priority:

| Critical behavior | Guarantees to preserve |
|---|---|
| Authority and security | Correct host/workspace identity and operator grants; denial before unauthorized effects; sandbox and path/symlink confinement; child-environment isolation; secret redaction; HTTP origin and host checks. |
| Durable state and task lifecycle | Valid transitions, atomic persistence, compatibility and migration safety, artifact-bound validation/review/completion, and refusal of missing or empty evidence. |
| Distributed execution and recovery | Exclusive claims, idempotent settlement, lost-reply reconciliation, restart/orphan recovery, graceful and forced cancellation, and child-process cleanup. |
| Delivery and PR repair | Required checks and exact candidate heads, bounded auto-fix attempts, merge-conflict recovery, lease-protected publication, and separate repair and merge authority. |
| Upgrades and cleanup | Existing-client compatibility or safe refusal, recoverable replacement, ownership-proven GC, retention of active/unknown output, and preservation of failed candidates. |
| Resource admission | Capacity ceilings, enforcement decisions, unavailable or stale measurements, and safe behavior when profiles or forecasts are uncertain. |
| Public contracts | Real CLI, MCP, API and UI behavior where relevant: inputs, scopes, identifiers, persisted effects, errors, machine output and critical user workflows. |

For each consolidation or retirement, record the invariant and important failure cases, the existing or replacement test that still detects them, and measured cost before and after. Keep the only guard of a security invariant, recovery path or known incident until equivalent behavioral coverage is demonstrated in the same change (STD-04 §R25). Strengthen assertions or add explicitly required boundary coverage when an audit demonstrates a critical gap; do not add speculative private-function tests or duplicate an existing invariant.

The claimed-review bridge's owner-store lock recovery lost its wait-then-succeed check when `04740a600` removed `next_owner_open_waits_out_a_lock_held_past_the_busy_budget`. The retained `ssh_stand_in_releases_the_owner_store_when_the_session_is_killed` checks that the killed runtime releases its lock; it does not check that a next owner open waits while the lock remains held. `claimed_review_bridge_sandbox::next_owner_open_waits_out_a_lock_held_past_the_busy_budget` now covers that second guarantee on Linux: after killing the stand-in, it verifies the next runtime has not started beyond Orbit's configured SQLite busy timeout, then releases the holder and verifies the next open acquires the store. The removed state had no wait-then-succeed behavioral check (zero test runtime); the restored focused debug run took 5.22 s, including its 5.1 s held-lock observation. The Python SQLite fixture derives its timeout from Orbit's production default, so this exercises session gating and recovery around the owner-store lock without claiming to exercise Orbit's own SQLite-open path.

Table-driven cases may amortize setup, but each row must remain isolated where needed and print its identity before running. Sharing fixtures must preserve authority scrubbing, state isolation, process ownership and cleanup. A smaller number of carriers with the same expensive work is not a runtime improvement, and a carrier that hangs or hides the failing row is a quality regression.

Conditional runtime scenario rows can return `scenarios::Outcome`: `Passed` records `qa-scenario-pass`, while `Unavailable(reason)` emits a recognized `SKIP:` notice without execution credit. Ordinary unit-returning rows retain their existing completion reporting. The provider PID-reuse cancellation table uses explicit outcomes for its native identity prerequisite; a subprocess control checks unavailable and executed reporting through both whole-table forwarding and single-row selection, with libtest capture enabled. Its deterministic unavailable control is consumed inside the observer, so it does not advertise missing native coverage to the surrounding test run.

Fixture templates follow these rules:

- **Profile first.** Each runtime scenario row prints `scenario: <row> row elapsed <seconds>`. The parent prints `child elapsed` for each child it runs, which includes process startup. A separate-process family has one child per row, and any other table runs as one `<module>::*` child.
- **What a template may replace.** Only setup whose result is the same for every row. One example is a migrated `orbit.db`; the schema migrations dominate a first runtime open. Another is the Git commands that build a checkout and its bare remote. A template may be built once per test process and copied into each row.
- **A quiescent source.** A template is captured only after every process that writes its tree has exited. Git detaches a `git maintenance run --auto` after commit, rebase and a push's receive-pack, so a Git template sets `maintenance.auto=false` in each repository it captures. Capture refuses a `*.lock` file, which names a writer still running.
- **A private copy for each row.** The template is held in memory as a tree (`tests/runtime/fixture_template.rs`) and written as a byte copy into the row's own root. It is never a shared path, handle, hard link or symlink, so nothing is left on disk to clean up.
- **No per-row identity.** A template carries no workspace registration, checkout path or other identity that differs per row. Each absolute path it records is rewritten to point at the row's copy. The runtime still opens and bootstraps each root itself.
- **A public isolation control.** Each template has a control table. One row writes into its copy. A later row in the same child process then proves its own copy is unchanged.
- **Child processes stay.** The 2026-10-17 profile measured child startup at about 1% of the delivery and distributed-drain row time. Families that install process-wide substitutions therefore keep one child per row.

The distributed no-diff read fixtures prepare unrelated media through a public
owner update before claim admission. This avoids encoding and persisting a
large worker replay receipt solely for read-fixture setup. Each case retains
its own task, store, claim, Git state and complete media bytes. Both report
verification and tagged-skip routes still prove the old bulk read exceeds the
unchanged MCP ceiling, verification reads only named evidence, and settlement
preserves every unrelated artifact. Worker evidence submission and its replay
authority remain covered by the claim-mutation and claimed-review fixtures.

The signed-release update fixtures share throwaway signing material while keeping each installation, trust file, manifest and signature isolated. Release verification does not require fresh random RSA primes for every row. Their hanging-probe case synchronizes with the real child before the supervisor's initial clock sample and checks that timeout reaps it without acquiring admission or replacing the installation. A fixture clock must keep advancing after readiness: a single fixed jump can become the initial sample when the child starts early, leaving the timeout unable to expire.

The reserved base-refresh completion fixture and accepted-handoff expiry fixture give native provider/Git subprocesses a finite 30-second allowance and advance their scoped prerequisite clocks beyond the persisted reservation deadline for expired resume. Poll waits use the existing no-wait hook. The completion fixture's eight isolated outcome rows preserve the same reservation and exactly one provider update across initial execution, replay and expiry; the handoff fixture proves the initial update and reservation before externally completing the merge and settling after expiry. Diagnostics retain the action error, bounded provider-call tail and reservation state before reading mutation markers. One two-second provider read in each fixture's unapplied or pending row is a controlled regression case for the former one-second subprocess allowance, not a retry or a host-load simulation.

Some suite waits are deadlines inside a debug `orbit` or dashboard child, where a thread-local clock does not apply. Those children read a debug-only environment variable and ignore it in a release build: `ORBIT_TEST_HOST_FORWARD_ATTACH_TIMEOUT_MS` shortens only the host-forward attach budget, `ORBIT_TEST_MCP_LIFECYCLE_INTERVAL_MS` shortens only the stdio lifecycle tick, and `ORBIT_TEST_PIPELINE_WAIT_VIRTUAL=1` advances a pipeline wait by each poll instead of sleeping. Each one still runs the real child and the real reap. The quiet-deadline fixture injects its bound through `run_child_test_within` and keeps the child's progress loop long enough that silence would still kill it.

Use timings and failure evidence to find the actual bottleneck: runner setup, compilation, linting, test execution, fixture startup, serial groups or teardown. Compare the same source selection, runner/build settings and cache conditions; retain failed and slow runs as evidence. Measure the full hosted job after changes as well as focused before/after costs. Fix flakes and hangs at their cause. Retries, artificial sleeps, hidden skips, weaker assertions, zero-test selections and omitted required checks cannot satisfy the budget.

The nextest phase (in hosted CI, each shard job) runs on a 4-vCPU runner with every slot busy. Comparing per-test durations on fast and slow runners of that size showed that about a fifth of slot time does not scale with CPU speed: those tests wait on timers, deadlines, or child processes, so a core can sit idle while the slot stays taken. `scripts/cpu-utilisation.sh` reads the aggregate `cpu` line of `/proc/stat` immediately before and after that phase and prints one `cpu-util:` line to the job log. Busy time is user + nice + system + irq + softirq + steal. Idle and iowait are their own percentages of the counter delta, followed by the elapsed seconds and the `cpuN` count. A missing file, a counter that does not move, or a counter that steps backwards prints `unavailable` and leaves the test command's exit status unchanged.

`.config/nextest.toml` profile `ci` sets `test-threads` to 6 and `retries` to 0. Six is two extra slots on the 4-vCPU runner: enough to overlap that wait-bound fraction, and short of doubling the CPU-bound tests. The profile inherits the default profile, so the group caps stay `stateful-runtime` 1, `task-contracts` 4, and `mcp-contracts` 8. `scripts/ci-guardrails.sh`, the hosted nextest shards and the Linux sandbox gate select it with `--profile ci`. A local run that does not pass `--profile ci` keeps the default profile, whose thread count is the logical CPU count. `NEXTEST_TEST_THREADS`, including the value `scripts/build-budget.py` exports from the host `test-threads` file, still overrides the profile.

The CI profile also gives priority 100 to the measured long tests, initially
six: `task_delivery::source_repair::published_completion_scenarios`,
`pr_landing::validation_repairs_and_logs`,
`pr_landing::ci_rerun::owner_retry_preserves_read_only_and_independent_candidate_routes`,
`public_tool_surface::github_log_goldens::every_selected_window_hides_credential_fragments`,
`events::change_stream_notices_other_process_writes_and_resumes_after_reconnect`
and `origin_fetch_hold::worktree_setup_origin_outage_holds_backlog_and_fails_in_progress`.
The seventh pass below expanded the list to every test measured at 12 s or
longer in its local profile.
The 2026-10-29 hosted audit observed 111.484, 49.947, 58.482 and 38.865 seconds
for the first four respectively (different runs; these are not additive).
The exhaustive credential
window test completed last in shard 3 of run 40941281957; the completion carrier
finished near the end of shard 2 of run 40945863498. A complete local baseline
shard on the delivery base finished with the change-stream test (10.913 seconds)
and the dashboard codebase fixture (30.133 seconds). The former is prioritized;
the latter now completes its unanswered mock fetches during teardown, letting
the production API's `finally` clear its real request timers. This keeps every
view assertion and the production timeout, and asserts no fetch remains
unanswered. The same baseline measured 51.195 seconds for the origin-outage
recovery test; priority keeps its deliberate transport deadline checks intact.
Starting the long tests before the ordinary queue overlaps their remaining rows with other work instead
of leaving idle slots at the tail. The [nextest priority setting](https://nexte.st/docs/configuration/test-priorities/)
requires version 0.9.91 or newer; hosted CI already pins 0.9.136. Priority
changes order only, preserving count-partition membership, ignored applicability,
zero retries, process isolation and all inherited group limits. No case or
assertion is removed. A complete local shard comparison measures scheduling;
only complete candidate hosted jobs and the subsequent PR distribution can
establish whether the p90 requirement then in force (600 s; 400 s since the ninth pass) is restored.

The controlled 2026-10-29 comparison used independent extracts and targets at
base `08bd9305eac9`, Rust 1.98.1, nextest 0.9.136, four Cargo jobs and six
nextest slots on one shared 32-CPU Linux host. Compiler caches and incremental
compilation were off; dependency sources were already downloaded. Cold means
a fresh target, warm means the same arm's compiled target; OS caches were not
flushed. Each complete portable shard ran alone, sequentially:

| Target stratum / arm | Shard 1 s | Shard 2 s | Shard 3 s |
|---|---:|---:|---:|
| Cold / baseline | 123.275 | 136.145 | 139.097 |
| Cold / candidate | 115.502 | 107.769 | 133.496 |
| Warm / baseline | 117.989 | 130.298 | 142.389 |
| Warm / candidate | 116.464 | 108.002 | 134.845 |

Every arm selected the same 3,190 portable identities and 61 ignored identities,
partitioned 1,082/1,062/1,046 exactly once. Each retained the same failed
`review_reset_claimed::claimed_leaf_review_escalation_requeues_onto_a_new_claim`
case (`before_pr_unsupported`), so these timings are not full-suite passes.
The longest individual shard improved by 5.601 seconds cold and 7.544 seconds
warm. The continuous **sequential** warm build/archive/list/shard replay fell
from 414.711 to 382.314 seconds; that is not a parallel hosted-job saving.
Cold build/archive times were 199.409/184.836 seconds, with unchanged Rust and
build flags: no compile saving is attributed to this change. The 64-second
critical-path goal is not demonstrated, and candidate hosted jobs and the
latest-20 PR distribution remain outstanding through normal publication.

`scripts/ci-guardrails.sh` arranges the full run around that phase. After formatting, the exclusive self-tests start one at a time at normal priority in their own lane. The repository-guard pool and the workflow test build run beside them in Linux's idle scheduling class, and both the pool and the lane join before any test runs. `test-ci-download-verified.py`, which waits out fixed delays for nearly all of its wall time, starts beside the pool outside its window and joins with it. nextest then runs the binaries listed from the folded test build (`--binaries-metadata`) without invoking Cargo. Production Clippy, doctests and documentation run beside it in Linux's idle scheduling class, on CPU time the tests leave idle, and join before the supply-chain gate. Documentation runs under production Clippy's lint environment, so it reuses that pass's check of the workspace and only runs rustdoc. The Check job's dev and test profiles carry no debuginfo (`CARGO_PROFILE_DEV_DEBUG` and `CARGO_PROFILE_TEST_DEBUG` are `0` in `ci.yml`); coverage keeps line tables in its own workflow. The `cpu-util:` line samples the whole host, so it includes that idle-class work. Every command that compiles workspace members after the folded build runs through `scripts/with-test-build-lint.sh`. Cargo fingerprints `RUSTC_WORKSPACE_WRAPPER` and `CLIPPY_ARGS`, so a nextest run without them recompiled every workspace member.

Timing evidence from 2026-10-17, in wall seconds from the `ci-guardrails-phase:` lines. The hosted column is a slow 4-vCPU runner (run 39042835730; Check job 1261 s, of which guardrails took 1214 s). The local columns ran the guardrails before and after this layout on one host pinned to four cores (`taskset -c 0-3`, `CARGO_INCREMENTAL=0`, after a source change to `orbit-common`), so they compare layouts rather than predict hosted time. The after column gives both samples.

| Phase | Hosted, before | Local, before | Local, after |
|---|---:|---:|---:|
| Workflow test build (folded all-targets Clippy) | 206 | 86 | 98 |
| Production Clippy | 51 | 24 | 24 |
| nextest, including any build it starts | 764 | 310 | 232–233 |
| Doctests and documentation in the foreground | 79 | 36 | 0 (206–209 overlapped at idle priority) |
| Guard pool after the tests | 20 | 11 | 0 (122 overlapping the builds) |
| Exclusive self-tests | 82 | 46 | 45–85 |
| Guardrails total | 1214 | 516 | 405–445 |

The before layout's nextest rebuilt the workspace, about 220 s hosted and 91 s locally. Overlapping the pool slowed the workflow test build by 12 s locally, and the idle-priority chain slowed nextest by 13–14 s. The 85 s exclusive sample came from one suite that took 45 s against 7–10 s standalone. Formatting (4–8 s) and the supply-chain gate (0–4 s) are omitted from the table.

A second pass the same day targeted slower hosted runners, where the Check job took 1102 s (run 39058921368: setup 41, guardrails before nextest 319, nextest 602, after nextest 140). A fast-runner run of the same layout took 910 s (run 39062382363). It made four changes:

- The job's dev and test profiles dropped line tables. In a separate target with `CARGO_INCREMENTAL=0` and no compiler cache, the workspace test rebuild fell from about 415 to 349 CPU seconds, nextest from 915 to 727 CPU seconds (306 to 259 s wall on the whole host), and the `orbit-core` runtime test binary from 308 to 173 MB. All 2406 tests passed in both.
- Production Clippy moved from the foreground, after the workflow test build, to the head of the idle-priority chain.
- `test-ci-download-verified.py` moved from the exclusive suites to beside the pool. It uses about 6 s of CPU time across about 63 s of wall time.
- `lock_busy_pass` shortens the task-commit lock deadline it waits out twice from 30 s to 2 s with a scoped `test-support` hook. It took 61.5 s and now takes 5.4 s. The deferral, holder-naming, reservation and deferral-limit assertions are unchanged.

No test was retired, skipped, retried or weakened. The local columns ran the full guardrails on one host pinned to four cores (`taskset -c 0-3`, `CARGO_INCREMENTAL=0`, no compiler cache, offline advisory database), each after the same source change to `orbit-store` and `orbit-core`, with the before and after `ci.yml` profile settings. The hosted column is run 39062382363.

| Phase | Hosted, before | Local, before | Local, after |
|---|---:|---:|---:|
| Workflow test build (folded all-targets Clippy) | 190 | 90 | 71 |
| Production Clippy in the foreground | 40 | 43 | 0 (124 overlapped at idle priority) |
| Wait for the guard pool after the build | 0 | 0 | 6 |
| nextest | 481 | 235 | 193 |
| Idle-priority chain after nextest | 0 | 0 | 5 |
| Exclusive self-tests | 122 | 184 | 40 |
| Guardrails total | 844 | 561 | 320 |

The before exclusive suites included `test-ci-download-verified.py` (63 s hosted, 89 s locally). `test-ci-fast-guards.py` took 54 s in the local before sample and 16 s after, against 21 s hosted, so host noise accounts for part of that row. The chain after nextest is production Clippy, doctests and documentation still running when the tests end; it finishes at full speed once nextest releases the CPU. The hosted after column is owed by the pull request and the following `agent-main` runs.

A third pass on 2026-10-18 followed agent-main runs of 638–957 s (run 39104730463: guardrails 908 s, of which formatting 9, workflow test build 236 with the pool overlapped, nextest 581 and exclusive self-tests 76). Locally, the base layout kept the four pinned cores busy through the build and nextest, then left three of them idle for the exclusive self-tests, which are mostly single-process suites that ran alone after everything else. It made two changes:

- The exclusive self-tests run in their own lane, one at a time and at normal priority, from the end of formatting. The guard pool and the workflow test build overlap them in the idle class (nice 19 off Linux), and the lane joins before nextest. The suites keep the host's normal-priority CPU to themselves, as nextest does beside the idle-class check chain.
- Production Clippy runs as `cargo check` under the environment `cargo clippy` sets, and documentation runs under the same environment. In a separate measurement, `cargo doc` after that check only documented the 17 workspace crates (21 s) instead of first re-checking them with plain rustc.

No test was retired, skipped, retried or weakened, and no time bound changed. The local columns ran the full guardrails on one host pinned to four cores (`taskset -c 0-3`, `CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, no compiler cache, offline advisory database), each after the same source change to `orbit-store` and `orbit-core`. The base and candidate runs alternated within one hour. Other jobs shared the host and its pinned cores; two earlier base samples taken under heavier load (555 and 617 s) are left out as not comparable.

| Phase | Hosted, before | Local, before (3 runs) | Local, after (2 runs) |
|---|---:|---:|---:|
| Formatting | 9 | 4 | 4 |
| Workflow test build, with the pool (and after, the exclusive lane) overlapped | 236 | 80–84 | 89–104 |
| nextest | 581 | 226–229 | 218–237 |
| Idle-priority chain after nextest | 1 | 22–31 | 2–15 |
| Exclusive self-tests after nextest | 76 | 45–50 | 0 |
| Guardrails total | 908 | 386, 386, 390 | 313, 361 |

The mean total fell from 387 to 337 s, about 13%. Three more candidate runs that also moved formatting into the exclusive lane took 344, 347 and 358 s, and that variant was dropped. Running the exclusive lane beside the build lengthens the build by about 20 s, because the lane takes one core at normal priority; that is less than the 45–50 s the suites took after the tests. The candidate keeps 3.8–3.9 of the four pinned cores busy from start to end, so further cuts need less CPU work in the build or in nextest rather than more overlap. Two variants measured in the same window did not help. Compiling workspace members at `opt-level = 1` shortened nextest from 272 to 212 s, but the pinned test build grew from 122 to 418 s. Giving the build precedence over the pool (nice 19 above the idle class) left the build at 84 s and stretched the pool to 116 s. The hosted after column is owed by the pull request and the following `agent-main` runs.

A fourth pass on 2026-10-18 split the test gate across runners. The base Check job took 770 s on a hosted runner (run 39119665438): setup about 48 s, then guardrails 714 s, of which formatting 8, workflow test build 221 and nextest 480 at 95.5% CPU busy. Production Clippy, doctests and documentation overlapped nextest. The tests were CPU-bound on one runner, so the change runs them on several:

- `Test build` compiles once and archives the test binaries. `Test shard 1/3` to `3/3` each run one nextest count partition from that archive. The Check job runs the checks without the test build and waits for those jobs ([DEVELOPMENT.md](../DEVELOPMENT.md#hosted-ci)). A partition guard in the build job fails unless the three shards together select every test the full list selects, each exactly once. No test was retired, skipped, ignored, retried or weakened, and no time bound changed.
- nextest's count partitioning assigns tests round-robin within each binary, so the shards stay balanced as tests are added: 844, 828 and 817 tests, 433, 484 and 467 summed seconds in the sample below.

The local samples ran on one host pinned to four cores (`taskset -c 0-3`, `CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, nextest profile `ci`). Other jobs shared the host. The full suite took 216 s of nextest wall time in one process: 2489 tests passed in 1259.6 summed seconds. From the archive, each shard ran alone on the same four cores in 72, 86 and 78 s, and all 2489 tests passed. The archive took 1–2 s after the folded build and the partition guard 12 s. On the same four cores, `ci-guardrails.sh --checks` took 273 s from a cold target. Formatting took 5 s. The guard pool and exclusive lane took 104 s, beside the idle-priority chain of production Clippy (155 s), doctests (103 s) and documentation (10 s). That sandbox could not lock the advisory database, so the supply-chain gate did not run there. Without the folded test build, the doctests compile the workspace libraries themselves. So the Check job's own chain, rather than the build and its shards, may set its elapsed time on some runners. If it does, the doctests are the next phase to move. The hosted after column is owed by the pull request and the following `agent-main` runs. Each shard uploads its JUnit report as `nextest-junit-k`, so the hosted per-test durations can be read from the run.

The 30 slowest tests of the full run, with their durations in the sharded run, in seconds from nextest's JUnit report:

| # | Binary | Test | Full run | Sharded | Shard |
|---:|---|---|---:|---:|---:|
| 1 | `orbit-core::runtime` | `task_delivery::source_repair::published_completion_scenarios` | 15.30 | 19.12 | 2 |
| 2 | `orbit-engine::engine` | `pr_landing::validation_repairs_and_logs` | 12.45 | 13.88 | 3 |
| 3 | `orbit-core::runtime` | `task_delivery::source_repair::publication::same_pr_publication_scenarios` | 11.23 | 12.18 | 1 |
| 4 | `orbit-cli::process` | `job_resume_detached::lifecycle::resume_lifecycle_contracts_0` | 11.19 | 12.23 | 1 |
| 5 | `orbit-core::runtime` | `distributed_drain::no_diff::verified_no_diff_and_already_satisfied_base_complete_without_a_pr_scenarios` | 10.22 | 11.70 | 3 |
| 6 | `orbit-engine::engine` | `pr_landing::absent_context::reserved_refresh_observes_lost_unapplied_and_pending_outcomes_without_another_write` | 9.33 | 8.46 | 1 |
| 7 | `orbit-core::runtime` | `task_delivery::source_repair::cancelled_source_repair_scenarios` | 9.01 | 9.55 | 3 |
| 8 | `orbit-engine::engine` | `pr_landing::infrastructure::bounds::prerequisite_bounds_entire_repeated_failed_job` | 8.21 | 7.26 | 1 |
| 9 | `orbit-cli::process` | `job_resume_detached::unix::worker_command_contracts_2` | 8.03 | 8.62 | 2 |
| 10 | `orbit-core::runtime` | `distributed_drain::landing_repair::retry::public_owner_handoff_retry_scenarios` | 7.97 | 11.16 | 2 |
| 11 | `orbit-cli::process` | `update::local_candidate::local_candidate_contracts_0` | 7.65 | 9.63 | 1 |
| 12 | `orbit-engine::engine` | `pr_landing::infrastructure::missing_required_prerequisite_requires_a_literal_api_read` | 7.60 | 8.05 | 3 |
| 13 | `orbit-core::runtime` | `distributed_drain::no_diff::no_diff_retains_owner_completion_authority_scenarios` | 6.46 | 7.35 | 1 |
| 14 | `orbit-web::http_api` | `workflows::replay_returns_a_durable_receipt_while_the_job_runs_in_a_detached_worker` | 6.36 | 6.26 | 2 |
| 15 | `orbit-engine::engine` | `pr_landing::infrastructure::missing_required_prerequisite_recovers_once_without_source_repair` | 5.96 | 6.01 | 1 |
| 16 | `orbit-engine::engine` | `pr_landing::absent_context::unsupported_or_unproven_emission_never_updates_or_repairs` | 5.80 | 7.03 | 3 |
| 17 | `orbit-core::runtime` | `task_delivery::source_repair::published_owner::successful_source_rework_scenarios` | 5.76 | 6.72 | 3 |
| 18 | `orbit-cli::tool` | `claimed_review_bridge_sandbox::ssh_stand_in_releases_the_owner_store_when_the_session_is_killed` | 5.63 | 5.94 | 1 |
| 19 | `orbit-cli::process` | `web_serve_handover::a_replaced_dashboard_execs_the_installed_executable_and_keeps_serving` | 5.46 | 5.62 | 1 |
| 20 | `orbit-core::runtime` | `dispatch_admission::lock_busy_pass::a_drain_defers_an_admission_pass_held_out_by_the_task_commit_lock` | 5.43 | 5.50 | 1 |
| 21 | `orbit-cli::workspace` | `host_registry::host_registry_contracts_1` | 5.33 | 5.13 | 1 |
| 22 | `orbit-cli::tool` | `claimed_review_bridge_sandbox::cleanup::outer_overrun_also_ends_the_separate_provider_group` | 5.32 | 5.32 | 3 |
| 23 | `orbit-cli::tool` | `claimed_review_bridge_sandbox::next_owner_open_waits_out_a_lock_held_past_the_busy_budget` | 5.26 | 5.27 | 3 |
| 24 | `orbit-core::runtime` | `task_delivery::source_repair::handoff_requeue::handoff_requeue_scenarios` | 5.24 | 6.19 | 3 |
| 25 | `orbit-cli::workspace` | `ambient_authority_isolation::a_managed_worker_child_without_a_binding_is_refused` | 5.22 | 5.20 | 3 |
| 26 | `orbit-exec::signal_fanout` | `pending_sigterm_interrupts_late_supervisors_without_delaying_forwarding` | 5.14 | 5.14 | 2 |
| 27 | `orbit-cli::mcp` | `mcp_roundtrip::plugins::live_plugin_configuration` | 5.08 | 5.38 | 1 |
| 28 | `orbit-cli::workspace` | `host_registry::host_registry_contracts_0` | 5.02 | 4.60 | 3 |
| 29 | `orbit-mcp::boundary` | `ssh_reuse::ssh_transports_reuse_private_bounded_sockets_and_respect_native_config` | 4.93 | 5.08 | 2 |
| 30 | `orbit-cli::process` | `update::local_candidate::local_candidate_contracts_1` | 4.89 | 6.88 | 2 |

Per-test durations did not fall, and none was trimmed. The sharded run's per-test sum was 10% higher than the full run's. Other jobs shared the host, and individual tests moved both ways between the two runs. The slowest tests are scenario tables and process-lifecycle contracts. Their time goes to per-row fixtures (runtime roots, Git checkouts and remotes), real detached workers, and production budgets they wait out on purpose, such as the owner-store busy budget and signal-forwarding bounds. Row profiles show no fixed wait to remove. For example, the slowest row of `published_completion_scenarios` takes 4.4 s across its drift cases, and the slowest row of `same_pr_publication_scenarios` takes 5.7 s across 18 fixture cases whose retry wait is already substituted. Under sharding, no single test bounds a shard: the slowest takes 19 s of an 86 s shard. Each shard kept all six slots busy until its last 0.5–6.3 s. The 6.3 s tail is `replay_returns_a_durable_receipt_while_the_job_runs_in_a_detached_worker`, whose job sleeps longer than the HTTP client's five-second timeout by design ([DEVELOPMENT.md](../DEVELOPMENT.md)). It guards a known incident and was left unchanged.

The split costs runner time for wall time. A pull request run now uses seven jobs instead of three. The shards set up while the build runs and then wait for it, and the build job keeps its own Rust dependency cache beside the Check job's.

A fifth pass on 2026-10-18 lowered the budget from 900 to 600 seconds. Its baseline is the 35 successful PR runs on 2026-10-18 under the split layout (runs 39126710800 to 39173984566), read from GitHub's job start and completion times. The Check job's elapsed time was p50 455 s, p90 581 s, max 1237 s. Queue wait before the Check job started was p50 9 s and p90 489 s, outside its elapsed time. Awaited jobs that started after the Check job queued for p50 0 s, p90 160 s and at most 983 s inside its elapsed time. Without that wait the elapsed time was p50 440 s, p90 492 s, max 502 s. So the gate already met 600 s at p90, and the slow tail was runner queueing, which more shard jobs would add to.

The critical path was the test build, then the slowest shard. The build job took a median 240 s (176–261 s): about 20 s of setup, a 110–191 s folded build, a 7–10 s archive, then the partition guard before the upload. Shard 1 finished last in 26 of the 35 runs. From the build job's completion, shard 1 took a median 226 s and shards 2 and 3 took 200 and 201 s. Two serial phases came off that path, with no change to what runs:

- The shards start on the build's `Upload nextest archive` step (`ci-await-jobs.py --step`), and the partition guard runs after the upload (`ci-guardrails.sh --test-partitions`). Before, the guard took 23–31 s (median 27) in 12 sampled runs, and the build job's last 0.4–0.7 s followed the upload. That time now overlaps the shards. A guard failure still fails `Test build` and so the Check job.
- Shard 1's Linux sandbox/policy enforcement gate reads the shard's own extraction (`--binaries-metadata` and `--cargo-metadata`) instead of extracting the archive twice more. Before, the gate took 14.3–16.7 s in the same 12 runs, and 15.4–15.9 s of that went on its two extractions. Its selection and the no-match exit 4 are unchanged.

Applied to each baseline run's last shard, these changes project a p90 of 539 s with queue wait and 450 s without, and a p50 of 419 s. The projection is not a measurement. The hosted after column is owed by the pull request and the next 10 PR runs, which must show p90 at most 600 s.

The compiler-object cache contributed nothing on PRs: both jobs restored a 4 KiB weekly snapshot and reported 0 compile requests, because the rust-cache archives already hold the dependencies and sccache does not cache the clippy-driver calls that build the workspace members. It has since been removed, and pull requests restore the rust-cache archives without saving them ([compiler-cache runbook](../runbooks/compiler-cache.md#ordinary-linux-ci)). Locally the folded Clippy lint added about 4 s to an 85 s workspace test build, so moving it to the Check job would save little. No test was retired, consolidated, skipped, retried or weakened, and the shard count and job names are unchanged. Count partitioning still leaves shard 1 about 10 s behind the others; rebalancing it from the shards' `nextest-junit-k` per-test durations is the next lever if the hosted sample misses the budget.

A sixth pass on 2026-10-24 repaired a PR p90 of 626 s (16 executed Checks, two over 600 s: 626 s and 642 s). Guardrails (184–244 s) and nextest Summary times (114–216 s) had not grown. Across 8 sampled runs, the time over budget sat between the build's upload and each shard's nextest start. Uploading the 448 MB transfer took about 6 s, but downloading it took one shard 4–18 s on most runners and 33–155 s on others, in the same run (153, 44 and 6 s for the three shards of the 626 s run; 111, 155 and 33 s in the 642 s run). The pack before the upload took 17–33 s, because Ubuntu 24.04's zstd compresses on one thread unless asked. Locally, four threads cut the same level-6, 512 MiB-window encode from 14.2 s to 4.9 s, with identical output size. Two changes shortened that path, with no change to what runs:

- `ci-repack-nextest.py pack` encodes on every core (`-T0`).
- The transfer travels as `NEXTEST_ARCHIVE_PARTS` (4) byte ranges, each its own artifact. `download-artifact` starts every matching artifact's download at once, so each shard reads four streams instead of one. Each artifact's digest is still verified, and `unpack` fails unless the parts it reads, in order, decode completely.

Applied to the sampled runs, with the pack at the measured 2.9× speed-up, each download at a quarter of its single-stream time and 3 s for the extra upload steps, the slowest Check projects to 516 s (626 → 505 s and 642 → 511 s). If parallel streams only halve the download, it is 550 s. These are projections, not measurements. The after column is owed by the pull request and the next 10 PR runs, which must show p90 at most 600 s. No test was retired, consolidated, skipped, retried or weakened.

A seventh pass on 2026-10-30 repaired a PR p90 of 652 s, re-measured that day over the latest 20 completed PR workflow runs (41030962890 back to 41019614363): 14 executed the Check job and 6 skipped it (docs/skip scope), with no cancellations; the executed Check durations sorted were 488, 501, 524, 535, 538, 540, 555, 569, 600, 607, 609, 649, 652 and 653 s (nearest-rank p90 rank 13 = 652 s, median 562 s). `scripts/ci-p90.py` now reproduces this measurement. A shard job finished last in all 14 executed runs (shard 1 in 9, shard 3 in 5, shard 2 in none); the Test build never finished last. Median job durations were Test build 304 s and shards 552/523/581 s. The awaited jobs started within 1 s of the Check job in every run, so runner queue wait inside the Check elapsed time was about zero in this cohort (the Check job's own queue wait before it started, median 21 s, is outside the budget's start-to-completion measure). The slowest successful run (41023394003, Check 653 s, every job `cache_hit=true` on Rust 1.98.1) decomposes from its step timings as: build setup 21 s, folded build 246 s, archive 8 s, pack 10.8 s (702 MB in, 516 MB out), four uploads about 10 s to the shards' await, shard 1 download 7.3 s, unpack 8.8 s (767 MB restored), nextest 328.4 s at 96.5% CPU busy, JUnit upload and the sandbox gate about 2 s (the gate's two tests took 0.14 s). The post-upload partition guard (24.7 s) and doctests (10.7 s) ended with the Test build at 331 s, and the Check job's own guardrails took 370.9 s ending about 425 s after its start, so neither bound that run: the binding path is build-to-upload, then transfer, then the slowest shard's nextest, and the Test build runner idled for roughly 300 s of it.

Locally (one real 704 MB archive from `ci-guardrails.sh --test-archive` at the same revision, shards pinned to four cores with `taskset -c 0-3`, profile `ci`, each run alone on a shared 32-CPU host at load average 13–17, so single wall samples carry roughly ±10% noise), the three count partitions' JUnit duration sums were 789.3, 652.9 and 635.9 slot-seconds (3,237 tests; walls 136.0, 122.0 and 116.3 s; each run kept the same bubblewrap-deferred rows and the same fast environment failures, so these are scheduling measurements, not suite passes). Partition 1 is systematically heaviest, which matches shard 1 finishing last most often hosted. Codec tuning is not a lever at this size: pinned to four cores, pack took about 5.3–6.8 s in every arm (`-3`/`-1`, `--long=29`/`27`), unpack 5.0–5.5 s as shipped (`-1`), 4.4–4.7 s re-encoding at `-3 -T0` and no better at `-1 -T0`, against 1.9 s for decode alone, so at most about a second is available there and the shipped options stay. Dropping `--success-output immediate` measured about 5% of a shard locally but was rejected: immediate output is what makes `DEFERRED:`/`SKIP:` notices visible in the shard logs, and hiding them would hide exactly the deferrals the budget rules forbid disguising. Two changes shorten the binding path, with no change to what runs:

- The test list now has `NEXTEST_PARTITIONS` (4) count partitions. The three shard jobs run partitions 1, 3 and 4; the Test build job runs partition 2 from the archive it built, after the upload, with the shards' ripgrep, uv and Bubblewrap preparation installed only then so nothing delays the build or the upload. A failure there fails Test build and so the Check wait, exactly like a shard failure. The partition guard proves the four selections cover the full list exactly once (locally: 830/817/802/788 of 3,237, each test once). Job names and `NEXTEST_SHARDS` are unchanged, so the agent-main ruleset needs no change. In the local duration set the four partitions sum to 600.7, 489.4, 494.5 and 493.6 slot-seconds; the build job takes the lightest (489.4) because its post-upload guard, doctests and tool setup (about 56 s hosted) precede its run, while a shard's download and unpack cost about 16 s.
- The priority list above now names every test measured at 12 s or longer in this profile (the 2026-10-29 list of six had gone stale: e.g. `pr_landing::ci_rerun::dependency_adapter_refuses_incomplete_or_forged_evidence` measured 35–51 s and finished last in one local partition run). Scheduling only, as before.

Applied to run 41023394003 by scaling its slowest shard's nextest by the local partition-sum ratio (600.7/789.3), the shard chain projects from 647 s to about 570 s and the Check job from 653 s to about 575 s, with the build job's own partition ending earlier still (upload at about 296 s + 56 s of guard, doctests and setup + about 205 s of nextest at the same scale). This is a projection from one duration set, not a measurement: per-test durations came from three sequential local runs on a loaded shared host, individual long tests varied by up to 40% between runs, and a slow test-build runner now also runs its partition slowly, where before it only delayed the upload. Local pinned walls for the four partitions (133.3, 101.1, 110.7, 98.7 s) could not confirm the projection against host noise. The after column is owed by the pull request and the following PR runs: re-run `scripts/ci-p90.py` over the latest 20 completed PR workflow runs and report executed, skipped and cancelled counts with the nearest-rank p90; ten PR runs must show p90 at most 600 s. Do not read a single candidate run, fast or slow, as the restored distribution. No test was retired, consolidated, skipped, retried or weakened, and the shard-1 sandbox gate, the partition guard and digest verification are unchanged.

An eighth pass on 2026-10-31 repaired a PR p90 of 664 s after the seventh pass landed. The latest 20 completed PR workflow runs (41158858782 back to 41140505053) had 18 executed Check jobs (15 succeeded, 3 failed) and 2 skipped, with no cancellations. Sorted, the executed durations were 469, 489, 491, 496, 526, 534, 539, 547, 557, 567, 579, 587, 617, 629, 642, 646, 664 and 693 s (nearest-rank p90 rank 17 = 664 s, median 562 s, six over 600 s). Every job log of those 18 runs was read for step timings. The Check and test jobs started within 10 s of each other. The Test build now finished last in 11 of the 18 runs, and in every cache-hit run over 600 s. The shards did so in 4 runs and the Check job's own guardrails in 3. Medians were: folded build 244 s (142–277 s on a cache hit), upload done at 292 s, then 56 s of non-test work before the build's own partition started (partition guard 32 s, doctests 11 s, ripgrep, uv and Bubblewrap preparation about 11 s), then its nextest at 206 s (119–277 s). After the upload a shard spent 19 s before its nextest (await, a four-stream download and the unpack), and its nextest took a median 201, 205 and 165 s for shards 1, 2 and 3. The build's partition was not heavier than a shard's: it finished last because its runner spent about 37 s more on non-test work after the upload. The partition guard spent 28 of its 32 s extracting the 819 MB archive again for each of its five listings (5.5 s each in run 41144357359). Two runs missed the Rust cache (`cache_hit=false`), 41155880316 (693 s) and 41155508765 (629 s). Both tested heads came right after the 0.40.0 version bump changed the root `Cargo.toml`, and with it the rust-cache `prefix-key`. In those runs the folded build took 354 s and the Check job's guardrails took 576 s respectively. Two changes shorten the binding path, with no change to what runs:

- The partition guard extracts the archive once and lists all six selections from that extraction's `--binaries-metadata` and `--cargo-metadata`, as the shard-1 sandbox gate does. The listings still execute the archive's own test binaries, and a failed extraction fails the guard.
- `NEXTEST_PARTITIONS` is 5. A new `Test extra partition` job runs the shard steps on a fourth runner with partition 5, so each of the five test runners takes a fifth of the list instead of a quarter. It is not a `Test shard k/N` job, so `NEXTEST_SHARDS`, the shard names and the agent-main ruleset are unchanged. The Check job awaits it like a shard, so its failure fails the required check. The sandbox gate still runs only in shard 1. A PR run uses one more runner, and nothing is added to its critical path except the runner's own queue wait. The test jobs started with the Check job in this cohort.

For each run, the projection replaced the observed binding end with the latest of: each shard's end minus a fifth of its nextest time; the build's end minus the guard saving (32 s down to about 11 s) and a fifth of its nextest time; a fourth runner starting like the slowest shard, with the run's mean shard overhead and 0.8 times the slowest partition's nextest time; and the Check guardrails' end. It projects the cohort's p90 from 664 to 629 s (median 562 to 525 s). Over the 16 cache-hit runs it projects p90 from 646 to 570 s, and the slowest from 664 to 588 s. The two cache-miss runs still bind the cohort (629 s on the Check job's own guardrails, 642 s on a shard), so the next lever is the cold rust-cache after a root `Cargo.toml` change, which this pass does not change ([compiler-cache runbook](../runbooks/compiler-cache.md#ordinary-linux-ci)). This is a projection from one cohort's step timings, not a measurement. The after column is owed by the pull request and the following PR runs: `scripts/ci-p90.py` over the latest 20 completed PR workflow runs, with executed, skipped and cancelled counts and the nearest-rank p90. Ten PR runs must show p90 at most 600 s. No test was retired, consolidated, skipped, retried or weakened. The shard-1 sandbox gate, the partition guard's exact-once proof and digest verification are unchanged.

A ninth pass on 2026-10-31 (ORB-20394) lowered the budget from 600 to 400 seconds and restructured the gate to reach it, without retiring a test. Before: over the 2553 executed (successful or failed) PR Check jobs from 2026-10-01 to 2026-10-31, p50 was 491 s and nearest-rank p90 814 s. Over the 377 successful ones from 2026-10-28 12:19 to 2026-10-31 04:49 UTC, p50 was 559 s and p90 674 s (push runs: 603 and 669 s). Step timings of six of those runs (41169473523 to 41169784003) located two lanes of equal length. The Check job's `Run CI guardrails` took 371–460 s, set by the guard pool and exclusive lane (374–445 s: `test-qa-full-sweep.py` 191–227 s, `test-check-dependency-direction.py` 80–99 s, `test-ci-download-verified.py` 87–91 s) beside production Clippy (98–123 s) and documentation (32–40 s). The test lane was the folded build (207–271 s), upload, then each partition's nextest (121–209 s). In a 40-run sample of `Change scope`, 18 PRs ran in full only because they touched a path outside `crates/` (9 under `plugin/skills/`, 4 `scripts/qa-full-sweep-inventory.json`), and every `affected:` run still tested its feature anchors (`orbit-core`, `orbit-mcp`, `orbit-web`), so an `orbit-cli`-only PR ran about 72% of the suite's test time. Changes:

- Guardrails moved to a `CI guardrails` job (`ci-guardrails.sh --guards`) on its own runner beside `Test build`; the Check job runs formatting, production Clippy, documentation and the supply-chain gate, and awaits it with the test jobs. On a pull request the `scripts/test-*` self-tests run only when their inputs changed, by the `make ci-fast` rule; the repository guards and the QA sweep's inventory check always run. Push runs keep every self-test.
- Every PR partition runs only its `affected:` crates' tests (a nextest package filter that the partition guard also lists); feature anchors are built, not run. Every PR adds the always-run set (`ALWAYS_RUN`: `orbit-exec`, `orbit-policy`, `orbit-auth`, `orbit-store`), so shard 1's Linux sandbox gate runs on every PR. In the 30-day failure record, `orbit-store` tests dominate the `agent-main` breakages and every migration failure is in `orbit-store`. The selector was replayed over the 171 PR failures heuristically classed as catches (failing on a branch, later fixed there), using each head's three-dot diff from `agent-main`. 88 came from heads that still run in full, and 79 were in a selected crate. The other 4 were `orbit-mcp`'s `task_route` boundary test on heads whose own diff changed only skill Markdown or `orbit-store`. That test reads only the compiled `orbit-tools` registry, so those failures came from a red base, not the diff, and the push run catches them. A governed tool added to `orbit-tools` or `orbit-common` still selects `orbit-mcp` and `orbit-cli` (`test-pr-state-workflows.py` asserts the `orbit-tools` case).
- `FILE_READERS` declares repository files read by Cargo tests, including the sweep and npm validator (`orbit-core`), affected-test selector (`orbit-core`), CI await helper and workflow YAML (`orbit-engine`), release-readiness adapter and example (`orbit-engine`), setup skill mirror and auto-task design catalog (`orbit-core`), cross-crate fixtures, and executor/job/activity/auto-task assets. The table also covers the vendored dashboard scripts (`orbit-mcp`), plugin hooks, registry metadata and configuration reference (`orbit-cli`). The sweep inventory has no Cargo reader and is checked by guardrail self-tests. A path outside `crates/` that `FILE_READERS` does not declare still runs the full suite, as does any change to a workspace manifest, the lock file, the toolchain, `.cargo/` or `.config/`.

Required check names are unchanged, and `agent-main` push runs test every crate, so a defect a narrowed PR run misses fails the push run and reaches the `ci_failure_sweep` as a repair task. After, measured on hosted runs:

| Run | Scope | Check / Clippy / Test | Test build | Slowest shard | CI guardrails |
|---|---|---|---|---|---|
| 41172338562, leaf-crate PR (one `orbit-cli` line, stacked on this change) | `affected:` `orbit-cli` + always-run | **349 s** | 342 s (build 196 s, own partition 68 s) | 333 s (nextest 61 s) | 106 s (guards 61 s) |
| 41172682236, this change's own PR (touches `.github/` and `scripts/`) | `full`, every self-test | 445 s | 359 s (build 161 s, own partition 129 s) | 439 s | 333 s (guards 291 s) |

In both runs the Check job's own checks (73–100 s) ended long before its wait, so the test lane binds. A leaf-crate PR now finishes in 349 s, against 520–560 s before (the leaf runs in the before cohort were `affected:` runs that still tested their feature anchors). A full-mode PR, one that changes `.github/`, `scripts/`, an undeclared non-crate path, a manifest or the lock file, still takes about 440 s. The guardrails no longer bind it (333 s beside a 439 s shard), but its full nextest partitions do. So the 400 s p90 holds only while full-mode runs stay under a tenth of PR runs. If they don't, the next lever is the full suite's partition time, not the guardrails. The p90 over the PR population is owed by the following runs: `scripts/ci-p90.py` (now defaulting to 400 s) over the latest 20 completed PR runs. No test was retired, consolidated, skipped, retried or weakened.

A tenth pass (ORB-20920) shortens two parts of the path the 2026-11-01 watch measured at nearest-rank p90 578 s over 16 executed PR Check jobs (budget 400 s). No test was retired, and the hosted p90 after the change is still the operator measurement below.

The guard pool reaped in launch order, so a slow early member kept the slots of members that had already finished. On run 41294900107 (CI guardrails job 124813628606) the width-4 pool's own durations replay to 487.5 s, matching the hosted phase wall of 488 s. `test-qa-full-sweep.py` was 241 s of that, and the exclusive lane summed to 120 s beside it. Replaying the same durations with free-slot admission, still at width 4, finishes the pool when the sweep finishes, at 241 s. The waiting download fixture (83 s) and the exclusive lane stay overlapped with that. Output is still printed in launch order, and the first failure in that order is still the pool's status. Required check names are unchanged.

Some of those full runs were full only because a path outside `crates/` was absent from `FILE_READERS`. Since the ux-rules routine and its filer were removed [ORB-22844], the UX rules are a hand-run QA tool: `scripts/ux-rules/ui/`, `scripts/ux-rules/cli/`, `scripts/ux-rules/tests/`, `scripts/ux-rules/README.md`, `scripts/ux-rules/lib/ui-context.mjs`, `scripts/ux-rules/lib/cli-context.mjs`, `scripts/ux-rules/lib/manifest.mjs` have no Cargo reader. No CI job runs the Node tests under `scripts/ux-rules/tests/`, so those paths select no crate and the PR runs the always-run set. `scripts/ux-rules/run.mjs` stays undeclared, so a change to it runs the full suite. There is no parent `scripts/ux-rules/` prefix, because that would declare `run.mjs` as well.

An `orbit-core` selection is the eight-crate set measured at 545 s on run 41289004379. A full partition on this watch was 560–580 s, with list-build 273–294 s and nextest about 207–222 s on four cores, so archive-ready time plus nextest stays over 400 s for a full run and for that eight-crate set. The always-run-only class is the leaf measured at 341–404 s, and a scripts-only PR still runs the guardrail self-tests. The hosted sample after merge is `scripts/ci-p90.py` over the latest 20 completed `pull_request` `ci.yml` runs at a 400-second budget, with executed, skipped, cancelled, missing, failures and queue wait recorded separately. A local filtered timing does not establish that p90.

An eleventh pass (ORB-21420) follows the 2026-11-02 watch: 13 executed PR Check jobs in the latest 20 completed PR runs (7 skipped, none cancelled), nearest-rank p90 627 s, median 532 s, min 403 s, every one over 400 s. The test lane bound them: the folded build's list-build (165–292 s) and then the slowest partition's nextest at 93–97% CPU busy. Nine ran in full, five of those only because they touched `scripts/unit-test-baseline.json` (4) or `plugin/evals/README.md` (1). It makes two changes, with no test retired, skipped, retried or weakened:

- A sixth count partition runs on its own runner (`Test partition 6`, the shard steps; `NEXTEST_PARTITIONS` 6), and the Check job awaits it. Each partition's summed per-test time divided by profile `ci`'s six threads matched its nextest Summary time within 5 s on the full run 41414742939 (partition 2: 1406 test-seconds, Summary 236 s), and the count model reproduced 3553 of the 3576 test assignments its shard logs show. Replaying those logged durations at six partitions moves the slowest partition from 234 to 164 s on that run, and from 251 to 183 s on full run 41417766911. Seven partitions gain at most 3 s more, because single tests of 130 s or more set the floor. nextest's `slice:` partitioning was replayed too and rejected: it beat `count:` on the first run (197 against 234 s) and lost on the second (267 against 251 s). The binding path after the build's upload is about 70 s shorter on full runs. The build itself is unchanged, so a full run still exceeds 400 s.
- `FILE_READERS` declares `scripts/unit-test-baseline.json` (read only by the always-run `unit-test-inventory.py --check` guard) and `plugin/evals/` (the `claude plugin eval` suite, read by no Cargo test, guard or CI job) with no Cargo reader. A pull request that changes only these runs the always-run set, and one that also changes crates runs those crates' selection. `scripts/test-pr-state-workflows.py` asserts both.

Required check names, the always-run set, the partition guard's exact-once proof, the shard-1 sandbox gate and full `agent-main` push runs are unchanged. The sixth runner adds about one runner's time per run, and queueing for it falls inside the Check job's elapsed time. These figures are replays of one cohort's logs, not a hosted measurement. The hosted after column is the operator's `scripts/ci-p90.py` run over the latest 20 completed `pull_request` `ci.yml` runs at 400 s, reporting executed, skipped, cancelled and missing counts, failures and queue wait separately, from runs on this workflow revision.

A twelfth pass (ORB-21846) follows the 2026-11-03 watch: 17 executed PR Check jobs in the latest 20 completed PR runs (3 skipped, none cancelled), nearest-rank p90 581 s, median 498 s, min 405 s, every one over 400 s. `Test partition 2` finished last in 9 of them. Its logs show one test that does not scale with CPU: `orbit-cli::process job_resume_detached::unix::worker_command_contracts_2` took 130.7–131.9 s in every examined run (41618439254, 41611947898, 41604497743, 41610521398, 41605212372), against 8.03 s in the fourth pass's table above. In 41611947898 it was partition 2's last test, so the partition ran at 48.9% CPU busy for a 146.5 s Summary; in 41618439254 and 41605212372 it ended 4–7 s before the Summary. Locally it took 128 s, and a process trace showed 120 s of it in one wait: its row `resume_wait_exits_nonzero_for_a_failed_detached_run` edited the catalog job to an unknown action, but the resumed run re-enters the source run's gate step, so nothing failed until `FIXTURE_STEP_DEADLINE` (120 s) killed the unanswered step. The row now holds the gate and fails it (`WorkflowGate::fail`, a nonzero waiter exit), so the run fails at once. It still asserts that `job resume --wait` returns only after execution, exits nonzero, and reports and stores the run as `failed`. Locally the test takes 7.6 s. No test was retired, skipped, retried or weakened, and the workflow, partitions and required check names are unchanged.

The same watch's build evidence bounds what else this pass could have changed. A local workspace test build at `-j4` (dependencies warm, workspace members cold, no compiler cache) took 145 s under the folded build's clippy-driver and 130 s with plain rustc. Cargo's timings show two causes. `orbit-core`'s unit-test harness (49 s) starts only after the whole `orbit-core` library (47 s), because its self dev-dependency (which enables `test-support`) makes the harness link that library, and up to three cores sit idle meanwhile. Dropping the self dev-dependencies in an experiment, with the same features enabled from the command line, also gave 130 s. Neither change is made here. Moving all-targets Clippy out of the test build needs its own pass in the Check job. Removing a self dev-dependency would lose `test-support` for `cargo test -p <crate>`. A full run's archive-ready time plus its slowest partition therefore still exceeds 400 s. The p90 after merge is the operator's `scripts/ci-p90.py` run over the latest 20 completed `pull_request` `ci.yml` runs at 400 s, from runs on this revision.

A thirteenth pass (ORB-22077) follows the 2026-11-04 watch: 15 executed PR Check jobs (5 skipped, none cancelled), nearest-rank p90 578 s, median 440 s, 10 over 400 s. Every Test build reported `cache_hit=true` on 4 CPUs, on image `ubuntu24/20261026.354.1` or `20261101.360.1`. Slow does not mean full. Six runs selected the changed crates' leaf set (always-run crates plus `orbit-cli`), five that plus `orbit-web`, three named `orbit-core` (8 to 11 crates) and one ran the full suite. The p90 sits in the last two groups, which carry `orbit-core::runtime` and `orbit-engine::engine`. Leaf runs spent 174 to 294 s in list-build and only about 50 to 90 s in the slowest partition's nextest, so their build was binding.

Their build was large for two reasons. CI sets `CARGO_TERM_COLOR=always`, which colors `cargo tree`'s `(*)` marker, so `ci-test-affected.py` read it as a feature and added spurious anchors (a deterministic replay reproduced the hosted build sets for all four selections: `orbit-agent`, `orbit-automation` and `orbit-runtime` were spurious). The real gap, `tokio`'s `test-util`, `rusqlite`'s `hooks` and the `test-support` of `orbit-exec` and `orbit-mcp`, then pulled in `orbit-core`, `orbit-mcp` and `orbit-web`, and an anchor always compiles its unit-test harness (`orbit-core`'s alone costs 54 s of CPU). The pass passes `--color never` to `cargo tree`, names those features in `orbit-cli`'s dev-dependencies so no anchor is needed, and adds `Test partition 7` and `8`.

| Local 4-core replay (members cold, dependencies warm, none rebuilt) | Before | After |
|---|---|---|
| Leaf selection, build | 125.7, 117.8 s | 85.3, 83.6 s |
| Leaf plus `orbit-web`, build | same 11-crate build set as leaf | 90.6 s |
| 11-crate core selection, build | 121.3 s | 118.6 s |
| Leaf archive | 761 MB (hosted) | 377 MB |

At the same tree the leaf archive lists the same 1047 tests as the old anchored build, so no test was dropped; doctests compile nothing after any of the three builds. The eight-partition guard passes on the leaf (1047), 11-crate (3170) and full (3765) archives. Replaying the logged durations, assigned as count partitions do (3145 of 3170 tests match the observed assignment at six partitions), the slowest partition's test time falls 35 to 57 s on core and full runs and 10 to 22 s on leaf runs; a ninth or tenth partition gains little because the longest test (96 to 136 s) then bounds it. Each new partition holds a runner for the whole run, because shards prepare while the build runs, so the pass adds two runners' time per run, and queueing for them falls inside Check's elapsed time.

Rejected by measurement: moving all-targets Clippy out of the folded build saves about 4 s wall (130.2 s against 125.8 s, also dropping examples), not worth a new Check phase; removing `orbit-core`'s self dev-dependency gains no wall time (twelfth pass); `resolver.feature-unification` would replace the anchors but Cargo 1.98.1 ignores it without a nightly flag.

Applying the replay's ratios to the cohort's logs projects p90 522 s, median 347 s, max 528 s and 4 over 400 s. That is a projection, not a measurement, and **it does not restore the budget**: the core and full runs (27% of the cohort) still take about 520 s, because their build is CPU-saturated on crates they really test. Closing that needs a decision outside this pass, such as a larger runner for `Test build`. The hosted per-unit timings were not read (`test-build-timings` needs an authenticated download), so the same selection's list-build ranging from 174 to 256 s on one image remains unexplained; `Report build inputs` now logs the CPU model to test runner variance. The hosted after column is the operator's `scripts/ci-p90.py` run once the latest 20 completed PR runs all carry this workflow.

A fourteenth pass (ORB-22218) follows the 2026-11-05 watch: the latest 20
completed PR workflows had 12 executed Checks, 7 skipped and 1 cancelled,
nearest-rank p90 482 s, median 449.5 s and max 496 s. Two early failures
(42 and 155 s) are elapsed observations, not fast successful gates. Slow
successful samples spent 226–263 s in list-build, then 109–176 s in their
last partition's nextest. Check's own phases finished earlier. Archive
transfer on those slow samples was truncated, so no transfer cause is established.

The controlled fixture used this base's full workspace selection, Rust
1.98.1, nextest 0.9.136, four Cargo jobs and six nextest slots pinned to
CPUs 0–3 on a shared 32-logical-CPU Ryzen AI MAX+ 395 Linux host. CI's
incremental-off, debug-off and symbol-strip settings were retained. Dependency
sources and compiled dependencies were warm; workspace members were cleaned
between build trials, the compiler-object cache was off and OS caches were
not flushed. Baseline list-build took 124.1/122.7 s, rebuilding the same 87
workspace units with all 385 dependency units fresh. The core library
(37.6 s) then its unit harness (49.5 s) bound the first build. An uncontaminated
archive reused those objects in 2.68 s; extraction took 2.99 s. No hosted
transfer, runner setup or queue saving is inferred from these local timings.

The eight baseline partition walls were 63.97, 49.71, 59.78, 48.04, 52.53,
52.27, 37.52 and 45.85 s, run sequentially on the same four CPUs. The
slowest partition's JUnit timestamps showed two late carriers:
`backlog_forecast::arrivals_past_the_generation_bound_report_the_cut_and_withhold_totals`
started at 51.86 s and finished last at 63.80 s;
`workflows::replay_returns_a_durable_receipt_while_the_job_runs_in_a_detached_worker`
started at 56.17 s and finished at 62.43 s. Prioritizing the forecast alone
left the worker observation deadline at the tail and measured 65.74 s.

The CI priority list now starts **both** carriers early. Reusing the exact
same archived binaries and partition, both started at zero. Partition wall
fell to 59.89/59.90 s; a subsequent baseline control took 63.54 s (initial
baseline 63.97 s). The repeated-control difference is about 3.85 s, or 6%
of this partition, with natural shared-host timing noise. The remaining
seven partitions' priorities are unchanged and were not replayed for the
candidate; the baseline's next-slowest partition was 59.78 s. This is a
measured partition improvement, not a complete candidate hosted gate or a
claim that its p90 meets 400 s. The build remains unchanged and binding
before archive publication; other partitions can now set the tail.
A final root-profile validation took 95.04 s: the separate
`distributed_drain::claimed_host_evidence::sandbox::seatbelt_probe_outcomes_scenarios`
carrier took 60.93 s in one passing probe-denial scenario. Its cause is
unconfirmed; the same 497 passes and four scratch-path failures remained.
This outlier is retained separately and prevents treating the controlled
scheduling gain as a complete candidate-gate runtime guarantee.

All 3,839 test identities retain their applicability (3,773 selected and 66
ignored), and the eight-way exact-once proof passes. Each critical-partition
arm ran the same 501 tests: 497 passed and the same four scratch-path cases
failed (config path display and Unix socket length limits). Other baseline
partitions also had scratch-environment failures. These are diagnostic
scheduling measurements, not full-suite passes; logs retain all successful
output and capability notices. No test, assertion, deliberate observation
deadline, partition, group cap, retry policy, always-run selection, required
check identity or full integration-push coverage changes. Local default
nextest ordering is unchanged because the addition is confined to `profile.ci`.

A plain-rustc build experiment measured 116.8/119.9 s, about 5.1 s below the
folded build's median, but required a separate all-targets Clippy phase,
cache migration and old-head compatibility handling. That experiment is not
landed; the final change only refreshes scheduling. The task's
`ci-budget/report.md` artifact retains commands, conditions, JUnit evidence,
rejected trials and validation outcomes. Two initial archive timings were
excluded after source edits/Python bytecode writes under the build script's
watched `scripts/` directory invalidated reuse.

Operator handoff: after merge, collect the latest **20 completed
`pull_request` workflows for `ci.yml`** once those runs carry this revision.
Run `python3 scripts/ci-p90.py --limit 20 --budget-seconds 400` and retain
run IDs, full head SHAs and raw job/step timestamps. Report executed,
skipped, cancelled and missing Check counts (classify `other_gaps`), failures
separately from successful completions, ascending executed durations,
nearest-rank rank `ceil(0.9 * n)`, p90, median and maximum. Do not count gaps
as zero-duration successes. Report Check's pre-start queue wait separately
from its elapsed wall time, and the awaited jobs' queue wait occurring after
Check starts separately again; obtain job creation/start timestamps from
the Actions jobs API and retain `ci-await:` start/completion lines. If a
queue timestamp or transfer step is unavailable, report the gap rather than
infer zero. Compare selections, cache hits, runner image/CPU and phase times
before attributing a cohort change. This measurement is operator work after
merge, not a requirement for this implementation leaf or a claim that the
400-second budget has been restored.

A fifteenth pass (ORB-22450) follows the 2026-11-06 watch: 16 executed PR Check
jobs in the latest 20 completed PR runs (four skipped, none cancelled or
missing), nearest-rank p90 542 s (rank 15/16), median 476.5 s, maximum 551 s.
Two slow extremes ended on CI guardrails, whose two longest members overlapped
(`test-check-dependency-direction.py` 362.5 s and 386.3 s, `test-qa-full-sweep.py`
363.4 s and 335.1 s). It reduces the work inside those two self-tests. No check,
job identity, runner, guard or production harness behavior changes.

Conditions for every local figure: base `7d98fa450`, one checkout path, a shared
32-logical-CPU Linux host pinned with `taskset -c 0-3` (pool width 4), Rust 1.98.1,
Python 3.14.4, bash 5.3.9, ripgrep 14.1.1, runs in sequence with no synthetic
load. The lane ran `CI_SELF_TESTS=all scripts/ci-guardrails.sh --guards`, the
selection a PR gets when it touches a self-test input. This host is not a hosted
4-vCPU runner, and setup and transfer are not measured.

- **Dependency self-test.** It ran the whole guard once per row, about 0.68 s each
  (39 Python and 48 ripgrep processes per run), and one comment matrix was 120 of
  the 266 runs. Nine large tables now share one guard run per batch, for **266 → 63
  guard runs**, a deterministic reduction. `check_rows` gives each row its own file
  at the same directory depth and requires the guard to report that row's own path,
  a stronger assertion than the former exit status. The guard is unchanged.
- **QA self-test.** `builtin_refusal_self_test(wire=True)` took 70.3 s of the
  sweep's 117.9–131.9 s self-test, because each of its 25 modes ran every built-in
  command through the supervisor although only `mcp serve` and `search reindex`
  vary. The other 24 modes now answer the unrelated fixture commands in process:
  14.2 s, and **2901 → 645 successful `execve` calls** in one traced run of each
  arm. The varying commands keep the real supervisor and fixture process.
  The `valid` row and the five `builtin_refusal` modes stay fully supervised, and
  every row's unrelated results must equal the `valid` row's.

| Sequential lane run | 1 | 2 | 3 | Median | Spread |
|---|---:|---:|---:|---:|---:|
| Baseline lane wall (s) | 238.21 | 213.47 | 210.73 | 213.47 | 27.48 |
| Candidate lane wall (s) | 104.57 | 102.67 | 105.96 | 104.57 | 3.29 |
| Baseline dependency self-test (s) | 211.135 | 188.386 | 183.757 | 188.386 | 27.378 |
| Candidate dependency self-test (s) | 50.807 | 48.119 | 48.508 | 48.508 | 2.688 |
| Baseline QA sweep (s) | 148.643 | 143.541 | 132.778 | 143.541 | 15.865 |
| Candidate QA sweep (s) | 93.422 | 91.723 | 94.001 | 93.422 | 2.278 |

The slowest candidate lane beat the fastest baseline lane by 104.77 s, 3.8 times
the baseline spread. The lane's next members are `test-qa-full-sweep.py` (about
93 s), `test-ci-download-verified.py` (about 84 s, mostly fixed waits) and
`test-pr-state-workflows.py` (about 59 s). A PR that touches no self-test input
already skips the dependency self-test and runs the QA check with `--static`, so
this pass does not shorten it.

Eight guard mutations (nested comments, qualified self, dropped modules and names,
raw strings, crate-root paths, a dropped lower-module loop) failed both the old and
new self-test, on the same test. Two QA validator mutations failed both wire arms on
the same row (`boolean-reindex`, `cross-success`). Two mutations of `finalize_result`
(accepting a nonzero exit, accepting unverified cleanup) pass the whole QA self-test
in both arms. That gap exists at the base and this pass neither causes nor closes it.

The build and test path was profiled and left unchanged. Slow run 42194401872
selected ten crates (`orbit-auth`, `orbit-cli`, `orbit-cmd`, `orbit-core`,
`orbit-engine`, `orbit-exec`, `orbit-policy`, `orbit-runtime`, `orbit-store`,
`orbit-web`), with a hosted cache hit, list-build 273 s and a 379-test partition 6
nextest of 162.6 s. The same selection locally, under the CI build flags with
dependencies warm and workspace members cold, took 127 s and 127 s for the list build
and 3–4 s for the archive. Partition 6 took 51.55 s and 51.57 s for 382 tests, all
passing (membership differs from the hosted 379 tests at this base). Its
longest row, `events::change_stream_notices_other_process_writes_and_resumes_after_reconnect`,
took 50.6–50.7 s, but no retained hosted excerpt shows that row, so it stays a lead.
Hosted busy time was 88.7% and no cause beyond breadth was confirmed, so no build or
partition change has evidence behind it yet.

Operator handoff: after merge, once runs carry this revision, follow the
[hosted CI budget handoff](../DEVELOPMENT.md#hosted-ci-budget-handoff) for the latest
20 completed `pull_request` `ci.yml` runs with
`python3 scripts/ci-p90.py --limit 20 --budget-seconds 400`. Retain full SHAs, run
IDs and raw job and step timestamps; report executed, skipped, cancelled and missing
Check counts and failures separately; report median, nearest-rank p90 and maximum;
report pre-Check queue and awaited-job queue within elapsed separately; and keep gaps
as uncertainty, never zero. Note which runs executed the self-tests. This measurement
and any live infrastructure or cost decision are operator-owned, not requirements of
this leaf, and no local figure above claims the 400-second budget is restored.

A sixteenth pass (ORB-22610) profiled the full-scope build, archive and test path after the
2026-11-07 audit (21 executed PR Checks in the latest 24 completed PR runs, nearest-rank p90
521 s, median 316 s, maximum 730 s). It found no software reduction that beats the
measurement spread, so **no workflow, script, manifest or test changed**. The profile and the
remaining decision are recorded here.

Conditions: base `543335a7e`, the full selection (`--workspace --lib --bins --tests
--examples` under `with-test-build-lint.sh`), CI's incremental-off, debug-0 and strip
settings, the compiler-object cache off, Rust 1.98.1 and nextest 0.9.136. Dependencies were
warm, all 86 workspace units were cleaned before each trial, and trials ran in sequence. The
fixture was pinned to two physical cores with their SMT siblings (CPUs 0, 1, 16 and 17 of a
shared 32-thread Ryzen AI MAX+ 395, load average 6–13), the shape of a hosted 4-vCPU runner.
Earlier passes pinned four physical cores, so their walls are not comparable.

| Sequential arm (4 threads, Cargo `-j4` unless noted) | Wall times (s) |
|---|---|
| Unchanged control | 176.1, 183.5, 169.3, 177.1, 185.7 (median 177.1, spread 16.5) |
| `-C codegen-units=16` on workspace members | 184.0, 195.8 |
| `-C codegen-units=64` on workspace members | 178.5 |
| Parallel frontend (`-Zthreads=4`, bootstrap) | 184.3, 202.3 |
| Cargo `-j6` | 184.4 |
| Same build, 8 threads (CPUs 0–3, 16–19), `-j8` | 137.5, 134.7 |

The build is bound by one dependency chain, not by CPU. In the 169.3 s control,
`orbit-types` (0–15 s), `orbit-common`, `orbit-store`, `orbit-engine` and the `orbit-core`
library (52–99 s) each start when the previous crate's metadata exists. The `orbit-core`
unit-test harness (64 s) follows that library, and the build ends with it. Between about 60
and 90 s only 1.5–2.3 of four slots were busy, while 26 units waited on `orbit-core`. In the
library's single-threaded frontend, `-Ztime-passes` measured borrow checking at 8.5 s, type
checking 7.2 s, crate metadata 6.9 s, the monomorphization walk 5.2 s, all lints including
Clippy 2.3 s and macro expansion 1.1 s. Raising proc-macro optimization therefore has nothing
to recover. Cargo's JSON shows each workspace unit built once, so there is no duplicate
compile to remove. The four zero-test library harnesses (`orbit-agent`, `orbit-auth`,
`orbit-policy`, `orbit-search`) cost 1.4 s of CPU together. Doubling the threads saves about
23% (41 s), which is the size of the larger-runner lever.

The archive reuses the build. Over two runs each, it took 3.9–4.9 s at nextest's default
`--zstd-level 0` and 2.8–3.2 s at level 1, and the pack took 7.0–8.1 s. The 618 MB transfer
was identical either way, and a third level-1 run took 14.4 s altogether, so there is no
robust saving in the codec.

On the test side, the hosted logs of full PR run 42341361073 give 5,623 slot-seconds over
3,805 passing rows. Each partition's Summary is close to its summed time divided by six
slots (partition 2: 871 slot-seconds, 145.6 s), so no single test sets a partition's wall.
Alone on the fixture, two rows ran at about 1% CPU and matched their hosted time.
`events::change_stream_notices_other_process_writes_and_resumes_after_reconnect` (50.2 s
locally, 52.3 s hosted) waits on the change stream's production cadences: 2 s scans, 5 s
host samples and the 15 s drain recheck.
`origin_fetch_hold::worktree_setup_origin_outage_holds_backlog_and_fails_in_progress`
(51.3 s, 52.2 s) waits on the production 15 s remote timeout and 25 s fetch-lock hold limit.
Shortening either would need a new test-only timing seam in production code. It would save
at most about 8 s of one partition's wall, less than the hosted spread between partitions
(94–146 s in that run), so both stay as leads. A local full run under profile `ci` (eight
threads on CPUs 0–7) passed 3,804 tests and skipped 58.

The remaining lever is an operator decision: a larger `Test build` runner, which this lane
may not change. A structural alternative is to shorten the `orbit-types` → `orbit-core`
chain, or start the `orbit-core` harness before its own library finishes. The twelfth pass
measured no wall gain from removing that crate's self dev-dependency, and splitting
`orbit-core` is a separate design task. The hosted per-unit `test-build-timings` artifacts
could not be read here (no authenticated artifact download on this lane), so whether the
hosted 220–288 s full list-builds have the same shape is still an inference. The p90 after
merge is the operator's measurement under the
[hosted CI budget handoff](../DEVELOPMENT.md#hosted-ci-budget-handoff): the latest 20
completed `pull_request` `ci.yml` runs with `python3 scripts/ci-p90.py --limit 20
--budget-seconds 400`. Report the full Check job wall time; executed, failed, skipped,
cancelled and missing runs separately; the queue before Check and the awaited jobs' queue
within it separately; and cache, scope and CPU strata. This pass claims no change to the
400-second p90.

A seventeenth pass (ORB-22688) made the first structural cut of the serial `orbit-core`
library that the sixteenth pass named. The host's sweep clock (`routines/clock`: unit
rendering and install, pause and resume, cadence, convergence, inspection and health), its
`clock.toml` settings, the tick budget `TICK_DEADLINE` and the three unit templates moved
into a new `orbit-clock` crate. It names only `orbit-common`. Core re-exports it at
`application::routines::clock` and the settings through `application::contracts`, so no
consumer changed. `orbit-automation` re-exports the tick budget, so the constant still has
one definition. The 19 clock unit tests moved with their code, unchanged.

Conditions: base `edadacb73` against the same tree with this change, the full selection
(`cargo test --workspace --lib --bins --tests --examples --no-run` under
`with-test-build-lint.sh`), CI's incremental-off, debug-0 and strip settings, no compiler
wrapper, Rust 1.98.1 and nextest 0.9.136, Cargo `-j4` on CPUs 0, 1, 16 and 17 (load
average 4.4–8.6). Both arms shared one target directory, so dependencies stayed warm and
identical. Before every trial every workspace member of either arm was cleaned with
`cargo clean -p`. Each arm had one primer, then three repetitions ran in sequence,
alternating baseline and candidate.

| Sequential arm | Wall times (s) |
|---|---|
| Primers (baseline compiled dependencies too) | baseline 321.1, candidate 176.4 |
| Unchanged baseline | 179.4, 175.8, 187.2 (median 179.4, spread 11.4) |
| Candidate | 178.4, 188.1, 176.7 (median 178.4, spread 11.4) |

The 1.0-second median difference is inside the baseline's own 11.4-second spread, so no
timing gain is claimed. The structural change is in Cargo's unit data. Workspace units went
from 86 to 88, the `orbit-clock` library and its test harness, and Cargo's JSON shows each
workspace unit built exactly once, with no duplicate. In each candidate trial, the
`orbit-clock` library compiled in 0.60–0.64 s, starting at 19.5–21.4 s and ending by 22.0 s.
That was while `orbit-store` was still compiling (17.5–42.6 s), and well before the
`orbit-core` library started (55.0–59.9 s). Its harness (1.1–1.9 s) finished by 46.8 s. The
clock is no longer input to the serial core unit: it moved 4,514 of the core library's
286,727 source lines (1.6%) and 19 of the 230 tests in its unit-test harness. That is too
small to show up in the core units:

- **Core library:** 45.2, 47.6 and 55.2 s in the baseline, 46.5, 48.5 and 49.2 s in the
  candidate.
- **Core harness:** 68.4, 65.2 and 74.8 s in the baseline, 68.6, 34.8 and 71.3 s in the
  candidate. Cargo's report starts the 34.8 s harness late (153 s), and that trial was the
  slowest candidate (188.1 s), so the low figure is not a saving.

`orbit-automation` now waits on `orbit-clock`, but the clock finishes before `orbit-store`,
which automation already waited for, so the chain gained no serial step. `cargo nextest
list` over the same selection counted 3,875 tests in both arms: 66 binaries became 67, the
`orbit-core` library harness went from 230 to 211 tests, and `orbit-clock` held the other
19, under the same names. The exact-once partition proof, the Linux sandbox gate, the
always-run crates and the required job names are unchanged. The new crate joins every
workspace selection through `cargo metadata`. The unit-test baseline records the 19 moved
identities.

Limits: one modest extraction does not shorten the `orbit-types` → `orbit-core` chain or
the core harness that ends the build. A wall-time gain would need cuts of this kind on a
much larger share of the core library and harness, or the larger `Test build` runner. That
runner remains an operator decision, not part of this leaf.

Operator handoff: after merge, once runs carry this revision, measure the latest 20
completed `pull_request` `ci.yml` runs under the
[hosted CI budget handoff](../DEVELOPMENT.md#hosted-ci-budget-handoff) with
`python3 scripts/ci-p90.py --limit 20 --budget-seconds 400`:

- Report the full Check job elapsed time, with the nearest-rank p90 against 400 s, the
  median and the maximum.
- Report executed, failed, skipped, cancelled and missing runs separately.
- Report the queue before Check separately from the awaited jobs' queue within it.
- Report the cache, scope and CPU strata, and keep every gap as uncertainty, never zero.

That measurement is the operator's, not an acceptance gate of this leaf, and no local
figure here claims the 400-second budget is restored.

An eighteenth pass (ORB-23406) follows the 2026-11-08 watch (ORB-23395: 17 executed PR
Check jobs in the latest 20 completed PR runs, 3 skipped, none cancelled, nearest-rank
p90 514 s, median 422 s, maximum 542 s). Job timings for 18 fetched runs of that cohort
(16 executed, 2 skipped) split into two strata by whether the PR diff made the
guardrails job run its fixture self-tests (`CI_SELF_TESTS=changed`). With self-tests
off, CI guardrails ended 104–144 s after the Check job started and the slowest test
partition bound the run. With self-tests on, CI guardrails ended at 460–783 s and was
the last awaited job in four of the five slowest successes, 33–300 s after the slowest
partition (run 42526696384: guardrails +760 s, slowest partition +460 s, Check 767 s).
Parallel job times are compared by their ends, never summed.

Step timings of the watch's three slowest runs (42524629248, 42525119185, 42525236912;
affected selections of 19 and 8 crates, rust-cache hits, 4-vCPU runners) decompose the
non-guardrails path: Check's own checks took 131 s and never bound. The folded build
took 210–293 s (list-build 200–282 s, archive 8–9 s), the repack 12.5 s and the four
serial uploads about 16 s; the slowest partition then spent about 16 s on transfer and
155–165 s in nextest at 95% CPU busy. The guard pool's wall was 451–611 s, and its
longest member in every run was `test-check-dependency-direction.py` at 380–486 s,
ahead of `test-qa-full-sweep.py` at 165–261 s. The fifteenth pass had batched that
self-test from 266 to 63 guard launches; tables added since (the landing, routines,
contracts, experiment and auto_tasks ports) again launched the whole guard once per
row. This pass batches those tables with the same `check_rows` helper — one guard run
per detector in each chain, each row in its own file at the same module depth, the
guard required to report that row's own path — and launches the self-test first in
the guard pool (longest-processing-time-first), instead of after the repository
scans. Rows whose identity is an exact file (the two `task/mod.rs` shim lines, the
config leaves, `tag_instructions.rs`, the exact-line store-read allowances) keep
their own runs and files, and `contracts/mod.rs` rows stay unrenamed because renaming
changes the depth a `super::` chain resolves against. The guard itself is unchanged,
and no row, assertion or check is removed. Two guard mutations (a neutered
super-chain detector and a neutered inherent-call detector) fail the batched tests
that exercise them.

Local evidence: base `d3519c28`, one shared 32-logical-CPU Linux host at load average
39–52, Python 3.14.4, each arm pinned with `taskset -c 0-3` in sequence. A counting
`bash` shim recorded the self-test's guard launches deterministically: **223 → 149
guard runs**. The pinned walls of one run per arm were 647.8 s before and 443.6 s
after; on this oversubscribed host those walls are indicative only, and the launch
count is the robust figure. This host is not a hosted 4-vCPU runner, and setup and
transfer are not measured. The folded build plus the slowest partition still sets a
floor above 400 s on the widest selections in this cohort, so no local figure here
claims the budget is restored.

Operator handoff: after merge, once runs carry this revision, measure the latest 20
completed `pull_request` `ci.yml` runs under the
[hosted CI budget handoff](../DEVELOPMENT.md#hosted-ci-budget-handoff) with
`python3 scripts/ci-p90.py --limit 20 --budget-seconds 400`. Report executed, failed,
skipped, cancelled and missing runs separately, the nearest-rank p90 against 400 s,
which runs executed the self-tests, and the cache, scope and CPU strata. That
measurement is the operator's, not an acceptance gate of this leaf.

A nineteenth pass (ORB-23755) follows the 2026-11-09 watch (ORB-23739: 16 executed PR
Check jobs in the latest 20 completed PR runs, 4 skipped, none cancelled, nearest-rank
p90 723 s, median 482.5 s, maximum 757 s). Job timings of that cohort's 16 executed runs
split again by whether CI guardrails ran the fixture self-tests. With self-tests on (eight
runs), the guard pool's wall equalled `test-check-dependency-direction.py`'s own wall to
the second in every run (335–505 s; `test-qa-full-sweep.py` 184–291 s beside it). In the
four warm-cache runs of that stratum where guardrails ended last (42594632810,
42588330612, 42583560449, 42583069998), Check ended 5–7 s after guardrails (496–571 s),
while the slowest test job ended at 238–481 s. With self-tests off, guardrails took
87–145 s and the test lane bound (392–528 s). Parallel job times are compared by their
ends, never summed.

That self-test's 45 cases each build their own fixture tree and spend nearly all their
time in 158 guard runs of about 1.3 s each, mostly about 70 short Python children per
run. The cases are independent, so the self-test now runs them four at a time on threads,
one `unittest` result per case. It fails unless every loaded case ran, so no case can be
lost or selected away. Named cases or unittest options keep the standard serial runner.
The guard, every case and every assertion are unchanged. With `inherent_calls_into`
neutered, the concurrent and serial runners report the same 85 failures.

Local evidence: base `83a476554`, a shared 32-logical-CPU AMD Ryzen AI MAX+ 395 Linux
host at load average 3–10, Python 3.14.4, arms in sequence with no synthetic load. The
self-test pinned with `taskset -c 0-3 chrt --idle 0` took **221.8 → 68.8 s** wall (user
CPU 167.8 → 166.7 s). `CI_SELF_TESTS=all taskset -c 0-3 scripts/ci-guardrails.sh
--guards` took **249 → 171 s** of guard-pool wall. The dependency self-test took 248.5 →
140.3 s there, and `test-qa-full-sweep.py` (136.5 → 170.8 s, under more contention) is now
the pool's longest member. The selection was the same 74 entries in both arms. This host
is not a hosted 4-vCPU runner, and the test lane, which bound every self-test-off run, is
unchanged, so no local figure here claims the budget is restored.

The recurring `test-quality-audit` creates evidence-backed proposed tasks for critical coverage gaps, weak assertions, duplication, flaky or slow fixtures and regressions against this budget. It searches existing tasks before filing, links each finding to the audit, and records concrete acceptance criteria. The audit leaves a durable report even when no new finding is confirmed; it does not treat a quota of findings or deletions as success.

### Build-runner partition delay (2026-10-31)

The budget-watch cohort measured 16 executed Check jobs in 20 completed PR
runs (four skipped, no cancellations): nearest-rank p90 571 s, median
492.5 s, maximum 732 s. The build job finished last in the three examined
runs below. Its folded compile took 210–443 s; bounded successful logs did
not retain Cargo unit timings, so these observations do not attribute that
variance to a particular crate or compiler phase. The folded build now uses
`--timings` and uploads `test-build-timings` for that investigation.

A confirmed scheduling cost follows publication: partition 2 waited for the
archive coverage proof, workspace doctests and tool setup on the build
runner. It now runs as literal job `Test partition 2`, sharing every shard
step and preparing beside compilation. The build still proves exact-once
coverage of the original archive and runs every workspace doctest. The
required Check job awaits the new runner as well as all existing jobs.

| Hosted run | Compile s | Observed Check s | Partition 2 publication-to-start s | Existing shard publication-to-start s | Projected Check s |
| --- | ---: | ---: | ---: | ---: | ---: |
| 41183374048 | 263 | 565 | 31.067 | 18.599 | 552.531 |
| 41183989799 | 443 | 732 | 30.347 | 22.126 | 723.779 |
| 41183528773 | 210 | 395 | 33.579 | 15.128 | 376.549 |

The after column is a scheduling counterfactual: keep the observed partition
2 runtime, every other job end and Check's final polling/cleanup tail; replace
partition 2's serial publication-to-command-start delay with shard 1's
observed polling/download/repack delay. The task's `ci-budget/replay.py` and
`ci-budget/timing-inputs.json` artifacts reproduce the arithmetic without
load generation. They project savings of 8.2–18.5 s, not a restored p90.
One additional runner and archive download cost runner time, and queueing or
a slower replacement runner can erase the saving. Compilation remains a
large part of the path; do not call the 400 s rule satisfied from this change
or these three runs. No test was retired, skipped, retried or weakened, and
all five count partitions, digest verification, namespace preparation and
the shard-1 sandbox gate retain their existing guarantees.

After merge the operator owns a fresh hosted measurement; see
[hosted CI measurement](../DEVELOPMENT.md#hosted-ci-budget-handoff). A rolling
sample containing old workflow revisions cannot establish the candidate's
p90. Report exact run/head identities, executed/skipped/cancelled/missing
counts, failures, queue gaps and the nearest-rank p90 against **400 s**.

### Awaited partition and QA contention reduction (2026-11-10)

ORB-23890 revalidates the historical profile before edits at source head
`ed219d0bda9100437b5f448fe559dfe0eea3b9a4`; the first profile was recorded
at `45beaebdcfa666b0c1a297601c47372fe9afa390`. The ORB-23889 audit's
22 executed PR Checks had nearest-rank
p90 **580 s**, with 18 over 400 s. Five additional run views retain complete
job durations, the failed run and self-test selection evidence:

| Run | Check s | Test build s | CI guardrails s | Self-tests | Last partition s |
| --- | ---: | ---: | ---: | --- | ---: |
| 42757620515 | 696 | 380 | 469 | executed | 690 (partition 2) |
| 42761390547 | 416 (failed) | 277 | 132 | skipped | 454 (partition 7) |
| 42761647024 | 580 | 401 | 136 | skipped | 594 (shard 2/3) |
| 42776520260 | 544 | 335 | 115 | skipped | 537 (shard 2/3) |
| 42777960025 | 493 | 331 | 111 | skipped | 487 (shard 2/3) |

Each duration is `completed_at - started_at`, including setup and post
steps; the parallel durations are never summed. The failed Check returned
before every partition finished; its last partition is retained rather than
filtered out. Run creation-to-Check-start waits in the table's run order are
20, 24, 43, 25 and 18 s, reported separately and never subtracted. The
individual awaited jobs' queue waits remain unknown because the views omit
their enqueue timestamps. The pre-edit profile and raw run views are task
artifacts under `ci-profile/` and `ci-profile-v2/`.

In run 42757620515 the folded build took 300 s, archive creation 9 s and
transfer repacking 12.619 s. Partition 2 then ran 501 tests in 143.662 s at
94.8% CPU busy: 859.847 summed testcase seconds, with a 58.806 s longest
case. This is a CPU-bound partition tail, so the workflow now uses twelve
count partitions instead of eight, all from the same build and archive.
The build still proves exact-once coverage against all twelve partitions;
Check awaits each literal partition job. This does not change compilation,
which remains a substantial floor, or the complete-job measurement.

That run's guard pool took 394 s, with the QA fixture taking 385.808 s beside
the dependency-direction suite's 321.316 s. The QA inventory and selected
self-test now run as `CI QA guardrails` on their own runner, with the same
changed-input selection as the core guardrails. Every repository guard and
fixture still runs once across the two routes; local guard modes retain both.
The dependency suite's concurrent runner and its failure reporting are intact.

The shared Bubblewrap preparation step executes
`scripts/ci-probe-bubblewrap.sh` from the workflow revision. Its existing
probes, deadlines, diagnostics and failure behavior stay intact. Extracting
that repeated shell body keeps the workflow small even when YAML aliases are
expanded by prerequisite readers; the two public landing regressions exercise
that representation without raising the source bound.

The allocation cost is five additional standard Ubuntu runners on every full
or affected run: four partitions plus one QA runner. The four partitions each
download the existing four-part archive (about 2.43 GB extra transfer for the
606.8 MB sample). The QA runner performs no workspace build and downloads no
archive. Runner setup, queue pressure, transfer variance and the unchanged
compilation floor can limit or erase the gain. No larger-runner provisioning
or price is assumed.

Candidate hosted timings are **unavailable before publication**. ORB-23922
must resolve this delivery's exact published PR/head and compare its complete
Check, Test build, both guardrails jobs and last partition durations against
these artifacts, including failures and queue gaps. The post-landing latest
20-run nearest-rank p90 check remains operator verification on ORB-23888.
Neither the scheduling change nor local checks establish a measured hosted
speedup or compliance with the **400 s** budget.

## Why

On 2026-10-03, unit tests under `crates/*/src/**` were about 295k lines and 7,675 test functions, more lines than the production code they covered. Crate-root integration tests were 37k lines. Most unit tests pinned internals, so a refactor that kept behaviour unchanged still broke dozens of them, and each break cost an agent tokens to repair a test rather than find a bug. A review of all of them kept 180 (2.3%).

The default is not "no unit tests". The aim is to put each guarantee at the cheapest layer that still proves it:

- A boundary test survives refactors.
- A unit test pins how the code is written today.
- An e2e test is the most expensive and the slowest to localise a failure, so it covers critical flows only.

## The layers

| Layer | Where | Use it for |
|---|---|---|
| **Integration** (default) | `crates/<crate>/tests/` | Behaviour through the crate's public API or built binary: tool dispatch, runtime operations, store transactions, HTTP routes, CLI commands. Hostile state is built directly in the fixture. |
| **Golden** | Beside the integration test that renders it (e.g. `crates/orbit-cli/tests/output_goldens/`); regenerated with `make goldens UPDATE=1` | Output contracts: CLI help and `--json`, MCP tool schemas, effective-config projections, sandbox profile text, parser output over captured real-world input (CI logs, provider streams). |
| **E2E** | `crates/orbit-cli/tests/` against the built `orbit` binary | A small set of critical flows end to end: ship and land, the drain, gates, update. |
| **Unit** (exception) | `<module>/tests/<file>.rs` per [test_layout.md](test_layout.md) | Only what meets the admission criteria below. |

## Unit-test admission criteria

A unit test is allowed only when at least one of these holds and a boundary test cannot prove the same thing economically:

1. **Combinatorial pure logic.** Examples: a state-machine transition table, a parser's edge cases, policy or selector matching, scheduler or backoff arithmetic, cycle detection. Write one table-driven test per invariant, not one test per case.
2. **Deterministic interleaving or fault injection.** The test needs a test seam to force a race, a crash mid-write, a TOCTOU swap or a lost reply.
3. **A security or safety invariant the boundary cannot reach cheaply.** Examples: path or symlink confinement, secret masking across chunk boundaries, privilege propagation, fail-closed validation of a crafted input.
4. **Platform-kernel behaviour.** A real `sandbox-exec` or bwrap result, or process-group reaping, where no integration test runs on that platform.

The test's name or assertion message says which invariant it guards. A test that guards a specific incident cites it.

## Not admitted as unit tests

These are not admitted:

- serde round-trips, defaults, builders, getters and `Display`, unless the test guards a persisted on-disk format that has a migration
- mock-heavy wiring tests that assert which collaborator was called
- argument parsing and output formatting that a golden already covers
- a second test of an invariant that already has one
- behaviour already exercised by an integration test

Regression tests follow STD-04 §R1: drive the entry point production callers use. A bug found at a boundary gets its regression test at that boundary.

## Growth ratchet

`scripts/unit-test-inventory.py` inventories the unit-test functions under each `crates/<crate>/src/**`, inline or in a sibling `tests/` directory. [`scripts/unit-test-baseline.json`](../../scripts/unit-test-baseline.json) records sorted admitted identities as `<path relative to src>::<function name>` per crate (schema version 2). Repeated names in separate inline modules in one file retain their multiplicity. `make ci-fast` fails when a present test is not named in the baseline, even when the crate's total count stays the same. The change that adds or renames unit tests must update the baseline too, so each admission appears as a reviewed diff. Concurrent admissions then either merge with both identities recorded or conflict textually and require reconciliation; identical count bumps can no longer silently lose an admission.

Removing tests still passes with stale baseline entries; those entries do not admit different tests in their place. Run `scripts/unit-test-inventory.py` for the current counts and identities, and `scripts/unit-test-inventory.py --write-baseline scripts/unit-test-baseline.json` to regenerate the baseline, including removing retired identities. The inventory scans test attributes, including the raw identifier spelling `#[r#test]`, without evaluating `cfg` or expanding macros, as in the original retirement measure. A test attribute counts wherever it sits on a masked source line, so `#[cfg(test)]`, `#[allow(dead_code)]`, or another attribute before `#[test]`, `#[r#test]`, or `#[path::test]` does not hide the test. `#[cfg(test)]` by itself is not a test. The baseline stays the reviewed admission list: a scan that newly sees a real test is a missing admission, and the baseline is not rewritten only to conceal it. Rust strings, character literals and comments (including nested block comments) are excluded, so source fixture data does not consume admissions.

Admitted unit tests:

- `orbit-engine`'s `executor/automation/vcs/operations/tests/push_retry.rs` cases
  `timed_out_evidence_push_lands_when_the_ref_names_the_commit`,
  `timed_out_evidence_push_fails_when_the_ref_is_absent`,
  `timed_out_evidence_push_fails_when_the_ref_names_another_sha`,
  `timed_out_evidence_push_fails_when_confirmation_is_unreadable`,
  `permanent_push_refusal_fails_without_confirmation_success` and
  `invalid_evidence_ref_and_head_are_refused_before_push` are a criterion 2
  regression for ORB-23936 (F2026-11-230). They drive `push.evidence_ref` and
  `push.candidate_ref` through the private VCS operation boundary. A substitute
  receive-pack applies the update, or leaves the ref alone, and then stalls;
  the supervision clock expires the production deadline once that stall is in
  place. The landed case returns the pushed commit. An absent ref, a ref at
  another sha, and an unreadable confirmation each fail once and do not push
  again. A non-fast-forward refusal fails on its own diagnostic while the ref
  already names the commit. Invalid tree, evidence-ref and head input are
  refused before any push. A crate-root integration test cannot install the
  supervision clock or the receive-pack stall without waiting out the 60s
  production timeout.

- `orbit-core`'s
  `application/agents/tests/api.rs::reviewer_interval_resamples_review_units_not_task_groups`
  is a criterion 1 regression for ORB-23843. It drives the public
  `agent_scorecard` API with synthetic facts to prove two reviewer settlements
  on one task resample as two row units, matching the same settlements on two
  tasks, and that the fixed-seed interval repeats. `AgentFacts` is opaque to
  crate-root integration callers; constructing this exact duplicate-task
  observation there would require populating mutable workspace state and
  scanning it, rather than exercising the scorecard boundary directly.

- `orbit-core`'s `application/task/tests/add.rs` cases
  `filing_refuses_an_unliftable_disallowed_required_tool`,
  `filing_accepts_an_allowed_required_tool` and
  `filing_accepts_a_disallowed_read_only_tool_a_grant_can_lift`, and
  `application/task/pilot/tests/prepare.rs::unliftable_required_tool_is_a_blocking_pilot_finding`
  (criteria 2 and 3, ORB-23342 regressing ORB-23331). They drive public
  `add_task` and `prepare_task_pilot`. Filing refuses a disallowed mutating
  required tool and names the activity and the operator remedy, accepts an
  allowed tool, and accepts an exact denial only while a test-only
  classification override marks that tool read/repeat-safe. The pilot finding
  holds promotion for a task stored before the catalog is seeded, which
  `add_task` would now refuse. No shipped tool is classified read/repeat-safe,
  and a crate-root integration test compiles without `cfg(test)`, so it cannot
  install that override. The mutating refusal is the observed admission
  failure: a disallowed mutating tool was accepted at filing and blocked only
  at dispatch.

- `orbit-config`'s `resolved/tests/execution_env.rs::inherit_mode_omits_privilege_bearing_orbit_names`
  is a criterion 3 regression for ORB-23049. It drives the public
  `ExecutionEnvPolicy::agent_subprocess_env` with the internal inherit seam
  set, for both a captured parent snapshot and the live process environment,
  and proves privilege-bearing `ORBIT_*` names are omitted while an envelope
  name and ordinary ambient names are still forwarded. No config file can
  enable inherit, so a crate-root integration test cannot reach this branch.
  The command-exec allowlist is covered separately at the `orbit.command.exec`
  boundary.

- `orbit-runtime`'s `task/tests/pr_forge_rerun.rs` cases
  `rerun_waits_for_running_workflow_before_requesting_failed_jobs`,
  `rerun_refuses_after_bounded_wait_and_names_running_workflow` and
  `rerun_still_refuses_without_failed_or_running_workflows_on_head` exercise the
  public `GhTaskPrForge::rerun_failed_checks` boundary for ORB-22619 / ORB-22211
  (criterion 2). Thread-local gh replies and a virtual clock force the observed
  failed-job / running-workflow interleaving and expiry without real waits,
  credentials or environment mutation. The real recovery integration fixture
  substitutes the entire forge and cannot exercise this race. The negative
  case preserves the immediate-refusal contract required by this repair.

- `orbit-exec`'s `build_sandbox/tests/script_macos_inputs.rs::an_input_growing_during_preparation_cannot_escape_the_scratch_cap`
  is a criterion 2 regression for ORB-21492. It calls the public
  `run_script_sandboxed` boundary while a private, test-only hook appends bytes
  to the open source after the first copied chunk. It proves that growth
  after metadata admission cannot overrun the scratch cap or start a script,
  without timing races, disk pressure or a shipped test-hook API. The separate
  integration regression covers known oversized sets and expired preparation.

- `orbit-cli`'s `tests/plugin_callback_surface.rs::recover_claim_commands_parse_with_each_phase_used_by_settlement_refusals`
  verifies the required recover-claim options parse with each phase used by
  settlement refusal guidance. The runtime producer coverage lives in
  `orbit-core`; this focused parser check exercises the CLI grammar without
  needing owner claim state or dispatching the recovery command.

- `orbit-core`'s `dispatch/claimed/tests/owner/pagination.rs::claimed_task_detail_pages_preserve_broker_scope_and_owner_validation`
  reproduces F2026-10-599 through the public broker socket and real owner task-tool
  dispatch, in an isolated child. It verifies bounded pages and owner validation
  while keeping internal-field, delivery and artifact-call refusals, and refuses
  a page of a task outside the claimed workspace.
  It lives with the existing claimed-worker broker fixture so it can provide a
  disposable owner transport without SSH or a native confined worker.

- `orbit-core`'s `dispatch/claimed/tests/owner/workspace_reads.rs::a_remote_owners_claimed_worker_reads_any_task_of_its_claimed_workspace`
  and `a_claimed_worker_on_its_owners_machine_gets_the_same_workspace_reads_and_refusals`
  are criterion 3 regressions for ORB-19270 (a claimed worker could list but not
  open related tasks). Over the bridge, the broker socket and the real owner
  task tools in isolated children, they prove a remote-owner and a local-owner
  claimed worker read any task of the claimed workspace, and that a task outside
  it, a caller-supplied workspace or view, an internal field and a put on another
  task stay refused. They replace `claimed_owner.rs::a_local_claimed_task_read_stays_on_the_claimed_task`,
  whose expectation was the old scope.

- `orbit-core`'s `pilot/tests/drain_promotion.rs::prerequisite_lost_before_atomic_write_holds_both_authorities_and_replay`
  and `completed_prerequisite_lost_before_approval_still_holds` are criterion 2
  regressions for ORB-17762. They drive the public runtime's pilot apply action
  in isolated children. The first uses the graph-discovery hook to move or
  delete a prerequisite before any task lock, proving drain and CI-sweep
  warnings holds, stored warning readout, and safe same-operation replay even
  when the prerequisite becomes edge-eligible again. The second deletes a
  previously completed prerequisite at the approval hook and proves no
  approval occurs. External integration callers cannot choose either
  interleaving without the private test-only hooks. Existing public-boundary
  scenarios retain normal dependency creation, completion and replay coverage;
  the existing covered-edge/done promotion check remains unchanged.

- `crates/orbit-core/src/application/automation/tests/source.rs::replay_moves_a_retained_exclusion_onto_its_canonical_commit` (criterion 3, fail-closed replay identity). A real Git divergence is replayed through the source adapter, using a provider lookup that panics. The test proves that a retained certificate exclusion is restated from its own record, so no fresh provider proof is needed. The store then accepts it onto its canonical commit with the certificate intact. The public `auto-task recover --replay-history` path cannot swap in that lookup without a live GitHub provider. The evaluator and store outcomes are covered at the public boundary in `crates/orbit-automation/tests/scheduling/delivery_recovery.rs`.

- `crates/orbit-core/src/application/job/tests/pipeline/admission.rs::a_redrawn_keyed_resubmission_is_refused_without_rewriting_the_task_crew` (criterion 3, fail-closed task write). A keyed automation resubmission of a delivery is refused as a changed key, because admission stamps fresh timestamps into the stored input. The test proves the refused draw writes nothing to its task. A redrawn keyed submission has no public boundary: `submit_automation_pipeline_run` is reached only from automation claims, whose setup dwarfs the invariant. It fails on the base, where the crew write-back landed before the refusal (ORB-19485).

- `crates/orbit-core/src/application/agents/tests/rank.rs` cases `rank_pools_models_bands_by_bootstrap_orders_by_cost_and_drops_unmeasured_weights`, `rank_expectations_match_the_implementer_scorecard_on_shared_terms` and `calibrated_season_stores_window_and_reference_size_and_drives_the_board` (criterion 1, ranking arithmetic; ORB-23302 acceptance criteria). They drive the public `agent_rank` and `agent_rank_calibration` over fixture scan facts: crew pooling, the settle lag, eligibility, shrinkage, bootstrap bands, cost order within a band, unmeasured measures leaving the weights, parity of expectations with the public implementer scorecard, and season scales with their window and `n_ref`. Scan facts are opaque outside the crate, and a crate-root workspace fixture would need hundreds of reviewed, regressed and priced job runs for one board.

- `crates/orbit-core/src/application/tests/vision_probe.rs::successful_run_without_the_code_is_image_not_seen_and_a_returned_code_is_seen` (criterion 1, probe-report mapping). A finished image-input probe whose canary check did not see the code is reported `image_not_seen` for that crew, and a returned code is reported seen, including when the probe run itself succeeded. The same table keeps a failed delivery that still carries the marker as `image_not_seen`, and a failure after a seen check as a failure. `classify` is `pub(super)`, so the engine dispatch test cannot call it. That dispatch test proves the probe step stays successful and a delivery step still fails; it cannot assemble a successful pipeline wait beside a chosen check. The old mapping treated every successful run as seen and ignored the check (ORB-21365).

- `crates/orbit-core/src/application/pull_drain/tests/drain.rs::claimed_leaf_settlement_carries_its_review_ledger` and `claimed_review_import_keeps_an_operator_decision` (criterion 3, durable review-state safety). The first exercises the production settlement path and proves an exhausted follower ledger reaches the owner under the claim lineage; the public escalation-and-requeue scenario starts from a prepared settlement and cannot prove that producer path. The second replays exhausted claim evidence after an operator reset and proves it cannot erase the reset decision; the public command scenario covers the escalation's requeue, not a later stale receipt.

- `orbit-common/src/process/tests/ancestry.rs::native_tree_cpu_counts_reaped_descendants_once`
  is a criterion 4 native macOS regression check for reaped-descendant CPU
  accounting. It drives the public `process_tree` and `TreeCpuAccount` APIs
  in an isolated child and compares cumulative CPU with
  `getrusage(RUSAGE_CHILDREN)`, covering both short-lived grandchildren and
  a long-lived child counted once after reaping. It stays beside the existing
  native calibration, outside recurring gates (`#[ignore]`); fixture-only
  accounting cannot verify the kernel's reaped-child counters and Mach timebase.
  The completed implementation measured about 3.5 seconds of workload and
  demonstrated failure with the previous own-only accounting.

- `orbit-registry/src/tests/workspace_registry.rs` retains two criterion 2
  harnesses. `injected_migration_write_failure_preserves_readable_legacy_registry`
  injects a migration writer error and proves the original bytes remain readable.
  `migrating_read_rechecks_under_lock_and_preserves_interleaved_registration`
  completes a locked registration between the legacy read and the migration
  recheck. The public loader supplies the real writer and cannot choose that
  fault or the observation order. The other nine identity, publication and
  registry cases use exported APIs in the crate-root `tests/registry/main.rs`
  binary, preserving their original behavior and platform guards.

- `crates/orbit-core/src/application/tests/executor.rs::seed_shipped_muse_is_unrestricted_with_platform_sandbox`
  is a criterion 1 table-driven platform seed check. It drives the production
  shipped-executor seeding path for both Linux and macOS targets, proving that
  Muse remains unrestricted while the declared sandbox marker translates to
  each target's backend. A public workspace-init boundary can only seed for
  the host running the test, so it cannot cover both target translations in a
  single CI run.

- `orbit-exec` retains six corrected security-seam admissions from the
  [applied audit](../dev/reviews/unit-test-audit-orbit-exec-applied-2026-10-12.md).
  `build_sandbox/tests/supervise.rs` checks host process-group reaping before
  Bubblewrap changes PID identity and isolates the fetch TCP ruleset from
  filesystem/namespace restrictions (criterion 3). The credential alias test
  injects an arbitrary bind destination; the two `host_alias` tests inject
  mount tables and confirm real device/inode identity without privileged
  mounts (criteria 2 and 3). The bundled-wrapper test injects a disposable
  candidate path that the fixed-path public launcher cannot accept (criterion
  3). The outside-symlink measurement moves through the public phase with a
  zero cap; the missing-path control joins the existing unreadable measurement
  fixture. Exported build-log, compiler, grant and login-shell cases live in the
  crate-root integration suites, and the native macOS PTY parent and child
  stay together there.

- `crates/orbit-common/src/process/supervisor/tests/wait.rs`: `a_stalled_caller_clock_does_not_apply_the_wall_clock_deadline` and `a_jumped_caller_clock_stops_at_the_deadline_without_the_wall_wait` (criterion 2, deterministic clock). An injected clock that stays still must let a child outlive a wall-clock timeout, and one that jumps past the timeout must stop without waiting out that wall time. Automation source measures its budget on this clock; the existing deadline test covers `clock: None`. `redirected_output_stops_when_the_periodic_check_crosses_its_limit` (criterion 4) writes to a caller-owned file, so no capture sink sees the bytes, and asserts the periodic check stops the run with `ProcessStopCause::Periodic` before the deadline.

- `crates/orbit-common/src/process/tests/bounded.rs::bounded_supervision_errors_kill_the_group_and_reap_the_leader` (criterion 2, fault injection). A per-thread seam injects preparation and wait errors, and transfers pipe-setup/read faults to the actual output worker, through all four public bounded-runner APIs. Before the injected fault, it places an independently waitable peer in the child's group. The test verifies the fault executes on the intended thread, returns before the deadline, reaps the leader and kills the peer; descriptor exhaustion or global wait-state corruption would otherwise be needed to reach those errors. The crate-root `process_group_liveness` tests cover owned-handle drop, unwinding, explicit cleanup, TERM escalation and signal guards without injection.

- `crates/orbit-common/src/process/tests/identity.rs::partial_linux_group_scans_cannot_certify_death_and_vanished_pids_do_not_delay_cleanup` (criterion 2, I/O fault injection). The production scan accepts directory entries and a stat reader so the table can force entry errors, unreadable and malformed stat records, and ENOENT between enumeration and reading. It proves an observed zombie cannot certify group death when a potentially live PID cannot be inspected, preserving cleanup's SIGKILL escalation, while vanished PIDs do not prevent zombie-only success. Live-member and unavailable-directory controls cover conclusive activity and the existing signal fallback. The public probe cannot force these partial `/proc` failures economically without host permission changes or nondeterministic races; the crate-root process-group test covers a real Linux zombie through that public probe.

- The [launchd enable tests](../../crates/orbit-clock/src/tests/enable.rs) (criterion 2,
  fault injection) drives the enable entry point with launchd selected and failed
  program/state probes. Version outcomes use the inspection probe seam rather
  than executing a fixture binary: healthy, timeout, empty output and unreachable
  outcomes are deterministic under host contention. It checks rewrite and reload for unhealthy loaded agents,
  error propagation when loading fails, and preservation of healthy and paused
  agents. The public API selects the native host platform, so an integration test
  on Linux cannot force these launchd states.

- The systemd activation matrices in
  `crates/orbit-clock/src/tests/enable.rs`
  (`systemd_enable_accepts_healthy_in_flight_and_finite_timers` and
  `systemd_enable_rejects_missing_inactive_failed_or_unhealthy_sweeps_without_next_trigger`)
  and `tests/install.rs`
  (`systemd_install_accepts_healthy_in_flight_and_finite_timers` and
  `systemd_install_rejects_missing_inactive_failed_or_unhealthy_sweeps_without_next_trigger`)
  meet criterion 2 (fault injection). The manager-runner seam fixes the
  observation immediately after activation: a loaded active timer has no next
  trigger while a healthy oneshot is still running, or its service is missing,
  inactive, failed, unhealthy, or lacks safe recovery settings. Finite-trigger
  rows include healthy, failed, and unreadable-service controls: service health
  may grant the in-flight exception but cannot revoke a finite trigger. The
  public enable/install APIs use the native manager
  and cannot force this transient or these failures economically without
  changing the host's user service. These matrices guard the premature
  activation refusal observed on 2026-10-13 and exercise the activation entry
  points, including success reports and execution errors, without timing loops.

- `crates/orbit-core/src/application/task/tests/transitions/reject.rs::delete_rechecks_a_status_changed_after_its_guard_and_preserves_force` (criterion 2, deterministic interleaving). The existing transition-read seam commits an in-progress status after deletion's initial proposed/rejected check, before the task write lock. The isolated fixture asserts refusal leaves the task present, with forced deletion and unchanged eligible statuses as controls. Crate-root integration tests cannot reach the `cfg(test)` seam to choose this ordering reliably.

- `crates/orbit-common/src/process/tests/bounded.rs`: `argv_probe_recovers_when_a_live_child_outgrows_the_queried_buffer` (criteria 2 and 4, native fault injection). A pipe handshake grows a live macOS child's argv between the size query and read. The stale buffer yields an unreadable record, which must not prove exit or authorize cleanup; the probe grows its buffer and recovers the recorded identity. Darwin can return success with a truncated `KERN_PROCARGS2` record, so handling only `ENOMEM` is insufficient. The public bounded runner cannot force this observer interleaving.

- `crates/orbit-common/src/process/tests/bounded.rs`: `leak_assertion_retries_unreadable_argv_without_accepting_it_as_reaped` (criterion 2, fault injection). An injected argv probe and clock force a read failure followed by readable live argv, an unreadable probe throughout the grace budget, or a confirmed exit. It checks the leak assertion's panic classification and full retry budget without racing macOS sysctl or generating host load. The public bounded runner cannot force these observer failures; the native blocked-writer test covers pid-only cleanup.

- `crates/orbit-common/src/process/tests/bounded.rs`: `bounded_run_returns_when_a_descendant_holds_either_pipe`, `bounded_run_enforces_deadline_while_a_stream_stays_readable` and `bounded_run_bounds_post_exit_drain_while_a_detached_writer_continues` (criterion 4, platform-kernel behaviour). A descendant keeps one output pipe open after the leader exits; sixteen `cat /dev/zero` writers keep a stream readable past the deadline; `setsid` writers outside the owned group keep filling a pipe during the post-exit drain. Each case's leak check and watchdog are this file's argv-identity harness (`assert_reaped`, `FloodCleanup`). It reads Linux `/proc` or macOS `KERN_PROCARGS2` and signals a pid only when its argv carries the recorded marker, and the admitted cleanup and leak-assertion tests above prove that harness safe. A crate-root copy would duplicate the native harness without those tests, so the public runner is not the cheaper boundary for these three.

- `orbit-engine`'s `branch_lookup_retries_only_supervisor_timeouts_with_a_bounded_budget` (criterion 2, fault injection). A stub Git stalls until the supervisor deadline, then succeeds, exhausts the budget, or completes with a refusal. It drives the branch lookup used by PR preparation and verifies the timeout class, elapsed time, bounded backoff and attempt count. The private Git budget guard scales the deadline; `pr_prepare` does not expose that guard through its public action boundary.

- `crates/orbit-engine/src/activity_job/cli_runner/supervisor/tests/stall.rs::silent_sleep_stalls_while_cpu_and_output_keep_the_run_alive` (criterion 4, process-tree sampling and process-group cleanup) runs a silent sleeper, a CPU-burning descendant, a periodic printer and a disabled-watch sleeper through the real supervisor. The short window makes the kernel process-tree samples and group termination observable in one bounded fixture; a dispatcher integration fixture would duplicate the real child process cases and the existing supervisor coverage.

- `crates/orbit-engine/src/activity_job/cli_runner/orchestrator/completion/tests/failure.rs::a_stalled_provider_fails_transient_without_a_wall_clock_timeout` (criterion 1, terminal-outcome mapping) checks the stalled and wall-clock-timeout rows through completion projection. It preserves the distinct `stalled` and `timed_out` outputs, transient marker, step error code and claim retry class without running another provider process; the supervisor test above covers the actual stall detection.

The [orbit-config audit delivery](../dev/reviews/unit-test-audit-orbit-config-delivery.md) maps the retired and relocated identities to their public boundaries. Config loading, compatibility warnings, environment projection, plugin toggles and store admission live in the existing crate-root integration binaries. The malformed-pool grammar table remains admitted under criterion 1; ordinary pool loading uses the public loader.

- `crates/orbit-config/src/tests/seed.rs`: `every_family_combination_seeds_valid_defaults_pools_and_enabled_flags` (criterion 1, combinatorial seed rendering) checks all detected-family subsets in memory: defaults name enabled crews, pools parse and reference enabled crews, and each crew's enabled flag follows its provider availability. It avoids filesystem setup for every subset. `rendering_refuses_a_pool_entry_the_seed_does_not_write_enabled` (criterion 2, fault injection) removes or disables a dynamically selected pool member in the private renderer's crew map; the public seeder always constructs that map itself. Neither test fixes crew choices, models or weights as policy.

- `crates/orbit-web/src/log_format/tests/scan.rs`: `post_window_errors_do_not_consume_the_match_budget_or_advance_coverage` (criterion 1, combinatorial scan budgets and coverage). A table drives the existing segmented-reader seam with complete, capped and retention-limited windows and excess newer records. The HTTP boundary fixes the process scan budget at 50,000; this seam proves those cases with a handful of rows, while the HTTP month test covers endpoint wiring and agreement with stderr.

- `crates/orbit-web/src/lease/tests/intake.rs::ambiguous_workspace_id_name_selector_is_refused_without_touching_either_workspace` (criterion 3, workspace-selection safety at the Unix socket boundary). A real registry-backed dashboard gives two workspaces the `ws_alpha` id/name collision, and the test sends request, renew and release frames through the server and public client. Unique name and id requests must grant leases in their own runtimes, while ambiguous operations are refused and both leases keep their original expiry and granted state. The server's test listener and registry-backed state builder are crate-private, so the crate-root integration binary cannot assemble the same fixture without widening the production API.

- `crates/orbit-web/src/lease/tests/intake.rs`: `oversized_read_responses_are_bounded_errors_and_other_connections_still_work` and `token_grant_answers_fit_the_frame_without_compensating_release` (criterion 3, bounded protocol frames and grant safety, ORB-18638, ORB-18776). Both exercise bounded responses through the real Unix socket and public intake client. The read table covers list, show and check, continued framing on the same connection, and small requests on another connection; the grant case verifies a fitting overlap prefix leaves a real active lease and an answer that cannot fit is refused before any lease or notice is written. The test listener and dashboard state are crate-private, so a crate-root integration fixture would require widening production visibility.

- The four [worker log sibling tests](../../crates/orbit-core/src/application/job/pipeline/worker/tests/log.rs) — `appends_after_the_size_snapshot_are_excluded_from_the_bounded_read`, `empty_short_and_exact_limit_logs_keep_their_expected_content`, `long_logs_report_the_snapshot_window_and_return_only_its_tail`, and `shrink_after_the_size_snapshot_reports_only_bytes_still_available` (criterion 2, deterministic interleaving). A captured size is passed through the same bounded reader used by the audit and supervisor callers; the append case proves bytes written after that snapshot cannot exceed the 16 KiB window, while controls cover empty, short, exact-limit, long and shrinking files. The callers do not lock out the append-oriented worker, and an integration test cannot force the metadata/read order reproducibly [ORB-15610].
- `crates/orbit-core/src/application/job/pipeline/worker/tests/supervisor.rs`: `store_read_faults_retain_supervision_and_reap_in_a_live_parent` (criterion 2, fault injection). A SQLite store wrapper injects unreadable and missing startup records and a post-exit read fault while a socket rendezvous bounds a real child. The isolated parent asserts fresh-state settlement and OS reaping before it exits; the public runtime cannot inject these observer reads.
- The [admissions-stop regressions](../../crates/orbit-core/src/application/job/run/tests/admissions_stop.rs) and `crates/orbit-core/src/application/job/run/tests/drain_cancel.rs`: initialization races (criterion 2, deterministic interleaving). One-shot seams after `NoState` force a worker's first checkpoint and child dispatch, or terminalization with and without that checkpoint, before control initialization. They verify preserved progress and lineage, a merged control for a live drain, and no new effective control for a terminal drain. Crate-root integration tests cannot force this transaction ordering [ORB-15611].
- `crates/orbit-cli/src/entry/tests/output_args.rs`: `every_assembled_leaf_accepts_json_and_lists_domain_input_exclusions` (criterion 1). It walks the assembled command tree, including hidden and plugin-derived leaves that help goldens cannot enumerate, and asserts that every leaf accepts `--json`. The only exclusion is a plugin tool input named `json`.
- `crates/orbit-cli/src/entry/tests/discover.rs`: `decision_submit_cli_preserves_signed_raw_message` (criteria 1 and 3, parser edge case and signed-content integrity). The observed submission defect (ORB-18396) trimmed the signed final body space and CRLF. This test feeds the same independently signed fixture as the governed runtime regression through the complete production argv parser and compares the forwarded tool input byte-for-byte. The binary-only CLI has no parser library for crate-root integration tests; a subprocess success fixture would need control of the host's DNS. Help goldens cannot observe input bytes. This test changes no state and calls no private projection helper.
- `crates/orbit-cmd/src/doctor/tests/worker_token.rs`: `clock_file_is_ready_only_when_it_holds_a_credential_the_workspace_admits` (criterion 1). It is a table over every pairing of clock-file credential names with the workspace's effective pass, asserting that the clock is ready only when an admitted credential is held, and that a held credential the pass omits is named and warned. The boundary needs an installed launchd or systemd clock to reach this classification. `absent_empty_refused_and_mismatched_clock_files_stay_distinct` (criterion 1) pins that absent, empty, refused and mismatched files stay distinct states.
- Orbit-core's missing-directory and relative-path disk cases now run through the exported native host-resource probe in `tests/runtime/admission_audit/disk_probe.rs`. The private platform tests retain the injected CPU counter/time/wait matrices below.

- `crates/orbit-runtime/src/host_resource/tests/platform.rs`: `cpu_window_reuses_advancing_or_repeated_ticks_without_refreshing_the_baseline`, `cpu_window_missing_stale_future_or_identical_baselines_take_a_real_window`, `cpu_window_native_failures_clock_reversal_and_stalls_remain_unknown`, and `cpu_window_handles_tick_wrap_and_the_freshness_boundary` (criteria 1 and 2). Injected counters, timestamps and waits cover XNU cache plateaus, expiry endpoints, failed reads, clock jumps and counter wrap deterministically without generating host load or waiting for a real wrap. The macOS CLI integration test covers actual cross-process history reuse and cold-start recovery.
- `crates/orbit-runtime/src/host_resource/profile/tests/cache.rs`: `no_diff_tag_edit_reclassifies_a_cached_template` (criterion 1, state transitions of the profile cache's hit path). Freshness, bounds and crew models stay fixed while a template's `no-diff-expected` tag is added and then removed. An untouched cache still returns the same profiles, and the next pass after a tag edit classifies the template under its new base class, with no sampled run finishing in between. It fails on the base, where a hit returned the old class [ORB-19555]. The composed path is guarded at the public boundary by `no_diff_tag_edit_reclassifies_a_template_on_the_next_read` in [`resource_profiles.rs`](../../crates/orbit-core/tests/runtime/resource_profiles.rs), which edits the tag through `update_task_with_identity`; this unit case stays the cheaper guard of the cache's hit transition.
- `crates/orbit-store/src/driver/sqlite/task_registry/tests/mod.rs`: `fresh_open_through_symlinked_root_matches_reopen_by_canonical_root` (criterion 3). It guards symlink confinement of the registry root: a fresh open through a symlinked root and a later open of the canonical root must key one `workspaces_dir`, because the repair gate keys its attempt budget on that path. `workspaces_dir` is crate-private, so no public-surface test can observe the split.
- `crates/orbit-store/src/repository/task/coordination/tests/admission.rs`: `a_grant_never_takes_a_footprint_the_owner_wave_admitted_before_its_section` and `an_owner_wave_after_a_grant_refuses_the_claimed_footprint` (criterion 2, deterministic interleaving). The first admits an owner drain wave through the crate-private `after_selection` seam, between a pull grant's selection and its exclusive section, and asserts the grant neither takes the admitted task nor one overlapping its footprint. The second commits the grant first and asserts the wave's section refuses the claimed footprint even though the wave's snapshot predates the claim. At the runtime boundary the claim's task is already `in-progress` before any later drain snapshot, and nothing can land a grant between a wave's snapshot and its section, so neither ordering is reachable there.
- `crates/orbit-store/src/repository/task/coordination/tests/claim_section.rs`: `a_claim_mutation_section_decodes_its_own_claim_and_syncs_only_its_commit` and `a_slow_claim_apply_holds_its_bundle_and_not_its_partition` (criterion 2, fault injection). The first counts, through the crate-private section probe, the claim-lifecycle rows decoded and the durability syncs requested inside a bind's and a budgeted release's exclusive section among other bound claims, asserts both stay under the pre-change baseline, and asserts an exact retry takes no exclusive section. The second holds a claim commit's apply, which runs after its admission section, on a release channel through a `#[cfg(test)]` seam. While it is held, another real admission proves partition release and an envelope read sees the decided bind. Hold reports may wait for the retained bundle lock to close before emission [ORB-19458]. A full task read waits on the task's bundle lock: the test waits for that read's contention diagnostic (the bundle lock's default 3 s warning) before releasing the writer, proves the read has not returned while the bind is half-applied, and checks the bind and its history after release. Reader assertions use channel ordering and load-tolerant hang guards, with no sleep or cross-thread completion-time bounds [ORB-15820]. Neither the counters nor the stall seam exist in the build the crate-root tests compile against [ORB-15797]. `exclusive_claim_sections_stay_short_under_concurrent_durable_mutations` is `#[ignore]`d and run on demand: it measures exclusive admission hold p50/p95 for concurrent claim mutations on a real disk with real syncs, and asserts those sections request no durability syncs [ORB-16389]. `a_listing_during_a_commit_apply_waits_for_the_writer_and_does_not_rebuild_the_index` holds the same apply seam and asserts a listing that meets the held bundle takes no index fallback and returns the decided state [ORB-22425].
- `crates/orbit-store/src/repository/task/coordination/tests/read_latency.rs`: `slow_release_reports_do_not_hold_task_boundaries` reproduces synchronous release-report I/O retaining both boundary locks (ORB-19458). An isolated child parks a subscriber after an 80 ms admission and probes both locks through the public acquisition API, then verifies full task and search results through `TaskStoreBackend` while the report is parked. It fails on the original guard drop order and passes after descriptor release and report deferral (focused candidate: 0.14 s). The private fixture only sets the hold-warning threshold; the assertions exercise public APIs. `task_read_report_latency_probe` uses the same fixture for ten sequential task reads and ten searches, with a 2.2 s sink stall per sample and eight representative bundles. This explicitly ignored experiment emits JSON timings and phase diagnostics, takes about 46 s per build, and runs only on demand; the regular regression has no performance deadline. The production boundary's 2 s warning threshold cannot place a report after a critical-section operation bounded to 100 ms, so the fixture needs the existing private threshold seam.
- `crates/orbit-store/src/repository/task/coordination/tests/commit_replay.rs`: `a_bind_interrupted_after_its_decision_converges_to_the_committed_state` and `opening_a_boundary_replays_a_decision_whose_lease_was_lost` (criterion 2, fault injection). A `#[cfg(test)]` seam makes a decided bind's apply die before it starts or after only its history, dropping its lease and bundle lock as a dead process would. The first asserts the bundle refuses readers and writers until the next store read replays it, and that the replay matches an uninterrupted bind's history exactly once, settles the journal, clears the lease and keeps the receipt replay. The second removes the dead lease, as a host crash may, and asserts that opening the boundary replays the decision. A real crash cannot be placed between the decision and the apply from a crate-root test [ORB-16389].
- `crates/orbit-store/src/repository/task/coordination/tests/commit_replay.rs::journal_retention_keeps_an_interrupted_decision_for_its_replay` (criterion 2, fault injection; explicitly required journal-retention recovery coverage). The existing `BeforeApply` seam interrupts a decided claim bind. After aging its committed journal row beyond the applied rows and pruning through `StoreRetentionBackend`, a task-store read must still replay the decision, settle the journal and preserve both the interrupted claim's receipt and a pruned claim's receipt. The seam is available only under `#[cfg(test)]`, so a crate-root integration test cannot force this crash point. This extends the existing replay fixture with the retention safety guarantee; it does not test a private implementation function.
- `crates/orbit-store/src/repository/task/coordination/tests/sidecar_reads.rs`: `a_settled_history_read_does_not_wait_for_a_partition_section` (criterion 2, deterministic interleaving) and `a_history_read_settles_an_interrupted_commit_or_write_first` (criterion 2, fault injection). The first holds the partition's admission section exclusively and asserts that a history and comments read through `TaskHistoryStoreBackend` completes during the hold, under a load-tolerant hang guard. The second interrupts a decided bind after its history with the existing apply seam, then has a bundle write die after appending whole, valid event and comment rows. It asserts that the next read replays the bind exactly once, settling its journal and lease, and never exposes the unfinished write's rows. Neither a held partition section nor either crash point can be placed from a crate-root test. A codebase scan's lock-free history reads are what these guard [ORB-16938].
- `crates/orbit-engine/src/executor/automation/vcs/base/tests/claim.rs`: `a_candidate_rerun_stopped_at_the_reserve_still_attributes_its_failures_to_the_base` (criterion 2, fault injection). It stops a candidate rerun that hangs after its failures are printed, using a replay budget of seconds, and asserts that settlement still replays only the printed failing tests on the base and names the cut. The boundary sets the budget in whole review minutes, so reaching the cut there would cost a quarter of a minute or more of wall time per run.
- `crates/orbit-engine/src/executor/automation/vcs/base/tests/claim.rs`: `supervisor_stops_never_classify_partial_failures_as_known_red` (criterion 2, fault injection). In an isolated child, a lowered capture ceiling forces an actual output-capture stop with only 2 KiB of output; a self-signalled run and a completed run are controls. It checks that the shared known-red predicate rejects supervisor stops and base-claim classification refuses them before replay. The settlement boundary covers the signal, but cannot reach the shared predicate after the early refusal [ORB-15590].
- `crates/orbit-common/src/fs/generation/tests/handoff.rs`: `a_probe_that_times_out_is_retried_at_the_next_boundary` and `a_completed_refusal_is_not_probed_again` (criterion 2, fault injection). A candidate's first exec can be held past the probe bound by the OS, which the boundary cannot reproduce on demand, and the process-wide refusal cache is keyed by the replaced image of the running test binary. The first holds a probe past a short timeout against a script that stalls, asserts the timeout is not cached, then answers under a longer budget and asserts that answering call probes exactly once; the second asserts that wrong-contract, missing-capability and non-zero-exit answers are probed once. Both answering probes get the longer budget, so neither passing assertion waits on a short window.
- `crates/orbit-core/src/application/task/pilot/tests/persist.rs`: `overlapping_pilot_graphs_lock_in_order_and_refuse_the_closing_edge` (criterion 2, deterministic interleaving). Two task-pilot applications whose proposed edges close a dependency cycle only together run on separate threads, ordered by a `#[cfg(test)]` seam at the two points of the apply lock protocol: the second discovers its graph before the first takes its locks and locks after. It asserts that neither waits out a file-lock timeout, the second rediscovers the grown graph, its edge is refused by the cycle check, and the persisted graph stays acyclic. The crate-root integration tests compile without `cfg(test)`, so they cannot reach the seam, and unforced timing would not choose this interleaving reliably [ORB-15408].

- [The plugin workspace lifecycle case](../../crates/orbit-core/tests/runtime/admission_audit/plugin_workspace.rs) now uses the exported enable API at the crate-root boundary. Its existing destination-file fault proves that failed seeding preserves a prior disabled toggle and a successful retry seeds definitions before enabling the workspace. An ordinary hostile destination needs no private seam, and the integration target reuses the isolated plugin fixture.

- `crates/orbit-engine/src/executor/automation/ci/` attribution tests: `nextest_regions_bind_only_complete_evidence_to_the_known_failed_step` (criteria 1 and 3). This table exercises private CI attribution with streamed nextest regions and incomplete selection/command flags. Public engine fixtures cannot provide scripted CI logs to this boundary; it prevents a partial or differently attributed block from becoming actionable evidence.

- `crates/orbit-engine/src/executor/automation/ci/tests/collect.rs::checkout_evidence_uses_all_scope_and_each_log_read_uses_the_requested_bound` (criterion 2, deterministic scope-specific log responses). It checks the collector requests `Failed` before `All` for the same run and job, applies the small configured byte limit, and recovers checkout identity only from the `All` response. The production query adapter invokes GitHub CLI, while public engine fixtures cannot script distinct per-scope log responses; this protects the fake contract gap confirmed in ORB-17356.
- `crates/orbit-engine/src/executor/automation/ci/tests/collect.rs::repository_and_run_view_query_failures_are_retryable` (criterion 2, deterministic query-error injection). One collect-boundary table scripts `repo_view`, repository-wide `run_list`, and `run_view` failures and asserts each operation appears in `retryable_errors`. The production query adapter invokes GitHub CLI, while public engine fixtures cannot script these errors; this guards the retryable failure paths that the fake previously could not reach.

- `crates/orbit-engine/src/executor/automation/ci/tests/history.rs::old_shape_history_keeps_streaks_and_keys_and_round_trips` (criterion 1, parser edge cases of a persisted format). One file in today's pre-typed shape, with a malformed count and a malformed landing gap, loads to the same streaks. Quoted string ids, bare numeric ids and a missing `job_id` render to the same keys, so existing streaks do not reset, and a save writes the exact JSON shape. The file's other test pins the three-sweep threshold, not the file format. The codec and key rendering are private to the sweep module, and public engine fixtures cannot script the CI logs that feed it (see the attribution admission above), so the test drives `load`, `observe` and `save` directly.

- [GC leftover tests](../../crates/orbit-engine/tests/engine/admission_audit/gc/leftovers.rs): `gc_keeps_trash_directory_when_deleter_pid_is_alive_and_within_timeout`, `gc_reclaims_trash_directory_when_deleter_pid_is_dead`, `gc_reclaims_trash_directory_when_age_exceeds_timeout`, and `gc_leaves_malformed_trash_directory_as_unrecognized` now call the exported `collect_worktrees` helper from the engine integration binary. Their temporary trash names and deleter PIDs reach the same parsing, age and liveness guarantees without real multi-minute deletion threads or private access [ORB-15702].

- `crates/orbit-core/src/application/plugin/tests/lifecycle/remove.rs`: `a_failed_tree_delete_keeps_the_row_for_a_retry_that_finishes_it` (criterion 2, fault injection). A `#[cfg(test)]` seam fails the next delete of the namespace install tree or of its build log, as an unlink error partway through `remove_dir_all` would. The test asserts that the row, grant witness and build witness survive the failure, and that a retried `orbit plugin remove` deletes both trees and then the row. Permission bits would not force this failure under root-run tests, and the crate-root integration tests compile without `cfg(test)`, so they cannot reach the seam.

- `crates/orbit-core/src/application/job/run/tests/owner.rs`: `provider_signalling_requires_a_verified_live_identity` (criteria 2 and 3). Injected unavailable, no-process and mismatched start-identity probes with conservative live observations must leave an owned child alive; a real matching probe is the positive teardown control. The public cancellation boundary cannot force unreadable kernel identity or the contradictory no-process-but-live outcome without changing host permissions or relying on PID reuse [ORB-15618].
- `crates/orbit-web/src/tests/ssh_tunnel.rs`: `paced_status_fragments_cannot_outlive_the_readiness_budget`, `cancel_during_a_paced_status_reaps_the_ssh_child`, and `oversized_status_line_is_refused_without_draining_the_flood` (criteria 2 and 4). A peer trickles a status line faster than the per-read socket timeout, or floods one with no newline, through the ssh stub's forward. `establish` and `ProbeLimit` are crate-private. Host forwarding starts the same readiness probe but fixes the attach/ready budgets at 5 s/30 s, hides direct probe completion and adds identity negotiation; it cannot inject these short deadlines or directly cancel the establish. `orbit web connect` also blocks on signals. The tests assert the attach and spawn budgets still end the attempt, cancellation reaps the owned ssh child, and a direct no-newline flood is refused before the probe deadline. `delayed_forward_bind_attaches_without_a_second_ssh` remains the healthy-attach control. `probes_verify_status_code_over_finite_peer` (criterion 2, fault injection [ORB-15675]) drives finite one-shot loopback peers through the production `healthz_ok` and `http_get_ok_body` probes: a non-200 status is refused even when its reason phrase contains `200`, and malformed versions, signed or non-three-digit codes, and missing or nonnumeric codes are refused.
- `crates/orbit-web/src/tests/runtime_memo.rs`: `stale_entry_is_served_while_one_refresh_replaces_it` (criterion 2, deterministic interleaving). A memo entry past its TTL is read while the refresh it starts is held on a channel: the read returns the stale entry before the refresh can finish, a second stale read starts no other refresh, and a miss after the release waits on the refresh's gate and finds its result. The codebase and agent scans serve the dashboard this way; through the router, expiring an entry would cost the five-minute TTL in wall time and could not hold the refresh open [ORB-16939].
- `crates/orbit-web/src/tests/host_tunnels.rs`: `a_paced_identity_body_cannot_outlive_its_deadline` (criteria 2 and 4). After a healthy attach, the identity body trickles faster than the socket timeout. The test asserts acquire fails inside the identity budget and the owned child is reaped. The router tests cannot pace that read or observe the child.

- `crates/orbit-core/src/application/job/run/tests/owner.rs`: `owner_and_provider_never_escalate_against_a_replacement_identity` (criterion 2). A bound-target seam forces exit/reuse during verification, immediately before TERM, and during grace for both cancellation callers, with ordinary TERM/KILL controls. It captures the generation at binding and verifies that binding precedes both identity-bearing probes, even when the replacement token matches. Group preparation follows authorization, and an injected snapshot refusal must reach both callers before any signal. The runtime boundary cannot force these interleavings. `orbit-common`’s `process::signal` test `retained_signal_identity_never_targets_a_reused_numeric_pid` (criteria 2 and 4) binds a real process, reaps it, substitutes a live bystander's number, and verifies that retained Linux pidfd/macOS unique-id signals never reach the bystander. No PID exhaustion or host load is needed.
- `crates/orbit-core/src/adapter/engine_host/actions/pipeline_actions/tests/invoke.rs`: `an_expired_gate_wait_cancels_its_delivery_child_and_keeps_its_candidate` and `a_child_that_finishes_as_the_wait_expires_reports_its_own_outcome` (criterion 2, deterministic interleaving). An injected wait clock lets the gate's four-hour dispatch timeout pass at once and settles the child between two polls, after the wait's last look and before the parent acts on the deadline. The first asserts the expired wait cancels the running delivery child, keeps its candidate through the child's failure handoff, and returns its task to the backlog naming the timeout and that candidate; the second asserts a child that settled in that window reports its own outcome and is not cancelled. The boundary cannot place the settlement inside that window without waiting out a real deadline.

- `crates/orbit-engine/src/activity_job/cli_runner/tests/inspection.rs::inspection_git_ignores_parent_locale_and_config` and `executor/automation/vcs/tests/git.rs::host_git_classifies_failures_in_the_c_locale` (criterion 3). These private Git boundaries must discard parent locale and Git configuration authority before classifying stderr. Isolated children supply hostile parent environments; the inspection shim queries real Git's effective fsmonitor setting before executing the command, and both runners classify real failures under C and German parent locale settings. Public provider dispatch cannot expose the host runner or cheaply inject an environment observer for every inspection command.

- `crates/orbit-engine/src/executor/automation/vcs/tests/git.rs::real_ssh_fetch_starts_under_lock_and_reaps_before_release` (criterion 4, platform-kernel behaviour) is the explicitly required positive control for the fetch deadline fixture split. Real Git starts an executable SSH shim while holding the actual fetch lock; a FIFO coordinates termination, after which the test checks Git reaping, SSH absence and lock release. The existing `stalled_delivery_fetch_releases_the_lock_before_a_waiter_times_out` independently starts its waiter from actual lock contention under the unchanged 1500 ms holder deadline. Public delivery dispatch cannot expose these private host-fetch process identities or coordinate their termination economically. The final focused four-test selection took 1.61 s in implementation evidence; the separate 20 s startup bound is a failure bound, not a delay. This admission separates coverage and does not establish the historical failure's cause.

- `crates/orbit-core/src/application/task/final_recovery/tests/apply.rs::terminal_replay_finishes_side_effects_after_a_post_lock_read_failure` (criterion 2, fault injection). A one-shot read seam stops final recovery after its decision is durable and before resolves or PR closure. The isolated fixture verifies replay finishes done/rejected/archived effects, writes one decision, and leaves successful effects and a human-reopened friction alone on another retry. The public boundary cannot deterministically fail only that post-lock read.

- `crates/orbit-core/src/application/task/desktop/tests/write.rs::completion_replay_finishes_side_effects_after_a_post_lock_read_failure` (criterion 2, fault injection). A one-shot read seam stops a desktop completion after its receipt commits and before resolves or PR closure. The isolated fixture verifies a same-request retry resolves the target and closes the stale PR, and that another retry leaves a human-reopened friction and a recorded PR closure alone. The public boundary cannot deterministically fail only that post-lock read.
- The [worker resource-sampler tests](../../crates/orbit-core/src/application/job/pipeline/worker/tests/usage/sampler.rs): `sampler_records_one_row_per_minute_with_mean_cores_and_peak_memory`, `counter_intervals_credit_the_minutes_they_cover`, `memory_counts_shmem_but_not_page_cache` and `concurrent_workers_store_one_host_row_per_minute` (criteria 1 and 2). Fixture `cpu.stat`, `memory.current` and `memory.stat` files and a fixture `/proc` are rewritten between samples taken at injected times, so minutes of per-scope aggregation, the resume continuation, boundary-ending bursts, intervals crossing one or several minutes, and the page-cache/tmpfs split are checked in milliseconds without a systemd scope or host load. The third holds concurrent offers of one host minute at a barrier, through separate connections, and asserts exactly one insert wins. A boundary worker cannot choose its cgroup, its `/proc` or its clock.
- The [process-tree sampler tests](../../crates/orbit-core/src/application/job/pipeline/worker/tests/usage/tree.rs): `tree_intervals_charge_deltas_and_new_processes_only` and `an_unreadable_tree_skips_that_sample_and_keeps_the_run` (criteria 1 and 2). One table drives injected snapshots through the same minute aggregator: a child that exits, a new child, a reused pid, a counter that steps backwards, and a missed read between two successes. The assertions pin non-negative cores, the full CPU of a process that appeared, the peak footprint, and that a miss adds no sample and does not move the baseline. A live tree cannot make a process exit on the sample boundary or refuse one read and then succeed. `macos_worker_records_a_spawned_child_tree` (criterion 4) spawns a child and grandchild that touch anonymous pages, then checks the stored minute covers that tree's footprint and CPU. It owns the child's process group from spawn, so a readiness failure or timeout still kills both, and teardown checks both identities are dead. `hold_child_tree` is the ignored re-exec those processes run; it is not a second invariant. `native_tree_attributes_a_build_like_workload` (criterion 4, native macOS kernel behavior; explicitly ignored) re-executes the worker in a fresh session, drives build-like processes through the production sampling interval, and compares the worker's session-tree CPU with each workload process's `getrusage` total. It asserts 85–110% attribution and emits the workload, tree-only, and host non-idle measurements as JSON. The host comparison is noisy on a shared machine, so this is run explicitly for native acceptance rather than as a recurring gate. The sampler is crate-private, and a boundary worker cannot substitute the tree it walks or reproduce launchd reparenting in another platform's integration test.
- `crates/orbit-common/src/process/tests/ancestry.rs::a_child_is_in_the_process_tree_until_it_exits` (criterion 4). It reads this process's libproc tree, checks the root's `ri_phys_footprint` against `proc_pid_rusage` (under the lifetime maximum, and nearer the footprint than resident size when those differ), then spawns `/bin/sleep`, waits until that pid is in the tree, reaps it, and checks it is gone. A sandbox that refuses the tree defers only when `ps` itself is denied. No other test walks `proc_listchildpids`.
- `crates/orbit-common/src/process/tests/ancestry.rs::native_cpu_time_agrees_with_getrusage` (criterion 4, native macOS kernel accounting). This explicitly ignored calibration compares own CPU nanoseconds from the production process tree with `getrusage`, retaining the native Mach timebase and raw counters. An exact child of the existing test binary isolates `RUSAGE_SELF` from concurrent tests; the workload is bounded to 250 ms of wall time with a 5% plus 5 ms sampling tolerance. No integration test performs this native comparison, and another binary would duplicate the isolated platform harness for the task's one-time calibration. Run it explicitly for native evidence, never as a recurring gate. It guards the nonunit Mach-timebase undercount [ORB-15994].
- `crates/orbit-common/src/process/tests/ancestry.rs::tree_walk_omits_reused_pids_and_reparented_children` (criterion 2, deterministic interleaving). Injected libproc reads replace or reparent a PID between its identity and usage reads, give a listed child another parent, replace a parent during its listing or before its queued child is checked, and refuse a read. A stable tree is kept whole; a replaced or reparented process and the children listed under a replacement are omitted, and a refused read or a replaced root yields no sample. A boundary test cannot force PID reuse at a kernel-call boundary without exhausting PIDs or generating host load.
- `crates/orbit-common/src/process/tests/ancestry.rs::an_orphan_stays_in_the_session_tree_its_shell_led` (criterion 4, native macOS process-session behavior). A shell leads a fresh session, starts a background process and exits without waiting; launchd adopts the still-running process while its session remains owned by the tree. Through the public `SessionTree` API, the regression check verifies the orphan remains visible with no parent after the shell exits, while the parent-link-only walk loses it. A fixture-only tree cannot reproduce launchd's reparenting, and using the public native entry point avoids testing the private traversal helper directly.
- `crates/orbit-common/src/process/tests/ancestry.rs::session_cpu_counts_a_child_once_when_orphaned_during_a_walk` (criterion 2, deterministic reparenting interleaving). Snapshot fixtures and the public `TreeCpuAccount::observe` identity callback reproduce an observed double charge: a child with 10 units of already charged CPU disappears during reparenting, then returns as an orphan with a lifetime of 20. The account must total 20, charge nothing during absent snapshots, and create no debt when launchd eventually reaps the orphan. This exercises the public accounting boundary without timing real processes or generating host load; native sampling cannot deterministically place reparenting between its kernel reads.
- The [host usage reader tests](../../crates/orbit-runtime/src/host_resource/tests/usage.rs): `host_usage_is_tick_utilization_in_cores_and_unavailable_memory` and `malformed_proc_files_read_as_unknown` (criterion 1, parser edge cases). Fixture `/proc/stat` and `/proc/meminfo` files pin busy cores from tick deltas (not load average) and `MemTotal − MemAvailable`, and truncated, non-numeric or inconsistent files read as unknown rather than zero use. The reader is crate-private and the boundary reads the real `/proc`.
- The [delivery work-class tests](../../crates/orbit-core/src/application/job/pipeline/worker/tests/delivery_class.rs): `work_class_prefers_template_then_no_diff_then_change` (criterion 1) is the class precedence table. `class_is_captured_once_per_delivery_and_survives_tag_edits` (criteria 1 and 3) walks the admission decision table: a child inherits its delivery's class after a tag edit, a fresh submission recaptures it and overwrites a caller-supplied class, and resumes, retry runs and adopted runs are marked resumed. Reaching each row at the boundary would need a full delivery pipeline per case. `worker_without_a_scope_records_nothing_and_logs_nothing` (criterion 2, fault injection) injects an absent scope and then a fixture scope into the sampler's start and asserts no rows and no warning, then one row; the boundary cannot choose whether a worker resolves a scope.
- `crates/orbit-store/src/driver/sqlite/migration/tests/ledger/compatibility.rs`: `resource_minute_tables_keep_an_older_binary_writing` (persisted format with a migration; criterion 3). It applies the ledger through the resource-minute migrations, then opens the database with the registry that ends before them, and asserts the older binary still opens it writable. No test can build an older binary.
- `crates/orbit-store/src/driver/sqlite/migration/tests/ledger/compatibility.rs::executor_census_keeps_an_older_binary_writing` (persisted format with an additive migration; criterion 3). It applies the executor-census migration, writes a census row, and opens the database with the older registry. It asserts that the older binary remains writable without changing the writer or reader floors and can still write a coordination receipt beside the census. The public store-open boundary always applies the latest registry, so it cannot exercise this rolling-upgrade compatibility path.

- `crates/orbit-store/src/driver/sqlite/migration/tests/ledger/registry.rs::store_schema_prefix_goldens` (criterion 3, persisted-schema safety): applies each shipped ledger prefix independently and compares its normalized SQLite schema with a lossless per-version golden. The public store-open boundary always applies the latest registry and cannot exercise historical prefixes without widening production visibility. The legacy recreation and actor data backfill cases use the public boundary in `tests/store/migration_freeze.rs`.
- `crates/orbit-store/src/driver/sqlite/migration/tests/ledger/registry.rs::task_registry_schema_prefix_goldens` (criterion 3, persisted-schema safety): the same per-prefix golden for the task registry's own ledger (`task_registry/tests/registry_schema.golden`), a separate SQLite file the store-open boundary never builds. Adoption, the `user_version` fence and newer-registry decisions use the public registry boundary in `tests/store/task_registry_ledger.rs`.

- `the_hold_includes_the_horizon_and_a_later_instant_admits` (criterion 1), beside `crates/orbit-runtime/src/host_signal.rs`. One table pins the horizon arithmetic: equality holds, one second later admits, and a past timestamp holds. The pull and local drain tests, and the sweep test below, place the schedule with `Utc::now()` at minute granularity, so they cannot pin the one-second boundary without a race [ORB-15796].
- `crates/orbit-core/src/application/routines/tests/sweep.rs::a_shutdown_inside_the_hold_horizon_skips_the_sweep_and_one_beyond_it_fires` (criterion 2). The public `test_support::run_sweep_at` hardcodes the default host-signal probe, so a crate-root test cannot place a shutdown on either side of the horizon. The test drives `pub(crate)` `run_sweep_at_with_providers_at` with an injected clock and a 30-minute horizon, and asserts the tick skips at the horizon and fires one minute past it. A hardcoded 180 would still hold that far side [ORB-15796].

- `schema` in `crates/orbit-runtime/src/recovery_authority/tests/mod.rs` (criterion 3, persisted-authority safety): exercises adoption from a frozen pre-ledger SQL fixture across the shipped certificate and optional binding shapes, preserving each existing SQLite table definition and row, and checks newer-schema writer/read-only/refusal decisions and WAL sidecar retention after those opens close. Host authority is private and cannot be seeded or opened directly through the public leaf runtime API without weakening its trust boundary. Mutable cases re-execute in isolated children.

- `crates/orbit-tools/src/plugin/tests/callback/mod.rs::a_failed_credential_open_removes_the_minted_record` (criterion 2, fault injection). Opening the credential fails only under descriptor exhaustion, or on a host with no inherited descriptors, so the test supplies a failing opener through the crate-private `mint_with` seam and proves the unowned `pid: 0` record is removed, since the sweep skips it forever.
  `crates/orbit-tools/src/plugin/tests/line_queue.rs::a_reader_stays_blocked_until_the_consumer_drains_bytes` and `dropping_the_consumer_releases_a_blocked_reader` (criterion 2, deterministic interleaving). A plugin MCP server's stdout reader may queue at most `MAX_QUEUED_BYTES` ahead of the consumer. A boundary test would need 64 MiB lines and a memory probe, so the tests drive the crate-private queue with a 10-byte budget: the reader cannot run more than one line ahead, and dropping the consumer releases a reader blocked on the budget.

- `crates/orbit-core/src/application/plugin/tests/seed.rs::plugin_auto_task_colliding_with_shipped_default_is_refused` and `non_colliding_plugin_auto_task_seeds_as_before` (criterion 3, fail-closed plugin seed validation for ORB-23050). They drive the public `seed_plugin_definitions` entry point the enable/sync paths use, with a fixture plugin whose `code` + `review` resolves to the shipped default `code-review`. The colliding test proves a typed refusal with and without `--force` and that no routine is left behind; the non-colliding test proves an ordinary name still seeds `Created` then `Unchanged`. The crate-root `plugin_workspace` integration boundary covers generic seed-failure toggle preservation but cannot choose the crafted namespace/name collision or the force matrix without a full install per case.

## Goldens

- A golden's fixture input is real or realistic, checked in, and small.
- `make goldens UPDATE=1` regenerates goldens. The PR explains every golden diff. Re-blessing a diff without explaining it is a review blocker, and so is blessing a diff to get CI green.
- A golden asserts a contract. Do not golden prose that is expected to change, such as prompts or UI copy (see the text-matching rule in `CLAUDE.md`).

## Integration-test cost

Cargo links each top-level `crates/<crate>/tests/<name>.rs` file and each `crates/<crate>/tests/<area>/main.rs` as its own binary, and every binary pays the link cost. Integration tests are therefore grouped by area: `tests/<area>/main.rs` declares one module per surface, `tests/<area>/<surface>.rs` (for example `orbit-cli`'s `output`, `mcp`, `tool`, `task`, `workspace` and `process` binaries). To keep that cost down:

- Add a new case to an existing area binary, or a new module to it for a new surface.
- Start a new binary only for a genuinely separate area, or for a test that must own its process because it installs signal handlers or mutates the environment outside `orbit_common::test_env`'s lock. Say why in its header.
- A test that re-runs itself as a child (`current_exe()` with `--exact <name>`) must pass the module-qualified name, run the child through `orbit_common::test_env::run_child_test` (a load-tolerant hang guard that reports the child's output on overrun), and verify execution with `orbit_common::test_env::assert_child_test_passed`, which rejects libtest's successful zero-test exit. For children intentionally killed or kept alive, use `assert_child_test_exists` before spawning and verify the child's readiness sentinel or handshake. Select ignored child entry points with `--ignored`.
- Fixtures that mutate Orbit state still run in an isolated child process ([DEVELOPMENT.md](../DEVELOPMENT.md#safe-mutable-cli-fixtures)).
- Waits and process guards follow STD-03 §R17 to §R20. Size them for a loaded host, not an idle one ([DEVELOPMENT.md](../DEVELOPMENT.md#fixtures-on-a-loaded-host)).
- Shell fixtures must not poll markers by forking `sleep` in `while`/`until` loops. Use a FIFO/pipe handshake or an in-process wait with a deadline. `make ci-fast` scans Rust strings in test paths, including scripts assembled across literals. A necessary bounded exception belongs in `test-shell-waits-allowlist.json` in the scripts directory with its path, exact loop header and a reason; unused exceptions fail the guard.
- A refusal fixture asserts the exact authorization cause, such as the refusal kind and the tool, grant or ceiling it names. A bare failure lets a bootstrap or sandbox error pass as the policy refusal.

- `orbit-engine`'s `claimed_base_ref_preserves_valid_refs_and_propagates_invalid_modes` (criterion 1, combinatorial pure ref selection) covers explicit modes, defaulting and claim ship-mode fallback together. The public claim action rejects invalid sync input during observation, before the red-base hold path, so it cannot expose the helper's former silent error-to-local fallback.

## Provider response boundary

`orbit-agent` keeps provider projection and response regressions in the existing
[`provider_invocation/response.rs`](../../crates/orbit-agent/tests/provider_invocation/response.rs)
integration module, which wires the `response/projection.rs` and
`response/envelope.rs` fixture modules. A few shared capture cases stay in the
parent module: the `provider_long_prompt_requests` cases cover Claude streamed
usage and per-request Codex rollout usage from captured provider data. Exported
`normalize_cli_stdout`, `project_cli_response`, `latest_assistant_message`,
`antigravity_terminal_error_diagnostic`, and `ParsedStdout` accept the same
crafted captures as the former sibling unit tests. Parser edge cases, failure
synthesis, transcript disclosure and usage rollups therefore need no private
test access or provider subprocess. Copilot prompt-echo fixtures obtain stdin
from public `Agent::invoke`; the Antigravity timeout matrix and its non-target
provider controls use exported `apply_antigravity_print_timeout` in
[`provider_limit.rs`](../../crates/orbit-agent/tests/provider_limit.rs). These
cases do not earn unit admission because the public boundary costs the same.

## Public types contract boundary

`orbit-types`' plugin, task, tool, workflow, workspace publication, digest and
deferred-sandbox cases live in
[`tests/contracts/main.rs`](../../crates/orbit-types/tests/contracts/main.rs),
with modules for each surface in one integration binary.
The admission audit's 40 moved cases use the same inputs and assertions through
exported helpers and types, including the vendored provider-resolution fixture;
none needs a provider process or production visibility change.

The two retired filesystem cases are covered by `orbit-policy`'s
`tests::matrix::precedence_matrix` (later exclusion and allowed sibling) and
`tests::matrix::newline_segment_request_matrix` (single-star grant followed by
recursive deny, newline/ordinary siblings and positive recursive newline grants).
The retired `only_a_won_start_grants_execution_authority` case merely enumerated
`JobRunStartOutcome::owns_execution`, a getter excluded by this strategy.
`orbit-store`'s `allocation_admission::removed_job_run_ids_are_never_reallocated`
asserts a real store Start owns execution through its `finish` helper; it is
not an exact duplicate of all three getter variants.

The later `workflow/review/tests/gate_note.rs` admission was outside this audit
slice and remains in its sibling unit location and baseline.

## Retiring a unit test

You may delete a unit test when one of these holds:

- An integration test or golden now covers its behaviour. The deletion PR names that replacement.
- It fails the admission criteria and guards nothing the boundary misses.

Before deleting a test, check whether it is the only coverage of a security or incident guard. If it is, either move that guard to the boundary in the same change or keep the test. Keep the support code a kept test depends on: shared helpers, `#[cfg(test)]` seams in production code, and `#[ignore]` child-process entry points that a kept test re-execs.

## When NOT to

- **Do not pad a boundary test to reach an edge case it cannot express cleanly.** That case meets criterion 1 or 2, so write a small unit test.
- **Do not add e2e tests for every branch.** E2E covers critical flows. Branches belong in integration tests or table-driven unit tests.
- **Do not reach around this rule by widening visibility** so that an integration test can call internals. Test the public surface, or admit a unit test under the criteria.

## orbit-tools admission audit reconciliation

The 2026-10-12 audit's 18 moves now live in the existing crate-root `tools`
integration binary. Artifact validation and managed domain controls dispatch
through `ToolRegistry`; program symlink denial dispatches `proc.spawn`.
Working-directory confinement uses the exported `confine_workspace_cwd`
helper. `plugin_build` calls exported planning, environment and installation
helpers directly, and `plugin_admission` calls exported backend profiles,
child-environment construction and `PluginTool::execute`. Unsafe Git sources
retain their isolated fake-Git child fixture under `plugin_source_admission`.
No production access is widened for these tests.

The rotation race retains its barrier, same-version updates, compare-and-swap
store and winner/refusal assertions. Two tiny exec backend replies carry the
existing updates through the public tool response boundary instead of calling
private `apply_secret_updates`; no live provider or build sandbox is needed.
The oversized-output fixture retains the production 256 MiB bound as a sparse
file, because that private constant is not an exported API.

Six unit identities remain on the pinned audit keep list: the Git config-write lexer table
(criterion 1), forged callback descriptor identity (criterion 3), secret-valued
update-name diagnostics before public masking (criterion 3), archive traversal
refusal and decompression with lowered private limits (criterion 3), and the
first-party Git-source spelling table (criterion 1). Their private fixtures and
isolated diagnostic child entry point remain under `src`. The first-party
classifier is currently exported; this slice preserves its pinned keep
identity rather than expanding the audit's 18-move mandate.

## CLI admission audit reconciliation

The CLI is a binary crate, so `pub` inside its command modules is not an
exported library boundary. The 2026-10-12 audit cleanup moves plugin consent,
secret argv refusal, filesystem diagnostics, redirected-discovery teardown,
non-terminal rendering and audit deduplication to the existing crate-root
`tool`, `output`, `workspace` and `task` binaries. Secret argv forms share the
existing leak-scan invariant rather than a second parser-only test. The four
retired Linux package/bundle tests pinned selection, call order or diagnostic
wiring; the Host refusal, signed-digest and custom-profile controls remain.

Three proposed moves require private inputs that the built CLI cannot supply
without widening production access or changing the host:

- `command/init/tests/linux_host.rs::apparmor_remedy_requires_ubuntu_and_the_exact_uid_map_denial`
  (criteria 2 and 3): `FixtureHost` injects OS identity, probe denials and
  privileged operations into private `prepare_with`. Its negative cases prove
  that derivatives and different denial signatures neither elevate nor stage
  a bundle nor install a profile. The CLI reads real host OS/kernel state;
  the namespace-denial integration fixture cannot emulate Ubuntu's uid-map
  denial and successful privileged profile installation safely.
- `command/task/tests/flow.rs::rejection_then_reopen_preserves_dropped_event_and_historical_boundaries`
  (criterion 1): private `compute_flow` accepts fixed timestamps and an explicit
  `now`, proving dropped/reopened arithmetic and the intervening closed bucket
  together. `task flow` always reads `Utc::now()` and persisted history. Retain
  the pure fixed-clock case instead of adding database/history rewriting and
  a wall-clock-dependent fixture just to reach the same arithmetic.
- `command/clock/tests/tick.rs::deadline_failure_returns_nonzero_with_deferred_workspaces_in_both_views`
  (criterion 2): private `ClockTickArgs::output` accepts a constructed partial
  `SweepOutcome`, preserving both reports and the failure status together.
  The CLI constructs default `SweepOptions` and exposes no injected deadline
  or outcome. Forcing expiration at a chosen workspace through the binary
  would require waiting out the real budget and holding scheduler work;
  the existing root/alias CLI tests cannot produce that partial outcome.

These are corrected admission decisions based on the current qualifying
seams, rather than claims that the proposed moves already happened. All 15
originally admitted CLI tests and their fixtures remain intact.

The simulated `tests/audit_middleware.rs::invalid_json_bail_before_runtime_yields_exactly_one_row`
is redundant with `tests/task/audit_cli.rs::audit_cli_missing_log_records_the_render_failure`:
that existing built-CLI case proves guard-side failure writes exactly one row,
with the failure status, exit code and diagnostic. The old unit case supplied
an `OrbitError::InvalidInput` after manually creating a guard; it never parsed
JSON. Current `ToolRunArgs::bootstrap_route` returns unusable input before
runtime initialization and before an `AuditGuard` exists. The built-CLI
`tool_run_unusable_input_does_not_migrate_a_pending_layout` case proves that
refusal precedes migration. Retire the simulated duplicate rather than change
this deliberate bootstrap behavior to satisfy its stale scenario.

## Command-layer admission audit reconciliation

`orbit-cmd` keeps no crate-root `tests/`, so the 2026-10-12 audit cleanup
moves its boundary cases into the CLI crate's existing integration binaries.
Those binaries link `orbit_cmd` and call its exported entry points directly,
or run the built CLI where it already sets up the state:

- `process` (`update::flow`, `update::bundled_bwrap`,
  `update::local_candidate_inputs`) calls `run_update`, `stage_bundled_bwrap`,
  `executable_target` and `CandidateManifest::parse` against a shell-script
  installation and a mirror signed with a throwaway key
  (`update::release_fixture`). The oversized-input case makes each input a
  sparse file larger than every limit instead of one byte past each private
  limit, so it does not pin the limit values.
- `workspace` (`agent_rules`, `layout_version`, `doctor_runtime`) calls
  `inject_agent_rules`, `OrbitRuntime::from_roots` with `migrate_dry_run_at`,
  the `task-relations` probe from `doctor_report_probes`, and the graph and
  stale-lock repairs on `DoctorCommands`.
- `task` (`task_registry_recovery`) calls `inspect_task_store_partitions` and
  `remove_unclaimed_task_stores` after the built-CLI fixture loses the task
  registry. The workspace catalog now claims a registered checkout's
  partition, so the unclaimed case first runs `orbit workspace remove`, which
  keeps the partition and its bundles.

Four moves already had an exact built-CLI duplicate, so the source test was
retired instead:

- `update/tests/flow.rs::a_tampered_archive_fails_the_checksum_and_never_reaches_the_install_path`,
  `an_archive_carrying_more_than_the_orbit_binary_is_rejected` and
  `a_second_concurrent_update_is_refused_rather_than_queued` duplicate
  `process/update/integrity.rs`'s
  `a_checksum_mismatch_never_reaches_the_install_path_and_staging_is_cleaned`,
  `an_archive_with_extra_members_including_traversal_is_rejected` and
  `a_second_concurrent_update_is_refused`. Each built-CLI case asserts the same
  refusal, an untouched install and no staging file.
- `doctor/tests/permissions.rs::ownership_scan_never_visits_or_reports_worktree_or_target_contents`
  duplicates `output/doctor.rs::permission_remediation_repairs_legacy_state_and_preserves_excluded_trees`.
  That case leaves writable worktree and `target` trees under both roots. The
  permission row turns `ok` after the repair, and every excluded directory
  stays `0777` through the first and a repeated repair. The ignored
  `default_doctor_large_store_and_worktrees_under_five_seconds` measures the
  walk cost of 10,000 `target` directories.

The three retirements consolidate rather than drop cases. The off-macOS and
no-Claude skip controls join
`doctor/tests/worker_token.rs::warns_for_each_place_the_worker_token_can_be_lost_and_is_ok_when_none_is`.
The full-object-id commit forms join the relocated
`manifests_are_v1_operator_attested_documents_only`, because the manifest's
`source_commit` passes through the same normalizer. The worktree-reclaim
command guard (dk-server-2, 2026-10-09) now lives in
`workspace/worktree_gc_routing.rs::reclaim_dry_run_reports_pattern_bytes_doctor_warns_and_confirm_preserves_the_checkout`.
That test runs the confirming command doctor prints, including its
environment prefix, instead of supplying the operator override itself.

The 15 originally admitted `orbit-cmd` tests remain. Their update fixture is
trimmed to the restore-callback fault injection, and the stale-lock holder
helpers sit beside the one test that uses them.

`update/tests/converge.rs::probe_timeouts_tear_down_descendants_before_and_after_parent_exit`
is admitted under criterion 2 (deadline fault injection). Its four rows call the
version probe and the private admission-contract probe, with the leader either
alive or exited while a descendant holds its output pipes. A readiness-triggered
supervision clock forces the real timeout/teardown path, and each row proves the
helper is gone. This replaces the two signed-release CLI probe-lifetime tests;
reaching the private contract entry point through the CLI would recreate a
signed mirror and wait through each deadline. The existing hanging-candidate
update test retains refusal, unchanged installation and no-backup assertions.

## Common-crate admission audit reconciliation

The 2026-10-12 audit's `orbit-common` moves run in the crate-root
`mechanisms` binary. It holds one module per responsibility (`fs`,
`governance`, `migration`, `observability`, `process`, `security`, `sqlite`,
`storage`) and calls the same exported helpers the sibling tests called. The
SQLite observation fixture keeps its re-executed writer child, now named
`sqlite::read_only_observation::external_writer_process`.

Two cases reach their guarantee through another public caller, because the
helper they called is crate-private:

- `private_open_repairs_a_permissive_existing_file` opens a permissive
  existing lock file through `try_acquire_exclusive_file_lock`. Lock files use
  the same create, read and write private open the case used.
- `private_append_rejects_a_final_symlink` joins `jsonl_exit_flush`, because
  the JSONL feed is the only public caller of the private append. The binary's
  existing child logs a warning with a symlink planted at the feed path, and
  the case asserts that the link's target is unchanged and the link remains.

`bounded_run_times_out_and_reaps_the_process_group` was retired as a
duplicate. `git_deadline`'s `hung_git_times_out_and_leaves_no_child` runs the
same stalled leader-and-`sleep` script through `run_bounded_capped`. It
asserts the timeout error and its `timeout_ms`, and that both pids are gone.
`process_group_liveness`'s
`bounded_timeout_reaps_a_child_that_moves_into_the_callers_group` asserts the
lower deadline bound, the reaped leader and an exited group on every Unix.

Five audited moves stay unit tests, with corrected rationale:

- `fs/generation/tests/handoff.rs::a_completed_refusal_is_not_probed_again`.
  The public `handover_target` keys its process-wide cache on the replaced
  image of the running test binary. Only the `judge_candidate` seam takes a
  caller's cache and probe timeout (see the handoff entry above).
- `process/tests/signal.rs::retained_signal_identity_never_targets_a_reused_numeric_pid`.
  It substitutes a bystander's number for a bound process's private pid to
  simulate reuse, which no public caller can do (see the run-owner entry
  above).
- The three `bounded_run_*` cases admitted above, whose leak checks and
  watchdog are the argv-identity harness the other admitted `bounded.rs`
  tests guard.

## Search-crate admission audit reconciliation

The two `orbit-search` LexicalStore cases live in the crate-root
[`lexical` integration suite](../../crates/orbit-search/tests/lexical/main.rs).
It uses `LexicalIndex` and the exported BM25 helpers to prove legacy-schema
migration preserves search and can reopen the migrated store, and to prove
task indexing, updates, deletion, and full reindexing keep FTS synchronous.
The migration fixture inspects its temporary SQLite file to retain the
existing obsolete-table and space-reclamation assertions. This public helper
boundary costs no extra process or test-only visibility, so the source tests
do not need a private `LexicalStore` or migration seam.

## Orbit-core audit reconciliation

The [delivery reconciliation](../dev/reviews/unit-test-admission-orbit-core-2026-10-13.json)
accounts for all 208 non-admitted identities in the pinned orbit-core audit:
130 cases move to the existing crate-root runtime target, six retirements remove
policy/redundant controls, four retired allowances were already absent, and 68
decisions are corrected against the current private seams or renamed source.
All 142 originally admitted cases remain intact, including the recovery-crew case
that had already moved into its source test subdirectory on the delivery base.

A `pub` function inside a private module is not an exported helper. The retained
cases name the qualifying seam and explain why ordinary dispatch cannot inject
their input, ordering or native observer state economically. For example,
`RunResourceSampler` accepts fixture cgroup/proc counters and sample times; its
concurrent-host-row control is retained for those injected observations, not
merely for starting two threads. The public resource evaluator/cache cases have
moved independently. Review gate cases use public policy views to prepare their
fixture DTOs and public deterministic/ledger APIs to observe decisions; capture
itself and the system-history writer remain at their private seams.

## Orbit-policy admission audit reconciliation

The 12 `orbit-policy` cases pinned by the 2026-10-12 audit now run in
`crates/orbit-policy/tests/policy/admission.rs`. Their fixtures use the
exported `PolicyEngine::from_def`, `check` and `check_resolved` methods and
`PolicyDef::merged`; in-memory definitions and temporary directories reach the
same validation, matching and symlink-resolution behavior without a private
seam or an extra process. This applies to parent traversal, host exceptions,
workspace exception validation, resolved symlinks, newline globs and rule
precedence. No source-level unit-test identities remain admitted for
`orbit-policy`, and no production visibility or behavior changed.

## MCP admission audit reconciliation

The 2026-10-12 audit cleanup moves orbit-mcp cases onto the crate-root
`boundary` binary, which already holds the listener and the id-routed tool
list. The moved cases call exported helpers. They do not widen production
visibility:

- `capability` calls `CapabilityClasses::for_checkout` and
  `ensure_tool_class_held`.
- `identity` calls `mcp_server_identity`.
- `listener` calls `McpListener::bind` and `serve`. `bind` refuses a
  non-loopback address with `InvalidInput` before it opens a socket. A silent
  peer is closed, and its slot is observed by filling the listener's session
  ceiling, because the permit semaphore is not part of the public listener.
- `federated_routing` calls `FederatedMcpHost` with a `DestinationProbe`
  implemented in the test. That trait, plus `RoutedSession`, is the seam the
  mux uses for every destination. Unknown selectors, a lost dispatched
  answer, prefix routing, and `host_workspace_selector` stay on that seam.

`the_locked_mapping_covers_exactly_the_advertised_surface` is retired. It
compared canonical tool names with a handwritten set and did not assert a
class. Discovery shape is the MCP tools-list golden
(`crates/orbit-cli/tests/snapshots/mcp_tools_list.json`). Classification stays
in `mcp_tool_class`, and the relocated replica refusals still prove that
control-plane and drain tools are refused.

Two proposed moves stay unit tests, with corrected rationale. The audit text
for them names listener and routing exports, but the cases call
`DestinationSession`, which is `pub(in crate::federated)`. Its `handshake`
helper exists only while the library is built for unit tests. A public
`DestinationProbe` returns a `RoutedSession` and cannot attach the bounded
line reader to a caller-supplied child. They now live next to the other
admitted session cases in `federated/probe/tests/session.rs`:

- `a_flooding_destination_is_refused_without_buffering_the_flood` (criterion
  2). A `yes` child floods non-JSON lines during handshake. The session
  returns `UnreachableDestination` before the deadline. The admitted
  peer-flood case runs after a valid handshake, during `tools/call`, and must
  stay `OutcomeUnknown`.
- `a_lost_internal_admission_answer_is_outcome_unknown` (criterion 2). A
  child that accepts `call_internal_drain` and never replies is
  `OutcomeUnknown`. The admitted malformed-reply case corrupts a consumed
  internal answer; it does not cover silence after a sent admission.

The eight originally admitted orbit-mcp tests remain, including the private
SSH proxy argv seam in `remote/tests/proxy.rs`.

## Orbit-engine audit reconciliation

The [per-identity delivery record](../dev/reviews/unit-test-admission-orbit-engine-2026-10-13.json)
accounts for all 85 owned decisions: 39 cases move to the existing `engine`
integration binary, eight identities retire through consolidation, and 38 moves
are corrected to admissions at specific private seams. The audit's 29 admitted
identities remain; the diff-chunk and missing-tool invariants absorb their existing
controls. The overflow deadline assertion joins the moved provider deadline case.

The moved cases call exported provider, job, deterministic-action and worktree
helpers. Provider launcher lookup uses its public read-only view; its existing
permanent-error check goes through public provider dispatch. The trusted-host
step/target distinction uses a one-step public job. Temporary Git helpers only
prepare or inspect fixture repositories. Mutable fixtures re-execute in exact
children with disposable homes and inherited authority cleared.

The generic audit assumption that `RuntimeHost` makes every executor seam
injectable does not hold for CI collection: the public action constructs
`HostCiQueries` directly. The retained CI transition/fault matrices use private
`CiQueries`, sweep identities and, for hold/reproduction cases, `collect_at`'s
clock. Replacing these with a `gh`/Git process emulator would add authentication,
projection and repository mechanisms without improving their behavioral proof.
Each retained identity records its input matrix or injected fault and the public
boundary's limitation; private visibility alone is not admission.

Inspection lease exhaustion and slot ownership require retained lease handles;
stalled copies require the constructor's copy-deadline argument. Raw process
progress/capture, text/byte Git diagnostics, required-command stream truncation,
base-comparator cache evidence and the delivery-message producer/parser pair also
name their precise observers in the record. Final worktree removal keeps three
private safety cases: Git's last non-force dirty refusal, forced removal after
permission repair, and preserving an outside symlink target's mode. The nine
collector cases, including the formerly admitted trash cases, use the exported
collector independently of those final-removal seams. No production visibility
is widened and no implementation is copied into a boundary fixture.

The [orbit-store per-identity reconciliation](../dev/reviews/unit-test-admission-orbit-store-2026-10-13.json)
checks the pinned audit against the current export surface. Twenty-nine existing
cases now use crate-root boundaries in `tests/store/main.rs`: catalog and
definition composition in `file_store.rs`, task/history/document mutations in
`task_mutations.rs`, store adoption and frozen-v1 compatibility in
`migration_freeze.rs`, relation validation in `task_registry_ledger.rs`, and
archive/publication workflows in `publication.rs`. Mutable cases re-execute in children with inherited Orbit
authority cleared and disposable homes. The ignored listing benchmark, three
stale run-state allowances, and the audit-aggregate planner identity removed
independently by ORB-15922 are retired; twenty-seven of the original twenty-eight
admitted identities and their child entry points remain. The landed
reconciliation records 29 moves, 10 keeps and 5 retirements, reducing the
orbit-store baseline from 76 to 43.

Ten proposed moves remain as keeps because their private seams prove behavior
the crate-root boundary cannot observe:

- The cached-citation oracle consumes private pre-fold incidents. The exported
  grouping result has already folded and ranked them; moving the oracle would
  require copying production grouping or exposing that intermediate state
  (criterion 1, combinatorial fold precedence).
- The run-recency plan matrix passes the real private `job_run_list_sql` builder
  to SQLite's planner. Public returned rows cannot prove index use or absence
  of workspace sorting (criterion 1, combinatorial SQL generation).
- Invocation attribution starts at the shipped v35 registry prefix, and the
  resource-minute compatibility case opens with the real older registry and
  compares compatibility floors. The public latest-schema open cannot supply
  those private prefixes (criterion 3, persisted-format safety).
- The registry symlink key check observes private `workspaces_dir`; ordinary
  persisted bindings would remain correct even if the process-local repair
  budget split (criterion 3, confinement).
- The three degraded-read/recovery cases retain private `RepairGate` counters,
  unresolved-target observations and listing-read counters. Equal public rows
  alone do not prove suppression or recovery to indexed hydration (criterion 2,
  fault injection). The strict rebuild-refusal message uses the exported registry
  validator and moves to the boundary.
- Publication's winning-evidence/losing-CAS race uses the private pending-write
  stage callback to force its ordering (criterion 2, deterministic interleaving).
  Its three no-hook pending/reconciliation cases move. The private Git failure
  formatter still guards credential and cache-path redaction (criterion 3): the
  public credential check refuses those arguments before invoking Git.

Each correction names its source identity and seam in the reconciliation; no
production export is widened to make a test move possible.

One originally proposed planner move has a different outcome: ORB-15922
(`e9bb5f2e`) removed the scoreboard aggregate SQL builders before PR4686. The
final reconciliation records
`driver/sqlite/audit_event_store/tests/aggregates.rs::tool_call_queries_search_the_invocation_index_for_every_window_and_limit`
as one of the five retirements, not as a current private-seam keep.

## Orbit-web admission audit boundary

The [per-identity reconciliation](../dev/reviews/unit-test-admission-orbit-web-2026-10-13.json)
moves 32 existing cases into the `http_api` integration binary. Real HTTP responses
cover origin/operator guards, artifact confinement and media types, task lifecycle
and readiness, claim summaries, cancellation, aggregate paging and security headers.
Log formatting uses the snapshot endpoint with the original input tables; the stream
cap case uses real SSE ownership and follows the configured capacity without pinning
its value. Dashboard Node fixtures live beside these integration modules and exercise
the shipped JavaScript with their existing assertions.

`crates/orbit-web/src/lease/tests/intake.rs::token_queued_selector_expansion_retries_do_not_double_count`
is admitted under criterion 3 for the token selector resource bound (ORB-19005,
regressing ORB-18776). It drives the real token intake socket and checks single
and batch retries, a mixed duplicate/new expansion, and refusal of a new 65th
selector without changing the lease. The test stays in the in-crate fixture
because `LeaseIntakeSocket::for_test_listener` is crate-private; an external
integration target cannot start the same socket without widening that seam.

`crates/orbit-web/src/lease/tests/intake.rs::token_expansion_accounts_for_a_near_limit_deadlock_cycle`
is admitted as the public token-intake regression for an expansion whose
deadlock cycle exceeds the frame (ORB-19007). The fixed answer is sized with
`deadlock` null, and committing the cycle made intake return `frame_too_large`
after the queue and notices had changed. The test drives the real intake
socket: a three-lease cycle that fits is delivered whole, and a 32-lease cycle
on a near-limit record is refused with `invalid_input` before any queue change
or notice. It stays beside the selector-bound case for the same private listener.

`crates/orbit-web/src/lease/tests/intake.rs::token_task_holder_prebound_reports_truncation_for_requests_and_expansions`
is admitted under criterion 3 for the reproduced task-holder truncation defect
(ORB-19080). Three in-progress task context holders block one selector. A
4096-byte token answer is sized to fit the first conflict but not its omission
witness; the real intake socket then verifies request and expansion answers,
their queued notices, the fitting prefix and the blocked path. An unbounded
operator request verifies that it still returns all three holders.

Two SSH decisions are corrected under criteria 2 and 4. The delayed-bind control
injects a 500 ms attach budget and a 2 s bind delay; host forwarding fixes that budget
at 5 s and also negotiates identity. The oversized-status case directly observes the
private `healthz_ok` probe returning before its 5 s deadline, then checks short-budget
establish failure and child reaping. A forwarding refusal alone could pass after a
timeout without proving the byte cap. These cases retain `TunnelSpec`, `ProbeLimit`
and the fake SSH/peer harness, alongside all 24 previously admitted private fault,
security and platform cases. No production visibility or behavior changes.

## Reconciliation during a cancellation admission

`crates/orbit-core/src/application/job/run/tests/reconcile.rs::reconciling_a_reaped_worker_mid_cancel_leaves_the_outcome_to_the_canceller`
(criterion 2, deterministic interleaving) reproduces ORB-21876. The private
signal seam of the public cancel path places a reconciling `show_job_run` read
after a real owner is signalled and reaped and before the canceller reads the
run again, the gap in which retained production cancels ended `interrupted`.
No public entry point can stop between those two steps without timing. It also
shows that a bystander process survives and the task returns to the backlog.

`crates/orbit-core/src/application/job/run/tests/reconcile.rs::reconciliation_interrupts_a_dead_owner_once_its_cancellation_cannot_finish`
(criterion 2, crash mid-operation) covers the two exits from that deferral: a
canceller that reported failure and one that vanished without reporting. A
crash between request and completion cannot be produced through a public entry
point, so the fixture records that durable state directly.

## Landing-review gate verdict parse admission

`crates/orbit-core/src/application/job/tests/delivery_attention.rs::abandoned_landing_review_settle_reports_abandoned_verdict`
and `blocked_landing_review_settle_still_parses_verdict_and_count` (criterion 1,
parser edge cases) guard the verdict parse in `landing_review_gate_facts` for
the observed ORB-18242 mislabel: an abandoned landing-review settlement records
no verdict, and its step error quotes the provisional `(verdict incomplete, ...`
parenthetical, which the generic scan misread as the token `incomplete,`.
Both tests drive the public `landing_review_gate_facts` entry point that
`orbit-web`'s Why stuck card consumes, feeding the exact abandoned settle
step-error text (asserting verdict `abandoned` with no finding count) and a
blocked message (asserting verdict `reject` with finding count 1). Reaching
either settlement at a boundary would need a full blocked-task delivery run
through the landing-review settle step, so a boundary fixture cannot prove
this two-token parse economically.

`crates/orbit-core/src/application/job/tests/delivery_attention.rs::blocked_settlement_quoting_abandoned_marker_still_parses_verdict_and_count`
(criterion 1, parser edge case) guards the ORB-18244 mislabel: a blocked
settlement embeds the reviewers free-text escalation verbatim, so the
escalation may quote the `review_abandoned:` marker, which the `contains`
check misread as an abandoned settlement. The test drives the public
`landing_review_gate_facts` entry point with a blocked message whose
escalation quotes the marker, asserting verdict `reject` with finding count 2.

`crates/orbit-core/src/application/job/tests/delivery_attention.rs::abandoned_landing_review_settle_step_error_reports_abandoned_verdict`
(criterion 1, parser edge case) guards the recorded shape of an abandoned settlement.
The step error is the refusal's `Display`, which prefixes the settlement with the
refused action (`deterministic action \`review_gate_settle\` refused: review_abandoned: ...`).
The verdict parse anchors the marker after that prefix, so the abandoned settlement
reports `abandoned`; anchoring on the bare settlement text alone misread it as `incomplete`.

`crates/orbit-engine/src/activity_job/workspace/tests/boundary_guard.rs::stationary_primary_generation_records_are_benign`
(criterion 1, policy matching) guards ORB-23736: a stationary primary whose
only dirt is Orbit's own generation records (`.generation-compat.json`,
`.generation-image-digest.json`, a `.generation-participants/` record) is
benign, while a stationary source path outside those records and outside
`.orbit/` still fails as `primary_checkout_drift`. It drives the production
`WorktreeBoundaryGuard::verify` entry point over two disposable Git checkouts
with real fingerprints, proving both the benign and the still-drift rows plus
that the guard stages, commits, resets, or cleans nothing. A full pipeline
with linked worktrees and a provider run cannot prove this guard cheaply.
