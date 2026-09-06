#![allow(missing_docs)]
// Integration tests use unwrap/expect for fixture setup and print skip details.
#![allow(clippy::expect_used, clippy::print_stdout, clippy::unwrap_used)]
#![cfg(target_os = "linux")]

use std::process::Stdio;

use orbit_exec::{
    LINUX_STABLE_BUILD_MOUNT, LINUX_STABLE_WORKSPACE_MOUNT, LinuxBwrapPostRunGuard,
    LinuxBwrapSpawnRequest, WriteAnchorKind, bwrap_path, bwrap_program_for_audit,
    compile_linux_bwrap_argv, linux_bwrap_write_grant_diagnostic, linux_bwrap_write_grants,
    prepare_linux_bwrap_write_grants, probe_bwrap, spawn_under_linux_bwrap,
};
use orbit_types::policy::ResolvedFsProfile;

fn profile(modify: Vec<String>) -> ResolvedFsProfile {
    ResolvedFsProfile {
        name: "test".to_string(),
        read: vec!["/**".to_string()],
        modify,
    }
}

/// [ORB-10917] Bubblewrap forwards its own environment into the confined
/// program, so the launcher must hand it exactly the environment the
/// dispatcher composed. The ambient variables are set here rather than read
/// from the developer's shell, and none carries a credential-shaped name — a
/// denylist would forward every one of them.
#[test]
fn bwrap_child_gets_only_the_supplied_environment() {
    let probe = probe_bwrap();
    if !probe.available {
        println!("skipping real Bubblewrap test: {}", probe.detail);
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().canonicalize().expect("canonical workspace");
    let resolved = profile(vec![format!("{}/**", workspace.display())]);
    let plan = compile_linux_bwrap_argv(&resolved, "/usr/bin/env", &[], Some(&workspace), false)
        .expect("compile");

    let _ambient = orbit_common::test_env::scoped([
        ("DATABASE_URL", Some("postgres://svc:hunter2@db.internal")),
        ("BILLING_ENDPOINT", Some("https://billing.internal.example")),
        ("ORB_10917_AMBIENT", Some("leaked")),
        ("ANTHROPIC_API_KEY", Some("sk-ant-000000000000000000000")),
    ]);
    let env = [
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ("ORB_10917_SUPPLIED".to_string(), "present".to_string()),
    ];
    let child = spawn_under_linux_bwrap(LinuxBwrapSpawnRequest {
        plan: &plan,
        env: &env,
        cwd: Some(&workspace),
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
    })
    .expect("spawn");
    let output = child.wait_with_output().expect("wait");
    let child_env = String::from_utf8_lossy(&output.stdout);

    assert!(
        child_env.contains("ORB_10917_SUPPLIED=present"),
        "supplied vars must reach the confined child: {child_env}"
    );
    for leaked in [
        "DATABASE_URL",
        "BILLING_ENDPOINT",
        "ORB_10917_AMBIENT",
        "ANTHROPIC_API_KEY",
    ] {
        assert!(
            !child_env.contains(leaked),
            "{leaked} must not reach a Bubblewrap-confined provider child: {child_env}"
        );
    }
}

#[test]
fn trusted_resolution_never_consults_path() {
    assert_eq!(bwrap_program_for_audit(), "/usr/bin/bwrap");
    if let Some(path) = bwrap_path() {
        assert_eq!(path.to_string_lossy(), "/usr/bin/bwrap");
    }
}

/// A worktree-shaped profile: broad writable root, the blanket `.orbit` deny,
/// then whatever narrow re-allows the caller wants to exercise.
fn worktree_profile(worktree: &std::path::Path, reallows: Vec<String>) -> ResolvedFsProfile {
    let mut modify = vec![
        format!("{}/**", worktree.display()),
        format!("!{}/.orbit/**", worktree.display()),
    ];
    modify.extend(reallows);
    profile(modify)
}

#[test]
fn absent_anchor_kind_comes_from_rule_semantics_not_filename_shape() {
    let temp = tempfile::tempdir().expect("tempdir");
    let worktree = temp.path().join("worktree");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let orbit = worktree.join(".orbit");
    let resolved = worktree_profile(
        &worktree,
        vec![
            // Exact rules denote files, including extensionless names.
            orbit.join("config").display().to_string(),
            // Subtree rules denote directories, including dotted names.
            format!("{}/**", orbit.join("cache.v1").display()),
            // A file grant *beneath* a granted directory: the case the old
            // hardcoded `(path, kind)` table missed, because it matched the
            // directory entry by exact tuple and never created the parent.
            format!("{}/**", orbit.join("routines").display()),
        ],
    );

    let prepared =
        prepare_linux_bwrap_write_grants(&resolved, &worktree).expect("prepare policy grants");

    assert!(
        orbit.join("config").is_file(),
        "an absent extensionless exact grant must be materialized as a file"
    );
    assert!(
        orbit.join("cache.v1").is_dir(),
        "an absent dotted subtree grant must be materialized as a directory"
    );
    assert!(orbit.join("routines").is_dir());
    assert_eq!(prepared.created.len(), 3, "{:?}", prepared.created);
    assert!(
        prepared.unsatisfied.is_empty(),
        "{:?}",
        prepared.unsatisfied
    );

    // Every prepared anchor now mounts; nothing is dropped.
    let plan = compile_linux_bwrap_argv(&resolved, "/bin/true", &[], Some(&worktree), true)
        .expect("compile");
    assert!(plan.dropped_grants.is_empty(), "{:?}", plan.dropped_grants);
    let joined = plan.args.join(" ");
    for anchor in ["config", "cache.v1", "routines"] {
        assert!(
            joined.contains(&format!("--bind {0} {0}", orbit.join(anchor).display())),
            "missing re-allow mount for {anchor} in {joined}"
        );
    }
}

#[test]
fn ungranted_paths_are_never_materialized_and_report_the_deny_that_shadows_them() {
    let temp = tempfile::tempdir().expect("tempdir");
    let worktree = temp.path().join("worktree");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let orbit = worktree.join(".orbit");
    let resolved = worktree_profile(
        &worktree,
        vec![format!("{}/**", orbit.join("auto_tasks").display())],
    );

    prepare_linux_bwrap_write_grants(&resolved, &worktree).expect("prepare policy grants");

    // Granted sibling exists; the ungranted store does not, and preparation
    // must not invent it just because it sits under the same denied parent.
    assert!(orbit.join("auto_tasks").is_dir());
    assert!(
        !orbit.join("tasks").exists(),
        "a path the policy does not grant must never be materialized"
    );

    assert!(
        linux_bwrap_write_grant_diagnostic(&resolved, &orbit.join("auto_tasks/x.yaml"))
            .expect("diagnose granted path")
            .is_none()
    );
    let denied = linux_bwrap_write_grant_diagnostic(&resolved, &orbit.join("tasks/x.yaml"))
        .expect("diagnose ungranted path")
        .expect("ungranted path must be attributable");
    assert!(
        denied.contains("tasks/x.yaml") && denied.contains("denyModify rule"),
        "diagnostic must name the path and the deny that shadows it: {denied}"
    );
}

/// ADR-0286: auto-task *definitions* resolve through the runtime local root
/// (the worktree), while scheduler cursors and coordination state stay under
/// the shared root. Grant computation must keep those two roots apart: the
/// definition anchor is a narrow re-allow beneath the worktree's own `.orbit`
/// deny and is materialized here, while shared-root coordination state is a
/// host-owned broad root this layer never creates and never relocates into the
/// worktree.
#[test]
fn definition_and_cursor_roots_stay_separate_across_local_and_shared_orbit_dirs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let shared = temp.path().join("primary/.orbit");
    let worktree = shared.join("state/worktrees/orbit-jrun-test");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let local_definitions = worktree.join(".orbit/auto_tasks");
    let shared_cursor_state = shared.join("state/auto-tasks.json");

    let resolved = worktree_profile(
        &worktree,
        vec![
            format!("{}/**", local_definitions.display()),
            shared_cursor_state.display().to_string(),
        ],
    );

    let grants = linux_bwrap_write_grants(&resolved).expect("derive grants");
    let definitions = grants
        .iter()
        .find(|grant| grant.anchor == local_definitions)
        .expect("definition grant resolves through the local root");
    assert_eq!(definitions.kind, WriteAnchorKind::Directory);

    assert!(
        grants
            .iter()
            .all(|grant| grant.anchor.starts_with(&worktree)),
        "cursor/coordination state under the shared root is never a worktree write grant: {grants:?}"
    );

    let prepared =
        prepare_linux_bwrap_write_grants(&resolved, &worktree).expect("prepare policy grants");

    assert_eq!(prepared.created, vec![local_definitions.clone()]);
    assert!(
        prepared.unsatisfied.is_empty(),
        "{:?}",
        prepared.unsatisfied
    );
    assert!(
        local_definitions.is_dir(),
        "definitions are materialized under the runtime local root"
    );
    assert!(
        !shared.join("auto_tasks").exists(),
        "the local-root definition grant must not reach into the shared root"
    );
    assert!(
        !shared_cursor_state.exists(),
        "shared-root coordination state is the host's to create, not the worktree preparer's"
    );

    // The cursor root is a broad host-owned root, not a narrow exception, so
    // compilation refuses to proceed until the host has materialized it. That
    // refusal is what keeps the two roots from collapsing into one.
    let error = compile_linux_bwrap_argv(&resolved, "/bin/true", &[], Some(&worktree), true)
        .expect_err("an absent host-owned root must fail closed");
    assert!(error.to_string().contains("auto-tasks.json"), "{error}");

    std::fs::write(&shared_cursor_state, "{}").expect("host materializes cursor state");
    let plan = compile_linux_bwrap_argv(&resolved, "/bin/true", &[], Some(&worktree), true)
        .expect("compile");
    let joined = plan.args.join(" ");
    assert!(joined.contains(&format!("--bind {0} {0}", shared_cursor_state.display())));
    assert!(joined.contains(&format!("--bind {0} {0}", local_definitions.display())));
}

#[cfg(unix)]
#[test]
fn write_grant_preparation_rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let worktree = temp.path().join("worktree");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&worktree).expect("worktree");
    std::fs::create_dir_all(&outside).expect("outside");
    symlink(&outside, worktree.join(".orbit")).expect("symlink Orbit root");
    let resolved = worktree_profile(
        &worktree,
        vec![worktree.join(".orbit/config.toml").display().to_string()],
    );

    let error = prepare_linux_bwrap_write_grants(&resolved, &worktree)
        .expect_err("symlink escape must fail closed");

    assert!(error.to_string().contains("resolves through symlink"));
    assert!(!outside.join("config.toml").exists());
}

#[cfg(unix)]
#[test]
fn existing_write_grant_rejects_intermediate_symlink_escape() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let worktree = temp.path().join("worktree");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&worktree).expect("worktree");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(outside.join("config.toml"), "outside").expect("outside target");
    symlink(&outside, worktree.join(".orbit")).expect("symlink Orbit root");
    let resolved = worktree_profile(
        &worktree,
        vec![worktree.join(".orbit/config.toml").display().to_string()],
    );

    let error = prepare_linux_bwrap_write_grants(&resolved, &worktree)
        .expect_err("an existing target through an intermediate symlink must fail closed");

    let message = error.to_string();
    assert!(message.contains("config.toml"), "{message}");
    assert!(message.contains("resolves through symlink"), "{message}");
    assert_eq!(
        std::fs::read_to_string(outside.join("config.toml")).expect("outside unchanged"),
        "outside"
    );
}

#[cfg(unix)]
#[test]
fn existing_write_grant_rejects_final_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let worktree = temp.path().join("worktree");
    let orbit = worktree.join(".orbit");
    let outside = temp.path().join("outside.toml");
    std::fs::create_dir_all(&orbit).expect("Orbit root");
    std::fs::write(&outside, "outside").expect("outside target");
    symlink(&outside, orbit.join("config.toml")).expect("symlink final anchor");
    let resolved = worktree_profile(
        &worktree,
        vec![orbit.join("config.toml").display().to_string()],
    );

    let error = prepare_linux_bwrap_write_grants(&resolved, &worktree)
        .expect_err("a final symlink must fail closed");

    let message = error.to_string();
    assert!(message.contains("config.toml"), "{message}");
    assert!(message.contains("resolves through symlink"), "{message}");
}

#[test]
fn materialization_uses_final_last_match_wins_decision() {
    let temp = tempfile::tempdir().expect("tempdir");
    let worktree = temp.path().join("worktree");
    let orbit = worktree.join(".orbit");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let denied_file = orbit.join("workspace-denied");
    let partially_writable = orbit.join("cache.v1");
    let resolved = worktree_profile(
        &worktree,
        vec![
            denied_file.display().to_string(),
            format!("!{}", denied_file.display()),
            format!("{}/**", partially_writable.display()),
            format!("!{}/private/**", partially_writable.display()),
        ],
    );

    let grants = linux_bwrap_write_grants(&resolved).expect("derive effective grants");
    assert!(
        grants.iter().all(|grant| grant.anchor != denied_file),
        "a later exact deny must remove the shadowed materialization candidate: {grants:?}"
    );
    assert!(
        grants
            .iter()
            .any(|grant| grant.anchor == partially_writable),
        "a narrower child deny must preserve the writable remainder: {grants:?}"
    );

    let prepared =
        prepare_linux_bwrap_write_grants(&resolved, &worktree).expect("prepare effective grants");
    assert!(!denied_file.exists());
    assert!(partially_writable.is_dir());
    assert_eq!(prepared.created, vec![partially_writable.clone()]);

    let plan = compile_linux_bwrap_argv(&resolved, "/bin/true", &[], Some(&worktree), true)
        .expect("compile");
    assert!(
        plan.dropped_grants
            .iter()
            .all(|grant| grant.anchor != denied_file),
        "a finally denied rule is not an unsatisfied grant: {:?}",
        plan.dropped_grants
    );
}

#[test]
fn argv_is_deterministic_and_orders_denies_after_writable_parent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let denied = workspace.join(".orbit");
    std::fs::create_dir_all(&denied).expect("create fixture");
    let resolved = profile(vec![
        format!("{}/**", workspace.display()),
        format!("!{}/**", denied.display()),
    ]);

    let plan = compile_linux_bwrap_argv(&resolved, "/bin/true", &[], Some(&workspace), false)
        .expect("compile");
    let joined = plan.args.join(" ");
    for required in [
        "--die-with-parent",
        "--new-session",
        "--unshare-all",
        "--share-net",
        "--ro-bind / /",
        "--dev /dev",
        "--proc /proc",
        "--tmpfs /tmp",
    ] {
        assert!(
            joined.contains(required),
            "missing `{required}` in {joined}"
        );
    }
    let writable = joined.find(&format!("--bind {0} {0}", workspace.display()));
    let readonly = joined.find(&format!("--ro-bind {0} {0}", denied.display()));
    assert!(writable.is_some_and(|index| readonly.is_some_and(|deny| index < deny)));

    let repeated = compile_linux_bwrap_argv(&resolved, "/bin/true", &[], Some(&workspace), false)
        .expect("compile twice");
    assert_eq!(plan, repeated);
}

/// [ORB-11259] Managed worktrees get stable `/tmp` workspace and build mounts
/// so compiler caches can key on path-independent prefixes.
#[test]
fn managed_worktree_argv_binds_stable_workspace_and_build_mounts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let resolved = profile(vec![format!("{}/**", workspace.display())]);

    let unmanaged = compile_linux_bwrap_argv(&resolved, "/bin/true", &[], Some(&workspace), false)
        .expect("compile unmanaged");
    let unmanaged_joined = unmanaged.args.join(" ");
    assert!(
        !unmanaged_joined.contains(LINUX_STABLE_WORKSPACE_MOUNT),
        "direct invocations must not grow the stable toolchain mounts: {unmanaged_joined}"
    );

    let reviewer = profile(Vec::new());
    let reviewer_plan =
        compile_linux_bwrap_argv(&reviewer, "/bin/true", &[], Some(&workspace), true)
            .expect("compile reviewer managed");
    let reviewer_joined = reviewer_plan.args.join(" ");
    assert!(
        !reviewer_joined.contains(LINUX_STABLE_WORKSPACE_MOUNT),
        "read-only managed profiles must not bind a writable stable workspace mount: {reviewer_joined}"
    );

    let managed = compile_linux_bwrap_argv(&resolved, "/bin/true", &[], Some(&workspace), true)
        .expect("compile managed");
    let joined = managed.args.join(" ");
    let cwd = workspace.canonicalize().expect("canonical workspace");
    let target = cwd.join("target");
    assert!(target.is_dir(), "managed compile must create target/");
    assert!(
        joined.contains(&format!("--dir {LINUX_STABLE_WORKSPACE_MOUNT}")),
        "missing workspace mount dir in {joined}"
    );
    assert!(
        joined.contains(&format!(
            "--bind {} {LINUX_STABLE_WORKSPACE_MOUNT}",
            cwd.display()
        )),
        "missing workspace bind in {joined}"
    );
    assert!(
        joined.contains(&format!(
            "--bind {} {LINUX_STABLE_BUILD_MOUNT}",
            target.display()
        )),
        "missing build bind in {joined}"
    );
}

#[test]
fn argv_reallows_only_narrow_existing_paths_after_orbit_deny() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let orbit = workspace.join(".orbit");
    let resources = orbit.join("resources");
    let tasks = orbit.join("tasks");
    std::fs::create_dir_all(&resources).expect("create resources");
    std::fs::create_dir_all(&tasks).expect("create tasks");

    let resolved = profile(vec![
        format!("{}/**", workspace.display()),
        format!("!{}/**", orbit.display()),
        format!("{}/**", resources.display()),
        orbit.join("missing-config.toml").display().to_string(),
        // A later positive ancestor is not a narrow exception and must not
        // mask the read-only `.orbit` mount.
        format!("{}/**", workspace.display()),
        format!("{}/**", tasks.display()),
    ]);
    let plan = compile_linux_bwrap_argv(&resolved, "/bin/true", &[], Some(&workspace), false)
        .expect("compile");
    let joined = plan.args.join(" ");
    let orbit_readonly = joined
        .find(&format!("--ro-bind {0} {0}", orbit.display()))
        .expect("orbit deny mount");
    let resources_writable = joined
        .find(&format!("--bind {0} {0}", resources.display()))
        .expect("resources re-allow mount");
    let tasks_writable = joined
        .find(&format!("--bind {0} {0}", tasks.display()))
        .expect("trusted store re-allow mount");

    assert!(orbit_readonly < resources_writable);
    assert!(orbit_readonly < tasks_writable);
    assert!(
        !joined.contains("missing-config.toml"),
        "a missing narrow exception is skipped instead of making every sandbox invocation fail"
    );
    let dropped = plan
        .dropped_grants
        .iter()
        .find(|grant| grant.anchor.ends_with("missing-config.toml"))
        .expect("a skipped narrow exception must be reported, never silently dropped");
    assert_eq!(
        dropped.rule,
        orbit.join("missing-config.toml").display().to_string()
    );
    assert_eq!(
        joined
            .match_indices(&format!("--bind {0} {0}", workspace.display()))
            .count(),
        2,
        "both broad profile entries are emitted before the deny, never as a re-allow: {joined}"
    );
    let last_workspace = joined
        .rfind(&format!("--bind {0} {0}", workspace.display()))
        .expect("workspace bind");
    assert!(last_workspace < orbit_readonly);
}

#[test]
fn direct_invocation_allows_non_subtree_denies_without_writable_roots() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let resolved = profile(vec![
        format!("!{}/**/.env", workspace.display()),
        format!("!{}/**/*.env", workspace.display()),
    ]);

    let plan = compile_linux_bwrap_argv(&resolved, "/bin/true", &[], None, false)
        .expect("a read-only direct invocation needs no mount-based write exclusion");

    assert!(
        !plan.args.iter().any(|arg| arg == "--bind"),
        "a no-modify profile must not gain a writable bind: {:?}",
        plan.args
    );
}

#[test]
fn direct_invocation_fails_closed_for_overlapping_non_subtree_deny() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let resolved = profile(vec![
        format!("{}/**", workspace.display()),
        format!("!{}/**/*.env", workspace.display()),
    ]);
    let error = compile_linux_bwrap_argv(&resolved, "/bin/true", &[], None, false)
        .expect_err("direct invocation must fail closed");
    assert!(error.to_string().contains("non-subtree denyModify"));
}

/// [ORB-11257] A read-only direct invocation can compile default dotenv glob
/// denials. Live Bubblewrap must leave an existing match intact and refuse a
/// newly created matching path; a write-capable sibling still fails closed.
#[cfg(target_os = "linux")]
#[test]
fn kernel_enforces_existing_and_new_protected_env_paths_for_read_only_direct_invocation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let existing = workspace.join(".env");
    let created = workspace.join("new.env");
    std::fs::write(&existing, "secret").expect("write existing protected path");
    let read_only = profile(vec![
        format!("!{}/**/.env", workspace.display()),
        format!("!{}/**/.env.*", workspace.display()),
        format!("!{}/**/*.env", workspace.display()),
        format!("!{}/**/*.env.*", workspace.display()),
    ]);
    let unsafe_profile = profile(vec![
        format!("{}/**", workspace.display()),
        format!("!{}/**/*.env", workspace.display()),
    ]);
    let error = compile_linux_bwrap_argv(&unsafe_profile, "/bin/true", &[], None, false)
        .expect_err("direct invocation must fail closed for unsafe profiles");
    assert!(error.to_string().contains("non-subtree denyModify"));

    let script = format!(
        "! printf overwritten > '{existing}'; ! printf created > '{created}'; test -r '{existing}'",
        existing = existing.display(),
        created = created.display()
    );
    let plan = compile_linux_bwrap_argv(
        &read_only,
        "/bin/sh",
        &["-c".to_string(), script],
        Some(&workspace),
        false,
    )
    .expect("compile read-only direct invocation");
    assert!(
        !plan.args.iter().any(|arg| arg == "--bind"),
        "a no-modify profile must not gain a writable bind: {:?}",
        plan.args
    );

    let probe = probe_bwrap();
    if !probe.available {
        println!("skipping real Bubblewrap test: {}", probe.detail);
        return;
    }
    let mut child = spawn_under_linux_bwrap(LinuxBwrapSpawnRequest {
        plan: &plan,
        env: &[],
        cwd: Some(&workspace),
        stdin: Stdio::null(),
        stdout: Stdio::null(),
        stderr: Stdio::null(),
    })
    .expect("spawn");
    assert!(child.wait().expect("wait").success());
    assert_eq!(
        std::fs::read_to_string(&existing).expect("read existing protected path"),
        "secret"
    );
    assert!(
        !created.exists(),
        "newly created matching protected path must stay absent"
    );
}

#[test]
fn managed_worktree_guard_rejects_new_forbidden_match() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let resolved = profile(vec![
        format!("{}/**", workspace.display()),
        format!("!{}/**/*.env", workspace.display()),
    ]);
    let guard = LinuxBwrapPostRunGuard::capture(&resolved)
        .expect("capture")
        .expect("guard required");
    std::fs::write(workspace.join("new.env"), "secret").expect("write forbidden fixture");
    let error = guard.verify().expect_err("new forbidden match rejected");
    assert!(error.to_string().contains("before commit"));
}

#[cfg(target_os = "linux")]
#[test]
fn kernel_enforces_allowed_outside_and_subtree_writes_when_available() {
    let probe = probe_bwrap();
    if !probe.available {
        println!("skipping real Bubblewrap test: {}", probe.detail);
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let denied = workspace.join(".orbit");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&denied).expect("create denied root");
    std::fs::create_dir_all(&outside).expect("create outside root");
    let allowed_file = workspace.join("allowed.txt");
    let outside_file = outside.join("outside.txt");
    let denied_file = denied.join("denied.txt");
    let resolved = profile(vec![
        format!("{}/**", workspace.display()),
        format!("!{}/**", denied.display()),
    ]);
    let script = format!(
        "printf allowed > '{}'; ! printf outside > '{}'; ! printf denied > '{}'",
        allowed_file.display(),
        outside_file.display(),
        denied_file.display()
    );
    let plan = compile_linux_bwrap_argv(
        &resolved,
        "/bin/sh",
        &["-c".to_string(), script],
        Some(&workspace),
        false,
    )
    .expect("compile");
    let mut child = spawn_under_linux_bwrap(LinuxBwrapSpawnRequest {
        plan: &plan,
        env: &[],
        cwd: Some(&workspace),
        stdin: Stdio::null(),
        stdout: Stdio::null(),
        stderr: Stdio::null(),
    })
    .expect("spawn");
    let status = child.wait().expect("wait");
    assert!(status.success());
    assert_eq!(
        std::fs::read_to_string(allowed_file).expect("allowed write"),
        "allowed"
    );
    assert!(!outside_file.exists());
    assert!(!denied_file.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn kernel_enforces_versioned_orbit_exceptions_and_protected_stores_when_available() {
    let probe = probe_bwrap();
    if !probe.available {
        println!("skipping real Bubblewrap test: {}", probe.detail);
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let orbit = workspace.join(".orbit");
    for directory in [
        "resources",
        "state",
        "tasks",
        "adrs",
        "frictions",
        "future-store",
    ] {
        std::fs::create_dir_all(orbit.join(directory)).expect("create Orbit fixture directory");
    }
    for file in [
        "config.yaml",
        "orbit.db",
        "config.lock",
        "state/run.json",
        "tasks/task.yaml",
        "adrs/adr.yaml",
        "frictions/friction.md",
        "future-store/record.json",
        "resources/private.env",
    ] {
        std::fs::write(orbit.join(file), "before").expect("create Orbit fixture file");
    }

    let resolved = profile(vec![
        format!("{}/**", workspace.display()),
        format!("!{}/**", orbit.display()),
        format!("{}/**", orbit.join("auto_tasks").display()),
        format!("{}/**", orbit.join("routines").display()),
        orbit.join("config.yaml").display().to_string(),
        orbit.join("config.toml").display().to_string(),
        format!("{}/**", orbit.join("resources").display()),
        format!("!{}/**/*.env", workspace.display()),
    ]);
    assert!(!orbit.join("config.toml").exists());
    assert!(!orbit.join("routines").exists());
    assert!(!orbit.join("auto_tasks").exists());
    prepare_linux_bwrap_write_grants(&resolved, &workspace)
        .expect("trusted setup prepares absent granted anchors");
    assert_eq!(
        std::fs::read_to_string(orbit.join("config.toml")).expect("prepared empty config"),
        ""
    );
    assert!(orbit.join("routines").is_dir());
    assert!(orbit.join("auto_tasks").is_dir());
    // Ungranted paths under the same denied parent stay absent and unwritable.
    assert!(!orbit.join("state/new.json").exists());
    assert!(!orbit.join("future-store-new").exists());
    assert!(!orbit.join("resources/new.env").exists());
    let allowed = [
        orbit.join("routines/new.yaml"),
        orbit.join("auto_tasks/new.yaml"),
        orbit.join("config.toml"),
    ];
    let denied = [
        orbit.join("state/run.json"),
        orbit.join("tasks/task.yaml"),
        orbit.join("adrs/adr.yaml"),
        orbit.join("frictions/friction.md"),
        orbit.join("orbit.db"),
        orbit.join("config.lock"),
        orbit.join("future-store/record.json"),
        orbit.join("resources/private.env"),
    ];
    let script = allowed
        .iter()
        .map(|path| format!("printf allowed > '{}'", path.display()))
        .chain(
            denied
                .iter()
                .map(|path| format!("! printf denied > '{}'", path.display())),
        )
        .collect::<Vec<_>>()
        .join("; ");
    let plan = compile_linux_bwrap_argv(
        &resolved,
        "/bin/sh",
        &["-c".to_string(), script],
        Some(&workspace),
        true,
    )
    .expect("compile");
    let mut child = spawn_under_linux_bwrap(LinuxBwrapSpawnRequest {
        plan: &plan,
        env: &[],
        cwd: Some(&workspace),
        stdin: Stdio::null(),
        stdout: Stdio::null(),
        stderr: Stdio::null(),
    })
    .expect("spawn");
    let status = child.wait().expect("wait");
    assert!(status.success());

    for path in allowed {
        assert_eq!(
            std::fs::read_to_string(path).expect("allowed write"),
            "allowed"
        );
    }
    for path in denied {
        assert_eq!(
            std::fs::read_to_string(path).expect("denied fixture"),
            "before"
        );
    }
}
