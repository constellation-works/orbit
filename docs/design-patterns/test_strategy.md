---
type: pattern
summary: "Boundary-First Testing: integration, golden and e2e by default; unit tests by exception"
last_validated: 2026-10-23
---
# Boundary-First Testing

Test behaviour where callers meet it. A new test is written when a bug or defect is fixed: one regression test that fails before the fix, preferably as a row in an existing table-driven test. Features, refactors and chores add no new tests unless an acceptance criterion asks for one; the existing suite gates them, and a change updates the tests it breaks. When a regression test is written, it goes at a stable boundary by default: a crate-root integration test, a golden, or an e2e flow. A unit test is the exception, and it must earn its place by meeting one of the admission criteria below.

Gate time is a finite budget. Optimize the suite for the critical failures it can detect and the time it costs, rather than its test count.

## Quality and the 10-minute budget

The complete **Check / Clippy / Test** CI job must finish in **at most 10 minutes (600 seconds) of elapsed wall time at p90 over PR runs**. Measure from the job's start to completion, including setup, compilation, checks, linting, tests and required capability checks. Report queue wait separately: the wait before the Check job starts, and the part of its elapsed time the awaited jobs spent queued after it started. The workspace tests run in separate jobs (a test build and parallel nextest shards, [DEVELOPMENT.md](../DEVELOPMENT.md#hosted-ci)) that the Check job waits for, so its elapsed time covers them, including their runners' queue wait. Its `ci-await:` lines give each awaited job's start and completion. A fast filtered run, a smaller test count or a sum of parallel testcase durations does not establish that the full job meets this budget. This is an acceptance requirement; documenting it does not claim the current suite already meets it.

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

The recurring `test-quality-audit` creates evidence-backed proposed tasks for critical coverage gaps, weak assertions, duplication, flaky or slow fixtures and regressions against this budget. It searches existing tasks before filing, links each finding to the audit, and records concrete acceptance criteria. The audit leaves a durable report even when no new finding is confirmed; it does not treat a quota of findings or deletions as success.

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

Removing tests still passes with stale baseline entries; those entries do not admit different tests in their place. Run `scripts/unit-test-inventory.py` for the current counts and identities, and `scripts/unit-test-inventory.py --write-baseline scripts/unit-test-baseline.json` to regenerate the baseline, including removing retired identities. The inventory scans test attributes without evaluating `cfg` or expanding macros, as in the original retirement measure.

Admitted unit tests:

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

- The [launchd enable tests](../../crates/orbit-core/src/application/routines/clock/tests/enable.rs) (criterion 2,
  fault injection) drives the enable entry point with launchd selected and failed
  program/state probes. Version outcomes use the inspection probe seam rather
  than executing a fixture binary: healthy, timeout, empty output and unreachable
  outcomes are deterministic under host contention. It checks rewrite and reload for unhealthy loaded agents,
  error propagation when loading fails, and preservation of healthy and paused
  agents. The public API selects the native host platform, so an integration test
  on Linux cannot force these launchd states.

- The systemd activation matrices in
  `crates/orbit-core/src/application/routines/clock/tests/enable.rs`
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

- `crates/orbit-core/src/application/task/tests/transitions.rs::delete_rechecks_a_status_changed_after_its_guard_and_preserves_force` (criterion 2, deterministic interleaving). The existing transition-read seam commits an in-progress status after deletion's initial proposed/rejected check, before the task write lock. The isolated fixture asserts refusal leaves the task present, with forced deletion and unchanged eligible statuses as controls. Crate-root integration tests cannot reach the `cfg(test)` seam to choose this ordering reliably.

- `crates/orbit-common/src/process/tests/bounded.rs`: `argv_probe_recovers_when_a_live_child_outgrows_the_queried_buffer` (criteria 2 and 4, native fault injection). A pipe handshake grows a live macOS child's argv between the size query and read. The stale buffer yields an unreadable record, which must not prove exit or authorize cleanup; the probe grows its buffer and recovers the recorded identity. Darwin can return success with a truncated `KERN_PROCARGS2` record, so handling only `ENOMEM` is insufficient. The public bounded runner cannot force this observer interleaving.

- `crates/orbit-common/src/process/tests/bounded.rs`: `leak_assertion_retries_unreadable_argv_without_accepting_it_as_reaped` (criterion 2, fault injection). An injected argv probe and clock force a read failure followed by readable live argv, an unreadable probe throughout the grace budget, or a confirmed exit. It checks the leak assertion's panic classification and full retry budget without racing macOS sysctl or generating host load. The public bounded runner cannot force these observer failures; the native blocked-writer test covers pid-only cleanup.

- `crates/orbit-common/src/process/tests/bounded.rs`: `bounded_run_returns_when_a_descendant_holds_either_pipe`, `bounded_run_enforces_deadline_while_a_stream_stays_readable` and `bounded_run_bounds_post_exit_drain_while_a_detached_writer_continues` (criterion 4, platform-kernel behaviour). A descendant keeps one output pipe open after the leader exits; sixteen `cat /dev/zero` writers keep a stream readable past the deadline; `setsid` writers outside the owned group keep filling a pipe during the post-exit drain. Each case's leak check and watchdog are this file's argv-identity harness (`assert_reaped`, `FloodCleanup`). It reads Linux `/proc` or macOS `KERN_PROCARGS2` and signals a pid only when its argv carries the recorded marker, and the admitted cleanup and leak-assertion tests above prove that harness safe. A crate-root copy would duplicate the native harness without those tests, so the public runner is not the cheaper boundary for these three.

- `orbit-engine`'s `branch_lookup_retries_only_supervisor_timeouts_with_a_bounded_budget` (criterion 2, fault injection). A stub Git stalls until the supervisor deadline, then succeeds, exhausts the budget, or completes with a refusal. It drives the branch lookup used by PR preparation and verifies the timeout class, elapsed time, bounded backoff and attempt count. The private Git budget guard scales the deadline; `pr_prepare` does not expose that guard through its public action boundary.

- `crates/orbit-engine/src/activity_job/cli_runner/supervisor/tests/stall.rs::silent_sleep_stalls_while_cpu_and_output_keep_the_run_alive` (criterion 4, process-tree sampling and process-group cleanup) runs a silent sleeper, a CPU-burning descendant, a periodic printer and a disabled-watch sleeper through the real supervisor. The short window makes the kernel process-tree samples and group termination observable in one bounded fixture; a dispatcher integration fixture would duplicate the real child process cases and the existing supervisor coverage.

- `crates/orbit-engine/src/activity_job/cli_runner/orchestrator/tests/completion.rs::a_stalled_provider_fails_transient_without_a_wall_clock_timeout` (criterion 1, terminal-outcome mapping) checks the stalled and wall-clock-timeout rows through completion projection. It preserves the distinct `stalled` and `timed_out` outputs, transient marker, step error code and claim retry class without running another provider process; the supervisor test above covers the actual stall detection.

The [orbit-config audit delivery](../dev/reviews/unit-test-audit-orbit-config-delivery.md) maps the retired and relocated identities to their public boundaries. Config loading, compatibility warnings, environment projection, plugin toggles and store admission live in the existing crate-root integration binaries. The malformed-pool grammar table remains admitted under criterion 1; ordinary pool loading uses the public loader.

- `crates/orbit-config/src/tests/seed.rs`: `every_family_combination_seeds_valid_defaults_pools_and_enabled_flags` (criterion 1, combinatorial seed rendering) checks all detected-family subsets in memory: defaults name enabled crews, pools parse and reference enabled crews, and each crew's enabled flag follows its provider availability. It avoids filesystem setup for every subset. `rendering_refuses_a_pool_entry_the_seed_does_not_write_enabled` (criterion 2, fault injection) removes or disables a dynamically selected pool member in the private renderer's crew map; the public seeder always constructs that map itself. Neither test fixes crew choices, models or weights as policy.

- `crates/orbit-web/src/log_format/tests/scan.rs`: `post_window_errors_do_not_consume_the_match_budget_or_advance_coverage` (criterion 1, combinatorial scan budgets and coverage). A table drives the existing segmented-reader seam with complete, capped and retention-limited windows and excess newer records. The HTTP boundary fixes the process scan budget at 50,000; this seam proves those cases with a handful of rows, while the HTTP month test covers endpoint wiring and agreement with stderr.

- The four [worker log sibling tests](../../crates/orbit-core/src/application/job/pipeline/worker/tests/log.rs) — `appends_after_the_size_snapshot_are_excluded_from_the_bounded_read`, `empty_short_and_exact_limit_logs_keep_their_expected_content`, `long_logs_report_the_snapshot_window_and_return_only_its_tail`, and `shrink_after_the_size_snapshot_reports_only_bytes_still_available` (criterion 2, deterministic interleaving). A captured size is passed through the same bounded reader used by the audit and supervisor callers; the append case proves bytes written after that snapshot cannot exceed the 16 KiB window, while controls cover empty, short, exact-limit, long and shrinking files. The callers do not lock out the append-oriented worker, and an integration test cannot force the metadata/read order reproducibly [ORB-15610].
- `crates/orbit-core/src/application/job/pipeline/worker/tests/supervisor.rs`: `store_read_faults_retain_supervision_and_reap_in_a_live_parent` (criterion 2, fault injection). A SQLite store wrapper injects unreadable and missing startup records and a post-exit read fault while a socket rendezvous bounds a real child. The isolated parent asserts fresh-state settlement and OS reaping before it exits; the public runtime cannot inject these observer reads.
- The [admissions-stop regressions](../../crates/orbit-core/src/application/job/run/tests/admissions_stop.rs) and `crates/orbit-core/src/application/job/run/tests/drain_cancel.rs`: initialization races (criterion 2, deterministic interleaving). One-shot seams after `NoState` force a worker's first checkpoint and child dispatch, or terminalization with and without that checkpoint, before control initialization. They verify preserved progress and lineage, a merged control for a live drain, and no new effective control for a terminal drain. Crate-root integration tests cannot force this transaction ordering [ORB-15611].
- `crates/orbit-cli/src/tests/main.rs`: `every_assembled_leaf_accepts_json_and_lists_domain_input_exclusions` (criterion 1). It walks the assembled command tree, including hidden and plugin-derived leaves that help goldens cannot enumerate, and asserts that every leaf accepts `--json`. The only exclusion is a plugin tool input named `json`.
- `crates/orbit-cmd/src/doctor/tests/worker_token.rs`: `clock_file_is_ready_only_when_it_holds_a_credential_the_workspace_admits` (criterion 1). It is a table over every pairing of clock-file credential names with the workspace's effective pass, asserting that the clock is ready only when an admitted credential is held, and that a held credential the pass omits is named and warned. The boundary needs an installed launchd or systemd clock to reach this classification. `absent_empty_refused_and_mismatched_clock_files_stay_distinct` (criterion 1) pins that absent, empty, refused and mismatched files stay distinct states.
- Orbit-core's missing-directory and relative-path disk cases now run through the exported native host-resource probe in `tests/runtime/admission_audit/disk_probe.rs`. The private platform tests retain the injected CPU counter/time/wait matrices below.

- `crates/orbit-runtime/src/host_resource/tests/platform.rs`: `cpu_window_reuses_advancing_or_repeated_ticks_without_refreshing_the_baseline`, `cpu_window_missing_stale_future_or_identical_baselines_take_a_real_window`, `cpu_window_native_failures_clock_reversal_and_stalls_remain_unknown`, and `cpu_window_handles_tick_wrap_and_the_freshness_boundary` (criteria 1 and 2). Injected counters, timestamps and waits cover XNU cache plateaus, expiry endpoints, failed reads, clock jumps and counter wrap deterministically without generating host load or waiting for a real wrap. The macOS CLI integration test covers actual cross-process history reuse and cold-start recovery.
- `crates/orbit-store/src/driver/sqlite/task_registry/tests/mod.rs`: `fresh_open_through_symlinked_root_matches_reopen_by_canonical_root` (criterion 3). It guards symlink confinement of the registry root: a fresh open through a symlinked root and a later open of the canonical root must key one `workspaces_dir`, because the repair gate keys its attempt budget on that path. `workspaces_dir` is crate-private, so no public-surface test can observe the split.
- `crates/orbit-store/src/repository/task/coordination/tests/admission.rs`: `a_grant_never_takes_a_footprint_the_owner_wave_admitted_before_its_section` and `an_owner_wave_after_a_grant_refuses_the_claimed_footprint` (criterion 2, deterministic interleaving). The first admits an owner drain wave through the crate-private `after_selection` seam, between a pull grant's selection and its exclusive section, and asserts the grant neither takes the admitted task nor one overlapping its footprint. The second commits the grant first and asserts the wave's section refuses the claimed footprint even though the wave's snapshot predates the claim. At the runtime boundary the claim's task is already `in-progress` before any later drain snapshot, and nothing can land a grant between a wave's snapshot and its section, so neither ordering is reachable there.
- `crates/orbit-store/src/repository/task/coordination/tests/claim_section.rs`: `a_claim_mutation_section_decodes_its_own_claim_and_syncs_only_its_commit` and `a_slow_claim_apply_holds_its_bundle_and_not_its_partition` (criterion 2, fault injection). The first counts, through the crate-private section probe, the claim-lifecycle rows decoded and the durability syncs requested inside a bind's and a budgeted release's exclusive section among other bound claims, asserts both stay under the pre-change baseline, and asserts an exact retry takes no exclusive section. The second holds a claim commit's apply, which runs after its admission section, on a release channel through a `#[cfg(test)]` seam. While it is held, the partition's hold report has already been emitted, another admission decides, and an envelope read sees the decided bind. A full task read waits on the task's bundle lock: the test waits for that read's contention diagnostic (the bundle lock's default 3 s warning) before releasing the writer, proves the read has not returned while the bind is half-applied, and checks the bind and its history after release. Reader assertions use channel ordering and load-tolerant hang guards, with no sleep or cross-thread completion-time bounds [ORB-15820]. Neither the counters nor the stall seam exist in the build the crate-root tests compile against [ORB-15797]. `exclusive_claim_sections_stay_short_under_concurrent_durable_mutations` is `#[ignore]`d and run on demand: it measures exclusive admission hold p50/p95 for concurrent claim mutations on a real disk with real syncs, and asserts those sections request no durability syncs [ORB-16389].
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

- `crates/orbit-config/src/tests/machine.rs::host_shutdown_hold_minutes_defaults_and_rejects_non_positive_values` (criterion 3, fail-closed crafted input). Zero is rejected by the resolver with a message that names `machine.host_shutdown_hold_minutes`; a negative fails the integer read and names the same key. The default 180 and an uncapped 10080 are controls in the same test. This admission [ORB-15796] was added after the pinned orbit-config audit and remains unchanged by that slice; the public workspace `[machine]` refusal now lives in the crate-root review-layer integration target.
- `the_hold_includes_the_horizon_and_a_later_instant_admits` (criterion 1), beside `crates/orbit-runtime/src/host_signal.rs`. One table pins the horizon arithmetic: equality holds, one second later admits, and a past timestamp holds. The pull and local drain tests, and the sweep test below, place the schedule with `Utc::now()` at minute granularity, so they cannot pin the one-second boundary without a race [ORB-15796].
- `crates/orbit-core/src/application/routines/tests/sweep.rs::a_shutdown_inside_the_hold_horizon_skips_the_sweep_and_one_beyond_it_fires` (criterion 2). The public `test_support::run_sweep_at` hardcodes the default host-signal probe, so a crate-root test cannot place a shutdown on either side of the horizon. The test drives `pub(crate)` `run_sweep_at_with_providers_at` with an injected clock and a 30-minute horizon, and asserts the tick skips at the horizon and fires one minute past it. A hardcoded 180 would still hold that far side [ORB-15796].

- `schema` in `crates/orbit-runtime/src/recovery_authority/tests/mod.rs` (criterion 3, persisted-authority safety): exercises adoption from a frozen pre-ledger SQL fixture across the shipped certificate and optional binding shapes, preserving each existing SQLite table definition and row, and checks newer-schema writer/read-only/refusal decisions and WAL sidecar retention after those opens close. Host authority is private and cannot be seeded or opened directly through the public leaf runtime API without weakening its trust boundary. Mutable cases re-execute in isolated children.

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
integration module, with fixture cases grouped under `response/projection.rs`
and `response/envelope.rs`. Exported `normalize_cli_stdout`,
`project_cli_response`, `latest_assistant_message`,
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

Two SSH decisions are corrected under criteria 2 and 4. The delayed-bind control
injects a 500 ms attach budget and a 2 s bind delay; host forwarding fixes that budget
at 5 s and also negotiates identity. The oversized-status case directly observes the
private `healthz_ok` probe returning before its 5 s deadline, then checks short-budget
establish failure and child reaping. A forwarding refusal alone could pass after a
timeout without proving the byte cap. These cases retain `TunnelSpec`, `ProbeLimit`
and the fake SSH/peer harness, alongside all 24 previously admitted private fault,
security and platform cases. No production visibility or behavior changes.

## Landing-review gate verdict parse admission

`crates/orbit-core/src/application/job/delivery_attention/tests.rs::abandoned_landing_review_settle_reports_abandoned_verdict`
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

`crates/orbit-core/src/application/job/delivery_attention/tests.rs::blocked_settlement_quoting_abandoned_marker_still_parses_verdict_and_count`
(criterion 1, parser edge case) guards the ORB-18244 mislabel: a blocked
settlement embeds the reviewers free-text escalation verbatim, so the
escalation may quote the `review_abandoned:` marker, which the `contains`
check misread as an abandoned settlement. The test drives the public
`landing_review_gate_facts` entry point with a blocked message whose
escalation quotes the marker, asserting verdict `reject` with finding count 2.

`crates/orbit-core/src/application/job/delivery_attention/tests.rs::abandoned_landing_review_settle_step_error_reports_abandoned_verdict`
(criterion 1, parser edge case) guards the recorded shape of an abandoned settlement.
The step error is the refusal's `Display`, which prefixes the settlement with the
refused action (`deterministic action \`review_gate_settle\` refused: review_abandoned: ...`).
The verdict parse anchors the marker after that prefix, so the abandoned settlement
reports `abandoned`; anchoring on the bare settlement text alone misread it as `incomplete`.
