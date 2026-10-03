use orbit_engine::RuntimeHost;

use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_layout, seed_executor,
};

/// Claude has no codex-style writable-dirs flag, so a worktree under
/// `.orbit/state/worktrees/` was unwriteable under the macOS sandbox
/// before T20260508-17. The host now appends the active worktree subpath
/// after the policy deny so SBPL last-match-wins re-grants writes there.
#[cfg(target_os = "macos")]
#[test]
fn resolve_executor_sandbox_reallows_claude_active_worktree_under_orbit() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    seed_executor(
        &runtime,
        "claude",
        Some(orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec),
    );

    let workspace_orbit = runtime
        .paths()
        .orbit_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().orbit_dir.clone());
    let worktree = workspace_orbit
        .join("state")
        .join("worktrees")
        .join("orbit-jrun-20260508-9999");
    std::fs::create_dir_all(&worktree).expect("create worktree");

    let resolved = runtime
        .resolve_executor_sandbox("claude", None, Some(&worktree))
        .expect("resolve")
        .expect("descriptor");
    let modify = &resolved.fs_profile.modify;
    let workspace_orbit_str = workspace_orbit.display().to_string();
    let workspace_orbit_deny = format!("!{workspace_orbit_str}/**");
    let deny_pos = modify
        .iter()
        .position(|entry| entry == &workspace_orbit_deny)
        .unwrap_or_else(|| {
            panic!(
                "default policy should deny workspace .orbit writes via {workspace_orbit_deny}; modify={modify:?}"
            )
        });
    let worktree_str = worktree
        .canonicalize()
        .unwrap_or_else(|_| worktree.clone())
        .display()
        .to_string();
    let allow_pos = modify
        .iter()
        .rposition(|entry| entry == &worktree_str)
        .unwrap_or_else(|| {
            panic!(
                "active worktree subpath should re-allow under sandbox: expected {worktree_str} in {modify:?}"
            )
        });
    assert!(
        deny_pos < allow_pos,
        "active worktree re-allow must come after policy deny: {modify:?}"
    );
}

/// Regression guard against a blanket reallow: when the cwd is NOT under
/// `.orbit/state/worktrees/`, no extra modify entry should be appended for
/// non-codex providers. Otherwise a misconfigured activity could quietly
/// widen the sandbox.
#[cfg(target_os = "macos")]
#[test]
fn resolve_executor_sandbox_does_not_reallow_for_non_worktree_cwd() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    seed_executor(
        &runtime,
        "claude",
        Some(orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec),
    );

    // Repo root is a sibling of `.orbit`, well outside the worktrees prefix.
    let resolved = runtime
        .resolve_executor_sandbox("claude", None, Some(&repo_root))
        .expect("resolve")
        .expect("descriptor");
    let modify = &resolved.fs_profile.modify;
    let workspace_orbit = runtime
        .paths()
        .orbit_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().orbit_dir.clone())
        .display()
        .to_string();
    // No reallow of `<workspace>/.orbit` itself for non-codex providers.
    assert!(
        !modify.iter().any(|entry| entry == &workspace_orbit),
        "claude must not blanket-reallow workspace .orbit when cwd is outside worktrees: {modify:?}"
    );
    // No reallow rooted at `.orbit/state/worktrees` either.
    let worktrees_root = format!("{workspace_orbit}/state/worktrees");
    assert!(
        !modify
            .iter()
            .any(|entry| entry.strip_prefix('!').unwrap_or(entry) == worktrees_root.as_str()),
        "claude must not reallow the worktrees root directly: {modify:?}"
    );
}

/// A cwd that resolves exactly to `.orbit/state/worktrees/` (no specific
/// jrun child) must not yield a grant — that would re-allow every worktree
/// in the registry. Only one path segment deeper qualifies.
#[cfg(target_os = "macos")]
#[test]
fn resolve_executor_sandbox_rejects_bare_worktrees_root_cwd() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    seed_executor(
        &runtime,
        "claude",
        Some(orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec),
    );
    let workspace_orbit = runtime
        .paths()
        .orbit_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().orbit_dir.clone());
    let worktrees_root = workspace_orbit.join("state").join("worktrees");
    std::fs::create_dir_all(&worktrees_root).expect("create worktrees root");

    let resolved = runtime
        .resolve_executor_sandbox("claude", None, Some(&worktrees_root))
        .expect("resolve")
        .expect("descriptor");
    let modify = &resolved.fs_profile.modify;
    let worktrees_root_str = worktrees_root
        .canonicalize()
        .unwrap_or_else(|_| worktrees_root.clone())
        .display()
        .to_string();
    assert!(
        !modify.iter().any(|entry| entry == &worktrees_root_str),
        "bare worktrees-root cwd must not re-allow the registry: {modify:?}"
    );
}
