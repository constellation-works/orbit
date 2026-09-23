---
type: context
summary: Root cause analysis of the cross-crate recursive test-worker fork bomb that exhausted memory and forced a hard reset of the Linux execution host.
incident_date: 2026-09-23
last_validated: 2026-09-23
tags: [incident, rca, pipeline, testing, operations]
paths: ["crates/orbit-core/src/application/job/pipeline/worker/**", "crates/orbit-web/src/api/**", "crates/orbit-core/src/application/routines/clock.rs"]
related_artifacts: [ORB-12887, ORB-12902, ORB-12903, F2026-09-210, ORB-11418, ORB-11415, F2026-09-042]
---

# Cross-crate recursive test-worker OOM

- **Incident date:** 2026-09-23
- **Affected system:** Orbit Linux execution host (KVM guest, 14 vCPU, 26 GiB RAM, 4 GiB swap)
- **Severity:** Full host outage for about 25 minutes, ended by a hard reset. Every in-flight run was lost. No data loss was observed.
- **Status:** Contained. The triggering task is blocked, and the corrective work is tracked separately.
- **Recurrence of:** [2026-09-06 recursive test-worker fork storm](2026-09-06-recursive-test-worker-fork-storm.md)

## Executive summary

At first the outage looked like a network partition: the host stopped
answering, and its logs show VPN, DHCPv6 and DNS timeouts. The network was not
the cause. The host ran out of memory. A pipeline agent validating ORB-12887
wrote an `orbit-web` test that drove the new `POST /api/jobs/:id/run` handler
into the real `OrbitRuntime::submit_job_run`. Submission spawns a detached
pipeline worker by re-executing `current_exe()`.

After the 2026-09-06 incident, ORB-11418 made that spawn path substitute a
test-only worker program and fail closed when none is installed. The guard is
written as `#[cfg(test)]`, and that `cfg` is set only when `orbit-core` itself is
the crate under test. When `orbit-core` is compiled as a dependency of
`orbit-web`'s test binary, the guard is compiled out and the production branch
runs.

The "current executable" was therefore the `orbit_web` libtest harness. It was
started as `<harness> job run-pipeline-worker <run_id>`, and libtest read that
argv as test-name filters. The filter `job` matched about 66 `orbit-web` tests,
including the one that spawns, so every worker spawned another worker. Workers
are `setsid()`-detached, so the agent's `cargo test` reported success within a
second while the recursion continued in the background.

About eight minutes later the recursion had reached 1,320 live harness
processes and about 18 GiB of anonymous memory. RAM and swap were exhausted,
and the kernel OOM killer fired repeatedly. The whole guest stalled for
several minutes at a time. The host was hard-reset about 12 minutes after its
last journal entry.

This is the same recursion mechanism as 2026-09-06. The earlier fix closed the
hole only inside `orbit-core`. The runtime-containment and saturation-visibility
follow-ups from that RCA had not landed, so the host again had no layer between
one test run and a machine-wide failure.

## Impact

- The host was unusable from about 02:25 to 02:48 UTC and had to be hard-reset.
  Interactive sessions, the dashboard, the MCP server and the sweep clock all
  went down with it.
- `orbit-web.service` was OOM-killed several times; the first kill was at
  02:25:51. Pipeline workers run inside that service's cgroup, so every
  in-flight run was interrupted:
  - `jrun-20260923-0209-c3` (ORB-12887, the trigger)
  - `jrun-20260923-0221-c1` and `jrun-20260923-0221-c3` (ORB-12893)
  - `jrun-20260923-0222-t1` (ORB-12898)

  All four are recorded as `interrupted / process_not_found`.
- The visible symptoms pointed operators at the network rather than memory.
  The mesh-VPN agent logged relay and coordination timeouts, `systemd-networkd`
  timed out on DHCPv6 and marked the primary NIC `Failed`, and
  `systemd-resolved` flushed caches under memory pressure. Diagnosis needed a
  kernel-log review of the previous boot.
- No repository corruption and no loss of task or artifact data was observed.
  ORB-12887's uncommitted work survived in its run worktree.

## Timeline

All timestamps are UTC on 2026-09-23. Agent commands come from the preserved
agent transcript. Host events come from the previous boot's journal.

| Time | Event |
| --- | --- |
| 02:09:57 | Pipeline run `jrun-20260923-0209-c3` started for ORB-12887 (crew `sol`). |
| 02:14:25 | The agent ran `cargo test -p orbit-web authorized_sweep_job_run_submits_a_persisted_run` in the background. This was the first invocation that could spawn a worker. Its completion was not captured. |
| 02:16:44 | The agent ran `cargo test -p orbit-web job_run`. This filter matches the new spawning test. |
| 02:17:07 | That invocation reported `test result: ok. 11 passed`, so the parent harness exited cleanly. This is the latest point by which detached recursive workers must have been running. |
| 02:17–02:23 | The agent ran overlapping background validation: several `make ci-fast` runs in the run worktree and in a nested baseline worktree, one `make goldens` run, and `cargo test -p orbit-web --lib`. Concurrent `rustc` jobs added memory pressure. |
| 02:25:34 | First OOM-killer invocation. The process table held 1,320 `orbit_web-106f5` harness processes (about 18 GiB anonymous RSS in total), and free swap was 212 kB. |
| 02:25:51 | `orbit-web.service` was OOM-killed (`Failed with result 'oom-kill'`, 24.7 G memory peak). `Restart=always` restarted it. |
| 02:26–02:30 | Further OOM kills. The guest stalled: the kernel logged hung-task reports, and `systemd-journald` was killed by its watchdog. |
| 02:31:00 | `systemd-networkd`: `Could not set DHCPv6 address: Connection timed out`, and the primary NIC was marked `Failed`. |
| 02:31:02 | The VPN agent logged `time jump detected (slept 4m11s)`: the whole guest had been frozen for about four minutes. A second OOM snapshot showed 2,227 harness processes. |
| 02:36:51 | Third OOM event, with 1,188 harness processes plus 12 `rustc` processes. |
| 02:36:55 | Last journal entry of the boot. The journal shows no shutdown. |
| 02:38:00 | A restart attempt of `orbit-web.service` logged dozens of `Found left-over process … (orbit_web-106f5) in control group`. The recursive workers had survived every service restart. |
| 02:48:45 | The host booted again after a hard reset. |

## Technical causal chain

1. ORB-12887 asked for a dashboard endpoint that starts a job "through the same
   runtime path the CLI `orbit run job` uses". The agent implemented
   `run_job_action` in `crates/orbit-web/src/api/jobs.rs` and covered it with
   `authorized_sweep_job_run_submits_a_persisted_run`. That test sent an
   authorized request through the router into the real
   `OrbitRuntime::submit_job_run`. The code was never committed; it survives only
   in the run worktree.
2. [`WorkerCommandConfig::build`](../../crates/orbit-core/src/application/job/pipeline/worker/command.rs#L216)
   selects the worker program at compile time. Under `#[cfg(test)]` it requires
   the thread-local
   [`worker_command_override`](../../crates/orbit-core/src/application/job/pipeline/worker/command.rs#L152)
   and fails closed without it. That is the ORB-11418 fix. Under
   `#[cfg(not(test))]` it re-executes `std::env::current_exe()`.
3. `cfg(test)` is a per-crate compile flag. When `orbit-web`'s tests are built,
   `orbit-core` is compiled as an ordinary dependency, so its production branch
   is compiled in. The override module does not exist in that build either, so
   `orbit-web` tests had no way to install a safe worker.
4. `current_exe()` resolved to
   `target/debug/deps/orbit_web-106f5a9f2d26e2cf`, the libtest harness. The
   worker argv `job run-pipeline-worker <run_id>` became three substring
   filters. `job` selected about 66 tests, including the spawning test.
5. The worker supervisor calls `setsid()` on each child, so every generation
   was detached from its parent. The agent's foreground `cargo test` finished
   green in 0.78 s and gave no sign that anything was still running.
6. Each child harness re-entered the spawning test and launched another
   detached worker. The children did not exit in step with their parents, so
   the live process count kept rising. It was 1,320 at the first OOM event and
   2,227 at the second.
7. The workers were descendants of the dispatching service, so they all
   lived in `orbit-web.service`'s cgroup. That unit runs with
   `MemoryMax=infinity` and `TasksMax=32710`. Nothing constrained the growth
   before the global OOM killer.
8. `orbit-web.service` uses `KillMode=process`, on purpose, so a service
   restart does not SIGKILL in-flight workers. That same setting meant the OOM
   kills and restarts of the service removed only its main process. The
   runaway harnesses stayed in the cgroup, and memory never recovered.
9. With RAM and swap exhausted, the guest stopped scheduling useful work.
   Networking, the journal and the VPN all timed out, which made the incident
   look like a network partition. Recovery required a hard reset.

## Cause classification

### Confirmed

- The kernel OOM process tables show 1,320, 2,227 and 1,188 processes named
  `orbit_web-106f5` (the 15-character truncation of
  `orbit_web-106f5a9f2d26e2cf`) at the three OOM events. Their summed anonymous
  RSS was about 18 GiB at the first event.
- Every OOM event names `task_memcg=…/app.slice/orbit-web.service` and
  `global_oom`, and reports swap nearly exhausted (132–212 kB free).
- The run worktree contains the uncommitted test that calls the real
  `submit_job_run` through the new handler.
- The `cfg(test)` / `cfg(not(test))` split in `WorkerCommandConfig::build`, and
  the production argv `job run-pipeline-worker <run_id>`, are in current
  source. The module's own doc comment names this exact recursion hazard.
- The agent transcript records `cargo test -p orbit-web job_run` passing at
  02:17:07. That filter includes the spawning test.
- `systemd` recorded `orbit-web.service` as `oom-kill` at 02:25:51, and later
  logged left-over `orbit_web-106f5` processes in its control group at 02:38.
- The guest froze for about 4 minutes (`time jump detected (slept 4m11s)`,
  journald watchdog, hung-task reports) before the network-looking failures
  appeared.

### Supported inference

- The first spawning invocation was the targeted run at 02:14:25 or the
  `job_run` run at 02:16:44. The 02:22:37 full `--lib` run and any tests inside
  `make ci-fast` probably re-armed the recursion. The source makes every
  invocation that selects the test unsafe.
- The agent's overlapping background builds and tests (two worktrees, goldens,
  a full `--lib` suite) added memory pressure but did not cause the outage. The
  harness processes accounted for the large majority of anonymous memory at
  every OOM event.
- The hard reset at 02:48 was an operator or hypervisor action. The guest
  journal records no shutdown.

### Unknown

- Whether the recursion was a single chain or branched: whether more than one
  test in a child harness spawns a worker. It is also unknown why child
  harnesses stayed alive instead of exiting once their test list finished.
- The exact invocation that created the first detached worker.
- Whether the child argv carried a `--root` override. If it had, libtest would
  have rejected the argv; the observed recursion implies it did not.

## Root cause and contributing factors

The direct root cause is that the test-worker safety guard is scoped by
`#[cfg(test)]`, a flag that covers only `orbit-core`'s own test build. Tests in
any downstream crate that reach the real submission path (`orbit-web`,
`orbit-cli`, plugins) silently get the production `current_exe()` re-exec. Under
libtest that re-exec is recursive.

The failure spread to the whole host because of these contributing factors:

- **The 2026-09-06 hardening was scoped to the crate where the incident
  happened.** ORB-11418 removed the unsafe `orbit-core` tests and made that
  crate's test build fail closed. It did not cover crates that depend on
  `orbit-core`, and there was no runtime check (for example, "the worker
  executable is a cargo test harness") independent of compile flags.
- **Detached workers make a recursive test look green.** `setsid()` means the
  test that triggers the recursion passes, and the agent gets no signal.
- **No resource bound on worker runs.** Workers inherit the dispatching
  service's cgroup, which has no memory limit and a very high task limit. ORB-11415
  ("make host resource saturation visible") from the previous RCA was parked
  in `someday`, and runtime containment was never tasked.
- **`KillMode=process` preserves runaways.** The property that protects
  in-flight workers across a service restart also keeps a runaway tree alive
  through every OOM kill and restart of the service.
- **The agent ran many overlapping background validations** across two
  worktrees, which added compiler memory pressure while the recursion grew.
- **The symptoms pointed at the network.** When memory runs out, the network
  stack, VPN and DNS are the first things an operator notices failing.

## Why it happened on this date

No new defect was introduced on 2026-09-23. The hole has existed since
ORB-11418 (commit `7b0bbff08`, 2026-09-06) put the guard behind `cfg(test)`,
and since ORB-10801 (commit `fa44a7107`, 2026-08-15) made job runs spawn
detached workers by default. Before ORB-12887, no `orbit-web` test had driven a
real job submission. ORB-12887 was the first task whose natural test reached
`submit_job_run` from outside `orbit-core`.

## Detection and response

Detection was manual and late. The operator noticed the host was unreachable
and suspected a network partition. After the reboot, the previous boot's
journal showed OOM kills rather than link-level failures. Summing the kernel
OOM process table by command name isolated one test harness with more than a
thousand instances. The harness path led to the run worktree, its diff and the
agent transcript.

Containment:

- The hard reset cleared the process tree.
- ORB-12887 was moved to `blocked`, with a dependency on ORB-12902, so no
  dispatcher re-runs its unsafe test.
- The host was checked after the reboot: no `orbit_web-*` processes remained
  and networking was healthy.

For a quick triage recipe, see F2026-09-210. Run `journalctl -b -1 -k` and
grep for `Out of memory|invoked oom-killer`. Then sum the OOM dump's process
table by command name. Hundreds of copies of a `<crate>-<hash>` name means a
libtest harness is re-executing itself.

## Follow-up tracking

| Record | State at RCA | Purpose |
| --- | --- | --- |
| F2026-09-210 | Triaged friction | Incident evidence and diagnosis recipe. Resolved by ORB-12902. |
| ORB-12902 | Proposed | Make the worker spawn unable to re-exec a test binary from any crate's tests: expose a test-support override to downstream crates and add a runtime fail-closed check. |
| ORB-12903 | Proposed | Launch each pipeline worker in its own bounded transient unit (memory and task limits), with a distinct failure reason when a run hits a limit. |
| ORB-12887 | Blocked on ORB-12902 | The originating feature. Its test must use the supported override before it is re-dispatched. |
| ORB-11415 | Someday (from the 2026-09-06 RCA) | Host saturation visibility for orchestrators. This recurrence argues for reprioritizing it. |

Until ORB-12903 ships, an operator can bound the existing services with
user-level drop-ins (`MemoryHigh`, `MemoryMax`, `TasksMax` on
`orbit-web.service` and the sweep service), applied live with
`systemctl --user set-property`. Do not restart `orbit-web` mid-run to apply
them. This RCA does not apply or authorize that change.

## Evidence retained

- The previous boot's kernel and user-manager journal, including three full
  OOM process tables and the `orbit-web.service` OOM and left-over-process
  records.
- The agent transcript for `jrun-20260923-0209-c3`, with the timestamped test
  invocations and their results.
- The run worktree for `jrun-20260923-0209-c3`, with the uncommitted diff and
  the `orbit_web-106f5a9f2d26e2cf` harness binary.
- Orbit run records for the trigger run and the collateral runs.
- Current source for `WorkerCommandConfig::build`, `worker_command_override`
  and the worker supervisor's `setsid()` call.

This document records the best-supported causal account as of 2026-09-23. If
more host telemetry becomes available, update the confirmed, inferred and
unknown sections rather than silently strengthening the claims.
