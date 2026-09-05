//! Crate-wide test isolation for process-global roots, `HOME`/`USERPROFILE`,
//! and cwd.
//!
//! Workspace-init and the adjacent init/MCP-setup tests exercise
//! [`WorkspaceInitArgs`](crate::command::workspace) and
//! [`InitCommand`](crate::InitCommand), which read process-global state — `HOME`
//! / `USERPROFILE` to locate the machine-global workspace registry, and the
//! current working directory to derive a default workspace name. Historically
//! each test module carried its own `Mutex` plus ad-hoc save/restore logic, so
//! tests in *different* modules were never serialized against one another. A
//! racing test could restore the operator's real `HOME` while another nameless
//! `.tmpXXXXXX` workspace was still current, persisting `ws_.tmpXXXXXX` into the
//! operator's real `~/.orbit/workspaces.json` (this exact shape,
//! `/tmp/.tmp99JQFP`, broke global task lookup — ORB-10293).
//!
//! This module owns the single crate-wide lock and the one RAII guard every
//! such test shares. Acquiring [`EnvGuard`] serializes all env-mutating tests
//! against each other, so no race is possible; dropping it restores every
//! mutation — on normal completion *and* on panic/unwind — before releasing the
//! lock.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// The one crate-wide lock. A single shared lock (not per-module) is what makes
/// parallel execution safe: no two env-mutating tests can run at once, so the
/// operator's real registry can never be observed through a half-restored
/// `HOME`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Restores a single environment variable to a captured previous value.
fn restore_var(key: &str, previous: Option<OsString>) {
    match previous {
        Some(value) => unsafe { std::env::set_var(key, value) },
        None => unsafe { std::env::remove_var(key) },
    }
}

/// RAII guard that serializes and isolates process-global `HOME`/`USERPROFILE`
/// and the current working directory for a single test.
///
/// Acquire it first thing in a test, then chain [`EnvGuard::home`] /
/// [`EnvGuard::cwd`] to point the process at isolated fixtures. Every mutation
/// is captured once and reverted when the guard drops, so a panicking test
/// cannot leak an isolated `HOME` into a sibling test. The lock is released only
/// after restoration completes.
pub(crate) struct EnvGuard {
    home: Option<Option<OsString>>,
    userprofile: Option<Option<OsString>>,
    orbit_root: Option<Option<OsString>>,
    orbit_registry_root: Option<Option<OsString>>,
    orbit_workspace: Option<Option<OsString>>,
    managed_run_context: Option<Option<OsString>>,
    run_id: Option<Option<OsString>>,
    cwd: Option<PathBuf>,
    path: Option<Option<OsString>>,
    // Declared last so it drops last: the lock is held until every restoration
    // in `Drop` has run.
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    /// Acquire the crate-wide env lock without mutating anything yet.
    ///
    /// Recovers from a poisoned lock instead of propagating it: a prior test
    /// that panicked will have restored the environment through this guard's
    /// `Drop`, so the unit payload carries no state and poison must not cascade
    /// spurious failures into unrelated tests.
    pub(crate) fn acquire() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        Self {
            home: None,
            userprofile: None,
            orbit_root: None,
            orbit_registry_root: None,
            orbit_workspace: None,
            managed_run_context: None,
            run_id: None,
            cwd: None,
            path: None,
            _lock: lock,
        }
    }

    /// Simulate the registry locator supplied to a trusted managed child.
    ///
    /// This exists for fixtures that must prove they remain isolated when the
    /// test process itself is launched by a managed run. The marker and run id
    /// are the trust boundary required for production registry-root precedence.
    pub(crate) fn managed_registry_root(mut self, root: &Path) -> Self {
        if self.orbit_registry_root.is_none() {
            self.orbit_registry_root = Some(std::env::var_os("ORBIT_REGISTRY_ROOT"));
        }
        if self.managed_run_context.is_none() {
            self.managed_run_context = Some(std::env::var_os("ORBIT_MANAGED_RUN_CONTEXT"));
        }
        if self.run_id.is_none() {
            self.run_id = Some(std::env::var_os("ORBIT_RUN_ID"));
        }
        unsafe {
            std::env::set_var("ORBIT_REGISTRY_ROOT", root);
            std::env::set_var("ORBIT_MANAGED_RUN_CONTEXT", "1");
            std::env::set_var("ORBIT_RUN_ID", "jrun-env-isolation");
        }
        self
    }

    /// Point `HOME` and `USERPROFILE` at `home`, capturing the prior values the
    /// first time either is set.
    ///
    /// Also clears `ORBIT_ROOT`, `ORBIT_REGISTRY_ROOT`, and `ORBIT_WORKSPACE`,
    /// capturing their prior values. The first is an explicit workspace-root
    /// escape hatch; the second selects a managed child's host registry; the
    /// third is the trusted logical workspace selector. Any of them would
    /// otherwise route an isolated fixture through real shared state.
    pub(crate) fn home(mut self, home: &Path) -> Self {
        if self.home.is_none() {
            self.home = Some(std::env::var_os("HOME"));
        }
        if self.userprofile.is_none() {
            self.userprofile = Some(std::env::var_os("USERPROFILE"));
        }
        if self.orbit_root.is_none() {
            self.orbit_root = Some(std::env::var_os("ORBIT_ROOT"));
        }
        if self.orbit_registry_root.is_none() {
            self.orbit_registry_root = Some(std::env::var_os("ORBIT_REGISTRY_ROOT"));
        }
        if self.orbit_workspace.is_none() {
            self.orbit_workspace = Some(std::env::var_os("ORBIT_WORKSPACE"));
        }
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("USERPROFILE", home);
            std::env::remove_var("ORBIT_ROOT");
            std::env::remove_var("ORBIT_REGISTRY_ROOT");
            std::env::remove_var("ORBIT_WORKSPACE");
        }
        self
    }

    /// Switch the current working directory to `dir`, capturing the prior cwd
    /// the first time it is set.
    pub(crate) fn cwd(mut self, dir: &Path) -> Self {
        if self.cwd.is_none() {
            self.cwd = Some(std::env::current_dir().expect("capture cwd"));
        }
        std::env::set_current_dir(dir).expect("enter isolated cwd");
        self
    }

    /// Point `PATH` at `dir` alone, capturing the prior value the first time it
    /// is set.
    ///
    /// `orbit init` probes `PATH` to decide which agent CLIs exist and seeds a
    /// crew per provider it finds (`init::agent_detect`). A test that asserts
    /// on the seeded crews therefore depends on which CLIs the developer
    /// happens to have installed, and passes or fails by machine. Pointing
    /// `PATH` at an empty fixture directory makes detection find nothing, which
    /// is the same answer on every host.
    pub(crate) fn path(mut self, dir: &Path) -> Self {
        if self.path.is_none() {
            self.path = Some(std::env::var_os("PATH"));
        }
        unsafe {
            std::env::set_var("PATH", dir);
        }
        self
    }

    /// Run `f` with `HOME`/`USERPROFILE` temporarily pointed at `home`,
    /// restoring the values this guard currently exposes afterward — even if
    /// `f` panics.
    ///
    /// Used by tests that must exercise a nested isolated-home scope (a
    /// validation home layered over a live home) without re-acquiring the lock
    /// this guard already holds.
    pub(crate) fn with_home<R>(&self, home: &Path, f: impl FnOnce() -> R) -> R {
        let _scope = HomeScope::set(home);
        f()
    }

    /// Restore all captured state while retaining the crate-wide lock.
    ///
    /// This lets tests verify restoration before another env-mutating test can
    /// acquire the lock. `Drop` calls the same implementation for panic-safe
    /// cleanup.
    pub(crate) fn restore_now(&mut self) {
        self.restore();
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

impl EnvGuard {
    fn restore(&mut self) {
        // Restore cwd before HOME so nothing observes a mismatched pair. The
        // `_lock` field remains held until EnvGuard itself drops.
        if let Some(previous) = self.cwd.take() {
            let _ = std::env::set_current_dir(previous);
        }
        if let Some(previous) = self.userprofile.take() {
            restore_var("USERPROFILE", previous);
        }
        if let Some(previous) = self.home.take() {
            restore_var("HOME", previous);
        }
        if let Some(previous) = self.orbit_root.take() {
            restore_var("ORBIT_ROOT", previous);
        }
        if let Some(previous) = self.orbit_registry_root.take() {
            restore_var("ORBIT_REGISTRY_ROOT", previous);
        }
        if let Some(previous) = self.orbit_workspace.take() {
            restore_var("ORBIT_WORKSPACE", previous);
        }
        if let Some(previous) = self.run_id.take() {
            restore_var("ORBIT_RUN_ID", previous);
        }
        if let Some(previous) = self.managed_run_context.take() {
            restore_var("ORBIT_MANAGED_RUN_CONTEXT", previous);
        }
        if let Some(previous) = self.path.take() {
            restore_var("PATH", previous);
        }
    }
}

/// A nested, lock-free `HOME`/`USERPROFILE` override that restores on drop.
/// Valid only while an [`EnvGuard`] already holds the crate-wide lock.
struct HomeScope {
    home: Option<OsString>,
    userprofile: Option<OsString>,
}

impl HomeScope {
    fn set(home: &Path) -> Self {
        let scope = Self {
            home: std::env::var_os("HOME"),
            userprofile: std::env::var_os("USERPROFILE"),
        };
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("USERPROFILE", home);
        }
        scope
    }
}

impl Drop for HomeScope {
    fn drop(&mut self) {
        restore_var("USERPROFILE", self.userprofile.take());
        restore_var("HOME", self.home.take());
    }
}
