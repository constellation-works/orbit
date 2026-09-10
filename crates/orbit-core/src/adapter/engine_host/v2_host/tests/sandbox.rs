use orbit_engine::RuntimeHost;
#[cfg(target_os = "linux")]
use orbit_exec::{
    compile_linux_bwrap_argv, linux_bwrap_write_grant_diagnostic, prepare_linux_bwrap_write_grants,
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

// [ORB-10946] The Linux half of the Copilot state-root gate. Every entry this
// returns is created by `ensure_owned_directory`, so an ungated entry would
// mkdir a `~/.copilot` on hosts that never installed the CLI.
#[cfg(target_os = "linux")]
mod copilot_state_roots {
    use std::path::{Path, PathBuf};

    use crate::adapter::engine_host::v2_host::sandbox::linux_copilot_state_roots_with;

    #[test]
    fn active_copilot_gets_its_state_and_extraction_cache_roots() {
        let roots =
            linux_copilot_state_roots_with("copilot", Some(Path::new("/home/test")), None, None);

        assert_eq!(
            roots,
            vec![
                PathBuf::from("/home/test/.copilot"),
                PathBuf::from("/home/test/.cache/copilot"),
            ]
        );
    }

    #[test]
    fn overrides_are_honored() {
        let roots = linux_copilot_state_roots_with(
            "copilot",
            Some(Path::new("/home/test")),
            Some(Path::new("/srv/copilot-home")),
            Some(Path::new("/srv/cache")),
        );

        assert_eq!(
            roots,
            vec![
                PathBuf::from("/srv/copilot-home"),
                PathBuf::from("/srv/cache/copilot"),
            ]
        );
    }

    #[test]
    fn other_providers_get_nothing() {
        for provider in [
            "claude",
            "codex",
            "gemini",
            "grok",
            "cursor",
            "ollama",
            "local-shell",
        ] {
            assert!(
                linux_copilot_state_roots_with(provider, Some(Path::new("/home/test")), None, None)
                    .is_empty(),
                "{provider} must not inherit copilot roots",
            );
        }
    }

    #[test]
    fn unknown_provider_is_not_treated_as_copilot() {
        assert!(
            linux_copilot_state_roots_with(
                "not-a-provider",
                Some(Path::new("/home/test")),
                None,
                None
            )
            .is_empty()
        );
    }
}

#[cfg(target_os = "linux")]
mod provider_state_root_validation {
    use std::path::Path;

    use crate::adapter::engine_host::v2_host::sandbox::{
        ensure_linux_provider_directory, validated_linux_provider_state_root,
    };

    #[test]
    fn rejects_root_and_home_wide_targets() {
        let home = tempfile::tempdir().expect("home");

        assert!(validated_linux_provider_state_root(Path::new("/"), Some(home.path())).is_err());
        assert!(validated_linux_provider_state_root(home.path(), Some(home.path())).is_err());
        assert!(
            validated_linux_provider_state_root(
                home.path().parent().expect("home parent"),
                Some(home.path()),
            )
            .is_err()
        );
    }

    #[test]
    fn accepts_absolute_custom_root_beneath_an_existing_parent() {
        let parent = tempfile::tempdir().expect("parent");
        let custom = parent.path().join("provider").join("state");

        let validated = validated_linux_provider_state_root(&custom, Some(parent.path()))
            .expect("validate custom provider root");

        assert_eq!(validated, custom);
    }

    #[test]
    fn creates_a_validated_custom_root() {
        let parent = tempfile::tempdir().expect("parent");
        let custom = parent.path().join("provider").join("state");

        let created = ensure_linux_provider_directory(&custom, Some(parent.path()))
            .expect("create custom provider root");

        assert!(custom.is_dir());
        assert_eq!(
            created,
            custom.canonicalize().expect("canonical custom root")
        );
    }

    #[test]
    fn rejects_relative_and_traversal_paths() {
        assert!(validated_linux_provider_state_root(Path::new("provider/state"), None).is_err());
        assert!(validated_linux_provider_state_root(Path::new("/tmp/../provider"), None).is_err());
    }
}

#[cfg(target_os = "linux")]
mod runtime_root_validation {
    use std::os::unix::fs::symlink;
    use std::path::Path;

    use crate::adapter::engine_host::v2_host::sandbox::validated_linux_runtime_root;

    #[test]
    fn accepts_an_existing_runtime_directory_and_returns_its_canonical_path() {
        let root = tempfile::tempdir().expect("runtime root");

        let validated = validated_linux_runtime_root(root.path()).expect("validate runtime root");

        assert_eq!(
            validated,
            root.path().canonicalize().expect("canonical runtime root")
        );
    }

    #[test]
    fn rejects_missing_relative_and_symlinked_runtime_roots() {
        let parent = tempfile::tempdir().expect("runtime parent");
        let missing = parent.path().join("missing");
        let link = parent.path().join("link");
        symlink(parent.path(), &link).expect("create runtime-root symlink");

        assert!(validated_linux_runtime_root(&missing).is_err());
        assert!(validated_linux_runtime_root(Path::new("runtime-root")).is_err());
        assert!(validated_linux_runtime_root(&link).is_err());
    }
}

#[cfg(target_os = "linux")]
mod cursor_state_roots {
    use std::path::{Path, PathBuf};

    use crate::adapter::engine_host::v2_host::sandbox::linux_cursor_state_roots_with;

    #[test]
    fn active_cursor_gets_only_its_home_state_root() {
        assert_eq!(
            linux_cursor_state_roots_with("cursor", Some(Path::new("/home/test"))),
            vec![PathBuf::from("/home/test/.cursor")]
        );
        assert!(linux_cursor_state_roots_with("cursor", None).is_empty());
    }

    #[test]
    fn other_and_unknown_providers_get_nothing() {
        for provider in [
            "claude",
            "codex",
            "gemini",
            "grok",
            "copilot",
            "ollama",
            "not-a-provider",
        ] {
            assert!(
                linux_cursor_state_roots_with(provider, Some(Path::new("/home/test"))).is_empty(),
                "{provider} must not inherit Cursor state roots",
            );
        }
    }
}

#[cfg(target_os = "linux")]
mod opencode_state_roots {
    use std::path::{Path, PathBuf};

    use crate::adapter::engine_host::v2_host::sandbox::{
        OpencodeStateEnv, linux_opencode_state_roots_with,
    };

    #[test]
    fn active_opencode_gets_its_four_xdg_roots_from_home_defaults() {
        assert_eq!(
            linux_opencode_state_roots_with(
                "opencode",
                Some(Path::new("/home/test")),
                OpencodeStateEnv::default(),
            ),
            vec![
                PathBuf::from("/home/test/.local/share/opencode"),
                PathBuf::from("/home/test/.config/opencode"),
                PathBuf::from("/home/test/.local/state/opencode"),
                PathBuf::from("/home/test/.cache/opencode"),
            ]
        );
        assert!(
            linux_opencode_state_roots_with("opencode", None, OpencodeStateEnv::default())
                .is_empty()
        );
    }

    #[test]
    fn xdg_variables_and_the_config_override_replace_the_home_defaults() {
        assert_eq!(
            linux_opencode_state_roots_with(
                "opencode",
                Some(Path::new("/home/test")),
                OpencodeStateEnv {
                    xdg_data_home: Some(PathBuf::from("/srv/data")),
                    xdg_config_home: Some(PathBuf::from("/srv/config")),
                    xdg_state_home: Some(PathBuf::from("/srv/state")),
                    xdg_cache_home: Some(PathBuf::from("/srv/cache")),
                    opencode_config_dir: None,
                },
            ),
            vec![
                PathBuf::from("/srv/data/opencode"),
                PathBuf::from("/srv/config/opencode"),
                PathBuf::from("/srv/state/opencode"),
                PathBuf::from("/srv/cache/opencode"),
            ]
        );

        // `OPENCODE_CONFIG_DIR` is the config root itself, not an XDG base, so
        // it is used verbatim and outranks `XDG_CONFIG_HOME`.
        let roots = linux_opencode_state_roots_with(
            "opencode",
            Some(Path::new("/home/test")),
            OpencodeStateEnv {
                xdg_config_home: Some(PathBuf::from("/srv/config")),
                opencode_config_dir: Some(PathBuf::from("/srv/opencode-config")),
                ..OpencodeStateEnv::default()
            },
        );
        assert_eq!(roots[1], PathBuf::from("/srv/opencode-config"));
    }

    #[test]
    fn other_and_unknown_providers_get_nothing() {
        for provider in [
            "claude",
            "codex",
            "gemini",
            "grok",
            "copilot",
            "cursor",
            "pi",
            "ollama",
            "not-a-provider",
        ] {
            assert!(
                linux_opencode_state_roots_with(
                    provider,
                    Some(Path::new("/home/test")),
                    OpencodeStateEnv::default(),
                )
                .is_empty(),
                "{provider} must not inherit OpenCode state roots",
            );
        }
    }
}

#[cfg(target_os = "linux")]
mod pi_state_roots {
    use std::path::{Path, PathBuf};

    use crate::adapter::engine_host::v2_host::sandbox::linux_pi_state_roots_with;

    #[test]
    fn active_pi_gets_only_its_agent_state_root() {
        assert_eq!(
            linux_pi_state_roots_with("pi", Some(Path::new("/home/test")), None),
            vec![PathBuf::from("/home/test/.pi")]
        );
        assert!(linux_pi_state_roots_with("pi", None, None).is_empty());
    }

    #[test]
    fn the_agent_dir_override_replaces_the_home_default() {
        assert_eq!(
            linux_pi_state_roots_with(
                "pi",
                Some(Path::new("/home/test")),
                Some(Path::new("/srv/pi-agent")),
            ),
            vec![PathBuf::from("/srv/pi-agent")]
        );
    }

    #[test]
    fn other_and_unknown_providers_get_nothing() {
        for provider in [
            "claude",
            "codex",
            "gemini",
            "grok",
            "copilot",
            "cursor",
            "ollama",
            "not-a-provider",
        ] {
            assert!(
                linux_pi_state_roots_with(provider, Some(Path::new("/home/test")), None).is_empty(),
                "{provider} must not inherit Pi state roots",
            );
        }
    }
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
