use orbit_engine::RuntimeHost;
#[cfg(target_os = "linux")]
use orbit_exec::{
    compile_linux_bwrap_argv, linux_bwrap_write_grant_diagnostic, prepare_linux_bwrap_write_grants,
};

use crate::adapter::engine_host::v2_host::sandbox::resolve::{
    append_orbit_child_runtime_write_roots, deny_registered_auto_task_definition_writes,
    resolve_fs_profile_absolute,
};
use crate::adapter::engine_host::v2_host::test_support::seeded_runtime_with_executor;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_layout, seed_executor,
};

#[test]
fn resolve_executor_sandbox_returns_none_when_executor_has_no_sandbox() {
    let runtime = seeded_runtime_with_executor(None);
    let resolved = runtime
        .resolve_executor_sandbox("codex", None, None)
        .expect("resolve");
    assert!(resolved.is_none());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn reviewer_read_rules_follow_inspection_cwd_without_primary_write_grants() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let inspection = tempfile::tempdir().unwrap();
    let inspection_root = inspection.path().canonicalize().unwrap();
    #[cfg(target_os = "linux")]
    let kind = orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap;
    #[cfg(target_os = "macos")]
    let kind = orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec;
    seed_executor(&runtime, "claude", Some(kind));
    let sandbox = runtime
        .resolve_executor_sandbox("claude", Some("reviewer"), Some(&inspection_root))
        .unwrap()
        .unwrap();
    assert!(
        sandbox
            .fs_profile
            .read
            .iter()
            .any(|rule| rule.starts_with(&inspection_root.display().to_string()))
    );
    assert!(
        !sandbox
            .fs_profile
            .read
            .iter()
            .any(|rule| rule.starts_with(&repo_root.display().to_string()))
    );
    assert!(!sandbox.fs_profile.modify.iter().any(|rule| {
        rule.starts_with(&inspection_root.display().to_string())
            || rule.starts_with(&repo_root.display().to_string())
    }));
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

#[cfg(target_os = "linux")]
#[test]
fn reviewer_runtime_database_grants_hold_one_wal_file_set_lease() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let inspection = tempfile::tempdir().expect("inspection checkout");
    seed_executor(
        &runtime,
        "codex",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );

    let sandbox = runtime
        .resolve_executor_sandbox("codex", Some("reviewer"), Some(inspection.path()))
        .expect("resolve reviewer sandbox")
        .expect("Linux sandbox");
    let database_grants = sandbox
        .runtime_write_authority
        .iter()
        .filter(|grant| {
            grant.path.file_name().is_some_and(|name| {
                matches!(
                    name.to_str(),
                    Some("orbit.db" | "orbit.db-wal" | "orbit.db-shm")
                )
            })
        })
        .collect::<Vec<_>>();

    assert_eq!(database_grants.len(), 3);
    assert!(
        database_grants
            .iter()
            .all(|grant| grant.wal_file_set_lease.is_some()),
        "the DB, WAL, and SHM descriptors must share a live SQLite lease"
    );
    let first = database_grants[0]
        .wal_file_set_lease
        .as_ref()
        .expect("database lease");
    assert!(database_grants.iter().all(|grant| {
        std::sync::Arc::ptr_eq(
            first,
            grant.wal_file_set_lease.as_ref().expect("sidecar lease"),
        )
    }));
    assert!(
        sandbox
            .fs_profile
            .modify
            .iter()
            .all(|grant| !grant.starts_with(inspection.path().to_string_lossy().as_ref())),
        "the runtime lease must not grant source-inspection writes"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn resolve_executor_sandbox_returns_linux_descriptor_with_absolute_mounts() {
    let runtime =
        seeded_runtime_with_executor(Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap));
    let resolved = runtime
        .resolve_executor_sandbox("codex", None, None)
        .expect("resolve")
        .expect("descriptor");
    assert_eq!(
        resolved.kind,
        orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap
    );
    assert!(!resolved.managed_worktree);
    for entry in &resolved.fs_profile.modify {
        let body = entry.strip_prefix('!').unwrap_or(entry);
        assert!(
            body.starts_with('/'),
            "linux-bwrap mount rule must be absolute: {entry}"
        );
    }
}

/// [ORB-11066] Linux grants already name the same narrow global runtime stores
/// as the corrected managed registry contract. In particular, the global root
/// is not writable as a whole and workspace-only bootstrap directories are not
/// added while chasing the macOS denial.
#[cfg(target_os = "linux")]
#[test]
fn linux_child_runtime_grants_do_not_include_global_workspace_layout() {
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
    assert!(
        !resolved
            .fs_profile
            .modify
            .iter()
            .any(|entry| entry == &global.display().to_string()),
        "Linux must not grant the whole global registry: {:?}",
        resolved.fs_profile.modify
    );
    for workspace_only in [
        "state/job-runs",
        "state/diagnostics",
        "state/scoreboard",
        "state/worktrees",
        "knowledge",
    ] {
        let denied = global.join(workspace_only).display().to_string();
        assert!(
            !resolved
                .fs_profile
                .modify
                .iter()
                .any(|entry| entry == &denied),
            "Linux must not grant global workspace-only path {denied}: {:?}",
            resolved.fs_profile.modify
        );
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

/// [ORB-11259] Implementer sandboxes receive a language-neutral host cache
/// write root; reviewer profiles do not. The global registry root itself
/// stays denied.
#[cfg(target_os = "linux")]
#[test]
fn linux_implementer_gains_host_cache_root_reviewer_does_not() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    seed_executor(
        &runtime,
        "claude",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );

    let global = runtime
        .paths()
        .global_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().global_dir.clone());
    let cache = global.join("cache").display().to_string();

    let writer = runtime
        .resolve_executor_sandbox("claude", None, Some(&repo_root))
        .expect("resolve implementer sandbox")
        .expect("descriptor");
    assert!(
        writer.fs_profile.modify.iter().any(|entry| entry == &cache),
        "implementer must grant host cache {cache}: {:?}",
        writer.fs_profile.modify
    );
    assert!(
        !writer
            .fs_profile
            .modify
            .iter()
            .any(|entry| entry == &global.display().to_string()),
        "host cache grant must not widen to the whole global registry: {:?}",
        writer.fs_profile.modify
    );

    let reviewer = runtime
        .resolve_executor_sandbox("claude", Some("reviewer"), Some(&repo_root))
        .expect("resolve reviewer sandbox")
        .expect("descriptor");
    assert!(
        !reviewer
            .fs_profile
            .modify
            .iter()
            .any(|entry| entry == &cache),
        "reviewer must not gain the host cache write root: {:?}",
        reviewer.fs_profile.modify
    );
}

#[cfg(target_os = "linux")]
#[test]
fn direct_reviewer_profile_does_not_gain_workspace_runtime_writes() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    seed_executor(
        &runtime,
        "claude",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );

    let reviewer = runtime
        .resolve_executor_sandbox("claude", Some("reviewer"), Some(&repo_root))
        .expect("resolve reviewer sandbox")
        .expect("descriptor");
    assert!(!reviewer.managed_worktree);
    let canonical_repo = repo_root.canonicalize().expect("canonical repo");
    assert!(
        reviewer
            .fs_profile
            .modify
            .iter()
            .filter(|rule| !rule.starts_with('!'))
            .all(|rule| !rule.starts_with(&canonical_repo.display().to_string())),
        "read-only activity profile must not gain workspace runtime writes: {:?}",
        reviewer.fs_profile.modify
    );
    assert!(
        reviewer
            .fs_profile
            .read
            .iter()
            .any(|rule| rule.starts_with('!') && rule.ends_with("/**/*.env")),
        "global denyRead rules must remain in the resolved profile: {:?}",
        reviewer.fs_profile.read
    );
    compile_linux_bwrap_argv(
        &reviewer.fs_profile,
        "/bin/true",
        &[],
        Some(&canonical_repo),
        reviewer.managed_worktree,
    )
    .expect("direct reviewer sandbox must compile with default dotenv denies");

    let writer = runtime
        .resolve_executor_sandbox("claude", None, Some(&repo_root))
        .expect("resolve write-capable sandbox")
        .expect("descriptor");
    let error = compile_linux_bwrap_argv(
        &writer.fs_profile,
        "/bin/true",
        &[],
        Some(&canonical_repo),
        writer.managed_worktree,
    )
    .expect_err("a direct write-capable sandbox must still fail closed");
    assert!(error.to_string().contains("non-subtree denyModify"));
}

#[cfg(target_os = "linux")]
#[test]
fn resolve_executor_sandbox_marks_only_specific_orbit_worktree_managed() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    seed_executor(
        &runtime,
        "claude",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );
    let worktrees = runtime.paths().orbit_dir.join("state/worktrees");
    let worktree = worktrees.join("orbit-jrun-test");
    std::fs::create_dir_all(&worktree).expect("create worktree");

    let managed = runtime
        .resolve_executor_sandbox("claude", None, Some(&worktree))
        .expect("resolve managed")
        .expect("descriptor");
    assert!(managed.managed_worktree);
    let canonical_worktree = worktree.canonicalize().expect("canonical worktree");
    assert!(
        managed
            .fs_profile
            .modify
            .iter()
            .any(|entry| entry == &format!("{}/**", canonical_worktree.display()))
    );
    let prepared = prepare_linux_bwrap_write_grants(&managed.fs_profile, &worktree)
        .expect("prepare resolved managed-worktree grants");
    assert!(
        prepared.unsatisfied.is_empty(),
        "resolved managed-worktree grants must all be mountable: {:?}",
        prepared.unsatisfied
    );
    let plan = compile_linux_bwrap_argv(
        &managed.fs_profile,
        "/bin/true",
        &[],
        Some(&worktree),
        managed.managed_worktree,
    )
    .expect("compile resolved managed-worktree sandbox");
    assert!(
        plan.dropped_grants.is_empty(),
        "prepared managed-worktree grants must not be dropped: {:?}",
        plan.dropped_grants
    );

    let direct = runtime
        .resolve_executor_sandbox("claude", None, Some(&repo_root))
        .expect("resolve direct")
        .expect("descriptor");
    assert!(!direct.managed_worktree);
}

/// [ORB-10879] The doc-duties shape: `agent_implement` declares no `fsProfile`,
/// so it resolves to the unrestricted profile, and its cwd is a managed
/// worktree. ORB-10878 parked itself in `blocked` claiming this configuration
/// could not write `docs/**`. It can.
///
/// The assertion runs entirely against the resolved profile and the compiled
/// argv, so it holds on any host — a test that probed a real write would be
/// reporting the runner's mount table rather than Orbit's policy.
#[cfg(target_os = "linux")]
#[test]
fn managed_worktree_without_an_fs_profile_can_write_under_docs() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    seed_executor(
        &runtime,
        "claude",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );
    let worktree = runtime
        .paths()
        .orbit_dir
        .join("state/worktrees/orbit-jrun-doc-duties");
    std::fs::create_dir_all(worktree.join("docs/design/task-artifacts"))
        .expect("create worktree docs fixture");

    // `None` is exactly what agent_implement passes: no fsProfile declared.
    let resolved = runtime
        .resolve_executor_sandbox("claude", None, Some(&worktree))
        .expect("resolve doc-duties sandbox")
        .expect("descriptor");
    assert!(
        resolved.managed_worktree,
        "a jrun worktree cwd must be recognized as managed"
    );

    let canonical_worktree = worktree.canonicalize().expect("canonical worktree");
    for relative in [
        "docs/design/task-artifacts/3_vision.md",
        "docs/design/task-migration/1_overview.md",
        "docs/runbooks/release.md",
    ] {
        let target = canonical_worktree.join(relative);
        let denial = linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, &target)
            .expect("grant diagnostic");
        assert!(
            denial.is_none(),
            "doc-duties must be able to stamp {relative} in its own worktree: {denial:?}"
        );
    }

    // The worktree's own Orbit store stays denied — this test must not pass by
    // having widened the grant set.
    let store = canonical_worktree.join(".orbit/state/orbit.db");
    assert!(
        linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, &store)
            .expect("grant diagnostic")
            .is_some(),
        "the worktree Orbit store must remain unwritable: {:?}",
        resolved.fs_profile.modify
    );

    // Every grant this profile declares is mountable for that worktree, so the
    // pre-spawn check has nothing to reject and no EROFS can surface mid-turn.
    let prepared = prepare_linux_bwrap_write_grants(&resolved.fs_profile, &worktree)
        .expect("prepare doc-duties grants");
    assert!(
        prepared.unsatisfied.is_empty(),
        "doc-duties grants must all be mountable: {:?}",
        prepared.unsatisfied
    );
    let plan = compile_linux_bwrap_argv(
        &resolved.fs_profile,
        "/bin/true",
        &[],
        Some(&worktree),
        resolved.managed_worktree,
    )
    .expect("compile doc-duties sandbox");
    assert!(
        plan.dropped_grants.is_empty(),
        "doc-duties grants must not be dropped: {:?}",
        plan.dropped_grants
    );
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

#[cfg(target_os = "macos")]
#[test]
fn resolve_executor_sandbox_returns_descriptor_with_absolutized_modify_paths() {
    let runtime = seeded_runtime_with_executor(Some(
        orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec,
    ));
    let resolved = runtime
        .resolve_executor_sandbox("codex", None, None)
        .expect("resolve")
        .expect("descriptor");
    assert_eq!(
        resolved.kind,
        orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec
    );
    let workspace_root = runtime
        .paths()
        .repo_root
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().repo_root.clone());
    let workspace_str = workspace_root.display().to_string();
    for entry in &resolved.fs_profile.modify {
        let body = entry.strip_prefix('!').unwrap_or(entry);
        assert!(
            body.starts_with('/') || body == workspace_str,
            "modify entry must be absolutized: {entry}"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn resolve_executor_sandbox_appends_codex_side_write_roots_after_policy_denies() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    seed_executor(
        &runtime,
        "codex",
        Some(orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec),
    );

    let resolved = runtime
        .resolve_executor_sandbox("codex", None, None)
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
    let workspace_orbit_deny = format!("!{workspace_orbit}/**");
    let deny_pos = modify
        .iter()
        .position(|entry| entry == &workspace_orbit_deny)
        .unwrap_or_else(|| {
            panic!(
                "default policy should deny workspace .orbit writes via {workspace_orbit_deny}; modify={modify:?}"
            )
        });
    let allow_pos = modify
        .iter()
        .rposition(|entry| entry == &workspace_orbit)
        .expect("codex side write root should re-allow workspace .orbit");

    assert!(
        deny_pos < allow_pos,
        "codex side write root must be appended after policy deny: {modify:?}"
    );
    let global_orbit = runtime
        .paths()
        .global_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().global_dir.clone())
        .display()
        .to_string();
    assert!(
        modify.iter().any(|entry| entry == &global_orbit),
        "codex side write roots should include global .orbit: {modify:?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn resolve_executor_sandbox_appends_gemini_orbit_runtime_roots_without_home_reallow() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    seed_executor(
        &runtime,
        "gemini",
        Some(orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec),
    );

    let resolved = runtime
        .resolve_executor_sandbox("gemini", None, None)
        .expect("resolve")
        .expect("descriptor");
    let modify = &resolved.fs_profile.modify;
    let global = runtime
        .paths()
        .global_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().global_dir.clone())
        .display()
        .to_string();
    let workspace_orbit = runtime
        .paths()
        .orbit_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().orbit_dir.clone())
        .display()
        .to_string();
    let workspace_orbit_deny = format!("!{workspace_orbit}/**");
    let deny_pos = modify
        .iter()
        .position(|entry| entry == &workspace_orbit_deny)
        .unwrap_or_else(|| {
            panic!(
                "default policy should deny workspace .orbit writes via {workspace_orbit_deny}; modify={modify:?}"
            )
        });
    let expected = [
        format!("{global}/state/logs/**"),
        format!("{global}/state/audit/**"),
        format!("{global}/orbit.db*"),
        format!("{global}/tasks/**"),
        format!("{global}/cache/**"),
        format!("{workspace_orbit}/tasks/**"),
        format!("{workspace_orbit}/frictions/**"),
        format!("{workspace_orbit}/state/audit/**"),
        format!("{workspace_orbit}/state/logs/**"),
        format!("{workspace_orbit}/state/semantic.db*"),
    ];
    for root in expected {
        let allow_pos = modify.iter().position(|entry| entry == &root);
        assert!(
            allow_pos.is_some(),
            "gemini sandbox should allow Orbit runtime root {root}; modify={modify:?}"
        );
        assert!(
            deny_pos < allow_pos.expect("root position checked above"),
            "Orbit runtime root {root} must be re-allowed after workspace .orbit deny: {modify:?}"
        );
    }
    assert!(
        !modify.iter().any(|entry| entry == &global),
        "gemini sandbox must not re-allow the whole global Orbit root: {modify:?}"
    );
    assert!(
        !modify.iter().any(|entry| entry == &workspace_orbit),
        "gemini sandbox must not re-allow the whole workspace .orbit root: {modify:?}"
    );
    assert!(
        !modify
            .iter()
            .any(|entry| entry.starts_with(&format!("{workspace_orbit}/auto_tasks"))),
        "a macOS child must not receive a direct scheduler-definition write root: {modify:?}"
    );
    // Registered-but-not-activity-exposed stores remain outside this child-runtime inventory.
    for excluded in [
        format!("{workspace_orbit}/knowledge/**"),
        format!("{workspace_orbit}/state/knowledge/**"),
    ] {
        assert!(
            !modify.iter().any(|entry| entry == &excluded),
            "gemini sandbox must not allow non-activity-exposed Orbit store {excluded}: {modify:?}"
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn macos_child_profile_denies_registered_auto_task_definitions() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut resolved = resolve_fs_profile_absolute(&runtime, None, None).expect("resolve profile");
    append_orbit_child_runtime_write_roots(&runtime, true, &mut resolved);
    deny_registered_auto_task_definition_writes(&runtime, &mut resolved);

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

#[cfg(target_os = "macos")]
#[test]
fn resolve_executor_sandbox_appends_workspace_semantic_store_after_policy_deny() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    seed_executor(
        &runtime,
        "gemini",
        Some(orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec),
    );

    let resolved = runtime
        .resolve_executor_sandbox("gemini", None, None)
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
    let workspace_orbit_deny = format!("!{workspace_orbit}/**");
    let deny_pos = modify
        .iter()
        .position(|entry| entry == &workspace_orbit_deny)
        .unwrap_or_else(|| {
            panic!(
                "default policy should deny workspace .orbit writes via {workspace_orbit_deny}; modify={modify:?}"
            )
        });
    let semantic_store = format!("{workspace_orbit}/state/semantic.db*");
    let allow_pos = modify
        .iter()
        .position(|entry| entry == &semantic_store)
        .unwrap_or_else(|| panic!("semantic store should be re-allowed under sandbox: {modify:?}"));
    assert!(
        deny_pos < allow_pos,
        "semantic store re-allow must come after policy deny: {modify:?}"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn resolve_executor_sandbox_errors_on_non_macos_platform() {
    let runtime = seeded_runtime_with_executor(Some(
        orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec,
    ));
    let err = runtime
        .resolve_executor_sandbox("codex", None, None)
        .expect_err("expected platform-mismatch error");
    let message = format!("{err}");
    assert!(
        message.contains("macos-sandbox-exec"),
        "error must name the sandbox kind: {message}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_resolution_appends_git_protection_after_provider_grants() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    std::fs::create_dir_all(repo_root.join(".git")).unwrap();
    seed_executor(
        &runtime,
        "codex",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );
    let sandbox = runtime
        .resolve_executor_sandbox("codex", None, Some(&repo_root))
        .unwrap()
        .unwrap();
    let git_dir = repo_root.join(".git").canonicalize().unwrap();
    assert_eq!(
        sandbox.fs_profile.modify.last(),
        Some(&format!("!{}/**", git_dir.display()))
    );
    assert!(
        linux_bwrap_write_grant_diagnostic(&sandbox.fs_profile, &git_dir.join("HEAD"))
            .unwrap()
            .is_some()
    );
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

/// A redirected runtime store must stay out of the profile that reaches
/// Bubblewrap, not just out of the helper that builds it.
#[cfg(target_os = "linux")]
#[test]
fn resolved_linux_sandbox_drops_a_redirected_global_runtime_store() {
    use std::os::unix::fs::symlink;

    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let outside = tempfile::tempdir().expect("redirect target");
    let outside = outside.path().canonicalize().expect("canonical target");
    let redirected = runtime.paths().global_dir.join("cache");
    symlink(&outside, &redirected).expect("redirect the host cache store");

    seed_executor(
        &runtime,
        "claude",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );
    let sandbox = runtime
        .resolve_executor_sandbox("claude", None, Some(&repo_root))
        .expect("resolve sandbox")
        .expect("descriptor");

    // Bubblewrap resolves the paths it binds, so naming the link is the same
    // grant as naming its target. Neither may appear.
    let denied = [
        outside.display().to_string(),
        redirected.display().to_string(),
    ];
    assert!(
        sandbox.fs_profile.modify.iter().all(|rule| denied
            .iter()
            .all(|prefix| !rule.trim_start_matches('!').starts_with(prefix))),
        "a redirected runtime store must not widen the sandbox: {:?}",
        sandbox.fs_profile.modify
    );
}

/// [ORB-13840] SBPL resolves runtime rules physically, so a redirected store
/// must be dropped before compilation. Safe aliases and missing stores keep
/// their grants. Linux exercises the same macOS grant builder; the macOS CI
/// leg drives the executor resolver that production callers use.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn resolved_macos_sandbox_runtime_stores_stay_inside_their_root() {
    use std::os::unix::fs::symlink;

    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &resolved_macos_sandbox_runtime_stores_stay_inside_their_root,
    )) {
        return;
    }

    for topology in ["outside", "dangling-outside", "in-root", "missing"] {
        let (root, runtime, repo_root) = runtime_with_workspace_layout();
        let global = runtime.paths().global_dir.canonicalize().unwrap();
        let cache = global.join("cache");
        let target = match topology {
            "outside" | "dangling-outside" => root.path().canonicalize().unwrap().join("outside"),
            "in-root" => global.join("real-cache"),
            _ => cache.clone(),
        };
        if topology != "missing" {
            if topology != "dangling-outside" {
                std::fs::create_dir_all(&target).expect("cache target");
            }
            symlink(&target, &cache).expect("redirect cache store");
        }

        #[cfg(target_os = "macos")]
        let profile = {
            seed_executor(
                &runtime,
                "claude",
                Some(orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec),
            );
            runtime
                .resolve_executor_sandbox("claude", None, Some(&repo_root))
                .expect("resolve macOS implementer sandbox")
                .expect("sandbox descriptor")
                .fs_profile
        };
        #[cfg(target_os = "linux")]
        let profile = {
            let mut profile = resolve_fs_profile_absolute(&runtime, None, Some(&repo_root))
                .expect("resolve implementer profile");
            append_orbit_child_runtime_write_roots(&runtime, true, &mut profile);
            profile
        };

        let writes = profile
            .modify
            .iter()
            .filter(|rule| !rule.starts_with('!'))
            .collect::<Vec<_>>();
        if matches!(topology, "outside" | "dangling-outside") {
            for forbidden in [&cache, &target] {
                assert!(
                    writes
                        .iter()
                        .all(|rule| !rule.starts_with(forbidden.to_string_lossy().as_ref())),
                    "ORB-13840: a redirected runtime store must not grant its link or target: {writes:?}"
                );
            }
        } else {
            assert!(
                writes.contains(&&format!("{}/**", target.display())),
                "safe and missing runtime stores must retain their physical grant ({topology}): {writes:?}"
            );
        }
        assert!(
            writes.contains(&&format!("{}/tasks/**", global.display())),
            "rejecting a redirected cache must preserve other runtime-store grants: {writes:?}"
        );
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

/// Every OS-sandboxed agent launch carries the plugin mask, with both trees
/// and the sentinel already on disk: Bubblewrap can only mount over a path
/// that exists, and a tree created later by the agent would escape the mask.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn sandboxed_agent_launch_carries_the_prepared_plugin_mask() {
    use std::os::unix::fs::PermissionsExt;

    #[cfg(target_os = "linux")]
    let kind = orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap;
    #[cfg(target_os = "macos")]
    let kind = orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec;
    let runtime = seeded_runtime_with_executor(Some(kind));
    let resolved = runtime
        .resolve_executor_sandbox("codex", None, None)
        .expect("resolve")
        .expect("descriptor");

    let mask = resolved.mask.expect("sandboxed agents are masked");
    let global = runtime.global_root().canonicalize().expect("global root");
    assert_eq!(
        mask.targets,
        vec![
            global.join("state/plugins"),
            global.join("state/plugin-secrets")
        ]
    );
    for tree in &mask.targets {
        let mode = std::fs::metadata(tree)
            .expect("tree exists")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "{} is host-private", tree.display());
    }
    assert_eq!(mask.sentinel, global.join("state/plugin-broker/masked"));
    assert!(mask.sentinel.join(".orbit-brokered").is_file());
}

/// Sandbox off keeps today's behavior: nothing hidden, nothing created.
#[test]
fn sandbox_off_carries_no_plugin_mask() {
    let runtime =
        seeded_runtime_with_executor(Some(orbit_types::workflow::ExecutorSandboxKind::Off));
    let resolved = runtime
        .resolve_executor_sandbox("codex", None, None)
        .expect("resolve")
        .expect("descriptor");

    assert!(resolved.mask.is_none());
    assert!(
        !runtime
            .global_root()
            .join("state/plugin-broker/masked")
            .exists()
    );
}
