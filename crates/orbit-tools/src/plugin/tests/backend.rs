//! The granted profile is what the kernel enforces: a write outside it is
//! refused, `requires.programs` is bounded by the caller's allowlist, and
//! `unsandboxed` is the only way around either.

use std::path::{Path, PathBuf};

use orbit_types::plugin::{
    PluginFsPermissions, PluginGrant, PluginNetworkPermission, PluginPermissions, PluginSandbox,
};
use serde_json::json;

use super::super::backend::PluginBackendSpec;
use super::support::{context, sandbox_unavailable, spec, stub_backend, tool};
use crate::{Tool, ToolContext};

/// Writes `$1`-style paths handed in via the envelope input: `inside` under
/// the granted state directory, `outside` beside the plugin root.
const WRITER_BACKEND: &str = "#!/bin/sh\ncat >/dev/null\nresult=ok\nif ! echo inside > \"$ORBIT_PLUGIN_STATE/inside.txt\" 2>/dev/null; then result=inside_denied; fi\nif echo outside > \"$OUTSIDE\" 2>/dev/null; then result=\"$result,outside_written\"; fi\nprintf '{\"ok\":true,\"output\":{\"result\":\"%s\"}}\\n' \"$result\"\n";

fn fs_state_permissions() -> PluginPermissions {
    PluginPermissions {
        fs: PluginFsPermissions {
            read: vec![],
            write: vec!["{{plugin_state}}".into()],
        },
        ..PluginPermissions::default()
    }
}

#[cfg(unix)]
#[test]
fn a_write_outside_the_granted_fs_profile_is_denied_under_the_sandbox() {
    if sandbox_unavailable() {
        return;
    }
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
fn unsandboxed_needs_the_grant_and_then_confines_nothing() {
    if sandbox_unavailable() {
        return;
    }
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
