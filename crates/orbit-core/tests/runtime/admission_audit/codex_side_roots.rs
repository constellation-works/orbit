//! Codex managed-checkout side roots through the exported provider config and sandbox resolver.
use super::engine_support::{runtime_with_workspace_config, seed_executor};
#[cfg(target_os = "macos")]
use super::sandbox_resolve::last_compiled_file_write_allows_under;
use orbit_engine::RuntimeHost;
#[cfg(target_os = "linux")]
use orbit_exec::{compile_linux_bwrap_argv, prepare_linux_bwrap_write_grants};
const CODEX_WORKSPACE_WRITE: &str = "[execution.codex]\nsandbox = \"workspace-write\"\n";
/// Host state a workspace-write Codex run must not reach, the runtime stores
/// it must, and the two non-registered cwds it runs from. [ORB-14538]
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct CodexSideRootFixture {
    #[cfg(target_os = "macos")]
    root: std::path::PathBuf,
    worktree: std::path::PathBuf,
    recovery: std::path::PathBuf,
    protected: Vec<std::path::PathBuf>,
    granted: Vec<std::path::PathBuf>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl CodexSideRootFixture {
    fn new(runtime: &orbit_core::OrbitRuntime) -> Self {
        let canonical = |path: &std::path::Path| path.canonicalize().expect("canonical root");
        let orbit = canonical(&runtime.paths().orbit_dir);
        let global = canonical(&runtime.paths().global_dir);
        let file = |path: std::path::PathBuf| {
            std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
            std::fs::write(&path, "host").expect("write fixture file");
            path
        };
        let worktree = orbit.join("state/worktrees/orbit-jrun-codex-side-roots");
        let recovery = orbit.join("state/recovery-checkouts/orbit-jrun-codex-side-roots");
        for checkout in [&worktree, &recovery] {
            std::fs::create_dir_all(checkout.join(".orbit/tmp")).expect("create checkout");
        }
        for store in [
            "tasks",
            "frictions",
            "state/audit",
            "state/logs",
            "state/job-runs",
        ] {
            std::fs::create_dir_all(orbit.join(store)).expect("workspace store");
        }
        for store in ["tasks", "state/audit", "state/logs", "cache"] {
            std::fs::create_dir_all(global.join(store)).expect("global store");
        }
        let protected = vec![
            file(orbit.join("config.toml")),
            file(orbit.join("auto_tasks/nightly.yaml")),
            file(orbit.join("routines/sweep.yaml")),
            file(orbit.join("resources/crew.yaml")),
            file(orbit.join("state/worktrees/orbit-jrun-other/src/lib.rs")),
            file(global.join("bin/orbit")),
            file(global.join("config.toml")),
            file(global.join("workspaces.json")),
            file(global.join("resources/crew.yaml")),
        ];
        let granted = vec![
            orbit.join("tasks/ORB-1.yaml"),
            orbit.join("state/job-runs/run.yaml"),
            global.join("tasks/ORB-1.yaml"),
            global.join("cache/artifact"),
        ];
        #[cfg(target_os = "macos")]
        let root = orbit_exec::physical_with_missing_tail(
            runtime.paths().repo_root.parent().expect("fixture root"),
        );
        Self {
            #[cfg(target_os = "macos")]
            root,
            worktree,
            recovery,
            protected,
            granted,
        }
    }
}

/// The side roots must be in play, or the reachability assertions are vacuous.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn assert_codex_side_roots_configured(runtime: &orbit_core::OrbitRuntime) {
    let config = RuntimeHost::agent_provider_config(runtime);
    let dirs: Vec<String> = serde_json::from_str(
        config
            .get("writable_dirs_json")
            .expect("workspace-write Codex carries side roots"),
    )
    .expect("side roots parse");
    assert!(
        !dirs.is_empty(),
        "the fixture must exercise the Codex side roots"
    );
}

/// Whether the last Bubblewrap mount covering `path` is writable. Mounts
/// stack in argv order, and a bind covers its destination's whole subtree.
#[cfg(target_os = "linux")]
fn bwrap_argv_writes(args: &[String], path: &std::path::Path) -> bool {
    let mut writable = false;
    let mut index = 0;
    while index < args.len() {
        let (arity, mount) = match args[index].as_str() {
            "--bind" | "--bind-try" | "--dev-bind" | "--dev-bind-try" => (2, Some(true)),
            "--ro-bind" | "--ro-bind-try" => (2, Some(false)),
            "--bind-fd" => (2, Some(true)),
            "--ro-bind-fd" => (2, Some(false)),
            "--tmpfs" | "--remount-ro" => (1, Some(false)),
            "--dir" => (1, None),
            _ => (0, None),
        };
        if let (Some(writes), Some(destination)) = (mount, args.get(index + arity))
            && path.starts_with(destination)
        {
            writable = writes;
        }
        index += arity + 1;
    }
    writable
}

/// [ORB-14538] A workspace-write Codex run from a managed worktree or a
/// recovery checkout used to receive the registered `.orbit` and the global
/// `~/.orbit` as bare side roots, which Bubblewrap binds as whole subtrees:
/// the host clock's stores, other runs' worktrees, global config and the
/// `orbit` binary were writable. Only the runtime stores may be.
#[cfg(target_os = "linux")]
#[test]
fn linux_codex_side_roots_from_a_managed_checkout_reach_only_runtime_stores() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "linux_codex_side_roots_from_a_managed_checkout_reach_only_runtime_stores",
    ) {
        return;
    }

    let (_root, runtime, _repo_root) = runtime_with_workspace_config(Some(CODEX_WORKSPACE_WRITE));
    seed_executor(
        &runtime,
        "codex",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );
    let fixture = CodexSideRootFixture::new(&runtime);
    assert_codex_side_roots_configured(&runtime);

    for cwd in [&fixture.worktree, &fixture.recovery] {
        let resolved = runtime
            .resolve_executor_sandbox("codex", None, Some(cwd))
            .expect("resolve Codex sandbox")
            .expect("descriptor");
        assert!(
            resolved.managed_worktree,
            "{} is host-managed",
            cwd.display()
        );
        prepare_linux_bwrap_write_grants(&resolved.fs_profile, cwd).expect("prepare grants");
        let plan = compile_linux_bwrap_argv(
            &resolved.fs_profile,
            "/bin/true",
            &[],
            Some(cwd),
            resolved.managed_worktree,
        )
        .expect("compile Codex sandbox");
        for path in &fixture.protected {
            assert!(
                !bwrap_argv_writes(&plan.args, path),
                "Codex from {} must not write {}: {:?}",
                cwd.display(),
                path.display(),
                resolved.fs_profile.modify
            );
        }
        for path in fixture.granted.iter().chain([&cwd.join("src/lib.rs")]) {
            assert!(
                bwrap_argv_writes(&plan.args, path),
                "Codex from {} must keep writing {}: {:?}",
                cwd.display(),
                path.display(),
                plan.args
            );
        }
    }
}

/// [ORB-14538] The full macOS resolution from a managed worktree or a
/// recovery checkout keeps the same boundary under SBPL `subpath` semantics.
#[cfg(target_os = "macos")]
#[test]
fn macos_codex_side_roots_from_a_managed_checkout_reach_only_runtime_stores() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "macos_codex_side_roots_from_a_managed_checkout_reach_only_runtime_stores",
    ) {
        return;
    }

    let (_root, runtime, _repo_root) = runtime_with_workspace_config(Some(CODEX_WORKSPACE_WRITE));
    seed_executor(
        &runtime,
        "codex",
        Some(orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec),
    );
    let fixture = CodexSideRootFixture::new(&runtime);
    assert_codex_side_roots_configured(&runtime);

    for cwd in [&fixture.worktree, &fixture.recovery] {
        let resolved = runtime
            .resolve_executor_sandbox("codex", None, Some(cwd))
            .expect("resolve Codex sandbox")
            .expect("descriptor");
        let sbpl = orbit_exec::compile_macos_sandbox_profile(&resolved.fs_profile, "codex")
            .expect("compile Codex profile");
        for path in &fixture.protected {
            let physical = orbit_exec::physical_with_missing_tail(path);
            assert!(
                !last_compiled_file_write_allows_under(&sbpl, &physical, &fixture.root),
                "Codex from {} must not write {}:\n{sbpl}",
                cwd.display(),
                physical.display()
            );
        }
        for path in fixture.granted.iter().chain([&cwd.join("src/lib.rs")]) {
            let physical = orbit_exec::physical_with_missing_tail(path);
            assert!(
                last_compiled_file_write_allows_under(&sbpl, &physical, &fixture.root),
                "Codex from {} must keep writing {}:\n{sbpl}",
                cwd.display(),
                physical.display()
            );
        }
    }
}
