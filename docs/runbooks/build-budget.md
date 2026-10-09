---
type: runbook
summary: Run Cargo builds within Orbit's host-wide cross-worktree admission and compiler-job budget.
tags: [operations, performance, rust]
paths: ["Makefile", "scripts/build-budget.py"]
related_artifacts: [ORB-11754, ORB-11760]
last_validated: 2026-10-09
---

# Bound Concurrent Orbit Repository Builds

Use the build budget when several Orbit worktrees validate on the same host. It limits
heavy build phases independently of agent concurrency while keeping each worktree's
private Cargo target directory.

## Supported entry points

The repository's heavy Make targets enter the budget automatically: `build`, `release`,
`run`, `check`, `test`, `clippy`, `ci`, `ci-test-affected`, `ci-lint`, `goldens`, `install`, and `watch`. `make ci` holds
one slot around the complete CI script; its nested Cargo commands inherit that admission
instead of reacquiring a slot. `dev` is covered through its `build` prerequisite.
`make ci-test-affected` selects packages without a slot, then admits each runtime
test and doctest command. A docs-only diff does not acquire a build slot.

`make run` admits compilation of the CLI binary, then launches the resolved executable
without holding a slot and without invoking `cargo run`. `make watch` does not occupy a
slot while idle; each `check` and `test` iteration is admitted separately. Wrapping a
whole `cargo run` or `cargo watch` process would starve other worktrees for as long as
those processes live.

Formatting, dependency inspection, supply-chain inspection, cleaning, release utilities,
and individual guardrail scripts do not enter a build slot. CI workflow commands and
arbitrary provider shell commands that invoke Cargo directly are also not intercepted.

For a direct Cargo command that should participate, run:

```bash
scripts/build-budget.py -- cargo test -p orbit-core command::job
```

An ordinary `cargo ...` remains the explicit escape path when admission is inappropriate.
For a Make target, `ORBIT_BUILD_BUDGET=0 make check` bypasses only slot admission and still
sets the resolved Cargo job count.

## Configuration

Defaults are two simultaneous build slots and four Cargo jobs per admitted command:

```bash
ORBIT_BUILD_SLOTS=2 ORBIT_CARGO_JOBS=4 make ci-lint
```

The default lock directory is `$HOME/.orbit/cache/build-budget`, which is shared by the
same user across Orbit worktrees. Tests and isolated operators may set
`ORBIT_BUILD_BUDGET_DIR`. Do not point separate workers at different directories if they
are intended to share one budget. The directory also holds optional host settings, so an
agent command with a cleared environment can still read the operator's limits:

```bash
mkdir -p "$HOME/.orbit/cache/build-budget"
printf '4\n' >"$HOME/.orbit/cache/build-budget/slots"
```

`slots` sets the simultaneous build limit. Its value must be a decimal integer from 1
through 128. Configuration precedence is `ORBIT_BUILD_SLOTS`, then `slots`, then the
default of 2. An explicit environment setting overrides the host file.

The optional `cargo-jobs` file sets Cargo parallelism, using a decimal integer from 1
through 1024. Precedence is `ORBIT_CARGO_JOBS`, `CARGO_BUILD_JOBS`, `cargo-jobs`, then the
default of 4. These environment and file settings are deliberate capacity decisions and
may exceed the defaults. `ORBIT_BUILD_BUDGET` must be `0` or `1`; invalid values exit with
status 64 before running the command. `ORBIT_BUILD_BUDGET=0` bypasses only slot admission
and still applies the resolved Cargo job count.

When `ORBIT_BUILD_BUDGET_DIR` is set for a command, the files are read from that directory.
For allowlisted commands that receive only `HOME` and `PATH`, put them in the host user's
default `$HOME/.orbit/cache/build-budget` directory, since those commands do not receive
`ORBIT_BUILD_BUDGET_DIR`.

Each slot is a kernel `flock` held by the supervising wrapper. The lock descriptor is
never passed to the admitted command. The slot is released when that direct command
exits, including failure or signal termination, even if a detached descendant remains
alive. The wrapper forwards termination signals and waits for the command to finish
handling them before releasing admission; it preserves the command's exit status or
terminating signal. An inherited internal marker prevents nested Make or wrapper entry
points from reacquiring a slot and deadlocking. Synchronous nested commands remain
covered for the lifetime of the direct command.

If every slot is occupied, stderr reports `build-budget: waiting for admission` with the
configured slot count and budget directory. While the command remains queued, periodic
`build-budget: still waiting for admission` lines show elapsed time; after a slot opens,
`build-budget: acquired slot` reports the slot and total wait. These messages confirm the
wrapper is waiting for admission before Cargo starts. A long-running build has already
acquired a slot and will not produce these wait messages, so use its process and build logs
to diagnose a possible hung compile or test.

Managed provider invocations export an invocation-private
`ORBIT_ACTIVITY_BUILD_BUDGET_DIR` beneath the checkout's `.orbit/tmp`. The wrapper
heartbeats admission waits there every 100 ms, independently of provider output
buffering. Completed waits keep their measured duration; a killed wrapper stops
heartbeating and earns no further credit. Orbit adds the union of queued intervals
to the activity deadline, capped at the original activity timeout. Even a slot
that never frees cannot keep the provider alive beyond twice its original timeout
(process teardown follows that deadline). Overlapping queued commands earn credit
once, while each command contributes to the wait count and total. Wrapper calls
outside a managed invocation retain ordinary admission behavior.

`orbit run show <RUN_ID>` prints each invocation's wait count, total and longest
seconds, queued wall time and deadline credit. JSON exposes these millisecond
statistics under `provider_processes[].build_budget_waits`; agent step output also
keeps `build_budget_waits`. The audit retains metrics after a timeout or failed
step, when no step output was checkpointed. Live metrics refresh on the provider's
10-second supervision observation interval.

`orbit run readiness` and `orbit doctor` warn when a running local or pull drain's
effective concurrency exceeds the host build slots, including a resized ceiling.
The warning names both counts, `ORBIT_BUILD_SLOTS` and the resolved `slots` file.
These diagnostics do not change capacity or admission; a drain may deliberately
run more agents than build slots. Disabled admission produces no mismatch warning.
An invalid build-budget setting or unreadable slots file does not prevent `orbit run readiness`
from reporting the snapshot; it surfaces the failure as an advisory `build_budget_error` field
on the capacity object (with `build_budget_warnings: []`) and in command text.
Fresh managed `proc.spawn` calls include accumulated queue credit in the activity's
remaining budget, while their configured per-call timeout still applies. Prefer
the provider's native long-running shell transport for build validation.

## Verification

Run the deterministic process test without compiling the workspace:

```bash
scripts/test-build-budget.sh
```

It exercises separate worktree paths, a two-slot concurrency ceiling, progress after
success, failure, and termination, release despite a surviving detached child, signal
forwarding and status preservation, nested admission, job-count precedence, bypass,
invalid settings, `make run` releasing its slot before application runtime, and
`make watch` admitting each check/test iteration without retaining a slot while idle.

Run a bounded comparison with private targets outside the checkout:

```bash
scripts/bench-build-budget.sh
```

The default workload starts four cold `cargo check -p orbit-types --offline` commands.
Both arms are capped at two Cargo jobs for safe measurement; the current arm admits all
four at once, while the budgeted arm admits two. The output records wall time, summed user
and system CPU from GNU `time`, and aggregate resident memory sampled every 50 ms from the
arm's process group. The RSS number is a sampled peak, not an instantaneous kernel
high-water mark. Results depend on host load, filesystem cache, toolchain, and package
graph; they characterize this workload rather than predict full-workspace Clippy.

### Representative Linux x86-64 14-CPU host result

Measured at `8b26169e91ca9dcf523bbae4d71308a2c82c4075` on 2026-09-08 with Linux
6.8.0 x86-64, 14 logical CPUs, rustc 1.96.0, and Cargo 1.96.0. The host had
26 GiB RAM; immediately before the run, 19 GiB was available, 3.7 of 4.0 GiB swap was
occupied from earlier activity, and load averages were 14.45/23.13/23.01. No active Cargo
or rustc process was visible inside the measurement sandbox.

| Arm | Wall (s) | User CPU (s) | System CPU (s) | Sampled peak RSS (KiB) |
|---|---:|---:|---:|---:|
| Current: four admitted at once, two jobs each | 39.726 | 163.79 | 25.03 | 2,199,416 |
| Budgeted: two slots, two jobs each | 48.762 | 124.07 | 20.11 | 1,197,452 |

For this small cold workload, two-slot admission reduced sampled peak RSS by 45.6% and
summed CPU time by 23.6%, at a 22.7% wall-time cost. Post-run available memory was 18 GiB;
the 1-minute load average was 25.02, showing why the result should not be treated as an
idle-host throughput benchmark. The two-slot default is retained because it materially
reduces concurrent memory pressure. Four Cargo jobs per slot is the operational default:
at full admission it caps compiler parallelism at eight workers on this 14-CPU host and
leaves capacity for eight agents' non-build work. The task benchmark intentionally used
two jobs per slot to cap added validation load, so the four-job default is bounded by the
deterministic environment test and host-capacity arithmetic rather than a second load run.

## Limitations

Admission is cooperative and per user: it covers the documented Make targets and explicit
wrapper calls, not arbitrary direct Cargo commands. Slot selection is work-conserving but
does not promise strict FIFO order. Killing the supervising wrapper with `SIGKILL` or a
wrapper crash releases its lock immediately; the command can still be running because
the wrapper cannot forward a signal or wait after such termination.
Changing `ORBIT_BUILD_SLOTS` while commands are active
changes which lock files new callers scan and should therefore be done only when the queue
is idle. The lock directory must be on a filesystem that implements advisory `flock`
consistently; the default local host cache satisfies that requirement.
