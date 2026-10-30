//! A workspace whose `.orbit/` layout is newer than this binary: the runtime
//! refuses to open it, while `orbit_cmd::migrate_dry_run_at` still inspects it.
//! A build that is not the installed orbit cannot migrate the layout past it.

use std::fs;

use orbit_core::OrbitRuntime;

#[test]
fn workspace_layout_newer_than_binary_refuses_to_open() {
    if crate::isolated_test::run_in_child(
        "layout_version::workspace_layout_newer_than_binary_refuses_to_open",
    ) {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let workspace_root = temp.path().join("repo").join(".orbit");
    let state_dir = workspace_root.join("state");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(&state_dir).expect("mkdir state");
    fs::write(state_dir.join("layout.version"), "99\n").expect("write marker");
    // The build that advanced the layout recorded itself.
    fs::write(
        state_dir.join("layout.writer"),
        r#"{"layout_version":99,"executable":"/candidate/target/debug/orbit"}"#,
    )
    .expect("write writer record");

    let Err(error) = OrbitRuntime::from_roots(&global_root, &workspace_root) else {
        panic!("open must refuse a newer layout");
    };
    let message = error.to_string();
    assert!(message.contains("layout version 99"), "{message}");
    assert!(message.contains("upgrade orbit"), "{message}");
    assert!(
        message.contains("layout version 99 was written by '/candidate/target/debug/orbit'"),
        "{message}"
    );

    // The dry-run surface still inspects it and flags the mismatch.
    let status =
        orbit_cmd::migrate_dry_run_at(&global_root, &workspace_root).expect("dry run status");
    assert_eq!(status.layout_version, 99);
    assert!(status.newer_than_binary());
    assert!(status.pending_layout.is_empty());
}

/// A build that is not the host's installed orbit (a development or candidate
/// build run for QA from a run worktree) must not migrate the workspace past
/// what the installed binary supports [ORB-20262]. The installed binary is a
/// stand-in in a private `ORBIT_INSTALL_DIR` that reports the layout before
/// this build's newest breaking migration.
#[cfg(unix)]
#[test]
fn a_build_that_is_not_installed_cannot_migrate_past_the_installed_orbit() {
    if crate::isolated_test::run_in_child(
        "layout_version::a_build_that_is_not_installed_cannot_migrate_past_the_installed_orbit",
    ) {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    let install_dir = temp.path().join("installed-bin");
    for dir in [&home, &repo, &install_dir] {
        fs::create_dir_all(dir).expect("fixture dir");
    }
    let init = crate::git_repo::command()
        .args(["init", "--quiet"])
        .current_dir(&repo)
        .output()
        .expect("git init");
    assert!(init.status.success(), "{init:?}");
    let orbit = |args: &[&str]| {
        let mut command = assert_cmd::cargo::cargo_bin_cmd!("orbit");
        orbit_common::test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&repo)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("XDG_CONFIG_HOME", home.join("xdg"))
            .env("ORBIT_INSTALL_DIR", &install_dir)
            .args(args)
            .output()
            .expect("run orbit")
    };
    for args in [
        &[
            "init",
            "--non-interactive",
            "--machine-name",
            "installed-guard",
            "--task-prefix",
            "IG",
        ][..],
        &["workspace", "init", "--name", "installed-guard"][..],
    ] {
        let output = orbit(args);
        assert!(output.status.success(), "{args:?}: {output:?}");
    }

    let compiled = orbit_core::composition::compiled_compatibility();
    let newest = compiled.workspace_layout.version;
    assert_eq!(
        compiled.workspace_layout.writer_floor, newest,
        "the fixture needs this build's newest layout migration to be breaking"
    );
    let older = newest - 1;
    let state = repo.join(".orbit").join("state");
    let marker = state.join("layout.version");
    fs::write(&marker, format!("{older}\n")).expect("rewind layout marker");
    let mut installed_identity = compiled.clone();
    installed_identity.workspace_layout.version = older;
    installed_identity.workspace_layout.writer_floor = older;
    installed_identity.workspace_layout.reader_floor = older;
    let installed = install_stand_in(&install_dir, &installed_identity);

    let refused = orbit(&["task", "list", "--json"]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "{refused:?}");
    assert!(stderr.contains("upgrade admission refused"), "{stderr}");
    assert!(stderr.contains(env!("CARGO_BIN_EXE_orbit")), "{stderr}");
    assert!(
        stderr.contains(&installed.display().to_string()),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("supports workspace layout v{older}")),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(&marker).expect("marker"),
        format!("{older}\n"),
        "a build that is not installed advanced the layout past the installed orbit"
    );

    // Once the installed orbit supports this build's layout, it migrates and
    // records itself as the writer.
    install_stand_in(&install_dir, &compiled);
    let opened = orbit(&["task", "list", "--json"]);
    assert!(opened.status.success(), "{opened:?}");
    assert_eq!(
        fs::read_to_string(&marker).expect("marker"),
        format!("{newest}\n")
    );
    let writer: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(state.join("layout.writer")).expect("layout writer record"),
    )
    .expect("layout writer JSON");
    assert_eq!(writer["layout_version"], newest);
    assert_eq!(
        std::path::Path::new(writer["executable"].as_str().expect("writer executable"))
            .canonicalize()
            .expect("writer resolves"),
        std::path::Path::new(env!("CARGO_BIN_EXE_orbit"))
            .canonicalize()
            .expect("binary resolves")
    );

    // A present installed executable that does not report compatibility keeps
    // the migration warning path, while allowing the open to proceed.
    fs::write(&marker, format!("{older}\n")).expect("rewind layout marker");
    install_non_reporting_stand_in(&install_dir);
    let opened_without_report = orbit(&["task", "list", "--json"]);
    let stderr = String::from_utf8_lossy(&opened_without_report.stderr);
    assert!(
        opened_without_report.status.success(),
        "{opened_without_report:?}"
    );
    assert!(
        stderr.contains("the installed orbit did not report its state compatibility"),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(&marker).expect("marker"),
        format!("{newest}\n"),
        "the missing compatibility report must warn but allow the migration"
    );
}

/// Install an `orbit` stand-in that reports `identity` through
/// `update --contract --json`, replacing any earlier one by rename.
#[cfg(unix)]
fn install_stand_in(
    install_dir: &std::path::Path,
    identity: &orbit_common::fs::generation::CompatibilityIdentity,
) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let report = serde_json::json!({
        "schema_version": 1,
        "contract": orbit_common::fs::generation::LEGACY_GENERATION_CONTRACT,
        "admission_contract": orbit_common::fs::generation::GENERATION_CONTRACT,
        "compatibility": identity,
        "resume": [],
    });
    let staged = install_dir.join(".orbit.staged");
    fs::write(
        &staged,
        format!("#!/bin/sh\ncat <<'JSON'\n{report}\nJSON\n"),
    )
    .expect("write stand-in");
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).expect("chmod stand-in");
    let installed = install_dir.join("orbit");
    fs::rename(&staged, &installed).expect("install stand-in");
    installed
}

#[cfg(unix)]
fn install_non_reporting_stand_in(install_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let staged = install_dir.join(".orbit.staged");
    fs::write(&staged, "#!/bin/sh\nexit 1\n").expect("write non-reporting stand-in");
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))
        .expect("chmod non-reporting stand-in");
    fs::rename(staged, install_dir.join("orbit")).expect("install non-reporting stand-in");
}
