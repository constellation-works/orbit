use super::*;

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
    let error = covering_spec.sandbox_profile(None).unwrap_err();
    // The sandbox boundary refusing a covering write is a policy refusal —
    // it must reach a caller as `PolicyDenied`, not `InvalidInput`, so an
    // agent can tell "your call was refused by policy" from "you sent
    // malformed input" [ORB-12837].
    assert!(
        matches!(error, orbit_common::OrbitError::PolicyDenied(_)),
        "{error:?}"
    );
    let error = error.to_string();
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
        super::super::super::loader::fs_write_root_covers(
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
    ungranted.grants = PluginGrantSet::default();
    ungranted.provenance.grants.clear();
    let profile = ungranted
        .sandbox_profile(Some(Path::new("/srv/checkout")))
        .expect("profile");
    assert!(profile.write.is_empty() && profile.write_files.is_empty());
    assert_eq!(
        profile.read,
        vec![
            PathBuf::from("/srv/plugins/demo"),
            PathBuf::from("/srv/plugins/demo/state"),
        ],
        "the plugin root and its own state, nothing of Orbit's"
    );
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
    let mut session = super::super::super::callback::PluginCallbackSession::mint(
        &global_root,
        &spec.provenance,
        &[],
    )
    .expect("mint callback session");
    session.bind_pid(std::process::id()).expect("bind pid");
    let other = super::super::super::callback::PluginCallbackSession::mint(
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

/// The host-owned secret store is unreadable to every plugin child: not
/// granted back to its own plugin, not reachable through the global-root read
/// `orbit_tools` opens, and not bought back by a manifest read root naming it.
/// The host reads a value and hands it over; the child never needs the store.
#[cfg(target_os = "linux")]
#[test]
fn the_landlock_ruleset_hides_the_plugin_secret_store() {
    use orbit_exec::{EnvironmentMode, ExecRequest, LandlockBoundary, StdinMode};

    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let plugin_root = temp.path().join("plugin");
    let secrets = global_root.join(crate::plugin::PLUGIN_SECRET_STORE_DIR);
    std::fs::create_dir_all(&plugin_root).expect("plugin root");
    std::fs::create_dir_all(&secrets).expect("secret store");
    std::fs::write(secrets.join("demo.json"), "{}").expect("own secrets");
    std::fs::write(secrets.join("other.json"), "{}").expect("other secrets");
    std::fs::write(global_root.join("config.toml"), "").expect("host config");

    let mut permissions = PluginPermissions::default();
    permissions.fs.read = vec![secrets.to_string_lossy().into_owned()];
    let mut spec = (*spec(
        plugin_root.join("bin"),
        &plugin_root,
        permissions,
        &[PluginGrant::OrbitTools, PluginGrant::Fs],
    ))
    .clone();
    spec.global_root.clone_from(&global_root);
    let profile = spec.sandbox_profile(None).expect("profile");
    assert!(
        profile.read_denies.contains(&secrets),
        "the secret store is a host-owned tree: {:?}",
        profile.read_denies
    );
    assert!(
        !profile.read.iter().any(|path| path.starts_with(&secrets)),
        "a manifest read root cannot buy the secret store back: {:?}",
        profile.read
    );

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
        !reads(&secrets.join("demo.json")),
        "a plugin cannot read even its own stored secrets"
    );
    assert!(!reads(&secrets.join("other.json")));
    assert!(!reads(&secrets), "the secret store must not be listable");
    assert!(
        reads(&global_root.join("config.toml")),
        "the rest of the global root stays readable under `orbit_tools`"
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
    let session = super::super::super::callback::PluginCallbackSession::mint(
        &global_root,
        &spec.provenance,
        &[],
    )
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
        &profile.readable_denied_trees(),
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

/// Runs `$TOOL` — a program the manifest declares — and reports whether the
/// sandbox let it execute.
const PROGRAM_RUNNER_BACKEND: &str = "#!/bin/sh\ncat >/dev/null\nif out=$(\"$TOOL\" 2>/dev/null); then result=\"$out\"; else result=denied; fi\nprintf '{\"ok\":true,\"output\":{\"result\":\"%s\"}}\\n' \"$result\"\n";

fn declared_program(dir: &Path) -> PathBuf {
    let program = dir.join("off-path/bin/tool");
    std::fs::create_dir_all(program.parent().expect("parent")).expect("program dir");
    std::fs::write(&program, "#!/bin/sh\necho ran\n").expect("write program");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    program.canonicalize().expect("canonical program")
}

#[test]
fn a_recorded_program_is_on_both_platform_profiles_and_an_unrecorded_one_is_not() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    std::fs::create_dir_all(&root).expect("plugin root");
    let program = declared_program(temp.path());
    let mut spec = (*spec(root.join("bin"), &root, PluginPermissions::default(), &[])).clone();
    spec.programs = vec!["tool".into(), "unrecorded".into()];
    spec.program_paths = [("tool".to_string(), program.clone())]
        .into_iter()
        .collect();

    let profile = spec.sandbox_profile(None).expect("profile");
    assert!(profile.read.contains(&program), "{:?}", profile.read);
    assert!(
        profile
            .macos_fs_rules()
            .read
            .contains(&format!("{}/**", program.display())),
        "the seatbelt profile allows the same program"
    );

    // A recorded path for a name the manifest no longer declares grants
    // nothing: an upgrade that drops a program drops its grant with it.
    spec.programs = vec!["unrecorded".into()];
    let profile = spec.sandbox_profile(None).expect("profile");
    assert!(!profile.read.contains(&program), "{:?}", profile.read);
}

/// The caller's `PATH` holds only the system directories, as a systemd unit
/// or a bare `env -i` shell would. The backend still executes a declared
/// program installed elsewhere because the sandbox grants the path recorded
/// at consent — and, under Landlock, cannot without that record.
#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn a_declared_program_off_the_caller_path_runs_under_the_sandbox() {
    require_sandbox();
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    std::fs::create_dir_all(&root).expect("plugin root");
    let program = declared_program(temp.path());
    let command = stub_backend(&root, PROGRAM_RUNNER_BACKEND);
    let ctx = ToolContext {
        proc_spawn_environment: Some(vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("TOOL".to_string(), program.to_string_lossy().into_owned()),
        ]),
        ..context(temp.path())
    };
    let mut declared = (*spec(command, &root, PluginPermissions::default(), &[])).clone();
    declared.programs = vec!["tool".into()];

    if cfg!(target_os = "linux") {
        let unrecorded = tool(std::sync::Arc::new(declared.clone()), None);
        let output = unrecorded.execute(&ctx, json!({})).expect("backend runs");
        assert_eq!(
            output["result"], "denied",
            "an unrecorded program off the caller PATH is not executable under Landlock"
        );
    }

    declared.program_paths = [("tool".to_string(), program)].into_iter().collect();
    let recorded = tool(std::sync::Arc::new(declared), None);
    let output = recorded.execute(&ctx, json!({})).expect("backend runs");
    assert_eq!(output["result"], "ran");
}
