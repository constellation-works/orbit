---
title: Process Supervision — Design
owner: claude
last_updated: 2026-10-12
last_validated: 2026-10-12
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
the three supervisors, two ad-hoc wait loops, and their callers, verified
at `f9d7a0ccd`, with step 1 ownership changes reflected below. §1–§3
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
`run_bounded_capped_complete_typed`. It is synchronous and needs no
threads on Unix. It spawns with `process_group(0)` and null stdin. One
loop drains both pipes nonblocking, with each turn capped at 64 KiB or
20 ms, and polls `try_wait` every 20 ms. At the deadline it sends SIGTERM
to the group, waits 200 ms, then SIGKILL. When the leader exits it kills
the group and drains for up to 500 ms more. Group signals go through
`OwnedChild::signal_group`, which skips the caller's own group and a pid
that no longer leads its group. Capture keeps a prefix and keeps reading past the
limit (`BoundedOutputCapture`, ending with `OUTPUT_TRUNCATED_MARKER`). The
`complete` mode also needs both pipes at EOF before the deadline. The
`_typed` variants return a spawn failure as its `io::Error`.

The spawned child is held through `process::OwnedChild`. A pipe or wait
error, including any `?` return from `supervise`, drops the handle, kills
its group, reaps the leader and waits up to one second for group drain.
Normal completion disarms the handle after descendant cleanup. It installs
no signal handling: a SIGTERM to Orbit during a call leaves the child's
group running.

### 1.2 Exec supervisor — `orbit-exec`

[`supervision/`](../../../crates/orbit-exec/src/supervision/mod.rs) is the
most complete supervisor.

- **Ownership.** `SupervisedChild` (`wait.rs`) holds the shared
  `process::OwnedChild` handle, which kills and reaps the group on drop
  unless cleanup was completed. Its wrapper releases the signal slot after
  cleanup and before dropping the signal guard. `SupervisedChild::spawn`
  installs the signal guard before the spawn closure runs, so post-spawn work such as a
  plugin's callback `bind_pid` is covered.
- **Signals.** `SignalHandlerGuard` (`signal.rs`) is a refcounted
  process-wide SIGINT/SIGTERM handler. It fans the signal out with `killpg`
  to a lock-free table of live child groups. The last guard restores the
  previous disposition and re-raises the signal.
- **Pipes.** One thread drains each stream, and a stdin writer thread
  delivers a payload. All of them `poll(2)` on their pipe and on a socket
  pair owned by `DrainStop` (`tee.rs`). After the child is reaped,
  `settle(DRAIN_BUDGET = 1 s)` stops any worker whose pipe is still held by
  a descendant outside the group.
- **Capture.** Each stream has a 1 MiB limit
  (`ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_BYTES`). Reaching it terminates the
  group and fails the run with `ProcessStopCause::OutputCaptureLimit`.
  Debug mode echoes both streams to Orbit's stderr, redacted.
  `wait_with_stdout_relay` forwards stdout to a consumer instead of
  capturing it.
- **Termination.** The deadline sends SIGTERM, allows a 5 s grace period,
  then SIGKILL. `kill_on_deadline` sends SIGKILL at once, for probes.
  Callers can also stop a run with a cancellation `AtomicBool`.
- **Stopped descendants.** A child that stays stopped past the configured
  threshold is ended and reported on stderr.
- **Outcome.** The result carries `timed_out`, a typed `ProcessStopCause`
  and `drain_stopped`, so callers never parse stderr to classify a run.

Every failure path reaps: fault-injection seams (`SupervisionFailure`)
prove it for signal install, drain-stop setup, watch setup and wait.

### 1.3 Engine provider supervisor — `orbit-engine`

[`cli_runner/supervisor/`](../../../crates/orbit-engine/src/activity_job/cli_runner/supervisor/mod.rs)
runs provider CLIs (`spawn_for_supervision`, then `spawn_with_timeout`).

- **Stdin.** A nonblocking writer polls the pipe together with a
  cancellation pair (`output.rs::PipeCancel`). When the pair cannot be
  created (`EMFILE`), it falls back to rechecking a flag every 25 ms
  instead of failing.
- **Capture.** `RollingOutputCapture` (`capture.rs`, 1 MiB,
  `ORBIT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES`) keeps a redacted head and
  a raw protocol tail, and sets `truncated`. The run continues past the
  limit.
- **Trace events.** Each output line becomes a tracing event in the job's
  dispatch context.
- **Hooks.** `on_spawn(pid)`; progress sampling of the newest 64 KiB of
  stdout at an interval; a stopped-descendant reporter callback.
- **Deadline.** It can be extended by build-budget wait credits read from
  `WAIT_DIRECTORY_ENV`.
- **Termination.** No grace period. `kill_child_process_tree` sends SIGKILL
  to the group, reaps, and polls up to 1 s for the group to drain. Readers
  get 500 ms to reach EOF, then are cancelled and drain at most 1 MiB more.
- **Test seams.** An injected clock, a try-wait hook, a cancel-pair hook and
  a live-reader counter.

`SpawnedChild` holds the bare `Child` with no `Drop`. Kills are explicit:
on stdin-setup failure and after every wait in `spawn.rs`, and on worker
registration failure in `orchestrator/dispatch.rs` and `auth_probe.rs`. A
panic, or a new early return, between spawn and the final kill leaks the
provider's group. Its only `Drop` is the test-only `LiveReaderGuard`. It
installs no signal fan-out. Non-Unix stdin is refused.

### 1.4 Ad-hoc wait loops

- **Plugin build phases** —
  [`build_sandbox/supervise.rs`](../../../crates/orbit-exec/src/build_sandbox/supervise.rs).
  It sets `setsid` and `RLIMIT_CORE=0` in `pre_exec`. Two relay threads send
  chunks over a channel into one head/tail `BuildLog` (1 MiB). It enforces a
  wall clock, checks the build-directory size every second, and stops
  through exec's `terminate_process_group`. After exit it drains for 2 s.
  `try_wait()?` returns without stopping the phase, and a reader that has not
  finished is detached, not joined.
- **Automation source commands** —
  [`source/command.rs`](../../../crates/orbit-core/src/application/automation/source/command.rs).
  Stdin, stdout and stderr are unlinked temp files, so a large Git listing
  streams from disk. A 10 ms `try_wait` loop enforces the per-command budget,
  `SOURCE_DEADLINE` and the size of the stdout file. `SourceChild` is its
  own kill-on-drop group handle.
- **Plugin MCP sessions** —
  [`crates/orbit-tools/src/plugin/mcp.rs`](../../../crates/orbit-tools/src/plugin/mcp.rs). This is
  a long-lived stdio JSON-RPC session, not a run to completion. It has its
  own `kill_group` and `Drop for McpSession`. Only its ownership handle is
  in scope (§5 step 2); its request loop is not a supervisor wait loop.

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

### 2.2 Exec supervisor callers

| Caller | Entry point | What it needs |
|--------|-------------|---------------|
| `orbit-core/src/runtime/task_pr_forge.rs` | `run_process` | Deadline, fail-at-limit capture, signal fan-out |
| `orbit-core/src/application/review/fulfilment/codeql.rs` | `run_process` (CodeQL evidence sandbox) | Sandbox spawn, deadline, capture, signal fan-out |
| `orbit-tools/src/external.rs`, `builtin/proc/spawn.rs` (`proc.spawn`) | `run_process` | Stdin payload, deadline, capture, debug echo |
| `orbit-tools/src/builtin/github/{common,auth,logs}.rs` | `run_process`; `run_process_streaming_stdout` (logs) | Deadline, capture or stdout relay, typed capture-limit stop |
| `orbit-tools/src/plugin/source.rs` | `run_process`; `run_process_streaming_stdout` | Deadline, stdout relay |
| `orbit-tools/src/plugin/tool.rs` (exec backend) | `spawn_supervised_cancellable` | Signal guard over spawn and `bind_pid`, stdin payload, cancellation flag |
| `orbit-engine/src/executor/automation/{dependabot,ci}/query.rs` | `run_process` | Deadline, capture |
| `orbit-engine/src/executor/automation/vcs/{git,host_evidence}.rs`, `vcs/commit/git_ops.rs`, `vcs/worktree/cleanup.rs`, `vcs/operations/process.rs` | `run_process`; `run_process_streaming_stdout` (`vcs/git.rs`) | Deadline, capture or stdout relay |
| `orbit-engine/src/executor/automation/vcs/required_command.rs` | `run_process_supervised` | Deadline, typed stop cause for inconclusive runs |
| `orbit-engine/src/executor/automation/shell.rs`, `vcs/deferral_probe.rs` | `supervise_child` (sandbox-spawned child) | Adopt a spawned child, empty stdin, deadline, `timed_out` |
| `orbit-exec/src/validation_env.rs` | `run_probe_process` | Kill at once on deadline (an interactive shell ignores SIGTERM) |
| `orbit-exec/src/build_sandbox/supervise.rs` | `terminate_process_group` only | Group termination with grace (§1.4) |

### 2.3 Engine provider supervisor callers

| Caller | Entry point | What it needs |
|--------|-------------|---------------|
| `orbit-engine/src/activity_job/cli_runner/orchestrator/dispatch.rs` (provider run) | `spawn_for_supervision` + `spawn_with_timeout` | Pre-supervision work on the child (worker registration, PID namespace, broker bind, Linux post-run guard), prompt on stdin, rolling capture with protocol tail, line trace events, `on_spawn`, progress, stopped-descendant reports, build-budget deadline extension, kill at once |
| `orbit-engine/src/activity_job/cli_runner/auth_probe.rs` (provider auth probe) | `spawn_for_supervision` + `spawn_with_timeout` | Same spawn path, worker registration, 16 KiB capture, deadline |

### 2.4 Ad-hoc loop callers

| Caller | Loop | What it needs |
|--------|------|---------------|
| `orbit-exec/src/build_sandbox/mod.rs` (plugin install phases) | `build_sandbox::supervise::run` | `setsid` + no core dumps, merged head/tail log, wall clock, periodic size check that stops the phase |
| `orbit-core/src/application/automation/source/command.rs` (`Source::command*`, `Source::git*`, used by automation evidence collection) | `Source::capture` | File-backed streams, budget plus a global deadline, periodic stdout-size check, kill at once |
| `orbit-tools/src/plugin/mcp.rs` (`McpBackend::spawn`) | session lifetime | Owned group killed when the session ends (no wait loop) |

## 3. Capability matrix

| Capability | Bounded | Exec | Engine | Build phase | Source |
|------------|---------|------|--------|-------------|--------|
| Kill-on-drop ownership | no | yes | no | no | yes |
| Survives `?` from pipe/wait errors | no | yes | yes (explicit) | no | yes |
| SIGINT/SIGTERM fan-out to child groups | no | yes | no | no | no |
| Stdin payload | no (null) | yes (thread, stop-aware) | yes (nonblocking, cancel pair + flag fallback) | no | temp file |
| Capture policy | prefix, continue | fail at limit | redacted head + protocol tail, continue | merged head/tail | file + size limit |
| Stdout relay to a consumer | no | yes | no | no | no |
| Line trace events | no | debug echo only | yes | no | no |
| Termination | TERM, 200 ms, KILL | TERM, 5 s, KILL (or KILL) | KILL, 1 s group drain | exec's (5 s) | KILL |
| Cancellation flag | no | yes | no | no | no |
| Post-exit drain bound | 500 ms (≤ deadline) | 1 s, workers stopped | 500 ms, then cancel | 2 s, readers detached | n/a |
| Require EOF before success | optional | no | no | no | n/a |
| Stopped-descendant handling | no | end + stderr note | end + callback | no | no |
| Progress / periodic hook | no | no | progress sampling | size check | size check |
| Extendable deadline | no | no | build-budget credits | no | no |
| Typed stop cause / `timed_out` | `ProcessTimeout` error | yes | `timed_out` | `BuildPhaseEnd` | `Deferred` reason |
| Typed spawn error kind | yes | no (`OrbitError`) | permanent vs transient | no | no |
| Threads per run (Unix) | 0 | 2–3 | 2–3 | 2 | 0 |

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
    pub stdin: Stdin,                         // Null | Payload(Vec<u8>) | Redirected
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

### 4.3 How each caller group maps

| Caller group | stdin | sinks | termination | other options |
|--------------|-------|-------|-------------|---------------|
| Bounded runner wrappers | Null | keep-prefix (`BoundedOutputCapture`) | Grace(200 ms) | `completion` per mode; drain 500 ms ≤ deadline; no signal interception; no stopped watch |
| Exec runner wrappers | Payload or Null | fail-at-limit (+ redacting echo in debug) or Relay | Grace(5 s); KillNow for probes | `intercept_signals`; `cancelled`; stopped watch with a stderr note; drain 1 s |
| Engine provider runner | Payload | engine-owned rolling head/tail sink with line trace events | KillNow + group drain | `on_spawn`; `every` = progress; extendable deadline; stopped watch with callback |
| Build phase | Null | one shared `BuildLog` for both streams | Grace(5 s) | `every` = build-directory size; drain 2 s |
| Automation source | Redirected (temp file) | none (Redirected) | KillNow | `every` = stdout file size |

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
it. Engine teardown and the build-phase termination adapter delegate to its
borrowed-child helpers until step 2 migrates their ownership. These adapters
share the handle's own-group and reused-PID checks; they add no new wait loop.

Each step is its own task, keeps every public entry point's signature, and
must pass the existing tests of the code it replaces.

| Step | Change | Fixes on landing | Task |
|------|--------|------------------|------|
| 1 | Add `OwnedChild` to `orbit_common::process`; bounded runner and `SupervisedChild` hold their child through it; fold the three group-signal helpers into it | Bounded runner leak on `?` | [ORB-15826] |
| 2 | Hold every ad-hoc child through `OwnedChild`: engine `SpawnedChild` and `spawn_with_timeout`, plugin exec `bind_pid` failure, MCP session, build phase, automation `SourceChild` | Engine and plugin leaks on failure and panic | [ORB-15827] |
| 3 | Move exec's supervisor into `orbit_common::process::supervisor` with the §4.2 options exec needs; `orbit-exec::supervision` becomes an adapter | Single implementation of the loop | [ORB-15828] |
| 4 | Re-implement `run_bounded*` on the supervisor; delete bounded's loop | Bounded callers gain the drain bound and kill-on-drop | [ORB-15829] |
| 5 | Run the engine provider runner on the supervisor (adds `on_spawn`, the periodic hook, extendable deadline, stopped reporter, flag fallback, test seams) | Engine loop and readers deleted | [ORB-15830] |
| 6 | Run plugin build phases on the supervisor | Build-phase `?` leak and detached readers | [ORB-15831] |
| 7 | Run automation source commands on the supervisor | Last ad-hoc loop deleted | [ORB-15832] |

Steps 1 and 2 come first because they close the leak class without
touching any wait loop, so they are safe to land alone. Step 3 must come
before 4–7, which all build on the moved supervisor. Steps 5–7 depend only
on step 3 and can land in any order. Step 4 is listed before them because
it moves the most callers with the least new API.

## 6. Concerns & Honest Limitations

- **Bounded callers gain threads.** Today a bounded call on Unix needs no
  threads. On the supervisor it has two pipe workers, joined before return,
  so the "no capture worker survives the probe" guarantee in
  [`DEVELOPMENT.md`](../../DEVELOPMENT.md#child-processes) still holds.
  `identity`'s `ps` call runs on sampling paths, so step 4 should check
  sampling overhead. If it regresses, the supervisor may keep a
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
- **Test seams merge.** Exec's thread-local fault injection and the
  engine's clock, wait, cancel-pair and live-reader seams must coexist in
  one module without becoming production API. Step 3 and step 5 own that,
  behind `cfg(test)` or the existing `test-support` feature.
- **Line numbers.** File references in §1–§2 were verified at `f9d7a0ccd`;
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
