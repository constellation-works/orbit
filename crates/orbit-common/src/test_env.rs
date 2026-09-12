//! Ambient-process isolation for tests — environment variables and umask.
//!
//! Both halves of this module exist for the same reason: a test that depends
//! on ambient process state passes or fails according to *how the suite was
//! launched* rather than what the code does. See [`unset`] for inherited env
//! vars and [`harden_dir`] for umask-derived directory permissions.
//!
//! Several Orbit surfaces read agent identity and run context from the
//! process environment — notably runtime actor identity and `tool run`
//! audit-role resolution. A test that asserts the *absence* of that context
//! ("attributes to the human actor", "falls back to the agent role") is only
//! correct when those variables are genuinely unset.
//!
//! GitHub CI runs the suite from a bare shell, so the assertions hold there by
//! accident. An agent running the same suite from inside a managed Orbit run
//! inherits `ORBIT_RUN_ID`, `ORBIT_AGENT_MODEL`, `ORBIT_TASK_ID`, … and those
//! tests flip to red for a reason that has nothing to do with the code under
//! test (ORB-10350). [`unset`] makes the expectation explicit instead of
//! ambient.
//!
//! Integration tests that *spawn* the `orbit` binary need the same defense
//! one process out, where the stakes include durable routing rather than just
//! attribution. [`INHERITED_AUTHORITY_ENV`] is the canonical list for that
//! case and [`clear_inherited_authority`] applies it.
//!
//! Exposed behind the `test-util` feature so integration tests and sibling
//! crates share one implementation rather than re-deriving the guard.

use std::sync::{Mutex, MutexGuard, OnceLock};

/// The identity pair consulted when a command carries no explicit
/// `--agent`/`--model` and no input attribution.
pub const AGENT_IDENTITY_ENV: &[&str] = &["ORBIT_AGENT_NAME", "ORBIT_AGENT_MODEL", "ORBIT_ACTOR"];

/// The variables an `orbit-engine` managed run exports into every spawned
/// activity (see `orbit-engine/src/context/env.rs`): job/task/session
/// identity plus the "this is a managed run" marker. A test that asserts
/// unmanaged-run behavior (default human attribution, an unleased audit
/// context, …) is only correct when these are genuinely unset — a child of a
/// managed Orbit run inherits them all (ORB-10436).
pub const MANAGED_RUN_ENV: &[&str] = &[
    "ORBIT_RUN_ID",
    "ORBIT_TASK_ID",
    "ORBIT_ACTIVE_TASK_ID",
    "ORBIT_SESSION_ID",
    "ORBIT_MANAGED_RUN_CONTEXT",
    "ORBIT_WORKSPACE",
];

/// Every ambient variable a test-spawned `orbit` child process must not
/// inherit from the process that launched the suite.
///
/// [`unset`] and [`MANAGED_RUN_ENV`] defend *in-process* tests. A test that
/// spawns the `orbit` binary needs the same defense one level out, and the
/// stakes are higher: the child re-reads the environment from scratch, and
/// two of these variables are durable routing authority.
/// `ORBIT_REGISTRY_ROOT` selects the host-global registry regardless of
/// `HOME`, and `ORBIT_WORKSPACE` selects a registered workspace inside it
/// (see `orbit-core/src/runtime/resolve.rs` and
/// `orbit-cmd/src/registry_runtime.rs`). Both are honored only behind the
/// managed-run trust boundary — but an agent running the suite from inside a
/// managed Orbit run supplies exactly that boundary, so a fixture that resets
/// only `HOME`/`ORBIT_ROOT` still routes its writes into the *live* workspace.
/// That is not hypothetical: it created three real task records before it was
/// caught (ORB-11300).
///
/// Clearing the whole set — routing, managed-run trust, actor identity, and
/// inherited sandbox grants — makes a fixture's authority a property of the
/// fixture rather than of how the suite was launched. Apply it with
/// [`clear_inherited_authority`] *before* any variable a test sets on
/// purpose, so the deliberate value wins.
pub const INHERITED_AUTHORITY_ENV: &[&str] = &[
    // Durable routing: which registry, workspace, and data root the child
    // writes to. `ORBIT_REGISTRY_ROOT` and `ORBIT_WORKSPACE` outrank `HOME`.
    "ORBIT_ROOT",
    "ORBIT_REGISTRY_ROOT",
    "ORBIT_WORKSPACE",
    "ORBIT_WORKSPACE_CLAIM_TOKEN",
    "ORBIT_WORKTREE_ROOT",
    "ORBIT_JOB_DIR",
    "ORBIT_ACTIVITY_DIR",
    // Managed-run trust boundary and run identity. Clearing the marker alone
    // would be enough to disarm routing today; clearing the whole envelope
    // keeps the fixture correct if that coupling ever changes.
    "ORBIT_MANAGED_RUN_CONTEXT",
    "ORBIT_RUN_ID",
    "ORBIT_TASK_ID",
    "ORBIT_ACTIVE_TASK_ID",
    "ORBIT_SESSION_ID",
    "ORBIT_ACTIVITY_ID",
    "ORBIT_STEP_INDEX",
    // Actor identity and audit role attributed to the child's writes.
    "ORBIT_AGENT_NAME",
    "ORBIT_AGENT_MODEL",
    "ORBIT_ACTOR",
    "ORBIT_OPERATOR",
    "ORBIT_TASK_ACTOR_KIND",
    // Sandbox and tool grants leased to the host activity, not to a fixture.
    "ORBIT_ACTIVITY_TOOLS",
    "ORBIT_ACTIVITY_FS_PROFILE",
    "ORBIT_PROC_ALLOWED_PROGRAMS",
    "ORBIT_BIN",
];

/// Clear every [`INHERITED_AUTHORITY_ENV`] variable from a child command.
///
/// `clear` is the command builder's `env_remove`. Passing it as a closure
/// keeps this crate free of a test-harness dependency while still giving every
/// fixture one shared list to drift against:
///
/// ```ignore
/// let mut command = cargo_bin_cmd!("orbit");
/// test_env::clear_inherited_authority(|name| {
///     command.env_remove(name);
/// });
/// command.current_dir(work).env("HOME", home);
/// ```
pub fn clear_inherited_authority(mut clear: impl FnMut(&str)) {
    for name in INHERITED_AUTHORITY_ENV {
        clear(name);
    }
}

/// Restores the variables captured by [`unset`] when dropped.
///
/// Holds a process-wide lock for its lifetime, so concurrent tests cannot
/// interleave environment mutations. Keep the guard's scope tight — in
/// particular, drop it before an `.await` (`clippy::await_holding_lock`); the
/// env is typically read during synchronous construction, so binding it around
/// just that call is enough.
#[must_use = "the environment is restored as soon as the guard is dropped"]
pub struct ScopedEnv {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(String, Option<String>)>,
}

/// Clear `names` for the returned guard's lifetime, restoring prior values on
/// drop. Names that are already unset are restored as unset.
pub fn unset<'a>(names: impl IntoIterator<Item = &'a str>) -> ScopedEnv {
    scoped(names.into_iter().map(|name| (name, None)))
}

/// Apply an explicit environment for the returned guard's lifetime, restoring
/// prior values on drop. `Some(value)` sets the variable, `None` clears it.
///
/// Use this instead of a bespoke set/restore pair whenever a test needs a
/// variable *present*: it captures the prior value the same way [`unset`] does
/// and takes the same process-wide lock, so a test that populates
/// `ORBIT_MANAGED_RUN_CONTEXT` cannot interleave with a sibling asserting the
/// unmanaged default (ORB-10540). A guard built here and a guard built by
/// [`unset`] are mutually exclusive; two independent locks would not be.
pub fn scoped<'a>(vars: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) -> ScopedEnv {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let lock = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let requested = vars.into_iter().collect::<Vec<_>>();
    let saved = requested
        .iter()
        .map(|(name, _)| ((*name).to_string(), std::env::var(name).ok()))
        .collect::<Vec<_>>();
    // SAFETY: the guard holds the process-wide lock for the whole mutation
    // window, so no other guarded reader/writer runs concurrently.
    unsafe {
        for (name, value) in &requested {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
    ScopedEnv { _lock: lock, saved }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        // SAFETY: the guard still holds the process-wide lock here.
        unsafe {
            for (name, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

/// Pin `path` to `0o700`, making a fixture directory independent of the
/// ambient umask.
///
/// `tempfile::tempdir()` creates its root with `0o777 & !umask`. CI runs with
/// the conventional `umask 022`, so the root lands `0o755` and every
/// permission-sensitive check downstream happens to pass. A developer box with
/// a permissive umask (`002`, common with user-private groups, or `000`) gets
/// a group- or world-writable root instead — and Orbit's search-companion
/// override validator legitimately refuses to execute a binary whose parent
/// directory is group/world writable. The fixture, not the validator, is what
/// needs to be deterministic (ORB-10350).
///
/// No-op on non-Unix targets.
#[cfg(unix)]
pub fn harden_dir(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::metadata(path)
        .unwrap_or_else(|error| panic!("read fixture dir metadata at {}: {error}", path.display()));
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(path, permissions)
        .unwrap_or_else(|error| panic!("chmod fixture dir {}: {error}", path.display()));
}

/// Non-Unix targets have no umask to defend against.
#[cfg(not(unix))]
pub fn harden_dir(_path: &std::path::Path) {}

#[cfg(test)]
#[path = "tests/test_env.rs"]
mod tests;
