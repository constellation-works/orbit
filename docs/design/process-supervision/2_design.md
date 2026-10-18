---
title: Process Supervision — Design
owner: claude
last_updated: 2026-10-12
last_validated: 2026-10-18
status: Draft
feature: process-supervision
doc_role: design
type: design
summary: Caller inventory and capability matrix for Orbit's child-process supervisors, the chosen single supervisor (exec's SupervisedChild loop, moved into orbit-common and generalized with output sinks and hooks), and the seven-step migration order.
tags: [process-supervision, code-health, architecture]
paths: ["crates/orbit-common/src/process/**", "crates/orbit-exec/src/supervision/**", "crates/orbit-exec/src/runner.rs", "crates/orbit-engine/src/activity_job/cli_runner/supervisor/**", "crates/orbit-exec/src/build_sandbox/supervise.rs", "crates/orbit-core/src/application/automation/source/command.rs"]
related_features: [process-supervision, policy-sandbox, executors]
related_artifacts: [ORB-15722]
---

# Process Supervision — Design

This doc covers every place Orbit waits on a child process it spawned:
the bounded runner, the shared supervisor and its exec, engine, build and
source adapters, plus long-lived MCP session ownership, verified
at `99f2b2e55`, with the changes of steps 1–7 reflected below. §1–§3
describe the current code. §4 specifies the
supervisor that survives, §5 orders the migration, and §6 lists what the
design leaves open. Two things are out of scope. One is how a child is
built (sandbox wrappers in `orbit-exec`, `spawn_child_with_optional_sandbox`
in the engine). The other is Core's identity-bound cancellation of worker
processes it does not own as children (`orbit_common::process::signal`).
Both stay as they are; the supervisor receives a spawned child or a
`Command`.

## 1. The supervisors today

### 1.1 Bounded runner — `orbit-common`

[`process/bounded.rs`](../../../crates/orbit-common/src/process/bounded.rs)
exposes `run_bounded`, `run_bounded_capped`, `run_bounded_capped_typed` and
`run_bounded_capped_complete_typed`. It is synchronous. It spawns with
`process_group(0)` and null stdin, then adopts the child into the shared
supervisor (§1.2) with `SupervisedChild::adopt(child).supervise(…)`. The
supervisor drains both pipes on joined workers. Each stream feeds a
prefix-keeping sink (`BoundedOutputCapture`, ending with
`OUTPUT_TRUNCATED_MARKER`) that keeps reading past the limit. The deadline is
the supervisor's fixed timeout, and the post-exit drain is capped at it. At
the deadline it sends SIGTERM to the group, waits 200 ms, then SIGKILL. When
the leader exits it kills the group and drains for up to 500 ms more. Group
signals go through `OwnedChild::signal_group`, which skips the caller's own
group and a pid that no longer leads its group. Termination waits for both
the direct child and its original group to exit. If the child joins another
group while alive, it is killed directly and reaped after the grace period;
its new group is never signalled. The `complete` mode also needs both pipes
at EOF before the deadline. The `_typed` variants return a spawn failure as
its `io::Error`. The child is spawned before the supervisor's signal guard,
so the OS error kind survives and the process-identity probe neither
intercepts signals nor samples itself.

The spawned child is held through `SupervisedChild`, which owns a
`process::OwnedChild`. A pipe or wait error, including any `?` return from
`supervise`, drops the handle, kills its group, reaps the leader and waits up
to one second for group drain.
Normal completion disarms the handle after descendant cleanup. It installs
no signal handling: a SIGTERM to Orbit during a call leaves the child's
group running.

### 1.2 Shared supervisor — `orbit-common`

[`process/supervisor/`](../../../crates/orbit-common/src/process/supervisor/mod.rs)
is the most complete supervisor. It was exec's until step 3 moved it into
`orbit-common` behind one `Supervision` request;
[`orbit-exec`'s `supervision/`](../../../crates/orbit-exec/src/supervision/mod.rs)
is now an adapter that builds that request with exec's policy and appends
the supervision notes to stderr.

- **Ownership.** `SupervisedChild` (`wait.rs`) holds the shared
  `process::OwnedChild` handle, which kills and reaps the group on drop
  unless cleanup was completed. Its wrapper releases the signal slot after
  cleanup and before dropping the signal guard. `SupervisedChild::spawn_guarded`
  installs the signal guard before the spawn closure runs, so post-spawn work such as a
  plugin's callback `bind_pid` is covered.
- **Signals.** `SignalHandlerGuard` (`signal_guard.rs`) is a refcounted
  process-wide SIGINT/SIGTERM handler. It fans the signal out with `killpg`
  to a lock-free table of live child groups. The last guard restores the
  previous disposition and re-raises the signal.
- **Pipes.** One thread drains each stream, and a stdin writer thread
  delivers a payload when requested. `Stream::Sink` uses a pipe worker;
  `Stream::Redirected` attaches no sink or reader because the caller has
  already directed the output to a file or null. `Stdin::Null` writes
  nothing and preserves the stdin configured before spawn, including a
  caller's temp file. All pipe workers `poll(2)` on their pipe and on a socket
  pair owned by `DrainStop` (`drain.rs`). After the child is reaped,
  `settle(drain_budget)` (exec: 1 s) stops any worker whose pipe is still
  held by a descendant outside the group.
  When the pair, or a worker's clone of it, cannot be created (`EMFILE`),
  that worker polls its pipe every 25 ms and rechecks a stop flag instead
  of failing the run.
- **Capture.** Each piped stream goes to an `OutputSink`. Exec's `CaptureSink`
  has a 1 MiB limit (`ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_BYTES`). Reaching it
  terminates the group and fails the run with
  `ProcessStopCause::OutputCaptureLimit`. Debug mode echoes both streams to
  Orbit's stderr, redacted. `RelaySink` forwards stdout to a consumer
  instead of capturing it.
- **Termination.** `Termination::Grace` sends SIGTERM (or the intercepted
  signal), allows a grace period (exec: 5 s), then SIGKILL.
  `Termination::KillNow` sends SIGKILL at once, for probes and source commands.
  Callers can also stop a run with a cancellation `AtomicBool`.
- **Stopped descendants.** With the opt-in watch, a descendant that stays
  stopped past the threshold is ended and reported in the outcome; exec
  notes it on stderr.
- **Periodic checks.** `Supervision::every` runs a caller's check on the
  supervisor thread at a nonzero wall-clock interval. A returned stop cause
  ends the group under the requested termination policy; `None` continues the run.
  Build phases use `ProcessStopCause::Requested` for their directory cap.
  Source commands use `ProcessStopCause::Periodic` for their stdout-file cap.
- **Clock and deadline.** `timeout` is measured from supervision's start,
  using the caller's `clock` when supplied, otherwise wall time. An
  `extendable_deadline` takes precedence and uses wall-clock instants.
- **Outcome.** The result carries `timed_out`, typed `ProcessStopCause`s
  and `drain_stopped`, so callers never parse stderr to classify a run.

- **Hooks.** `on_spawn(pid)` before pipe setup; a `Periodic` hook on the
  supervising thread between waits; a stopped-descendant report callback;
  and an extendable deadline the supervisor re-reads at most every 100 ms
  and afresh before acting on its passing.

Every failure path reaps and joins its pipe workers before returning:
fault-injection seams (`SupervisionFailure`) prove it for signal install and
wait. After the child exits, the group is killed and given up to 1 s to
drain before the workers settle.

Tests in other crates reach two thread-scoped seams through
`orbit_common::test_env::with_supervisor_seam`: a `try_wait`-style hook that
replaces the blocking wait, and a host without a wakeup pair.

### 1.3 Engine provider runner — `orbit-engine`

[`cli_runner/supervisor/`](../../../crates/orbit-engine/src/activity_job/cli_runner/supervisor/mod.rs)
runs provider CLIs (`spawn_for_supervision`, then `spawn_with_timeout`).
Since step 5 it has no wait loop, pipe readers or stdin writer of its own:
`spawn_with_timeout` adopts the child into `SupervisedChild` and supervises
it under one `Supervision` request.

- **Stdin.** The prompt is a `Payload` for the shared writer.
- **Capture.** `RollingOutputCapture` (`capture.rs`, 1 MiB,
  `ORBIT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES`) keeps a redacted head and
  a raw protocol tail, and sets `truncated`. The run continues past the
  limit. Each stream's engine `OutputSink` (`output.rs`) feeds a shared
  capture, which the runner freezes after supervision has joined the workers.
- **Trace events.** The sink turns each output line into a tracing event in
  the job's dispatch context.
- **Hooks.** `on_spawn(pid)`; progress sampling of the newest 64 KiB of
  stdout as the periodic hook; a stopped-descendant reporter callback.
- **Deadline.** Extendable: build-budget wait credits read from
  `WAIT_DIRECTORY_ENV`. It is kept on the supervision clock that
  `with_cli_supervision_clock` replaces in tests and handed to the supervisor
  as the matching real instant.
- **Termination.** `KillNow`, with the group drain wait. The drain budget is
  500 ms. Signal interception stays off (§6).

`SpawnedChild` holds `process::OwnedChild`, which kills and reaps the
provider group on early return or panic during worker registration, broker
binding or `on_spawn`. The Linux mount plan outlives supervision, which
reaps the child before returning. The macOS profile is passed inline; there
is no profile temp file to retain. Non-Unix stdin is refused.

### 1.4 Build and source supervisor adapters

- **Plugin build phases** —
  [`build_sandbox/supervise.rs`](../../../crates/orbit-exec/src/build_sandbox/supervise.rs).
  It keeps `setsid` and `RLIMIT_CORE=0` in `pre_exec` and adopts the child
  into the shared supervisor. Both stream sinks share one head/tail
  `BuildLog` (1 MiB), retained across phases. A fixed deadline enforces the
  wall clock (`clock: None`), and the supervisor's one-second periodic hook
  checks the build-directory size, requesting a stop when the cap is
  exceeded. The caller also checks the directory after exit to catch fast writes.
  Termination uses exec's five-second grace. The shared supervisor owns
  group cleanup and reaping, including on supervision failure. The sandbox
  guard stays live through supervision; after exit the supervisor drains for
  2 s and joins both pipe workers, stopping readers whose pipes remain held
  outside the group.
- **Automation source commands** —
  [`source/command.rs`](../../../crates/orbit-core/src/application/automation/source/command.rs).
  `Source::capture` configures stdin as an unlinked temp file when input is
  supplied, otherwise null; stdout and stderr are unlinked temp files, so a
  large Git listing streams from disk. It adopts the child into
  `SupervisedChild` and calls `supervise` with `Stdin::Null` (preserving
  that configured stdin) and two `Stream::Redirected` outputs, with no
  pipe workers. `supervision_timeout` supplies the shorter of the command
  budget and the time left on `SOURCE_DEADLINE`, measured by the source's
  `budget_now` clock. With test-support, a frozen source clock supplies no
  timeout unless the budget is already exhausted. A 10 ms wall-clock
  `Periodic` hook checks the stdout file size and returns
  `ProcessStopCause::Periodic` when over limit or metadata is unreadable;
  the caller rechecks the size after exit to catch writes between polls.
  `Termination::KillNow` stops the group at once. The supervisor owns
  cleanup on completion, timeout and supervision errors through
  `OwnedChild::kill_and_reap`, including its 1 s group-drain bound.
  The pipe-drain budget is zero because no pipe workers are attached.

### 1.5 Long-lived sessions

- **Plugin MCP sessions** —
  [`crates/orbit-tools/src/plugin/mcp.rs`](../../../crates/orbit-tools/src/plugin/mcp.rs). This is
  a long-lived stdio JSON-RPC session, not a run to completion. It uses the
  shared `OwnedChild` handle for startup failures and session cleanup. Only its ownership
  is in scope (§5 step 2); its request loop is not a supervisor wait loop.

## 2. Caller inventory

Production callers and the shared test fixtures, by supervisor entry point.
Tests that call a supervisor directly are left out.

### 2.1 Bounded runner callers

| Caller | Entry point | What it needs |
|--------|-------------|---------------|
| `orbit-store/src/workflow/task/git.rs` (publication Git) | `run_bounded` | Deadline, full capture, no stdin |
| `orbit-common/src/fs/git.rs` (`run_git`) | `run_bounded_capped` | Deadline, prefix cap |
| `orbit-common/src/process/identity.rs` (`ps -o lstart`) | `run_bounded_capped_typed` | Deadline, prefix cap, spawn error kind; must not itself sample the process table (§4.4) |
| `orbit-core/src/runtime/command_exec.rs` (`orbit.command.exec`) | `run_bounded_capped_typed` | Deadline (clamped), prefix cap, spawn error kind; timed-out output discarded |
| `orbit-core/src/application/gc/scratch.rs` (`lsof`) | `run_bounded_capped` | Deadline, prefix cap |
| `orbit-core/src/application/task/final_recovery.rs` (`git fetch`) | `run_bounded_capped` | Deadline, prefix cap |
| `orbit-core/src/application/task/retained_candidate.rs` (`git status`) | `run_bounded_capped` | Deadline, prefix cap |
| `orbit-core/src/application/job/catalog.rs` (`git check-ref-format`) | `run_bounded_capped` | Deadline, prefix cap |
| `orbit-core/src/application/routines/clock/manager.rs` (launchd/systemd) | `run_bounded_capped` | Deadline, prefix cap, timeout mapped to its own error |
| `orbit-core/src/application/routines/clock/inspect.rs` (version probe) | `run_bounded_capped` | Deadline, prefix cap |
| `orbit-cmd/src/update/converge.rs` (install steps; probes) | `run_bounded_capped_typed`; `run_bounded_capped_complete_typed` | Spawn error kind (`ETXTBSY` retry); probes need both pipes at EOF |
| `orbit-cmd/src/update/channel.rs` (`brew list`) | `run_bounded_capped` | Deadline, prefix cap |
| `orbit-cmd/src/doctor/workspace.rs` (`git ls-files`) | `run_bounded_capped` | Deadline, prefix cap |
| `orbit-exec/src/linux_sandbox/probe.rs` (bwrap probe) | `run_bounded_capped_typed` | Deadline, prefix cap, spawn error kind |
| `orbit-exec/src/macos_sandbox/probe.rs` (Seatbelt probe) | `run_bounded_capped` | Deadline, prefix cap |
| `orbit-common/src/test_env/process.rs`, `test_env/child.rs` (fixtures) | `run_bounded_capped(_typed)`; `OwnedChild` | Deadline; a fixture-owned group killed on teardown |

### 2.2 Shared supervisor callers (exec wrappers and direct adapters)

| Caller | Entry point | What it needs |
|--------|-------------|---------------|
| `orbit-core/src/runtime/task_pr_forge.rs` | `run_process` | Deadline, fail-at-limit capture, signal fan-out |
| `orbit-core/src/application/review/fulfilment/codeql.rs` | `run_process` (CodeQL evidence sandbox) | Sandbox spawn, deadline, capture, signal fan-out |
| `orbit-tools/src/external.rs`, `builtin/proc/spawn.rs` (`proc.spawn`) | `run_process` | Stdin payload, deadline, capture, debug echo |
| `orbit-tools/src/builtin/github/{common,auth,logs}.rs` | `run_process`; `run_process_streaming_stdout` (logs) | Deadline, capture or stdout relay, typed capture-limit stop |
| `orbit-tools/src/plugin/source.rs` | `run_process`; `run_process_streaming_stdout` | Deadline, stdout relay |
| `orbit-tools/src/plugin/tool.rs` (exec backend) | `spawn_owned_supervised_cancellable` | Owned transfer, signal guard over spawn and `bind_pid`, stdin payload, cancellation flag |
| `orbit-engine/src/executor/automation/{dependabot,ci}/query.rs` | `run_process` | Deadline, capture |
| `orbit-engine/src/executor/automation/vcs/{git,host_evidence}.rs`, `vcs/commit/git_ops.rs`, `vcs/worktree/cleanup.rs`, `vcs/operations/process.rs` | `run_process`; `run_process_streaming_stdout` (`vcs/git.rs`) | Deadline, capture or stdout relay |
| `orbit-engine/src/executor/automation/vcs/required_command.rs` | `run_process_supervised` | Deadline, typed stop cause for inconclusive runs |
| `orbit-engine/src/executor/automation/shell.rs`, `vcs/deferral_probe.rs` | `spawn_owned_supervised_cancellable` (sandbox-spawned child) | Adopt an owned child, empty stdin, deadline, `timed_out` |
| `orbit-exec/src/validation_env.rs` | `run_probe_process` | Kill at once on deadline (an interactive shell ignores SIGTERM) |
| `orbit-exec/src/build_sandbox/mod.rs` (plugin install phases), `build_sandbox/supervise.rs` | `build_sandbox::supervise::run` → `SupervisedChild::adopt` + `supervise` | Null stdin; two sinks sharing a head/tail log; wall-clock timeout; 1 s periodic directory cap and post-exit check; group termination with 5 s grace; 2 s pipe drain with workers joined; sandbox guard retained through cleanup (§1.4) |
| `orbit-core/src/application/automation/source/command.rs` (`Source::command*`, `Source::git*`, used by automation evidence collection) | `Source::capture` → `SupervisedChild::adopt` + `supervise` | Preconfigured file/null stdin; redirected stdout/stderr files; source clock, command budget clamped to global deadline; 10 ms wall-clock stdout-size check and post-exit check; kill at once; owned group cleanup; no pipe workers (§1.4) |

### 2.3 Engine provider supervisor callers

| Caller | Entry point | What it needs |
|--------|-------------|---------------|
| `orbit-engine/src/activity_job/cli_runner/orchestrator/dispatch.rs` (provider run) | `spawn_for_supervision` + `spawn_with_timeout` | Pre-supervision work on the child (worker registration, PID namespace, broker bind, Linux post-run guard), prompt on stdin, rolling capture with protocol tail, line trace events, `on_spawn`, progress, stopped-descendant reports, build-budget deadline extension, kill at once |
| `orbit-engine/src/activity_job/cli_runner/auth_probe.rs` (provider auth probe) | `spawn_for_supervision` + `spawn_with_timeout` | Same spawn path, worker registration, 16 KiB capture, deadline |

### 2.4 Long-lived session callers

| Caller | Lifetime | What it needs |
|--------|------|---------------|
| `orbit-tools/src/plugin/mcp.rs` (`McpBackend::spawn`) | session lifetime | Owned group killed when the session ends (no wait loop) |

## 3. Capability matrix

| Capability | Bounded | Exec | Engine | Build phase | Source |
|------------|---------|------|--------|-------------|--------|
| Kill-on-drop ownership | yes | yes | yes | yes | yes |
| Survives `?` from pipe/wait errors | yes | yes | yes | yes | yes |
| SIGINT/SIGTERM fan-out to child groups | no | yes | no | no | no |
| Stdin payload | no (null) | yes (thread, stop-aware) | yes (shared writer) | no (null) | preconfigured temp file or null; `Stdin::Null`, no writer |
| Capture policy | prefix, continue | fail at limit | redacted head + protocol tail, continue | merged head/tail | file + size limit |
| Stdout relay to a consumer | no | yes | no | no | no |
| Line trace events | no | debug echo only | yes | no | no |
| Termination | TERM, 200 ms, KILL | TERM, 5 s, KILL (or KILL) | KILL, 1 s group drain | exec's (5 s) | KILL, 1 s group drain |
| Cancellation flag | no | yes | no | no | no |
| Post-exit drain bound | 500 ms (≤ deadline) | 1 s, workers stopped | 500 ms, workers stopped | 2 s, workers stopped and joined | no pipes; 1 s group drain |
| Require EOF before success | optional | no | no | no | n/a |
| Stopped-descendant handling | no | end + stderr note | end + callback | no | no |
| Progress / periodic hook | no | no | progress sampling | size check | size check |
| Extendable deadline | no | no | build-budget credits | no | no |
| Typed stop cause / `timed_out` | `ProcessTimeout` error | yes | `timed_out` | `BuildPhaseEnd` | `Deferred` reason |
| Typed spawn error kind | yes | no (`OrbitError`) | permanent vs transient | no | no |
| Threads per run (Unix) | 2 | 2–3 | 2–3 | 2 | 0 |

## 4. The single supervisor

### 4.1 Choice

The survivor is **exec's supervisor**: `SupervisedChild`, `wait_cancellable`,
`DrainStop`, `SignalHandlerGuard`, the stop-aware stdin writer and
`ProcessStopCause`. It moves into a new `orbit_common::process::supervisor`
module.

- **Exec's loop** already has the hardest guarantees: kill-on-drop
  ownership, a signal guard over the spawn window, a drain bound that
  survives escaped holders, typed stop causes, and fault injection proving
  every setup failure reaps. The others lack these, and adding them to
  either of the others means rebuilding exec.
- **`orbit-common`** is the only crate every caller can reach.
  `orbit-store` (publication Git) and `orbit-common` itself (`fs::git`,
  `identity`'s `ps`) depend on nothing above `orbit-common`, and
  `orbit-exec` depends on `orbit-common`. Leaving the survivor in
  `orbit-exec` would either keep the bounded runner as a second supervisor
  or create a dependency cycle. See
  [One supervisor, in the lowest crate that spawns](./4_decisions.md#one-supervisor-in-the-lowest-crate-that-spawns).
- **Kept from the others:** the engine's flag fallback when no wakeup pair
  can be created, its post-kill group-drain wait, the bounded runner's
  `signal_owned_group` checks (own group, reused pid), its `require_eof`
  mode and its typed spawn error.

### 4.2 Shape

```rust
// orbit_common::process::supervisor (sketch, not a final API)
pub struct OwnedChild { /* Child, reaped flag; Drop = SIGKILL group, reap, bounded drain */ }

pub struct Supervision<'a> {
    pub stdin: Stdin,                         // Null (preserves configured stdin) | Payload(Vec<u8>)
    pub stdout: Stream<'a>,                   // Sink(Box<dyn OutputSink>) | Relay(PipeWriter) | Redirected
    pub stderr: Stream<'a>,
    pub deadline: Deadline<'a>,               // None | Fixed(Duration) | Extendable(base, &dyn Fn() -> Instant)
    pub termination: Termination,             // Grace(Duration) | KillNow
    pub completion: Completion,               // ChildExit | StreamsEof
    pub drain_budget: Duration,
    pub cancelled: Option<&'a AtomicBool>,
    pub intercept_signals: bool,
    pub stopped_descendants: Option<StoppedPolicy<'a>>, // threshold + report callback
    pub every: Option<(Duration, &'a mut dyn FnMut() -> Option<StopCause>)>,
    pub on_spawn: Option<&'a dyn Fn(u32)>,
}

pub trait OutputSink: Send {
    fn push(&mut self, bytes: &[u8]) -> SinkVerdict; // Continue | Stop(StopCause)
    fn finish(self: Box<Self>) -> Box<dyn Any + Send>;
}

pub fn spawn_and_supervise(command: &mut Command, run: Supervision<'_>) -> Result<Outcome, SuperviseError>;
pub fn supervise(child: OwnedChild, run: Supervision<'_>) -> Result<Outcome, SuperviseError>;
pub fn spawn_guarded(spawn: impl FnOnce() -> Result<Child, OrbitError>, run: Supervision<'_>) -> Result<Outcome, SuperviseError>;
```

`Outcome` carries `status`, `timed_out`, `stop_cause`, `drain_stopped`,
`duration`, any stopped descendants it ended, and each stream's finished
sink. `SuperviseError::Spawn(io::Error)` keeps the spawn error kind.

Step 3 landed the subset exec needs, as
`SupervisedChild::{spawn_guarded, adopt}` followed by
`SupervisedChild::supervise(Supervision)`. Its request carries stdin
(`Null` | `Payload`), one `OutputSink` per stream (built-ins `CaptureSink`,
fail-at-limit with an optional redacting echo, and `RelaySink`), a timeout
with `Termination`, the cancellation flag, `intercept_signals`, the drain
budget and an optional stopped-descendant threshold. A sink's `finish`
returns the bytes the outcome reports, and errors are still `OrbitError`.
Step 5 added what the engine needs: `on_spawn`, a `Periodic` hook (`every`
in the sketch), an `extendable_deadline` beside
`timeout`, an `on_stopped_descendant` callback, the flag fallback when no
wakeup pair can be created, and the post-kill group drain after every exit.
Step 6 lets `Periodic::run` return `Option<ProcessStopCause>`: build-directory
checks can stop the group, while progress sampling returns `None` to continue.
The engine's sink keeps its capture behind a shared handle instead of
returning it from `finish`, because progress sampling reads it while the
child runs. The remaining options and `SuperviseError` arrive with the steps
that need them.

Step 7 added the options automation source needs. Each output is a `Stream`:
`Sink` keeps the pipe worker, and `Redirected` means the caller already
pointed the descriptor at a file or null, so the supervisor attaches no sink
and starts no reader. `every` reuses `Periodic::run`, on the wall clock, at
a nonzero interval measured from the end of the previous check. It may stop
the run with `ProcessStopCause::Periodic`. `clock`, when set,
measures `timeout` instead of wall time, so a stalled caller clock does not
spend the deadline. The engine progress hook returns no stop cause and
keeps its interval.
`extendable_deadline` still takes precedence over `timeout` and uses wall-clock
instants; its cached value is refreshed before supervision acts on expiry.
`SuperviseError` remains deferred to a step that needs it.

### 4.3 How each caller group maps

| Caller group | stdin | sinks | termination | other options |
|--------------|-------|-------|-------------|---------------|
| Bounded runner wrappers | Null | keep-prefix (`BoundedOutputCapture`) | Grace(200 ms) | `completion` per mode; drain 500 ms ≤ deadline; no signal interception; no stopped watch |
| Exec runner wrappers | Payload or Null | fail-at-limit (+ redacting echo in debug) or Relay | Grace(5 s); KillNow for probes | `intercept_signals`; `cancelled`; stopped watch with a stderr note; drain 1 s |
| Engine provider runner | Payload | engine-owned rolling head/tail sink with line trace events | KillNow + group drain | `on_spawn`; `every` = progress; extendable deadline; stopped watch with callback |
| Build phase | Null | two `Sink`s sharing one `BuildLog` | Grace(5 s) | wall-clock timeout; `every` = build-directory size every 1 s; post-exit size check; drain 2 s, workers joined |
| Automation source | Null (preserves preconfigured temp file or null) | none (both `Redirected`) | KillNow | `clock` = source `budget_now`; timeout = min(command budget, remaining source deadline); `every` = stdout file size every 10 ms on wall time; post-exit size check; pipe drain zero, group drain 1 s |

Engine-specific capture (`capture.rs`) and the build log stay with their
owners as `OutputSink` implementations. The supervisor owns no
provider- or plugin-specific policy.

### 4.4 Invariants

- From spawn until reap, an `OwnedChild` holds the child. Every path that
  does not reap it, including panics, kills the group in `Drop`.
- The group is never signalled after the leader is reaped, unless the pid
  still leads its own group and that group is not Orbit's.
- Every worker thread is joined before `supervise` returns. A holder outside
  the group can delay return by at most `drain_budget` plus one poll.
- A stop cause comes from the supervisor, never from child output.
- The stopped-descendant watch is opt-in. It reads the process table through
  `identity`, which itself spawns `ps` through the bounded wrappers, so that
  path runs with the watch off.

## 5. Migration order

Step 1 introduces `orbit_common::process::OwnedChild`. The bounded runner,
exec's `SupervisedChild` and the child-test runner own their processes through
it. Step 2 extends that ownership to the engine, plugin startup and MCP
sessions, build phases and automation source commands. The plugin exec
backend transfers its handle through `spawn_owned_supervised_cancellable`;
the existing bare-child entry point remains available. No wait loop changes.

Each step is its own task, keeps every public entry point's signature, and
must pass the existing tests of the code it replaces.

| Step | Change | Fixes on landing | Task |
|------|--------|------------------|------|
| 1 | Add `OwnedChild` to `orbit_common::process`; bounded runner and `SupervisedChild` hold their child through it; fold the three group-signal helpers into it | Bounded runner leak on `?` | [ORB-15826] |
| 2 | Hold every ad-hoc child through `OwnedChild`: engine `SpawnedChild` and `spawn_with_timeout`, plugin exec `bind_pid` failure, MCP session, build phase, automation `SourceChild` | Engine and plugin leaks on failure and panic | [ORB-15827] |
| 3 | Move exec's supervisor into `orbit_common::process::supervisor` with the §4.2 options exec needs; `orbit-exec::supervision` becomes an adapter | Single implementation of the loop | [ORB-15828] |
| 4 | Re-implement `run_bounded*` on the supervisor; delete bounded's loop | Bounded callers gain the drain bound and kill-on-drop | [ORB-15829] |
| 5 | Run the engine provider runner on the supervisor (adds `on_spawn`, the periodic hook, extendable deadline, stopped reporter, flag fallback, test seams) | Engine loop and readers deleted | [ORB-15830] |
| 6 | Run plugin build phases on the supervisor | Build-phase loop and detached readers replaced | [ORB-15831] |
| 7 | Run automation source commands on the supervisor | Last ad-hoc loop deleted | [ORB-15832] |

Steps 1 and 2 come first because they close the leak class without
touching any wait loop, so they are safe to land alone. Step 3 must come
before 4–7, which all build on the moved supervisor. Steps 5–7 depend only
on step 3 and can land in any order. Step 4 is listed before them because
it moves the most callers with the least new API.

## 6. Concerns & Honest Limitations

- **Bounded callers gain threads.** Step 4 has landed, so a bounded call on
  Unix now has two pipe workers, joined before return. Whether the "no capture
  worker survives the probe" guarantee in
  [`DEVELOPMENT.md`](../../DEVELOPMENT.md#child-processes) holds on this path,
  and the sampling overhead of `identity`'s `ps` call, have not been re-measured
  since the change. If sampling regresses, the supervisor may keep a
  single-threaded nonblocking mode for `Null` stdin with capture-only sinks.
  That mode would be a second implementation of the loop, so add it only if
  measured.
- **Signal interception is not unified.** Bounded and engine callers keep
  interception off, to preserve behaviour. Whether a provider run should
  forward SIGINT/SIGTERM to its group is open. The engine worker's own
  shutdown and Core's recovery authority currently handle that, and turning
  interception on changes what a SIGTERM to a worker does mid-provider. The
  same question applies to long-running servers that use the bounded runner
  (publication Git in `orbit-store`).
- **Termination grace stays per caller.** 200 ms, 5 s and none are all
  kept. This design does not decide whether they should converge.
- **MCP sessions are only partly converged.** They get the ownership handle
  (step 2) but keep their own request loop, because a long-lived interactive
  session is not a run to completion.
- **Non-Unix parity is unchanged.** Exec's non-Unix stubs and the bounded
  runner's thread-based fallback become the supervisor's non-Unix path. The
  engine's refusal of stdin off Unix (Windows runs Orbit through WSL2)
  stays.
- **Test seams merge.** Exec's fault injection stays `cfg(test)` inside
  the supervisor. The engine's wait and cancel-pair seams became one
  thread-scoped `SupervisorSeam` that other crates' tests set through
  `orbit_common::test_env`, which `orbit-common` always compiles (it has no
  `test-support` feature). The engine's clock stays behind its
  `test-support` feature and only moves the deadline it hands over. The
  unused live-reader counter was dropped.
- **Line numbers.** File references in §1–§2 were verified at `99f2b2e55`;
  the migration tasks re-verify before editing.

## Task References

- [ORB-15722] — authored this design and filed the migration tasks.
- [ORB-15826] — step 1: adds the owned process-group child handle.
- [ORB-15827] — step 2: holds every ad-hoc spawned child through that handle.
- [ORB-15828] — step 3: moves exec's supervisor into `orbit-common`.
- [ORB-15829] — step 4: runs the bounded runner on the single supervisor.
- [ORB-15830] — step 5: runs the engine CLI provider runner on it.
- [ORB-15831] — step 6: runs plugin build phases on it.
- [ORB-15832] — step 7: runs automation source commands on it.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
