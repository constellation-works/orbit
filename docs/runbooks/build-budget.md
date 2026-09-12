---
type: runbook
summary: Run Cargo builds within Orbit's host-wide cross-worktree admission and compiler-job budget.
tags: [operations, performance, rust]
paths: ["Makefile", "scripts/build-budget.py"]
related_artifacts: [ORB-11754, ORB-11760]
last_validated: 2026-09-08
---

# Bound Concurrent Orbit Repository Builds

Use the build budget when several Orbit worktrees validate on the same host. It limits
heavy build phases independently of agent concurrency while keeping each worktree's
private Cargo target directory.

## Supported entry points

The repository's heavy Make targets enter the budget automatically: `build`, `release`,
`run`, `check`, `test`, `clippy`, `ci`, `ci-lint`, `goldens`, `install`, and `watch`. `make ci` holds
one slot around the complete CI script; its nested Cargo commands inherit that admission
instead of reacquiring a slot. `dev` is covered through its `build` prerequisite.

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

`ORBIT_CARGO_JOBS` takes precedence over a caller's `CARGO_BUILD_JOBS`; if the Orbit-specific
setting is absent, `CARGO_BUILD_JOBS` overrides the four-job default. These overrides are
deliberate capacity decisions and may exceed the defaults. Slot values must be decimal
integers from 1 through 128, Cargo job values from 1 through 1024, and
`ORBIT_BUILD_BUDGET` must be `0` or `1`. Invalid values exit with status 64 before running
the command.

The default lock directory is `$HOME/.orbit/cache/build-budget`, which is shared by the
same user across Orbit worktrees. Tests and isolated operators may set
`ORBIT_BUILD_BUDGET_DIR`. Do not point separate workers at different directories if they
are intended to share one budget.

Each slot is a kernel `flock`. The wrapper replaces itself with the admitted command while
retaining the lock descriptor, so normal completion, command failure, cancellation, and
process termination release the slot. An inherited internal marker prevents nested Make
or wrapper entry points from reacquiring a slot and deadlocking.

## Verification

Run the deterministic process test without compiling the workspace:

```bash
scripts/test-build-budget.sh
```

It exercises separate worktree paths, a two-slot concurrency ceiling, progress after
success, failure, and termination, nested admission, job-count precedence, bypass,
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

### Representative dk-server-1 result

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
does not promise strict FIFO order. Changing `ORBIT_BUILD_SLOTS` while commands are active
changes which lock files new callers scan and should therefore be done only when the queue
is idle. The lock directory must be on a filesystem that implements advisory `flock`
consistently; the default local host cache satisfies that requirement.
