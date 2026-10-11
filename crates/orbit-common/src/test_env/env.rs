use std::sync::{
    Mutex, MutexGuard, OnceLock,
    atomic::{AtomicUsize, Ordering},
};

static ACTIVE_SCOPED_ENVS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn scoped_env_active() -> bool {
    ACTIVE_SCOPED_ENVS.load(Ordering::SeqCst) != 0
}

/// Restore real durability syncs, process-wide, until the returned guard drops.
///
/// Test runs set [`RELAXED_DURABILITY_ENV`](crate::fs::io::RELAXED_DURABILITY_ENV),
/// so `fs::io::sync_file`, `sync_file_data` and `sqlite_synchronous` skip
/// their flush. A test whose subject is the sync itself, or a durability or
/// crash-recovery path that should exercise the real call, holds this guard.
/// It is process-wide because a sync can run on another thread, so tests
/// running beside it only pay for real syncs again. A SQLite connection keeps
/// the level it was opened with, so take the guard before opening the store.
/// An `orbit` child the test spawns reads its own environment: remove the
/// variable from that child's command instead.
#[must_use = "real syncs last only while the guard lives"]
pub fn require_durable_syncs() -> DurableSyncs {
    crate::fs::io::DURABLE_SYNC_HOLDS.fetch_add(1, Ordering::SeqCst);
    DurableSyncs(())
}

/// Guard from [`require_durable_syncs`].
pub struct DurableSyncs(());

impl Drop for DurableSyncs {
    fn drop(&mut self) {
        crate::fs::io::DURABLE_SYNC_HOLDS.fetch_sub(1, Ordering::SeqCst);
    }
}

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
/// Git's repository locators also outrank cwd. Scrub Git setup commands as
/// well as Orbit children: otherwise `git init` can initialize the parent's
/// repository, and a workflow worker can acquire the parent's fetch lock even
/// though its home and Orbit authority were isolated (ORB-13940).
///
/// Clearing the whole set — routing, managed-run trust, actor identity,
/// inherited sandbox grants, and the plugin broker socket — makes a fixture's
/// authority a property of the fixture rather than of how the suite was
/// launched. Apply it with [`clear_inherited_authority`] *before* any variable
/// a test sets on purpose, so the deliberate value wins.
pub const INHERITED_AUTHORITY_ENV: &[&str] = &[
    // Git repository and write destinations override a fixture's cwd.
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
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
    "ORBIT_WORKER_CONTEXT_REQUIRED",
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
    "ORBIT_ACTIVITY_TOOL_POLICY",
    "ORBIT_ACTIVITY_TOOLS_DENY",
    "ORBIT_ACTIVITY_NAME",
    "ORBIT_ACTIVITY_FS_PROFILE",
    "ORBIT_ACTIVITY_DEADLINE_UNIX_MS",
    "ORBIT_PROC_ALLOWED_PROGRAMS",
    "ORBIT_PROC_PROGRAM_POLICY",
    "ORBIT_PROC_DISALLOWED_PROGRAMS",
    "ORBIT_BIN",
    // Routes plugin tool calls to the enclosing run's host broker instead of
    // the fixture's own install; a fixture that needs a broker sets its own.
    "ORBIT_PLUGIN_BROKER",
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
    ACTIVE_SCOPED_ENVS.fetch_add(1, Ordering::SeqCst);
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
        ACTIVE_SCOPED_ENVS.fetch_sub(1, Ordering::SeqCst);
    }
}
