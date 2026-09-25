use super::*;

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

/// The point of a path-scoped grant: the kernel, not a projection, is what
/// keeps the backend out of the part of its own request the operator did not
/// allow [ORB-12840].
#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn a_grant_narrower_than_the_request_denies_the_un_granted_part_of_it() {
    require_sandbox();
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&root).expect("plugin root");
    // Both directories exist, so the only thing separating them is the grant.
    std::fs::create_dir_all(workspace.join("output/granted")).expect("granted");
    std::fs::create_dir_all(workspace.join("output/ungranted")).expect("ungranted");
    let command = stub_backend(&root, SCOPED_ROOT_WRITER_BACKEND);
    // The manifest asks for the whole `output` tree...
    let permissions = PluginPermissions {
        fs: PluginFsPermissions {
            read: vec![],
            write: vec!["{{workspace}}/output".into()],
        },
        ..PluginPermissions::default()
    };
    // ...and the operator granted one directory inside it.
    let grants = parse_grants(&["fs={{workspace}}/output/granted".to_string()]).expect("grammar");
    assert_eq!(
        grants.fs_roots(),
        Some(["{{workspace}}/output/granted".to_string()].as_slice())
    );
    let backend = tool(scoped_spec(command, &root, permissions, grants), None);
    let ctx = ToolContext {
        workspace_root: Some(workspace.clone()),
        proc_spawn_environment: Some(vec![("PATH".to_string(), "/usr/bin:/bin".to_string())]),
        ..context(&workspace)
    };

    let output = backend.execute(&ctx, json!({})).expect("backend runs");
    assert_eq!(
        output["result"], "ok",
        "the granted directory writes and the un-granted one does not"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("output/granted/allowed.txt"))
            .expect("granted write"),
        "allowed\n"
    );
    assert!(
        !workspace.join("output/ungranted/denied.txt").exists(),
        "the manifest requested `output`, but the grant stopped at `output/granted`"
    );
}

/// The compiled profile, without a live sandbox: what the intersection keeps,
/// what it narrows, and what it drops.
#[test]
fn a_scoped_grant_intersects_the_manifests_request() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    let workspace = temp.path().join("workspace");
    let permissions = PluginPermissions {
        fs: PluginFsPermissions {
            read: vec!["{{workspace}}/shared".into()],
            write: vec!["{{workspace}}/output".into(), "{{plugin_state}}".into()],
        },
        ..PluginPermissions::default()
    };
    let spec = scoped_spec(
        root.join("backend.sh"),
        &root,
        permissions.clone(),
        parse_grants(&[
            "fs={{workspace}}/output/granted".to_string(),
            "{{workspace}}/shared".to_string(),
        ])
        .expect("grammar"),
    );
    let profile = spec
        .sandbox_profile(Some(&workspace))
        .expect("profile compiles");

    assert_eq!(
        profile.write,
        vec![workspace.join("output/granted")],
        "a grant inside a requested root narrows it, and `{{{{plugin_state}}}}` overlaps no \
         granted root, so it is dropped rather than refused"
    );
    assert_eq!(
        profile.read,
        vec![root.clone(), workspace.join("shared")],
        "the plugin root is always readable, and a root that is granted exactly as \
         requested passes through"
    );

    // A grant wider than a requested root does not widen it: the request is
    // still the ceiling, so the profile opens the request and not the grant.
    let wider = scoped_spec(
        root.join("backend.sh"),
        &root,
        permissions.clone(),
        parse_grants(&["fs={{workspace}}".to_string()]).expect("grammar"),
    );
    let profile = wider
        .sandbox_profile(Some(&workspace))
        .expect("profile compiles");
    assert_eq!(profile.write, vec![workspace.join("output")]);

    // The same manifest under the unscoped shorthand keeps its whole request.
    let shorthand = scoped_spec(
        root.join("backend.sh"),
        &root,
        permissions,
        parse_grants(&["fs".to_string()]).expect("grammar"),
    );
    let profile = shorthand
        .sandbox_profile(Some(&workspace))
        .expect("profile compiles");
    assert_eq!(
        profile.write,
        vec![workspace.join("output"), root.join("state")],
        "`--grant fs` still means every root the manifest requests"
    );
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
    unsandboxed.grants = PluginGrantSet::from_grants([PluginGrant::Fs, PluginGrant::Unsandboxed]);
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
