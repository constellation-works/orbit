---
type: context
summary: Bound test synchronization, reap every spawned child on every exit path, and contain descendants at the sandbox as a separate layer.
created_date: 2026-09-06
last_validated: 2026-10-29
---

# L03. The September 2026 Test Fixture Fork Storm

On 2026-09-06, the Linux host reached a load average of 393.68 on 14 CPUs. It
had 831 processes, including 162 in uninterruptible sleep. One Orbit task
sandbox owned 386 direct children; 382 of them were shell processes left by
repeated test executions. The task itself was already done, but its pipeline
run remained alive for about 113 minutes.

The command responsible was not in the compiler-cache operator script. It was
a now-removed owner-process fixture formerly in
`crates/orbit-core/src/application/tests/job_pipeline.rs`:

```sh
while [ ! -f "$ORBIT_TEST_OWNER_RELEASE" ]; do sleep 0.01; done
```

The agent triggered an ordinary validation run. A workflow-tool test submitted
real pipeline runs without the test-only worker override, causing the
`orbit_core` test binary to re-execute as a worker and recurse through libtest.
The recursively reached fixture then created the runaway processes: it spawned
the waiter before a sequence of assertions and wrote the release file only on
the normal success path. If an assertion panicked or the test timed out first,
no guard killed the child. Dropping the test's temporary directory also removed
the location where the release file could have been created, so the orphaned
waiter could never satisfy its exit condition.

The ten-millisecond interval made this much worse. `sleep` is an external
process, so every leaked waiter attempted roughly 100 process launches per
second. With 382 waiters, the fixture could demand about 38,200 launches per
second. The pipeline sandbox then failed to contain the defect: its descendant
tree survived after the task completed and accumulated across repeated tests.
Cancelling the stale run removed the tree; blocked processes fell to zero and
the host returned to 90--96% CPU idle. The incident is recorded as
F2026-09-042; the full causal account is in the
[incident RCA](../dev/rca/2026-09-06-recursive-test-worker-fork-storm.md).

**Lesson**: Test synchronization must be bounded and preferably in-process,
not a short-interval shell loop that repeatedly forks. Every fixture that
spawns a child needs panic-safe cleanup that terminates and reaps it on success,
failure, panic, and timeout. The outer sandbox or pipeline must independently
terminate remaining descendants when the owning run ends, because fixture
cleanup and runtime containment are separate safety layers.

