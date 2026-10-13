use orbit_engine::RuntimeHost;
#[cfg(target_os = "linux")]
use orbit_exec::{
    compile_linux_bwrap_argv, linux_bwrap_write_grant_diagnostic, prepare_linux_bwrap_write_grants,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::adapter::engine_host::v2_host::sandbox::resolve::{
    append_codex_side_write_roots, append_orbit_child_runtime_write_roots,
    deny_registered_checkout_host_stores, resolve_fs_profile_absolute,
};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_config, runtime_with_workspace_layout, seed_executor,
};

/// Whether the last `file-write*` clause rooted inside `fixture` allows
/// `path`. Temp fixtures sit beneath the compiler's host scratch allows
/// (`/tmp`, `/private/var/folders`), which never cover a real `~/.orbit`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn last_compiled_file_write_allows_under(
    profile: &str,
    path: &std::path::Path,
    fixture: &std::path::Path,
) -> bool {
    last_compiled_file_write_under(profile, path, fixture) == Some(true)
}

/// Whether the last `file-write*` clause rooted inside `fixture` that covers
/// `path` allows it, or `None` when no such clause exists. A temp fixture
/// sits beneath the host scratch allows, so a `None` path is writable there
/// even though a real checkout would leave it to the default deny. A `regex`
/// clause, which policy globs such as `**/.env` compile to, counts whenever
/// it matches a `path` inside `fixture`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn last_compiled_file_write_under(
    profile: &str,
    path: &std::path::Path,
    fixture: &std::path::Path,
) -> Option<bool> {
    let rendered = path.display().to_string();
    let mut allowed = None;
    for line in profile.lines() {
        let (is_deny, filter) = if let Some(filter) = line.trim().strip_prefix("(deny file-write* ")
        {
            (true, filter)
        } else if let Some(filter) = line.trim().strip_prefix("(allow file-write* ") {
            (false, filter)
        } else {
            continue;
        };
        let filter = filter.trim_end_matches(')');
        let quoted = |prefix: &str| {
            filter
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix('"'))
        };
        let in_fixture = if let Some(root) = quoted("(subpath \"") {
            (rendered == root || rendered.starts_with(&format!("{root}/")))
                && std::path::Path::new(root).starts_with(fixture)
        } else if let Some(entry) = quoted("(literal \"") {
            rendered == entry && std::path::Path::new(entry).starts_with(fixture)
        } else if let Some(pattern) = quoted("(regex \"") {
            sbpl_regex(pattern).is_match(&rendered) && path.starts_with(fixture)
        } else {
            continue;
        };
        if in_fixture {
            allowed = Some(!is_deny);
        }
    }
    allowed
}

/// The pattern of an SBPL `regex` filter, with its string escapes undone.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn sbpl_regex(quoted: &str) -> regex::Regex {
    let mut pattern = String::with_capacity(quoted.len());
    let mut chars = quoted.chars();
    while let Some(c) = chars.next() {
        pattern.push(if c == '\\' {
            chars.next().expect("complete SBPL escape")
        } else {
            c
        });
    }
    regex::Regex::new(&pattern).unwrap_or_else(|error| panic!("SBPL regex `{pattern}`: {error}"))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn macos_child_profile_denies_registered_auto_task_definitions() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut resolved = resolve_fs_profile_absolute(&runtime, None, None).expect("resolve profile");
    append_orbit_child_runtime_write_roots(&runtime, true, &mut resolved);
    deny_registered_checkout_host_stores(&runtime, &mut resolved);

    let workspace_orbit = runtime
        .paths()
        .orbit_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().orbit_dir.clone())
        .display()
        .to_string();
    assert!(
        !resolved
            .modify
            .iter()
            .any(|entry| entry.starts_with(&format!("{workspace_orbit}/auto_tasks"))),
        "a macOS child must not receive a direct scheduler-definition write root: {:?}",
        resolved.modify
    );
    #[cfg(target_os = "linux")]
    {
        let auto_task = runtime
            .paths()
            .orbit_dir
            .join("auto_tasks/scheduled-job.yaml");
        assert!(
            linux_bwrap_write_grant_diagnostic(&resolved, &auto_task)
                .expect("diagnose scheduler-definition write")
                .is_some(),
            "registered scheduler definitions must remain unwritable"
        );
        let task_store = runtime.paths().orbit_dir.join("tasks/ORB-00001.yaml");
        assert!(
            linux_bwrap_write_grant_diagnostic(&resolved, &task_store)
                .expect("diagnose admitted task-store write")
                .is_none(),
            "admitted task-store writes must remain available"
        );
    }
}

/// [ORB-13841 / jrun-20261003-2101-c5] Deterministically remove the lock
/// after read_dir yields it, through the sandbox-preparation boundary used by
/// worktree setup. Both shared and separate per-worktree metadata remain denied.
#[cfg(target_os = "linux")]
#[test]
fn linux_worktree_sandbox_preparation_revalidates_disappearing_git_entries() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &linux_worktree_sandbox_preparation_revalidates_disappearing_git_entries,
    )) {
        return;
    }

    use std::cell::Cell;
    use std::rc::Rc;

    use crate::runtime::git_sandbox::{GitScanHookGuard, GitScanStage};

    for (location, nested, directory) in [
        ("shared", true, false),
        ("per-worktree", false, false),
        ("shared", true, true),
    ] {
        let (_root, runtime, repo_root) = runtime_with_workspace_layout();
        let repo_root = repo_root.canonicalize().unwrap();
        let common = repo_root.join(".git");
        let objects = common.join("objects");
        let git_dir = if nested {
            common.join("worktrees/worker")
        } else {
            repo_root.join("worker-metadata")
        };
        let worktree = repo_root.join(".orbit/state/worktrees/orbit-jrun-git-scan");
        std::fs::create_dir_all(&objects).unwrap();
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        let pointer = worktree.join(".git");
        std::fs::write(&pointer, format!("gitdir: {}\n", git_dir.display())).unwrap();
        std::fs::write(git_dir.join("commondir"), format!("{}\n", common.display())).unwrap();
        let lock = if directory {
            objects.join("transient-directory")
        } else if location == "shared" {
            objects.join("maintenance.lock")
        } else {
            git_dir.join("index.lock")
        };
        if directory {
            std::fs::create_dir(&lock).unwrap();
        } else {
            std::fs::write(&lock, "lock").unwrap();
        }
        seed_executor(
            &runtime,
            "claude",
            Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
        );
        let disappearances = Rc::new(Cell::new(0));
        let observed = Rc::clone(&disappearances);
        let scans = Rc::new(Cell::new(0));
        let scanned = Rc::clone(&scans);
        let scan_root = lock.parent().unwrap().to_path_buf();
        let _hook = GitScanHookGuard::install(move |stage, path| {
            if stage == GitScanStage::ReadDirectory && path == scan_root {
                scanned.set(scanned.get() + 1);
            }
            let removal_stage = if directory {
                GitScanStage::ReadDirectory
            } else {
                GitScanStage::InspectEntry
            };
            if stage == removal_stage && path == lock {
                if directory {
                    std::fs::remove_dir(path)?;
                } else {
                    std::fs::remove_file(path)?;
                }
                observed.set(observed.get() + 1);
            }
            Ok(())
        });
        let sandbox = runtime
            .resolve_executor_sandbox("claude", None, Some(&worktree))
            .expect("a genuinely vanished Git lock must not deny sandbox preparation")
            .unwrap();
        assert_eq!(
            disappearances.get(),
            1,
            "the fixture must reproduce the race"
        );
        assert!(
            scans.get() >= 2,
            "the vanished entry requires a complete rescan"
        );
        assert!(sandbox.managed_worktree);
        for denied in [
            &pointer,
            &git_dir.join("HEAD"),
            &common.join("objects/new-object"),
        ] {
            assert!(
                linux_bwrap_write_grant_diagnostic(&sandbox.fs_profile, denied)
                    .unwrap()
                    .is_some(),
                "{location}: {} must stay denied after revalidation",
                denied.display()
            );
        }
        let prepared = prepare_linux_bwrap_write_grants(&sandbox.fs_profile, &worktree).unwrap();
        assert!(
            prepared.unsatisfied.is_empty(),
            "{:?}",
            prepared.unsatisfied
        );
        let plan = compile_linux_bwrap_argv(
            &sandbox.fs_profile,
            "/bin/true",
            &[],
            Some(&worktree),
            sandbox.managed_worktree,
        )
        .expect("the revalidated worktree sandbox must compile for provider launch");
        assert!(plan.dropped_grants.is_empty(), "{:?}", plan.dropped_grants);
    }
}

/// Seed Orbit's shipped executors as `orbit init` would on `target_os`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn seed_shipped_executors(runtime: &crate::OrbitRuntime, target_os: &str) {
    crate::application::executor::seed_default_executors_for_platform(
        runtime.stores().executors(),
        false,
        target_os,
    )
    .expect("seed shipped executors");
}

/// [ORB-13857] A shipped agent executor on an OS with no sandbox backend used
/// to lose its declared kind and spawn bare. Sandbox availability is a host
/// precondition, so dispatch must refuse it — both on a fresh seed and on an
/// install whose earlier seed dropped the kind — before any child exists.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn shipped_agent_executor_without_a_host_backend_is_refused_with_remedies() {
    use orbit_engine::DispatchError;

    use crate::adapter::engine_host::v2_host::sandbox::resolve::resolve_executor_sandbox_on;

    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    // An install seeded before the refusal existed carries no sandbox.
    seed_executor(&runtime, "claude", None);
    seed_shipped_executors(&runtime, "windows");

    for provider in ["claude", "codex"] {
        let error =
            resolve_executor_sandbox_on(&runtime, provider, Some("implementer"), None, "windows")
                .expect_err("an unavailable declared sandbox must not resolve to a bare spawn");
        let DispatchError::CliInvocationPermanent(message) = error else {
            panic!("refusal must be permanent so retries do not re-attempt it: {error:?}");
        };
        for remedy in [
            format!("`{provider}`"),
            "`windows`".to_string(),
            "spec.sandbox: off".to_string(),
            "WSL2".to_string(),
        ] {
            assert!(
                message.contains(&remedy),
                "refusal must name {remedy}: {message}"
            );
        }
    }
}

/// [ORB-13857] The audited operator opt-out stays available where no backend
/// exists: re-seeding preserves it and resolution hands the runner the same
/// `off` descriptor it audits as `write_unrestricted` on every OS.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn explicit_off_still_resolves_on_a_host_without_a_backend() {
    use orbit_types::workflow::ExecutorSandboxKind;

    use crate::adapter::engine_host::v2_host::sandbox::resolve::resolve_executor_sandbox_on;

    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    seed_executor(&runtime, "claude", Some(ExecutorSandboxKind::Off));
    seed_shipped_executors(&runtime, "windows");

    let resolved =
        resolve_executor_sandbox_on(&runtime, "claude", Some("implementer"), None, "windows")
            .expect("explicit off resolves")
            .expect("explicit off is carried to the runner for auditing");
    assert_eq!(resolved.kind, ExecutorSandboxKind::Off);
    assert_eq!(
        resolved.fs_profile.name,
        orbit_types::policy::UNRESTRICTED_FS_PROFILE
    );
    assert!(resolved.fs_profile.modify.is_empty());
    assert!(!resolved.allow_fallback);
}

/// [ORB-13857] Executors that never declared a sandbox — `local-shell` and
/// user-authored ones — keep resolving to no sandbox on every OS.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn undeclared_sandbox_still_resolves_to_none_on_every_os() {
    use crate::adapter::engine_host::v2_host::sandbox::resolve::resolve_executor_sandbox_on;

    for target_os in ["windows", "freebsd", "linux", "macos"] {
        let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
        seed_executor(&runtime, "custom-agent", None);
        seed_shipped_executors(&runtime, target_os);

        for provider in ["local-shell", "custom-agent"] {
            let resolved = resolve_executor_sandbox_on(&runtime, provider, None, None, target_os)
                .unwrap_or_else(|error| {
                    panic!("{provider} on {target_os} must keep resolving: {error}")
                });
            assert!(
                resolved.is_none(),
                "{provider} on {target_os} declared no sandbox: {resolved:?}"
            );
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
const CODEX_WORKSPACE_WRITE: &str = "[execution.codex]\nsandbox = \"workspace-write\"\n";

/// Host state a workspace-write Codex run must not reach, the runtime stores
/// it must, and the two non-registered cwds it runs from. [ORB-14538]
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct CodexSideRootFixture {
    root: std::path::PathBuf,
    worktree: std::path::PathBuf,
    protected: Vec<std::path::PathBuf>,
    granted: Vec<std::path::PathBuf>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl CodexSideRootFixture {
    fn new(runtime: &crate::OrbitRuntime) -> Self {
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
        let root = orbit_exec::physical_with_missing_tail(
            runtime.paths().repo_root.parent().expect("fixture root"),
        );
        Self {
            root,
            worktree,
            protected,
            granted,
        }
    }
}

/// The side roots must be in play, or the reachability assertions are vacuous.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn assert_codex_side_roots_configured(runtime: &crate::OrbitRuntime) {
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

/// [ORB-14538] The macOS side-root appender emits contained store subpaths,
/// never a runtime root. Exercised through the SBPL compiler on every host,
/// without the registered-store denies that would mask a whole-root grant.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn codex_side_roots_compile_to_store_subpaths_only() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_config(Some(CODEX_WORKSPACE_WRITE));
    let fixture = CodexSideRootFixture::new(&runtime);
    assert_codex_side_roots_configured(&runtime);
    let mut resolved =
        resolve_fs_profile_absolute(&runtime, None, Some(&fixture.worktree)).expect("profile");
    append_codex_side_write_roots(&runtime, "codex", &mut resolved).expect("side roots");

    let sbpl = orbit_exec::compile_macos_sandbox_profile(&resolved, "codex").expect("compile");
    for path in &fixture.protected {
        let physical = orbit_exec::physical_with_missing_tail(path);
        assert!(
            !last_compiled_file_write_allows_under(&sbpl, &physical, &fixture.root),
            "a Codex side root must not reach {}:\n{sbpl}",
            physical.display()
        );
    }
    for path in &fixture.granted {
        let physical = orbit_exec::physical_with_missing_tail(path);
        assert!(
            last_compiled_file_write_allows_under(&sbpl, &physical, &fixture.root),
            "the Codex side roots must keep {} writable:\n{sbpl}",
            physical.display()
        );
    }
}
