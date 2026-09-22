//! The granted profile is what the kernel enforces: a write outside it is
//! refused, `requires.programs` is bounded by the caller's allowlist, and
//! `unsandboxed` is the only way around either.

use std::path::{Path, PathBuf};

use orbit_types::plugin::{
    PluginFsPermissions, PluginGrant, PluginNetworkPermission, PluginPermissions, PluginSandbox,
};
use serde_json::json;

use super::super::backend::{PLUGIN_TIMEOUT_CEILING_MS, PluginBackendSpec};
use super::support::{context, require_sandbox, spec, stub_backend, tool};
use crate::{Tool, ToolContext};

/// Writes `$1`-style paths handed in via the envelope input: `inside` under
/// the granted state directory, `outside` beside the plugin root.
const WRITER_BACKEND: &str = "#!/bin/sh\ncat >/dev/null\nresult=ok\nif ! echo inside > \"$ORBIT_PLUGIN_STATE/inside.txt\" 2>/dev/null; then result=inside_denied; fi\nif echo outside > \"$OUTSIDE\" 2>/dev/null; then result=\"$result,outside_written\"; fi\nprintf '{\"ok\":true,\"output\":{\"result\":\"%s\"}}\\n' \"$result\"\n";

const WORKSPACE_METADATA_WRITER_BACKEND: &str = "#!/bin/sh\ncat >/dev/null\nresult=ok\nif ! echo allowed > \"$ORBIT_WORKSPACE_ROOT/output/allowed.txt\" 2>/dev/null; then result=allowed_denied; fi\nif echo schedule > \"$ORBIT_WORKSPACE_ROOT/.orbit/routines/demo.yaml\" 2>/dev/null; then result=\"$result,orbit_written\"; fi\nif echo hook > \"$ORBIT_WORKSPACE_ROOT/.git/hooks/pre-commit\" 2>/dev/null; then result=\"$result,git_written\"; fi\nprintf '{\"ok\":true,\"output\":{\"result\":\"%s\"}}\\n' \"$result\"\n";

const NOOP_BACKEND: &str =
    "#!/bin/sh\ncat >/dev/null\nprintf '{\"ok\":true,\"output\":{\"result\":\"ok\"}}\\n'\n";

fn fs_state_permissions() -> PluginPermissions {
    PluginPermissions {
        fs: PluginFsPermissions {
            read: vec![],
            write: vec!["{{plugin_state}}".into()],
        },
        ..PluginPermissions::default()
    }
}

#[test]
fn a_manifest_timeout_cannot_exceed_the_host_ceiling() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut backend = (*spec(
        temp.path().join("backend"),
        temp.path(),
        PluginPermissions::default(),
        &[],
    ))
    .clone();
    backend.timeout_ms = Some(PLUGIN_TIMEOUT_CEILING_MS.saturating_add(1));

    assert_eq!(backend.timeout_ms(), PLUGIN_TIMEOUT_CEILING_MS);
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn a_write_outside_the_granted_fs_profile_is_denied_under_the_sandbox() {
    require_sandbox();
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    std::fs::create_dir_all(&root).expect("plugin root");
    let outside = temp.path().join("outside.txt");
    let command = stub_backend(&root, WRITER_BACKEND);
    let tool = tool(
        spec(command, &root, fs_state_permissions(), &[PluginGrant::Fs]),
        None,
    );
    let ctx = ToolContext {
        proc_spawn_environment: Some(vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            (
                "OUTSIDE".to_string(),
                outside.to_string_lossy().into_owned(),
            ),
        ]),
        ..context(temp.path())
    };
    let output = tool.execute(&ctx, json!({})).expect("backend runs");
    assert_eq!(
        output["result"], "ok",
        "inside the grant writes; outside does not"
    );
    assert!(
        root.join("state/inside.txt").exists(),
        "the granted state directory was created and written"
    );
    assert!(
        !outside.exists(),
        "a write outside the granted profile must not reach the disk"
    );
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn a_workspace_subdirectory_grant_cannot_write_orbit_or_git_metadata() {
    require_sandbox();
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(root.clone()).expect("plugin root");
    std::fs::create_dir_all(workspace.join(".orbit/routines")).expect("routines");
    std::fs::create_dir_all(workspace.join(".git/hooks")).expect("hooks");
    let command = stub_backend(&root, WORKSPACE_METADATA_WRITER_BACKEND);
    let permissions = PluginPermissions {
        fs: PluginFsPermissions {
            read: vec![],
            write: vec!["{{workspace}}/output".into()],
        },
        ..PluginPermissions::default()
    };
    let backend = tool(spec(command, &root, permissions, &[PluginGrant::Fs]), None);
    let ctx = ToolContext {
        workspace_root: Some(workspace.clone()),
        proc_spawn_environment: Some(vec![("PATH".to_string(), "/usr/bin:/bin".to_string())]),
        ..context(&workspace)
    };

    let output = backend.execute(&ctx, json!({})).expect("backend runs");
    assert_eq!(output["result"], "ok");
    assert_eq!(
        std::fs::read_to_string(workspace.join("output/allowed.txt")).expect("allowed write"),
        "allowed\n"
    );
    assert!(!workspace.join(".orbit/routines/demo.yaml").exists());
    assert!(!workspace.join(".git/hooks/pre-commit").exists());
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn unsandboxed_needs_the_grant_and_then_confines_nothing() {
    require_sandbox();
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    std::fs::create_dir_all(&root).expect("plugin root");
    let outside = temp.path().join("outside.txt");
    let command = stub_backend(&root, WRITER_BACKEND);
    let ctx = ToolContext {
        proc_spawn_environment: Some(vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            (
                "OUTSIDE".to_string(),
                outside.to_string_lossy().into_owned(),
            ),
        ]),
        ..context(temp.path())
    };

    // `sandbox: none` without the grant still runs confined: the loader
    // would have refused this plugin, and the backend spec fails closed.
    let mut confined = (*spec(
        command.clone(),
        &root,
        fs_state_permissions(),
        &[PluginGrant::Fs],
    ))
    .clone();
    confined.sandbox = PluginSandbox::None;
    let profile = confined.sandbox_profile(None).expect("profile");
    assert!(!profile.unsandboxed, "no grant, no escape");

    let mut unsandboxed = confined.clone();
    unsandboxed.grants.push(PluginGrant::Unsandboxed);
    let profile = unsandboxed.sandbox_profile(None).expect("profile");
    assert!(profile.unsandboxed);
    let output = tool(std::sync::Arc::new(unsandboxed), None)
        .execute(&ctx, json!({}))
        .expect("backend runs");
    assert_eq!(output["result"], "ok,outside_written");
    assert!(outside.exists(), "an unsandboxed backend writes anywhere");
}

#[test]
fn the_profile_follows_the_grants_not_the_requests() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    let permissions = PluginPermissions {
        fs: PluginFsPermissions {
            read: vec!["{{workspace}}".into()],
            write: vec!["{{workspace}}/.cache".into(), "{{plugin_state}}".into()],
        },
        network: PluginNetworkPermission::Any,
        ..PluginPermissions::default()
    };
    let ungranted = spec(root.join("bin"), &root, permissions.clone(), &[]);
    let profile = ungranted
        .sandbox_profile(Some(temp.path()))
        .expect("profile");
    assert_eq!(profile.read, vec![root.clone()], "only the plugin root");
    assert!(profile.write.is_empty());
    assert_eq!(profile.network, PluginNetworkPermission::None);

    let granted = spec(
        root.join("bin"),
        &root,
        permissions,
        &[PluginGrant::Fs, PluginGrant::Network],
    );
    let profile = granted.sandbox_profile(Some(temp.path())).expect("profile");
    assert_eq!(profile.read, vec![root.clone(), temp.path().to_path_buf()]);
    assert_eq!(
        profile.write,
        vec![temp.path().join(".cache"), root.join("state")]
    );
    assert_eq!(profile.network, PluginNetworkPermission::Any);

    // `{{workspace}}` with no workspace is a refusal, not an empty path.
    let error = granted.sandbox_profile(None).unwrap_err().to_string();
    assert!(error.contains("no workspace"), "{error}");
}

#[test]
fn call_time_refuses_workspace_metadata_but_allows_a_similar_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&root).expect("plugin root");
    std::fs::create_dir_all(&workspace).expect("workspace");

    for declared in [
        "{{workspace}}",
        "{{workspace}}/.orbit/routines",
        "{{workspace}}/.git/hooks",
    ] {
        let permissions = PluginPermissions {
            fs: PluginFsPermissions {
                read: vec![],
                write: vec![declared.into()],
            },
            ..PluginPermissions::default()
        };
        let granted = spec(root.join("bin"), &root, permissions, &[PluginGrant::Fs]);
        let error = granted
            .sandbox_profile(Some(&workspace))
            .expect_err("workspace metadata must be refused at call time")
            .to_string();
        assert!(
            error.contains("spec.permissions.fs.write[0]")
                && error.contains(".orbit")
                && error.contains(".git"),
            "{declared}: {error}"
        );
    }

    let permissions = PluginPermissions {
        fs: PluginFsPermissions {
            read: vec![],
            write: vec!["{{workspace}}/.orbit-graph".into()],
        },
        ..PluginPermissions::default()
    };
    let profile = spec(root.join("bin"), &root, permissions, &[PluginGrant::Fs])
        .sandbox_profile(Some(&workspace))
        .expect("a similarly named workspace directory remains writable");
    assert_eq!(profile.write, vec![workspace.join(".orbit-graph")]);
}

/// The settled materialization rule, exercised through a real spawn
/// [ORB-12872]. Orbit creates the write directories *it* named — the
/// `orbit_tools` stores under the global root and the workspace's `.orbit/`,
/// and the plugin's own state tree — and creates nothing else. A path the
/// *manifest* named outside those prefixes is refused with a diagnostic
/// naming the root and is never brought into existence, which is the
/// security property four repairs of this path have had to preserve.
#[cfg(unix)]
#[test]
fn the_host_materializes_its_own_write_roots_and_never_a_manifest_path_outside_them() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    let workspace = temp.path().join("workspace");
    let global_root = root.join("global");
    std::fs::create_dir_all(&root).expect("plugin root");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let command = stub_backend(&root, NOOP_BACKEND);

    let unsandboxed = |permissions: PluginPermissions, grants: &[PluginGrant]| {
        let mut backend_spec = (*spec(command.clone(), &root, permissions, grants)).clone();
        backend_spec.sandbox = PluginSandbox::None;
        tool(std::sync::Arc::new(backend_spec), None)
    };
    let ctx = ToolContext {
        workspace_root: Some(workspace.clone()),
        ..context(&workspace)
    };

    // None of the host-owned store directories exist yet: the grant names
    // them, so the host creates them rather than refusing the call. This is
    // the half that has been hand-patched into fixtures four times. The
    // manifest's own `{{plugin_state}}` tail rides the same rule, because
    // the plugin state tree is a host-materialized prefix too.
    let mut state_permissions = PluginPermissions::default();
    state_permissions.fs.write = vec!["{{plugin_state}}/cache".into()];
    let host_owned = unsandboxed(
        state_permissions,
        &[
            PluginGrant::Fs,
            PluginGrant::OrbitTools,
            PluginGrant::Unsandboxed,
        ],
    );
    host_owned
        .execute(&ctx, json!({}))
        .expect("the host materializes the write roots it named itself");
    for relative in ["state/logs", "state/audit", "tasks"] {
        assert!(
            global_root.join(relative).is_dir(),
            "{relative} under the global root was not materialized"
        );
    }
    for relative in [
        "tasks",
        "frictions",
        "state/audit",
        "state/logs",
        "state/job-runs",
    ] {
        assert!(
            workspace.join(".orbit").join(relative).is_dir(),
            "{relative} under the workspace `.orbit` was not materialized"
        );
    }
    assert!(
        root.join("state/cache").is_dir(),
        "an absent tail inside the plugin's own state tree was not materialized"
    );

    // A manifest-named path outside every host-materialized prefix: absent,
    // consented, and still never created.
    let outside = temp.path().join("consented-but-absent/tasks");
    let mut permissions = PluginPermissions::default();
    permissions.fs.write = vec![outside.to_string_lossy().into_owned()];
    let error = unsandboxed(permissions, &[PluginGrant::Fs, PluginGrant::Unsandboxed])
        .execute(&ctx, json!({}))
        .expect_err("a manifest path outside the host-owned prefixes must be refused")
        .to_string();
    assert!(
        error.contains(&outside.display().to_string()) && error.contains("does not exist"),
        "the diagnostic must name the root: {error}"
    );
    assert!(
        !temp.path().join("consented-but-absent").exists(),
        "Orbit created a manifest-named path outside its own prefixes"
    );
}

#[test]
fn spawn_refuses_an_absent_write_root_escaping_the_workspace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    let workspace = temp.path().join("workspace/nested");
    std::fs::create_dir_all(&root).expect("plugin root");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let command = stub_backend(&root, NOOP_BACKEND);
    let permissions = PluginPermissions {
        fs: PluginFsPermissions {
            read: vec![],
            write: vec!["{{workspace}}/../../escaped".into()],
        },
        ..PluginPermissions::default()
    };
    let mut backend_spec = (*spec(
        command,
        &root,
        permissions,
        &[PluginGrant::Fs, PluginGrant::Unsandboxed],
    ))
    .clone();
    backend_spec.sandbox = PluginSandbox::None;
    let backend = tool(std::sync::Arc::new(backend_spec), None);
    let ctx = ToolContext {
        workspace_root: Some(workspace.clone()),
        ..context(&workspace)
    };

    let error = backend
        .execute(&ctx, json!({}))
        .expect_err("an absent host path outside materialization roots must be refused")
        .to_string();
    assert!(error.contains("does not exist"), "{error}");
    assert!(
        error.contains("create this consented directory before running the plugin"),
        "{error}"
    );
    assert!(
        !temp.path().join("escaped").exists(),
        "the host must not create a normalized write root outside the workspace"
    );
}

#[cfg(unix)]
#[test]
fn spawn_refuses_to_materialize_through_a_workspace_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    let workspace = temp.path().join("workspace");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&root).expect("plugin root");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::create_dir_all(&outside).expect("outside");
    symlink(&outside, workspace.join("linked")).expect("symlink");
    let command = stub_backend(&root, NOOP_BACKEND);
    let permissions = PluginPermissions {
        fs: PluginFsPermissions {
            read: vec![],
            write: vec!["{{workspace}}/linked/created".into()],
        },
        ..PluginPermissions::default()
    };
    let mut backend_spec = (*spec(
        command,
        &root,
        permissions,
        &[PluginGrant::Fs, PluginGrant::Unsandboxed],
    ))
    .clone();
    backend_spec.sandbox = PluginSandbox::None;
    let backend = tool(std::sync::Arc::new(backend_spec), None);
    let ctx = ToolContext {
        workspace_root: Some(workspace.clone()),
        ..context(&workspace)
    };

    let error = backend
        .execute(&ctx, json!({}))
        .expect_err("a symlinked prefix must fail before spawn")
        .to_string();
    assert!(error.contains("symbolic link"), "{error}");
    assert!(!outside.join("created").exists());
}

#[test]
fn call_time_fs_write_refuses_protected_global_paths_but_allows_plugin_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let root = global_root.join("plugins/demo/1.0.0");
    let state_dir = global_root.join("state/plugins/demo");
    std::fs::create_dir_all(&root).expect("plugin root");
    std::fs::create_dir_all(&state_dir).expect("plugin state");

    let mut covering = PluginPermissions::default();
    covering.fs.write = vec!["{{plugin_root}}".into()];
    let mut covering_spec = (*spec(root.join("bin"), &root, covering, &[PluginGrant::Fs])).clone();
    covering_spec.global_root.clone_from(&global_root);
    covering_spec.state_dir.clone_from(&state_dir);
    let error = covering_spec.sandbox_profile(None).unwrap_err().to_string();
    assert!(
        error.contains("spec.permissions.fs.write[0]") && error.contains("plugin install root"),
        "{error}"
    );

    for declared in [
        global_root.join("bin").to_string_lossy().into_owned(),
        global_root
            .join("plugins/.grants")
            .to_string_lossy()
            .into_owned(),
        global_root
            .join("plugins/other/1.0.0")
            .to_string_lossy()
            .into_owned(),
        "{{plugin_state}}/../../../plugins/.grants".to_string(),
    ] {
        let mut permissions = PluginPermissions::default();
        permissions.fs.write = vec![declared.clone()];
        let mut protected =
            (*spec(root.join("bin"), &root, permissions, &[PluginGrant::Fs])).clone();
        protected.global_root.clone_from(&global_root);
        protected.state_dir.clone_from(&state_dir);
        let error = protected.sandbox_profile(None).unwrap_err().to_string();
        assert!(
            error.contains("spec.permissions.fs.write[0]")
                && error.contains("protected path beneath Orbit global root"),
            "{declared}: {error}"
        );
    }

    let mut deferred_permissions = PluginPermissions::default();
    deferred_permissions.fs.write = vec!["{{workspace}}/plugins/.grants".into()];
    let mut deferred = (*spec(
        root.join("bin"),
        &root,
        deferred_permissions,
        &[PluginGrant::Fs],
    ))
    .clone();
    deferred.global_root.clone_from(&global_root);
    deferred.state_dir.clone_from(&state_dir);
    let error = deferred
        .sandbox_profile(Some(&global_root))
        .expect_err("a deferred workspace path into the global root must be refused")
        .to_string();
    assert!(
        error.contains("spec.permissions.fs.write[0]")
            && error.contains("protected path beneath Orbit global root"),
        "{error}"
    );

    let mut allowed = (*spec(
        root.join("bin"),
        &root,
        fs_state_permissions(),
        &[PluginGrant::Fs],
    ))
    .clone();
    allowed.global_root = global_root;
    allowed.state_dir.clone_from(&state_dir);
    let profile = allowed
        .sandbox_profile(None)
        .expect("the current plugin_state tree remains writable");
    assert_eq!(profile.write, vec![state_dir]);
}

/// A declared write root whose tail does not exist yet is judged where its
/// existing ancestors physically live, so an alias a backend planted inside
/// its own writable state cannot present the plugin's protected install
/// namespace as plugin state. The refusal lands on `sandbox_profile`, which
/// is what both platforms compile their write rules from, and it lands
/// before anything is created or any rule is bound [ORB-12799].
#[cfg(unix)]
#[test]
fn a_call_time_write_tail_below_a_state_symlink_is_refused_before_creation() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let install_root = global_root.join("plugins/demo");
    let root = install_root.join("1.0.0");
    let state_dir = global_root.join("state/plugins/demo");
    std::fs::create_dir_all(&root).expect("plugin root");
    std::fs::create_dir_all(&state_dir).expect("plugin state");
    symlink(&install_root, state_dir.join("alias")).expect("state alias");

    let mut permissions = PluginPermissions::default();
    permissions.fs.write = vec!["{{plugin_state}}/alias/9.0.0".into()];
    let mut aliased = (*spec(root.join("bin"), &root, permissions, &[PluginGrant::Fs])).clone();
    aliased.global_root.clone_from(&global_root);
    aliased.state_dir.clone_from(&state_dir);

    let error = aliased
        .sandbox_profile(None)
        .expect_err("an alias into the install namespace must be refused")
        .to_string();
    assert!(
        error.contains("spec.permissions.fs.write[0]")
            && error.contains("protected path beneath Orbit global root"),
        "{error}"
    );
    assert!(
        !install_root.join("9.0.0").exists(),
        "no version tree is materialised inside the protected install namespace"
    );

    // An absent directory inside the real state tree still compiles.
    let mut allowed_permissions = PluginPermissions::default();
    allowed_permissions.fs.write = vec!["{{plugin_state}}/cache/runs".into()];
    let mut allowed = (*spec(
        root.join("bin"),
        &root,
        allowed_permissions,
        &[PluginGrant::Fs],
    ))
    .clone();
    allowed.global_root.clone_from(&global_root);
    allowed.state_dir.clone_from(&state_dir);
    let profile = allowed
        .sandbox_profile(None)
        .expect("an absent directory inside plugin state stays writable");
    assert_eq!(profile.write, vec![state_dir.join("cache/runs")]);
}

/// The other half of that boundary: compiling the grant cannot reach a path
/// the check did not judge. The kernel rule is bound to the directory the
/// same resolution names, so a root the guard refuses is exactly the root a
/// rule would have carried — the check cannot be outflanked by resolving the
/// path differently at call time [ORB-12799].
#[cfg(all(unix, target_os = "linux"))]
#[test]
fn a_compiled_write_grant_binds_the_path_the_guard_judges() {
    use orbit_exec::{EnvironmentMode, ExecRequest, LandlockBoundary, StdinMode};
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let install_root = global_root.join("plugins/demo");
    let plugin_root = install_root.join("1.0.0");
    let state_dir = global_root.join("state/plugins/demo");
    std::fs::create_dir_all(&plugin_root).expect("plugin root");
    std::fs::create_dir_all(&state_dir).expect("plugin state");
    symlink(&install_root, state_dir.join("alias")).expect("state alias");
    let declared = state_dir.join("alias/9.0.0");

    let request = ExecRequest {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "true".to_string()],
        current_dir: None,
        timeout_ms: Some(1_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::ClearAndSet(vec![(
            "PATH".to_string(),
            "/usr/bin:/bin".to_string(),
        )]),
        debug: false,
    };
    let boundary = LandlockBoundary {
        write: vec![declared.clone()],
        ..LandlockBoundary::default()
    };
    let grants =
        orbit_exec::linux_landlock_boundary_grants(&request, &boundary).expect("compile grants");

    let resolved = install_root
        .canonicalize()
        .expect("canonicalize install root")
        .join("9.0.0");
    assert!(
        grants.iter().any(|grant| grant.writes(&resolved)),
        "the rule binds the resolved install directory, not the alias spelling"
    );
    assert_eq!(
        super::super::loader::fs_write_root_covers(
            &resolved,
            &plugin_root,
            &global_root,
            &state_dir,
            None,
        ),
        Some("protected path beneath Orbit global root"),
        "the guard refuses the very path the compiled grant would carry"
    );
}

#[test]
fn declared_programs_are_bounded_by_a_restricted_caller() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    let mut declared = (*spec(root.join("bin"), &root, PluginPermissions::default(), &[])).clone();
    declared.programs = vec!["git".into(), "rg".into()];

    // An unrestricted caller (direct CLI) imposes nothing.
    declared
        .enforce_programs(&context(temp.path()), "demo.hello")
        .expect("unrestricted");

    let restricted = ToolContext {
        proc_allowed_programs: vec!["git".into()],
        proc_spawn_activity_scoped: true,
        ..context(temp.path())
    };
    let error = declared
        .enforce_programs(&restricted, "demo.hello")
        .unwrap_err()
        .to_string();
    assert!(error.contains("'rg'"), "{error}");

    let env = declared.child_environment(&restricted, "/tmp", Some("demo.hello"));
    let programs = env
        .iter()
        .find(|(key, _)| key == "ORBIT_PROC_ALLOWED_PROGRAMS")
        .map(|(_, value)| value.as_str());
    assert_eq!(programs, Some("git,rg"));
    let allowed = env
        .iter()
        .find(|(key, _)| key == "ORBIT_ALLOWED_TOOLS")
        .map(|(_, value)| value.as_str());
    assert_eq!(allowed, Some(""), "always stamped, empty without the grant");
}

#[test]
fn env_pass_cannot_forward_a_privilege_bearing_orbit_name_even_if_requested() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    let permissions = PluginPermissions {
        env_pass: vec!["ORBIT_OPERATOR".into(), "DATABASE_URL".into()],
        ..PluginPermissions::default()
    };
    let granted = spec(
        root.join("bin"),
        &root,
        permissions,
        &[PluginGrant::EnvPass],
    );

    // Set in the real process env for the duration of this test (the
    // process-locked, race-free way this crate does that — see
    // `orbit_common::test_env`), simulating an operator session
    // (`ORBIT_OPERATOR=1`) whose plugin call requests it by name via
    // `env_pass`. The whole point of ORB-12768 is that it must not survive
    // that request even though it is genuinely present in the parent.
    let _env_guard = orbit_common::test_env::scoped([
        ("ORBIT_OPERATOR", Some("1")),
        (
            "DATABASE_URL",
            Some("postgres://svc:hunter2@db.internal/prod"),
        ),
    ]);
    let ctx = context(temp.path());

    let env = granted.child_environment(&ctx, "/tmp", None);

    assert!(
        !env.iter().any(|(key, _)| key == "ORBIT_OPERATOR"),
        "a privilege-bearing ORBIT_* name must never reach a plugin child, requested or not: {env:?}"
    );
    assert_eq!(
        env.iter()
            .find(|(key, _)| key == "DATABASE_URL")
            .map(|(_, value)| value.as_str()),
        Some("postgres://svc:hunter2@db.internal/prod"),
        "an ordinary requested name is still forwarded"
    );
}

/// Everything under Orbit's roots that holding `orbit_tools` must not make
/// writable: the binary the scheduler and every worker run *unconfined*, the
/// recorded plugin installs, the provider commands, the MCP authorization
/// ceiling, and the workspace's install pin [ORB-12777].
const DENIED_RELATIVE_PATHS: &[(&str, &str)] = &[
    ("global", "bin/orbit"),
    ("global", "plugins/demo/1.0.0/plugin.yaml"),
    ("global", "config.toml"),
    ("global", "mcp-callers.toml"),
    ("global", "clock.toml"),
    ("workspace", "plugins.yaml"),
    ("workspace", "config.toml"),
    ("workspace", "routines/sweep.yaml"),
];

fn orbit_tools_spec(global_root: &Path, plugin_root: &Path) -> PluginBackendSpec {
    let mut spec = (*spec(
        plugin_root.join("bin"),
        plugin_root,
        PluginPermissions::default(),
        &[PluginGrant::OrbitTools],
    ))
    .clone();
    spec.global_root = global_root.to_path_buf();
    spec
}

/// The `orbit_tools` grant opens Orbit's stores, not Orbit's roots. The roots
/// stay readable — `orbit tool run` cannot start without `config.toml` and the
/// recorded install — and every write is a path the host named.
#[test]
fn orbit_tools_writes_named_stores_and_never_the_roots() {
    let global_root = Path::new("/srv/orbit-global");
    let workspace_root = Path::new("/srv/checkout");
    let workspace_orbit = workspace_root.join(".orbit");
    let spec = orbit_tools_spec(global_root, Path::new("/srv/plugins/demo"));
    let profile = spec.sandbox_profile(Some(workspace_root)).expect("profile");

    assert!(
        !profile.write.contains(&global_root.to_path_buf()),
        "the global root is not a write tree: {:?}",
        profile.write
    );
    assert!(
        !profile.write.contains(&workspace_orbit),
        "the workspace .orbit is not a write tree: {:?}",
        profile.write
    );
    assert!(
        profile.read.contains(&global_root.to_path_buf())
            && profile.read.contains(&workspace_orbit),
        "both roots stay readable so the callback can start: {:?}",
        profile.read
    );

    assert_eq!(
        profile.write,
        vec![
            global_root.join("state/logs"),
            global_root.join("state/audit"),
            global_root.join("tasks"),
            workspace_orbit.join("tasks"),
            workspace_orbit.join("frictions"),
            workspace_orbit.join("state/audit"),
            workspace_orbit.join("state/logs"),
            workspace_orbit.join("state/job-runs"),
        ],
    );
    assert_eq!(
        profile.write_files,
        vec![
            global_root.join("orbit.db"),
            global_root.join("orbit.db-wal"),
            global_root.join("orbit.db-shm"),
            global_root.join(".generation.lock"),
            global_root.join(".generation-admission.lock"),
            workspace_orbit.join("state/semantic.db"),
            workspace_orbit.join("state/semantic.db-wal"),
            workspace_orbit.join("state/semantic.db-shm"),
        ],
    );
    // Every host-added write root is a materialization root, and nothing
    // wider: the global root's `state/` is not a prefix here, only the two
    // named stores beneath it. The tail of this list is the same inventory
    // the write assertion above pins, which is the property that keeps the
    // two from drifting apart [ORB-12872].
    assert_eq!(
        profile.materialization_roots,
        vec![
            workspace_root.to_path_buf(),
            PathBuf::from("/srv/plugins/demo/state"),
            global_root.join("state/logs"),
            global_root.join("state/audit"),
            global_root.join("tasks"),
            workspace_orbit.join("tasks"),
            workspace_orbit.join("frictions"),
            workspace_orbit.join("state/audit"),
            workspace_orbit.join("state/logs"),
            workspace_orbit.join("state/job-runs"),
        ],
        "the workspace, the plugin state tree and every host-added write root"
    );
    assert!(
        !profile
            .materialization_roots
            .contains(&global_root.join("state")),
        "the global state tree is not a materialization prefix: {:?}",
        profile.materialization_roots
    );
    // The rule, mechanised. This spec declares no `fs.write`, so every
    // granted write root is one the host added — and a host-added root Orbit
    // cannot create is exactly the "does not exist" refusal this path has
    // produced four times. Asserting containment rather than a third copy of
    // the inventory means a new entry cannot reintroduce it.
    for granted in &profile.write {
        assert!(
            profile
                .materialization_roots
                .iter()
                .any(|root| granted == root || granted.starts_with(root)),
            "host-added write root {} is not materializable",
            granted.display()
        );
    }

    // No granted write path is an ancestor of anything on the denied list.
    for (root, relative) in DENIED_RELATIVE_PATHS {
        let denied = match *root {
            "global" => global_root.join(relative),
            _ => workspace_orbit.join(relative),
        };
        assert!(
            !profile
                .write
                .iter()
                .any(|granted| denied.starts_with(granted)),
            "{} is reachable through a granted write tree",
            denied.display()
        );
        assert!(
            !profile.write_files.contains(&denied),
            "{} is a granted write file",
            denied.display()
        );
    }
}

/// Without the grant nothing of Orbit's own is opened at all — the narrowing
/// is a property of the grant, not a default the profile always carries.
#[test]
fn without_orbit_tools_no_orbit_store_is_opened() {
    let global_root = Path::new("/srv/orbit-global");
    let spec = orbit_tools_spec(global_root, Path::new("/srv/plugins/demo"));
    let mut ungranted = spec.clone();
    ungranted.grants.clear();
    ungranted.provenance.grants.clear();
    let profile = ungranted
        .sandbox_profile(Some(Path::new("/srv/checkout")))
        .expect("profile");
    assert!(profile.write.is_empty() && profile.write_files.is_empty());
    assert_eq!(profile.read, vec![PathBuf::from("/srv/plugins/demo")]);
}

/// The seatbelt profile draws the same line as the Landlock one: the write
/// set the two platforms compile comes from one inventory, so a plugin is not
/// confined on Linux and free on macOS.
///
/// Runs on every platform — `macos_sandbox::compile` is not gated on the host
/// OS — so Linux CI keeps the macOS half honest.
#[test]
fn the_seatbelt_profile_expresses_the_same_write_boundary_as_landlock() {
    let global_root = Path::new("/srv/orbit-global");
    let workspace_root = Path::new("/srv/checkout");
    let spec = orbit_tools_spec(global_root, Path::new("/srv/plugins/demo"));
    let profile = spec.sandbox_profile(Some(workspace_root)).expect("profile");
    let rules = profile.macos_fs_rules();

    // One inventory, two renderings: a directory becomes a `subpath` root,
    // a named file stays literal so it never widens into its parent.
    let expected: Vec<String> = profile
        .write
        .iter()
        .map(|path| format!("{}/**", path.display()))
        .chain(
            profile
                .write_files
                .iter()
                .map(|path| path.display().to_string()),
        )
        .collect();
    assert_eq!(rules.modify, expected);

    let profile_text =
        orbit_exec::compile_macos_sandbox_profile(&rules, "plugin").expect("compile sbpl");
    assert!(
        profile_text.contains(&format!("(subpath \"{}/tasks\")", global_root.display())),
        "the task store is writable"
    );
    assert!(
        profile_text.contains(&format!("(subpath \"{}/orbit.db\")", global_root.display())),
        "the store file is writable"
    );
    for (root, relative) in DENIED_RELATIVE_PATHS {
        let denied = match *root {
            "global" => global_root.join(relative),
            _ => workspace_root.join(".orbit").join(relative),
        };
        assert!(
            !rules
                .modify
                .iter()
                .any(|rule| denied.starts_with(rule.trim_end_matches("/**"))),
            "{} is reachable through a seatbelt modify rule: {:?}",
            denied.display(),
            rules.modify
        );
    }
}

/// The compiled Landlock ruleset is the enforcement, so assert on the grants
/// themselves rather than on the paths that produced them: nothing the child
/// holds may write the denied files, and the store it needs is writable.
#[cfg(target_os = "linux")]
#[test]
fn the_landlock_ruleset_refuses_every_denied_orbit_path() {
    use orbit_exec::{EnvironmentMode, ExecRequest, LandlockBoundary, StdinMode};

    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let workspace_root = temp.path().join("checkout");
    let workspace_orbit = workspace_root.join(".orbit");
    for (root, relative) in DENIED_RELATIVE_PATHS {
        let path = match *root {
            "global" => global_root.join(relative),
            _ => workspace_orbit.join(relative),
        };
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        std::fs::write(&path, "pre-existing").expect("write denied fixture");
    }
    // A real store file, so the file grant has an inode to bind.
    std::fs::write(global_root.join("orbit.db"), "SQLite format 3\0").expect("write store");

    let plugin_root = temp.path().join("plugin");
    std::fs::create_dir_all(&plugin_root).expect("plugin root");
    let spec = orbit_tools_spec(&global_root, &plugin_root);
    let profile = spec
        .sandbox_profile(Some(&workspace_root))
        .expect("profile");

    let request = ExecRequest {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "true".to_string()],
        current_dir: None,
        timeout_ms: Some(1_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::ClearAndSet(vec![(
            "PATH".to_string(),
            "/usr/bin:/bin".to_string(),
        )]),
        debug: false,
    };
    let boundary = LandlockBoundary {
        read_denies: profile.read_denies.clone(),
        read: profile.read.clone(),
        write: profile.write.clone(),
        write_files: profile.write_files.clone(),
        deny_tcp: true,
    };
    let grants =
        orbit_exec::linux_landlock_boundary_grants(&request, &boundary).expect("compile grants");

    let writes = |path: &Path| {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        grants.iter().any(|grant| grant.writes(&canonical))
    };
    for (root, relative) in DENIED_RELATIVE_PATHS {
        let denied = match *root {
            "global" => global_root.join(relative),
            _ => workspace_orbit.join(relative),
        };
        assert!(!writes(&denied), "{} is writable", denied.display());
        assert!(
            orbit_exec::grants_read(&grants, &denied),
            "{} stays readable: the callback needs the roots it cannot rewrite",
            denied.display()
        );
    }
    assert!(
        writes(&global_root.join("orbit.db")),
        "the store is writable"
    );
    assert!(
        writes(&global_root.join("tasks/ORB-1.yaml")),
        "the task store is writable"
    );
    assert!(
        writes(&workspace_orbit.join("frictions/FR-1.yaml")),
        "the workspace friction store is writable"
    );
    // An absent WAL sidecar yields no grant rather than a directory standing
    // where SQLite expects a file.
    assert!(
        !global_root.join("orbit.db-wal").exists(),
        "a named write file is never materialised by the compiler"
    );
}

/// The live callback sessions and the grant witnesses are host state, not
/// plugin state. A backend holding `orbit_tools` reads Orbit's global root,
/// so those two trees are carved back out of that grant — otherwise plugin B
/// reads plugin A's live token, or reads the witness that decides what A is
/// allowed to do [ORB-12798].
#[cfg(target_os = "linux")]
#[test]
fn the_landlock_ruleset_hides_callback_sessions_and_grant_witnesses() {
    use orbit_exec::{EnvironmentMode, ExecRequest, LandlockBoundary, StdinMode};

    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let plugin_root = temp.path().join("plugin");
    std::fs::create_dir_all(&plugin_root).expect("plugin root");
    std::fs::create_dir_all(global_root.join("plugins/.grants")).expect("grant witness dir");
    std::fs::write(global_root.join("plugins/.grants/demo.json"), "{}").expect("own witness");
    std::fs::write(global_root.join("plugins/.grants/other.json"), "{}").expect("other witness");
    std::fs::create_dir_all(global_root.join("plugins/demo/1.0.0")).expect("install tree");
    std::fs::write(global_root.join("plugins/demo/1.0.0/plugin.yaml"), "").expect("manifest");
    std::fs::write(global_root.join("config.toml"), "").expect("host config");

    let spec = orbit_tools_spec(&global_root, &plugin_root);
    let mut session =
        super::super::callback::PluginCallbackSession::mint(&global_root, &spec.provenance, &[])
            .expect("mint callback session");
    session.bind_pid(std::process::id()).expect("bind pid");
    let other = super::super::callback::PluginCallbackSession::mint(
        &global_root,
        &orbit_types::plugin::PluginProvenance {
            name: "other".to_string(),
            version: "1.0.0".to_string(),
            manifest_digest: "0".repeat(64),
            grants: Vec::new(),
        },
        &[],
    )
    .expect("mint a second plugin's session");

    let profile = spec
        .sandbox_profile(None)
        .expect("profile")
        .with_callback_session(&session);
    let request = ExecRequest {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "true".to_string()],
        current_dir: None,
        timeout_ms: Some(1_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::ClearAndSet(vec![(
            "PATH".to_string(),
            "/usr/bin:/bin".to_string(),
        )]),
        debug: false,
    };
    let boundary = LandlockBoundary {
        read: profile.read.clone(),
        read_denies: profile.read_denies.clone(),
        write: profile.write.clone(),
        write_files: profile.write_files.clone(),
        deny_tcp: true,
    };
    let grants =
        orbit_exec::linux_landlock_boundary_grants(&request, &boundary).expect("compile grants");
    let reads = |path: &Path| orbit_exec::grants_read(&grants, path);

    assert!(
        !reads(&global_root.join("state/plugin-callbacks")),
        "the session directory must not be listable"
    );
    assert!(
        !reads(&global_root.join("state")),
        "no ancestor of a denied directory is granted, or it could be listed \
         through that grant"
    );
    assert!(
        !reads(other.path()),
        "another plugin's live token must not be readable"
    );
    assert!(
        !reads(&global_root.join("plugins/.grants")),
        "the witness directory must not be listable"
    );
    assert!(
        !reads(&global_root.join("plugins/.grants/other.json")),
        "another plugin's grant witness must not be readable"
    );
    assert!(
        reads(&global_root.join("plugins/.grants/demo.json")),
        "the child's own witness stays readable: its nested `orbit tool run` \
         verifies the grants recorded for this plugin"
    );
    assert!(
        reads(session.path()),
        "the child's own callback record stays readable: it is how the child \
         identifies itself"
    );
    assert!(
        reads(&global_root.join("config.toml")),
        "the rest of the global root stays readable"
    );
    assert!(
        reads(&global_root.join("plugins/demo/1.0.0/plugin.yaml")),
        "the recorded installs stay readable beside the witness that is not"
    );
}

/// The same carve-out on macOS, where reads are broadly allowed and the
/// boundary is a deny appended after them. Compiled on any host so the two
/// platforms cannot drift.
#[test]
fn the_macos_profile_denies_callback_sessions_and_re_allows_the_childs_own_record() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let plugin_root = temp.path().join("plugin");
    std::fs::create_dir_all(&plugin_root).expect("plugin root");
    let spec = orbit_tools_spec(&global_root, &plugin_root);
    let session =
        super::super::callback::PluginCallbackSession::mint(&global_root, &spec.provenance, &[])
            .expect("mint callback session");
    let profile = spec
        .sandbox_profile(None)
        .expect("profile")
        .with_callback_session(&session);

    let mut profile_text =
        orbit_exec::compile_macos_sandbox_profile(&profile.macos_fs_rules(), "plugin")
            .expect("compile seatbelt profile");
    orbit_exec::append_macos_read_boundary(
        &mut profile_text,
        &profile.read_denies,
        &profile.readable_denied_files(),
    );

    for denied in ["state/plugin-callbacks", "plugins/.grants"] {
        assert!(
            profile_text.contains(&format!(
                "(deny file-read* (subpath \"{}\"))",
                global_root.join(denied).display()
            )),
            "{denied} is not denied: {profile_text}"
        );
    }
    let deny_at = profile_text
        .find("(deny file-read* (subpath")
        .expect("a read deny");
    let allow_at = profile_text
        .find(&format!(
            "(allow file-read* (literal \"{}\"))",
            session.path().display()
        ))
        .expect("the child's own record is re-allowed");
    assert!(
        allow_at > deny_at,
        "SBPL is last-match-wins: the re-allow must follow the deny\n{profile_text}"
    );
}
