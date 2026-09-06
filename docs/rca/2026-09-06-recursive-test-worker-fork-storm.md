---
type: context
summary: Root cause analysis of the recursive Orbit test-worker fork storm that saturated a 14-CPU Linux host.
incident_date: 2026-09-06
last_validated: 2026-09-06
tags: [incident, rca, pipeline, testing, operations]
paths: ["crates/orbit-core/src/application/job/**", "crates/orbit-core/src/application/tests/**", "crates/orbit-core/src/adapter/tool_host/tests/**"]
related_artifacts: [ORB-11365, ORB-11414, ORB-11415, ORB-11416, F2026-09-042]
---

# Recursive test-worker fork storm

- **Incident date:** 2026-09-06
- **Affected system:** Orbit Linux execution host
- **Severity:** Host saturation; no data loss observed
- **Status:** Contained; corrective work tracked separately

## Executive summary

An Orbit agent run validating ORB-11365 triggered a latent recursion path in the
`orbit-core` test binary. A workflow-tool test submitted real pipeline runs
without installing the test-only worker-command override. The production
fallback re-executed the current binary as a detached pipeline worker; under
`cargo test`, that binary was the libtest harness, not the Orbit CLI. The worker
arguments were interpreted by libtest as filters and recursively entered the
test suite.

The recursive suites reached a pipeline fixture that detached a shell and
waited for a sentinel file using this unbounded loop:

```sh
while [ ! -f "$ORBIT_TEST_OWNER_RELEASE" ]; do sleep 0.01; done
```

The sentinel was written only on the test's normal success path. A panic,
timeout, or terminated parent could bypass that write and remove the temporary
directory containing the sentinel path. The detached shell then had no bounded
exit or panic-safe owner, and every 10 ms iteration launched an external
`sleep` process. Repeated recursive entries accumulated hundreds of waiters
inside the still-live run sandbox.

At observation time the 14-CPU host was fully busy with a load average of
393.68. It had 831 processes, 162 in uninterruptible sleep, and the affected
sandbox had 386 direct children, including 382 copies of the shell waiter.
Cancelling the stale run removed the descendant tree; blocked processes fell to
zero and CPU returned to 90--96% idle. Available memory remained about 21 GiB,
so this was a CPU, scheduler, and process-creation incident rather than memory
exhaustion. The operator reports roughly three hours of lost time; the owning
run itself remained active for about 113 minutes.

## Impact

- The host was effectively unavailable for useful work while CPU and scheduler
  capacity were consumed by the process storm.
- The affected run, `jrun-20260906-0508-11`, remained alive after task ORB-11365
  had been marked done.
- The agent and operator received no early host-saturation signal. Diagnosis
  began only after the host was already severely degraded.
- No memory exhaustion, repository corruption, or task-artifact loss was
  observed.
- With 382 waiters and a 10 ms interval, the fixture requested up to roughly
  38,200 external `sleep` launches per second before scheduler contention. This
  is a rate implied by the loop, not a measured completed-fork rate.

## Timeline

All timestamps are UTC on 2026-09-06.

| Time | Event |
| --- | --- |
| 05:08:25 | Pipeline run `jrun-20260906-0508-11` started for ORB-11365 with a three-hour agent wall timeout. |
| 05:13:29 | The agent began focused validation of the artifact changes. |
| 05:18:56 | The transcript records two `cargo test --workspace` invocations routed through output filters. |
| 05:21:31 | The agent ran another workspace suite with `--no-fail-fast`, again through output filtering. |
| 05:26:48 | The agent began focused validation involving `workflow_tools`. |
| 05:27:51 | A baseline comparison ran `cargo test -p orbit-core --lib workflow_tools`. |
| 06:08:47 | Another full workspace suite ran with `--no-fail-fast`. |
| 06:53:12 | The agent's remote branch received its implementation commit. |
| 06:54:50 | ORB-11365 was marked done, while its owning pipeline run remained in `implement_one`. |
| 07:00:58 | A `make ci-fast`/lint command returned. |
| 07:01:03 | The agent launched another full workspace suite in the background. |
| 07:01:35 | The agent began polling the background test process. |
| 07:01:49 | The stale run was cancelled. Its process tree disappeared and the host recovered. |

The surviving evidence does not identify which individual test invocation
created the first unreaped waiter. The first full-workspace commands are the
earliest known opportunities; later focused and full-suite commands provided
additional activation opportunities.

## Technical causal chain

1. The ORB-11365 agent performed broad and repeated validation, including the
   `orbit-core` library test suite.
2. [`unmanaged_environment_admits_operator_ship_and_resume`](../../crates/orbit-core/src/adapter/tool_host/tests/workflow_tools.rs#L180)
   exercised `orbit.workflow.ship` and `orbit.workflow.run.resume`. Both calls
   created real runs, but the test did not install the test-only pipeline-worker
   command override.
3. [`pipeline_worker_command`](../../crates/orbit-core/src/application/job/pipeline.rs#L1240)
   therefore fell back to `std::env::current_exe()`. In a library test this was
   the `orbit_core` test harness. The source's own test-only override comment
   documents the hazard: re-executing that binary makes libtest interpret the
   pipeline-worker arguments as test filters and recurse through the suite.
4. Worker spawning called `setsid()`, so each nested worker was placed in a new
   session. Process inspection confirmed nested `orbit_core` test binaries
   running with `job run-pipeline-worker jrun-...` arguments.
5. Recursive test entry reached
   [`duplicate_worker_exit_leaves_real_owner_authoritative_and_non_terminal`](../../crates/orbit-core/src/application/tests/job_pipeline.rs#L313),
   which detached the 10 ms shell waiter before later assertions.
6. The fixture released the owner only near the end of the normal path. It had
   no scope guard that would terminate and reap the child during unwinding or
   abrupt parent termination. Removal of the runtime's temporary directory
   could also remove the only possible sentinel location.
7. Orphaned waiters repeatedly forked external `sleep` commands. Repeated test
   entry multiplied the waiters until process creation and scheduling saturated
   the host.
8. The owning run sandbox remained alive after the task record was done, so its
   descendant tree was not reclaimed until explicit run cancellation.

## Cause classification

### Confirmed

- Host inspection found 382 shell processes with the exact
  `ORBIT_TEST_OWNER_RELEASE` loop from `job_pipeline.rs`.
- The affected sandbox also contained nested `orbit_core` test binaries invoked
  as pipeline workers.
- The production fallback uses `current_exe`, and the source explicitly warns
  that a test binary re-executing itself recurses through libtest.
- The workflow-tool test created real ship and resume runs without installing
  the available worker-command override.
- The sentinel write existed only on the normal test path; there was no RAII
  cleanup guard for the child.
- The affected worker process was started with `setsid()`.
- Explicit cancellation removed the process tree and restored host CPU idle
  capacity.

### Supported inference

- One or more workspace or focused `workflow_tools` test invocations activated
  the recursive path. The transcript proves those invocations occurred, while
  process inspection proves the resulting recursive shape.
- Piped output filters, background execution, a panic, or timeout likely caused
  at least one parent path to end before its sentinel release. More than one of
  these may have contributed.
- Repeated validation amplified a single unsafe path into the observed process
  count. It was not necessary for every invocation to fail in the same way.

### Unknown

- Which exact invocation created the first leaked waiter.
- Which assertion, timeout, signal, or upstream pipe closure first bypassed the
  normal release path.
- The exact number of attempted and completed forks; 38,200 per second is the
  loop's unsaturated implied rate, not a direct measurement.

The run's structured event history recorded its start but no finish, and its
run-log records were empty. The ephemeral worktree had already been removed by
the time of the RCA. Those gaps prevent stronger attribution of the first
activation event.

## Root cause and contributing factors

The direct root cause was an unsafe composition of two test behaviors: a real
pipeline submission in a test without the required worker override recursively
re-executed the test harness, and a recursively reached fixture created an
unbounded, fork-heavy detached child with cleanup only on its happy path.

The incident became host-wide because independent safety layers were absent or
ineffective:

- test isolation did not prevent the production `current_exe` fallback;
- fixture lifecycle management did not guarantee child termination and reaping;
- the 10 ms shell poll converted one leaked child into continuous process churn;
- detached descendants outlived the test process that created them;
- task completion and run termination diverged;
- the long-lived sandbox did not reclaim the tree until cancellation;
- broad suites were repeated and sometimes piped, truncated, or backgrounded;
- no visible CPU, memory, process-count, or blocked-process alert reached the
  orchestrator or operator.

## Why it happened on this date

The components were latent rather than newly introduced that morning:

- the workflow-tool test had submitted real runs since commit `e682d7fae8` on
  2026-08-01;
- the test-only worker override and its recursion warning existed by commit
  `fa44a7107e` on 2026-08-15, but were not applied to that workflow-tool test;
- the exact owner-wait fixture was introduced by commit `4b1d51fe4a` on
  2026-08-30.

On 2026-09-06, ORB-11365's long-lived agent sandbox ran an unusually broad,
repeated, and partially backgrounded validation sequence. That sequence entered
the unprotected workflow path enough times for detached waiters to accumulate
instead of disappearing with one short test process. The three-hour run window
gave the failure time to compound.

No part of `scripts/compiler-cache.sh` directly created the waiter or the
recursive worker. Its companion validation script exercises the compiler-cache
wrapper and operator surface; the observed command came from the Rust pipeline
test fixture. The compiler cache may have changed how quickly test binaries
could be rebuilt, but there is no evidence that it caused or activated the
recursion.

## Detection and response

Detection was manual and late. The first visible symptom was a host that had
become unusably slow. Process-tree inspection then isolated the load to one
Orbit run sandbox, identified the repeated shell command, and traced that
command back to the test fixture. Source and transcript inspection established
the preceding recursive test-worker path.

The effective containment action was cancellation of
`jrun-20260906-0508-11`. Cancellation removed the owning sandbox tree. The
system recovered without rebooting, and memory remained healthy.

## Follow-up tracking

| Record | State at RCA | Purpose |
| --- | --- | --- |
| F2026-09-042 | Open friction | Preserve the operational incident and unsafe polling-shell evidence. |
| ORB-11414 | Done | Record the durable test-fixture and containment lesson in `docs/LESSONS.md`. |
| ORB-11415 | Proposed | Make sustained CPU or memory saturation visible to Orbit orchestrators without prescribing a solution yet. |
| ORB-11416 | Review | Author and validate this RCA. |

Code remediation is intentionally outside this RCA's scope. The open friction
record is the handoff for turning test isolation, fixture cleanup, and run
containment gaps into separately scoped implementation work. Automatic
cancellation or throttling was not authorized as part of the visibility task.

## Evidence retained

- ORB-11365 task and run records, including the task/run lifecycle mismatch.
- The preserved Claude transcript for the affected worktree, containing 147
  shell-tool calls and the validation timestamps above.
- Live process-tree and scheduler samples taken immediately before cancellation.
- Current source for the worker fallback, worker override, workflow-tool test,
  and owner-wait fixture.
- Git history for the three latent components.

This document records the best-supported causal account as of 2026-09-06. If
new run logs or host telemetry become available, update the confirmed,
inferred, and unknown sections rather than silently strengthening the claims.
