---
type: design
summary: "Spec: Sandboxed Exec Contract"
tags: ["policy-sandbox"]
last_validated: 2026-09-05
---

# Spec: Sandboxed Exec Contract

`orbit-exec::run_process` is the single primitive every shell-invoking tool spawns through. This spec names the supervision invariants and failure modes that contract must preserve.

## Why This Exists

Process supervision is full of subtle deadlocks (full pipe buffers, orphan grandchildren, signal races). Without a prescriptive contract, callers may build tools that bypass the supervision layer or assume invariants that the layer does not actually provide.

## Spawn Invariants

- **Sandbox validation first.** `run_process` calls `sandbox.validate(req)` before spawning. The default `NoSandbox` always returns `Ok`, but any future impl that returns `Err` aborts the spawn before any state changes.
- **Pipes for capture.** Stdout and stderr are always piped to the parent. Tools that want live terminal output use `ExecRequest::debug = true`, which tees the captured bytes through a redaction-aware drain rather than skipping capture.
- **Stdin mode.** `StdinMode::Inherit` (default), `Null`, or `Bytes(Vec<u8>)`. `Bytes` allocates a stdin pipe and a writer thread; the other modes do not.
- **Environment mode.** `EnvironmentMode::Inherit` (default) or `ClearAndSet(pairs)`. `ClearAndSet` calls `command.env_clear()` then sets the supplied pairs. The `Debug` impl redacts values for keys that match `is_sensitive_env_name`.
- **Process group leadership (Unix).** Children spawn with `command.process_group(0)`, so the child's PGID equals its PID. Non-Unix builds skip this step.
- **Working directory.** `current_dir` is applied to the spawn before the child runs.
- **Spawn failure.** If the OS fails to spawn the program, `run_process` returns `OrbitError::Execution("failed to spawn `<program>`: <error>")` and never enters the supervision loop.

## Supervision Invariants

- **Background drains.** `wait_with_optional_timeout` spawns reader threads for stdout and stderr immediately after spawn. The child must never block on a full pipe buffer because the parent is not reading.
- **Stdin writer thread.** When `StdinMode::Bytes` is set, a writer thread copies the payload to the child's stdin. A failed write terminates the child via `terminate_process_group` and surfaces as `OrbitError::Execution(<message>)`.
- **Poll interval.** The wait loop polls with `WAIT_POLL_INTERVAL = 100ms` (or the remaining deadline, whichever is smaller). The interval is global and not per-request configurable.
- **Signal handler installation (Unix).** A `SignalHandlerGuard` installs SIGINT and SIGTERM handlers for the duration of the wait loop. Installation acquires a process-global `Mutex`; the guard holds it until drop, so concurrent supervised waits are serialized. Drop restores the previous handlers and closes the signal pipe.
- **Timeout escalation.** When the deadline expires, `terminate_process_group(child, SIGTERM, poll_interval)` is called. If the group does not exit within `TERMINATION_GRACE_PERIOD = 5 seconds`, `kill_process_group` (SIGKILL) is invoked plus a direct `child.kill()`/`child.wait()`.
- **Parent-signal escalation.** When the parent receives SIGINT or SIGTERM during the wait, the same termination path runs with the received signal. The result reports `exit_code = Some(128 + signal)` and `success = false`.
- **Clean-exit reaping.** When the child exits cleanly, the wait loop calls `kill_process_group(child.id())` to reap any orphan subprocesses still holding pipe write ends, then joins the reader threads. Without this, an orphan grandchild can keep the pipes open and block reader-thread completion indefinitely.
- **Stderr annotation.** Timeouts append `process timed out` to stderr; parent-signal interruption appends `process interrupted by signal SIG<NAME>`. The annotations are added before the result is constructed, not by the caller.
- **Exit code reporting.** `ExecutionResult::exit_code` is `Some(code)` for clean exits, `Some(128 + signal)` for parent-signal exits, and `None` for timeouts.

## Result Shape

`ExecutionResult { success, stdout, stderr, exit_code, duration_ms, output }`:

- `success` reflects the child's exit status (clean exit with zero status). Timeouts and parent-signal exits report `success = false`.
- `stdout` and `stderr` are `String::from_utf8_lossy` conversions of the captured bytes. Non-UTF-8 output is preserved as replacement characters.
- `duration_ms` is wall-clock time from `Instant::now()` at spawn entry to spawn return.
- `output` is reserved for callers that want to attach a parsed-output payload after the fact; `run_process` itself does not populate it.

## Failure Modes

- **Spawn failure.** `OrbitError::Execution("failed to spawn …")` — caller cannot retry without changing the request.
- **Stdin write failure.** Writer-thread error → child terminated → `OrbitError::Execution(<error>)` returned. Captured stdout/stderr up to that point are discarded.
- **Stdin writer panic.** Writer-thread panic → `OrbitError::Execution("stdin writer thread panicked")` returned.
- **Signal handler install failure.** If `sigaction` fails for SIGINT or SIGTERM, the guard rolls back any partial install and `run_process` returns `OrbitError::Execution(<error>)` before entering the wait loop.
- **Wait error.** `child.wait_timeout` errors surface as `OrbitError::Execution("wait timeout error: …")`. The child is left to be reaped by the OS rather than force-killed in this path; this is a known soft spot.
- **Timeout.** `success = false`, `exit_code = None`, stderr suffixed with `process timed out`.
- **Parent signal.** `success = false`, `exit_code = Some(128 + signal)`, stderr suffixed with the signal name.

## Concurrency Constraints

- **Single signal-handler install at a time.** The global `Mutex` in `SignalHandlerGuard` serializes the complete guarded wait on Unix. Two concurrent `run_process` calls in the same process take turns from install through drop; their wait loops do not overlap.
- **No assumption about thread-local state.** Reader threads, writer threads, and the signal handler are spawned with `'static` requirements; callers must not rely on thread-local data from the spawning thread.
- **No retry inside `run_process`.** The runner does not retry spawn failures, wait errors, or signal-install failures. Retry policy belongs to the caller.

## Migration Rules

- New `ExecRequest` fields must default to a backwards-compatible behavior; `EnvironmentMode::default()` and `StdinMode::default()` exist precisely so callers can adopt new fields incrementally.
- A future kernel-level `Sandbox` impl must implement `validate` to either (a) gate at request-time before spawn, or (b) wrap the spawned process inside its isolation primitive. Mid-spawn isolation that races with `process::spawn` is out of scope for this contract.
- Changes to `TERMINATION_GRACE_PERIOD` or `WAIT_POLL_INTERVAL` require an ADR because both constants are observable in the timeout/cancel behavior of every shell-invoking tool.

## Agent Signature

Last revised by claude / claude-opus-4-7 for [T20260426-0622].
