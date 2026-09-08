---
type: pattern
summary: "RAII Guard Pattern"
last_validated: 2026-09-07
---
# RAII Guard Pattern

Bind a side effect to a lexical scope: do something at construction, undo it in `Drop`. Callers write `let _g = Guard::enter(...);` and rely on scope exit — including `?`-return and panic unwind — to clean up. The defining trait: **`Drop` does meaningful work** (restoring state, releasing a lock, persisting a record), not just freeing memory.

```rust
struct Guard { /* captured state to undo */ }

impl Guard {
    fn enter(...) -> Self { /* install / acquire / stage */ }
}

impl Drop for Guard {
    fn drop(&mut self) { /* restore / release / finalize */ }
}
```

Four shapes in the codebase carry distinct lessons.

## Reference: `AuditGuard` — record the scope's outcome once

From `crates/orbit-cli/src/audit_middleware.rs:28`:

```rust
pub struct AuditGuard<'a> {
    runtime: &'a OrbitRuntime,
    meta: CommandMeta,
    start: Instant,
    status: AuditEventStatus,    // defaults to Failure
    exit_code: i32,              // defaults to -1
    error_message: Option<String>,
}

impl AuditGuard<'_> {
    pub fn mark_success(&mut self) { /* ... */ }
    pub fn mark_failure(&mut self, error: &OrbitError) { /* ... */ }
}

impl Drop for AuditGuard<'_> {
    fn drop(&mut self) {
        if take_tool_audit_recorded() { return; }       // suppression flag
        let params = AuditEventInsertParams { /* ... */ };
        let write = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || self.runtime.record_audit_event(&params),
        ));
        // log and swallow; never propagate from Drop
    }
}
```

Patterns to copy:

- **Default to the "bad" outcome.** Status starts as `Failure`/`-1`; scope exits without an explicit `mark_*` correctly reflect "process died mid-command."
- **Mutation methods on `&mut self`, not constructor params.** Caller updates the outcome as it learns; `Drop` reads final state.
- **`catch_unwind` around the side effect.** A panic *during audit emission* can't double-panic the unwind.

## Reference: `StagedTextFile` — `Drop` as rollback

From `crates/orbit-common/src/fs/io.rs:112`:

```rust
pub struct StagedTextFile {
    target_path: PathBuf,
    temp_path: PathBuf,
    committed: bool,
}

impl StagedTextFile {
    pub fn new(target: &Path, content: &str) -> io::Result<Self> { /* write temp file */ }
    pub fn commit(&mut self) -> io::Result<()> { /* rename, set committed = true */ }
}

impl Drop for StagedTextFile {
    fn drop(&mut self) {
        if self.committed { return; }
        let _ = fs::remove_file(&self.temp_path);
    }
}
```

Patterns to copy:

- **`committed: bool` is the lever.** Caller explicitly opts into the success path by calling `commit()`. Drop = rollback by default.
- **Shape for "stage → validate → commit-or-bail."** Between `new()` and `commit()`, the caller can inspect the staged content; any early-return cleans up the temp file automatically.

## Reference: `SignalHandlerGuard` — restore global state

From `crates/orbit-exec/src/supervision/signal.rs`:

```rust
pub(super) struct SignalHandlerGuard {
    start_gen: u64,          // signals observed after this wait started
    slot: Option<usize>,     // lock-free live-pgid table index
}

impl SignalHandlerGuard {
    pub(super) fn install(pgid: u32) -> Result<Self, OrbitError> {
        let start_gen = acquire_handlers()?;   // refcount++; first waiter installs
        Ok(Self { start_gen, slot: register_pgid(pgid) })
    }
}

impl Drop for SignalHandlerGuard {
    fn drop(&mut self) {
        unregister_pgid(self.slot);
        release_handlers();                    // last waiter restores prior sigaction
    }
}
```

`acquire_handlers` takes a process-wide `Mutex` only for the refcount/`sigaction` critical section. The first waiter snapshots the previous SIGINT/SIGTERM dispositions and installs a handler that stores a generation counter, records a pending forward, and `killpg`s every registered child; the last drop restores those dispositions and re-raises a captured signal (except `SIG_IGN`) with the mutex released. Concurrent waits overlap.

Patterns to copy:

- **Refcount a process-global side effect.** The guard's job is "this wait is live", not "I own the handler exclusively". Last `Drop` restores prior state.
- **Keep the mutex off the long path.** Holding `MutexGuard` as a field would serialize every supervised child for its entire lifetime.
- **Hand-rollback on partial first install.** `Drop` only runs on values that successfully return; if SIGINT is installed and SIGTERM fails, restore SIGINT before returning `Err`, and do not bump the refcount.
- **Capture prior state next to the refcount, not on every guard.** Previous `sigaction` structs live in the shared install record because only the first/last waiter should swap them.

## Reference: `FileLockGuard` — resource held until `Drop`

From `crates/orbit-store/src/fs/lock/mod.rs`:

```rust
#[must_use = "the advisory lock is released as soon as the guard is dropped"]
pub(crate) struct FileLockGuard {
    _file: File,
}
```

Patterns to copy:

- **Hold the resource in a field.** `FileLockGuard` keeps the locked `File`
  alive in `_file`; dropping the file releases the OS advisory lock. There is
  no separate explicit release path to call or duplicate.

---

**Note on test fixtures.** Several test files use small `EnvVarGuard` / `TempDir` structs that save-and-restore env vars or `remove_dir_all` on drop. They follow the pattern but are too thin to be reference-grade; lift from a production guard above when writing new ones.
