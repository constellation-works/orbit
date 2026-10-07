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

/// A run whose cwd is the registered checkout must not receive the policy's
/// versioned `.orbit` exceptions against that checkout's live host-clock
/// stores. The assertion goes through `resolve_executor_sandbox`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn registered_checkout_denies_live_orbit_versioned_stores() {
    use orbit_types::workflow::ExecutorSandboxKind;

    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let repo_root = repo_root.canonicalize().expect("canonical repo");
    std::fs::create_dir_all(repo_root.join(".git")).expect("git metadata");
    let sandbox = if cfg!(target_os = "linux") {
        ExecutorSandboxKind::LinuxBwrap
    } else {
        ExecutorSandboxKind::MacosSandboxExec
    };
    for provider in ["claude", "codex"] {
        seed_executor(&runtime, provider, Some(sandbox));
        let resolved = runtime
            .resolve_executor_sandbox(provider, None, Some(&repo_root))
            .unwrap_or_else(|error| panic!("{provider} sandbox must resolve: {error}"))
            .unwrap_or_else(|| panic!("{provider} must resolve a sandbox"));
        assert_registered_host_stores_denied(&runtime, &resolved, provider);
        assert_registered_checkout_boundaries_unchanged(&runtime, &resolved, &repo_root);
    }

    #[cfg(target_os = "macos")]
    {
        let worktree = repo_root.join(".orbit/state/worktrees/orbit-jrun-registered-deny");
        std::fs::create_dir_all(worktree.join(".orbit/auto_tasks")).expect("worktree orbit");
        std::fs::create_dir_all(worktree.join(".orbit/routines")).expect("worktree routines");
        std::fs::create_dir_all(worktree.join(".orbit/resources")).expect("worktree resources");
        std::fs::write(worktree.join(".orbit/config.toml"), "versioned = true")
            .expect("worktree config");
        let resolved = runtime
            .resolve_executor_sandbox("claude", None, Some(&worktree))
            .expect("resolve worktree sandbox")
            .expect("sandbox");
        let sbpl = orbit_exec::compile_macos_sandbox_profile(&resolved.fs_profile, "claude")
            .expect("compile worktree profile");
        let orbit = worktree.join(".orbit");
        for relative in [
            "auto_tasks/child-write.yaml",
            "routines/x.yaml",
            "config.toml",
            "resources/x.yaml",
        ] {
            let path = orbit_exec::physical_with_missing_tail(&orbit.join(relative));
            assert!(
                last_compiled_file_write_allows(&sbpl, &path),
                "worktree-local `{relative}` must stay writable:\n{sbpl}"
            );
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn assert_registered_host_stores_denied(
    runtime: &crate::OrbitRuntime,
    resolved: &orbit_engine::ResolvedSandbox,
    provider: &str,
) {
    let orbit = runtime
        .paths()
        .orbit_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().orbit_dir.clone());
    let protected = [
        orbit.join("auto_tasks/child-write.yaml"),
        orbit.join("routines/x.yaml"),
        orbit.join("config.toml"),
        orbit.join("resources/x.yaml"),
    ];

    #[cfg(target_os = "linux")]
    {
        for path in &protected {
            let denied = linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, path)
                .unwrap_or_else(|error| panic!("diagnose {}: {error}", path.display()));
            assert!(
                denied.is_some(),
                "{provider} must deny {} from the registered checkout: {denied:?}\nrules: {:?}",
                path.display(),
                resolved.fs_profile.modify
            );
        }
        for still_granted in [orbit.join("tmp/scratch"), orbit.join("tasks")] {
            assert!(
                linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, &still_granted)
                    .unwrap_or_else(|error| panic!("diagnose {}: {error}", still_granted.display()))
                    .is_none(),
                "{provider} must keep {} granted: {:?}",
                still_granted.display(),
                resolved.fs_profile.modify
            );
        }
    }

    #[cfg(target_os = "macos")]
    {
        let sbpl = orbit_exec::compile_macos_sandbox_profile(&resolved.fs_profile, provider)
            .unwrap_or_else(|error| panic!("compile {provider} profile: {error}"));
        for path in &protected {
            let physical = orbit_exec::physical_with_missing_tail(path);
            assert!(
                !last_compiled_file_write_allows(&sbpl, &physical),
                "{provider} compiled profile must deny {}:\n{sbpl}",
                physical.display()
            );
        }
    }
}

/// Recovery, Git metadata, and the plugin mask are applied on the same
/// resolution that denies the registered host-clock stores.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn assert_registered_checkout_boundaries_unchanged(
    runtime: &crate::OrbitRuntime,
    resolved: &orbit_engine::ResolvedSandbox,
    repo_root: &std::path::Path,
) {
    let mask = resolved
        .mask
        .as_ref()
        .expect("plugin mask stays on a sandboxed resolution");
    let targets = mask
        .targets
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    assert!(
        targets.iter().any(|path| path.ends_with("state/plugins")),
        "plugin state mask missing: {targets:?}"
    );
    assert!(
        targets
            .iter()
            .any(|path| path.ends_with("state/plugin-secrets")),
        "plugin secret mask missing: {targets:?}"
    );

    #[cfg(target_os = "linux")]
    {
        let global = runtime
            .paths()
            .global_dir
            .canonicalize()
            .unwrap_or_else(|_| runtime.paths().global_dir.clone());
        let authority = global.join("state/recovery-authority/authority.db");
        let denied = linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, &authority)
            .expect("diagnose recovery authority");
        assert!(
            denied.is_some(),
            "recovery authority must stay denied: {denied:?}"
        );
        let git = repo_root
            .canonicalize()
            .expect("canonical repo")
            .join(".git");
        assert!(
            resolved
                .fs_profile
                .modify
                .iter()
                .any(|rule| rule == &format!("!{}/**", git.display())),
            "git metadata deny missing from {:?}",
            resolved.fs_profile.modify
        );
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (runtime, repo_root);
    }
}

/// Last `file-write*` clause whose `subpath` covers `path`. The compiled
/// profile denies by default, so a path with no covering clause is denied.
#[cfg(target_os = "macos")]
fn last_compiled_file_write_allows(profile: &str, path: &std::path::Path) -> bool {
    last_compiled_file_write_allows_under(profile, path, std::path::Path::new("/"))
}

/// [`last_compiled_file_write_allows`] over the clauses rooted inside
/// `fixture`. Temp fixtures sit beneath the compiler's host scratch allows
/// (`/tmp`, `/private/var/folders`), which never cover a real `~/.orbit`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn last_compiled_file_write_allows_under(
    profile: &str,
    path: &std::path::Path,
    fixture: &std::path::Path,
) -> bool {
    let rendered = path.display().to_string();
    let mut allowed = false;
    for line in profile.lines() {
        let (is_deny, filter) = if let Some(filter) = line.trim().strip_prefix("(deny file-write* ")
        {
            (true, filter)
        } else if let Some(filter) = line.trim().strip_prefix("(allow file-write* ") {
            (false, filter)
        } else {
            continue;
        };
        let Some(root) = filter
            .trim_end_matches(')')
            .strip_prefix("(subpath \"")
            .and_then(|rest| rest.strip_suffix('"'))
        else {
            continue;
        };
        let covers = rendered == root || rendered.starts_with(&format!("{root}/"));
        if covers && std::path::Path::new(root).starts_with(fixture) {
            allowed = !is_deny;
        }
    }
    allowed
}

/// [ORB-13458] Every positive macOS `modify` entry compiles to an SBPL write
/// allow, so provider and runtime conveniences must not hand a reviewer the
/// source checkout, its managed worktree, or primary workspace `.orbit` stores.
#[cfg(target_os = "macos")]
#[test]
fn macos_reviewer_profile_grants_no_source_or_workspace_writes() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let canonical_repo = repo_root.canonicalize().expect("canonical repo");
    let worktree = canonical_repo.join(".orbit/state/worktrees/orbit-jrun-orb-13458");
    std::fs::create_dir_all(&worktree).expect("create managed worktree");
    let inspection = tempfile::tempdir().expect("inspection checkout");
    let inspection_root = inspection
        .path()
        .canonicalize()
        .expect("canonical inspection");
    let global = runtime
        .paths()
        .global_dir
        .canonicalize()
        .expect("canonical global root")
        .display()
        .to_string();
    let protected = [
        canonical_repo.display().to_string(),
        inspection_root.display().to_string(),
    ];

    for provider in ["claude", "codex"] {
        seed_executor(
            &runtime,
            provider,
            Some(orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec),
        );
        for cwd in [&inspection_root, &worktree] {
            let sandbox = runtime
                .resolve_executor_sandbox(provider, Some("reviewer"), Some(cwd))
                .expect("resolve reviewer sandbox")
                .expect("macOS sandbox");
            let writes = sandbox
                .fs_profile
                .modify
                .iter()
                .filter(|rule| !rule.starts_with('!'))
                .collect::<Vec<_>>();
            for root in &protected {
                assert!(
                    writes.iter().all(|rule| !rule.starts_with(root.as_str())),
                    "{provider} reviewer from {} must not write under {root}: {writes:?}",
                    cwd.display()
                );
            }
            assert!(
                writes
                    .iter()
                    .any(|rule| *rule == &format!("{global}/tasks/**")),
                "{provider} reviewer keeps the global task store for nested Orbit calls: {writes:?}"
            );
            assert!(
                !writes
                    .iter()
                    .any(|rule| rule.starts_with(&format!("{global}/cache"))),
                "{provider} reviewer must not gain the implementer host cache: {writes:?}"
            );
        }
    }
}

/// The recovery authority is the one durable record a resume trusts, so it
/// must sit outside every grant the leaf receives — including the run store
/// the leaf legitimately writes through `orbit.task.*` and audit tools.
#[cfg(target_os = "linux")]
#[test]
fn linux_leaf_keeps_run_store_grants_but_never_reaches_the_recovery_authority() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    seed_executor(
        &runtime,
        "claude",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );

    let resolved = runtime
        .resolve_executor_sandbox("claude", None, Some(&repo_root))
        .expect("resolve Linux sandbox")
        .expect("descriptor");
    let global = runtime
        .paths()
        .global_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().global_dir.clone());
    let authority = global.join("state/recovery-authority");

    // The database and both SQLite sidecars: a writer holding any one of them
    // could inject rows, so the deny has to cover the whole root.
    for name in ["authority.db", "authority.db-wal", "authority.db-shm"] {
        let denied =
            linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, &authority.join(name))
                .expect("diagnose authority path")
                .expect("authority path must be attributably denied");
        assert!(
            denied.contains(name) && denied.contains("denyModify rule"),
            "the deny must name the path and the rule that shadows it: {denied}"
        );
    }

    // Removing the run-store grants is not the fix and must not be the effect:
    // admitted leaf tool writes keep working.
    for granted in [
        global.join("orbit.db"),
        global.join("orbit.db-wal"),
        global.join("orbit.db-shm"),
        global.join("tasks"),
        global.join("state/audit"),
    ] {
        assert!(
            linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, &granted)
                .expect("diagnose granted path")
                .is_none(),
            "admitted leaf write {} must stay granted: {:?}",
            granted.display(),
            resolved.fs_profile.modify
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn resolved_sandbox_never_materializes_checkout_identity_as_a_write_anchor() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    seed_executor(
        &runtime,
        "claude",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );
    let worktree = runtime
        .paths()
        .orbit_dir
        .join("state/worktrees/orbit-jrun-versioned-config");
    for directory in ["auto_tasks", "routines", "resources", "state", "tasks"] {
        std::fs::create_dir_all(worktree.join(".orbit").join(directory))
            .expect("create worktree Orbit fixture");
    }
    std::fs::write(worktree.join(".orbit/config.toml"), "versioned = true")
        .expect("create versioned config fixture");

    let resolved = runtime
        .resolve_executor_sandbox("claude", None, Some(&worktree))
        .expect("resolve")
        .expect("descriptor");
    let modify = &resolved.fs_profile.modify;
    let registered_auto_tasks = runtime
        .paths()
        .orbit_dir
        .join("auto_tasks/child-write.yaml");
    assert!(
        linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, &registered_auto_tasks)
            .expect("diagnose direct child write")
            .is_some(),
        "a child must not write the registered checkout's scheduler definitions"
    );
    let canonical_worktree = worktree.canonicalize().expect("canonical worktree");
    let orbit = canonical_worktree.join(".orbit");
    let deny = format!("!{}/**", orbit.display());
    let deny_pos = modify
        .iter()
        .position(|rule| rule == &deny)
        .unwrap_or_else(|| panic!("default Orbit deny missing from {modify:?}"));

    for allowed in [
        format!("{}/auto_tasks/**", orbit.display()),
        format!("{}/routines/**", orbit.display()),
        format!("{}/config.toml", orbit.display()),
        format!("{}/resources/**", orbit.display()),
    ] {
        let allow_pos = modify
            .iter()
            .position(|rule| rule == &allowed)
            .unwrap_or_else(|| panic!("versioned exception `{allowed}` missing from {modify:?}"));
        assert!(deny_pos < allow_pos, "exception must follow default deny");
    }
    let identity = orbit.join("config.yaml");
    assert!(
        linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, &identity)
            .expect("diagnose runtime identity")
            .is_some(),
        "checkout-local runtime identity must remain read-only"
    );
    let prepared = prepare_linux_bwrap_write_grants(&resolved.fs_profile, &worktree)
        .expect("prepare versioned write grants");
    assert!(
        !identity.exists(),
        "an absent runtime identity must never become an empty sandbox anchor"
    );
    assert!(
        prepared.created.iter().all(|path| path != &identity),
        "runtime identity appeared in prepared anchors: {:?}",
        prepared.created
    );
    for protected in [
        format!("{}/state/**", orbit.display()),
        format!("{}/tasks/**", orbit.display()),
        format!("{}/future-store/**", orbit.display()),
    ] {
        assert!(
            !modify.iter().any(|rule| rule == &protected),
            "worktree store must not be re-allowed by policy: {protected} in {modify:?}"
        );
    }
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

/// Sandbox preparation appends runtime write roots before the recovery
/// authority deny that rejects a symlinked `<global>/state`. The later
/// rejection is not a substitute for validating the store first: without it,
/// preparation has already created directories at the redirect target by the
/// time dispatch fails.
#[cfg(target_os = "linux")]
#[test]
fn a_failed_linux_resolution_still_writes_nothing_at_a_redirect_target() {
    use std::os::unix::fs::symlink;

    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let outside = tempfile::tempdir().expect("redirect target");
    let outside = outside.path().canonicalize().expect("canonical target");
    symlink(&outside, runtime.paths().global_dir.join("state"))
        .expect("redirect the global state store");

    seed_executor(
        &runtime,
        "claude",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );
    runtime
        .resolve_executor_sandbox("claude", None, Some(&repo_root))
        .expect_err("a symlinked global state root must not resolve a sandbox");

    assert!(
        !outside.join("logs").exists() && !outside.join("audit").exists(),
        "sandbox preparation must not create runtime stores at a redirect target"
    );
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
    recovery: std::path::PathBuf,
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
            recovery,
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

/// [ORB-14538] The full macOS resolution from a managed worktree or a
/// recovery checkout keeps the same boundary under SBPL `subpath` semantics.
#[cfg(target_os = "macos")]
#[test]
fn macos_codex_side_roots_from_a_managed_checkout_reach_only_runtime_stores() {
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
