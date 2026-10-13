#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

fn ship_sweep_selects_explicit_environment_and_default_registries() {
    let fixture = tempdir().expect("fixture tempdir");
    let home = fixture.path().join("home");
    let custom_root = fixture.path().join("custom-root");
    let custom_repo = fixture.path().join("custom-repo");
    let home_repo = fixture.path().join("home-repo");
    let scheduler_cwd = fixture.path().join("scheduler");
    for dir in [&home, &custom_repo, &home_repo, &scheduler_cwd] {
        fs::create_dir_all(dir).expect("fixture directory");
    }
    crate::git_repo::SeededRepo::new(&custom_repo)
        .readme("# fixture\n")
        .build();
    crate::git_repo::SeededRepo::new(&home_repo)
        .readme("# fixture\n")
        .build();

    let root = custom_root.to_str().expect("UTF-8 custom root");
    orbit(
        &custom_repo,
        &home,
        None,
        &[
            "--root",
            root,
            "init",
            "--non-interactive",
            "--machine-name",
            "custom-host",
            "--task-prefix",
            "CR",
        ],
    );
    orbit(
        &custom_repo,
        &home,
        None,
        &["--root", root, "workspace", "init", "--name", "custom-only"],
    );
    assert!(
        !home.join(".orbit").exists(),
        "custom setup must not create the home registry"
    );

    let explicit = orbit_json(
        &scheduler_cwd,
        &home,
        None,
        &["--root", root, "run", "ship-sweep", "--dry-run", "--json"],
    );
    assert_workspace(&explicit, "custom-only");
    let from_env = orbit_json(
        &scheduler_cwd,
        &home,
        Some(&custom_root),
        &["run", "ship-sweep", "--dry-run", "--json"],
    );
    assert_workspace(&from_env, "custom-only");
    assert!(
        !home.join(".orbit").exists(),
        "an override must not fall back to HOME"
    );
    assert!(
        !scheduler_cwd.join(".orbit").exists(),
        "the scheduler cwd must remain untouched"
    );

    orbit(
        &home_repo,
        &home,
        None,
        &[
            "init",
            "--non-interactive",
            "--machine-name",
            "home-host",
            "--task-prefix",
            "HR",
        ],
    );
    orbit(
        &home_repo,
        &home,
        None,
        &["workspace", "init", "--name", "home-only"],
    );
    let default = orbit_json(
        &scheduler_cwd,
        &home,
        None,
        &["run", "ship-sweep", "--dry-run", "--json"],
    );
    assert_workspace(&default, "home-only");

    let explicit_wins = orbit_json(
        &scheduler_cwd,
        &home,
        Some(&home.join(".orbit")),
        &["--root", root, "run", "ship-sweep", "--dry-run", "--json"],
    );
    assert_workspace(&explicit_wins, "custom-only");
    assert!(
        !scheduler_cwd.join(".orbit").exists(),
        "sweep must not bootstrap the scheduler cwd"
    );
}

fn ship_sweep_preserves_unchanged_registry_modification_time() {
    let fixture = tempdir().expect("fixture tempdir");
    let home = fixture.path().join("home");
    let custom_root = fixture.path().join("custom-root");
    let custom_repo = fixture.path().join("custom-repo");
    let scheduler_cwd = fixture.path().join("scheduler");
    for dir in [&home, &custom_repo, &scheduler_cwd] {
        fs::create_dir_all(dir).expect("fixture directory");
    }
    crate::git_repo::SeededRepo::new(&custom_repo)
        .readme("# fixture\n")
        .build();

    let root = custom_root.to_str().expect("UTF-8 custom root");
    orbit(
        &custom_repo,
        &home,
        None,
        &[
            "--root",
            root,
            "init",
            "--non-interactive",
            "--machine-name",
            "custom-host",
            "--task-prefix",
            "CR",
        ],
    );
    orbit(
        &custom_repo,
        &home,
        None,
        &["--root", root, "workspace", "init", "--name", "custom-ws"],
    );

    let registry_path = custom_root.join("workspaces.json");
    let original_bytes = fs::read(&registry_path).expect("read initialized registry");
    let fixed_stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    fs::File::options()
        .write(true)
        .open(&registry_path)
        .expect("open registry to set modification time")
        .set_modified(fixed_stamp)
        .expect("set registry modification time");
    let modified = fs::metadata(&registry_path)
        .expect("metadata before non-dry-run sweep")
        .modified()
        .expect("mtime before non-dry-run sweep");

    // Non-dry-run with no workspace status change must not rewrite the registry.
    let output = orbit_json(
        &scheduler_cwd,
        &home,
        None,
        &["--root", root, "run", "ship-sweep", "--json"],
    );
    assert_eq!(output["workspaces"], 1, "{output}");
    assert_eq!(
        output["reports"][0]["workspace_name"], "custom-ws",
        "{output}"
    );
    assert_eq!(
        fs::metadata(&registry_path)
            .expect("metadata after non-dry-run sweep")
            .modified()
            .expect("mtime after non-dry-run sweep"),
        modified,
        "unchanged non-dry-run sweep must preserve registry modification time"
    );
    assert_eq!(
        fs::read(&registry_path).expect("read registry after non-dry-run sweep"),
        original_bytes,
        "unchanged non-dry-run sweep must preserve registry bytes"
    );

    // Dry-run with no workspace status change must also preserve modification time and bytes.
    let dry_run_output = orbit_json(
        &scheduler_cwd,
        &home,
        None,
        &["--root", root, "run", "ship-sweep", "--dry-run", "--json"],
    );
    assert_eq!(dry_run_output["dry_run"], true);
    assert_eq!(
        fs::metadata(&registry_path)
            .expect("metadata after dry-run sweep")
            .modified()
            .expect("mtime after dry-run sweep"),
        modified,
        "unchanged dry-run sweep must preserve registry modification time"
    );
    assert_eq!(
        fs::read(&registry_path).expect("read registry after dry-run sweep"),
        original_bytes,
        "unchanged dry-run sweep must preserve registry bytes"
    );
}

fn ship_sweep_dry_run_does_not_persist_invalid_status_flip_for_missing_checkout() {
    let fixture = tempdir().expect("fixture tempdir");
    let home = fixture.path().join("home");
    let custom_root = fixture.path().join("custom-root");
    let custom_repo = fixture.path().join("custom-repo");
    let scheduler_cwd = fixture.path().join("scheduler");
    for dir in [&home, &custom_repo, &scheduler_cwd] {
        fs::create_dir_all(dir).expect("fixture directory");
    }
    crate::git_repo::SeededRepo::new(&custom_repo)
        .readme("# fixture\n")
        .build();

    let root = custom_root.to_str().expect("UTF-8 custom root");
    orbit(
        &custom_repo,
        &home,
        None,
        &[
            "--root",
            root,
            "init",
            "--non-interactive",
            "--machine-name",
            "custom-host",
            "--task-prefix",
            "CR",
        ],
    );
    orbit(
        &custom_repo,
        &home,
        None,
        &["--root", root, "workspace", "init", "--name", "custom-ws"],
    );

    let registry_path = custom_root.join("workspaces.json");
    let initial_registry: Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("read initial registry"))
            .expect("parse json");
    assert_eq!(
        initial_registry["workspaces"][0]["status"], "active",
        "registered workspace starts active"
    );

    // Remove the checkout repository so that repo_root is missing.
    fs::remove_dir_all(&custom_repo).expect("remove workspace checkout repo");

    let fixed_stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    fs::File::options()
        .write(true)
        .open(&registry_path)
        .expect("open registry to set modification time")
        .set_modified(fixed_stamp)
        .expect("set registry modification time");
    let modified_before = fs::metadata(&registry_path)
        .expect("metadata before dry run")
        .modified()
        .expect("mtime before dry run");
    let bytes_before = fs::read(&registry_path).expect("bytes before dry run");

    // Dry-run must report the missing checkout as inactive without rewriting workspaces.json.
    let dry_run = orbit_json(
        &scheduler_cwd,
        &home,
        None,
        &["--root", root, "run", "ship-sweep", "--dry-run", "--json"],
    );
    assert_eq!(dry_run["dry_run"], true);
    assert_eq!(dry_run["reports"][0]["action"], "skipped", "{dry_run}");
    assert_eq!(
        dry_run["reports"][0]["skip_reason"], "workspace_inactive",
        "{dry_run}"
    );

    assert_eq!(
        fs::metadata(&registry_path)
            .expect("metadata after dry run")
            .modified()
            .expect("mtime after dry run"),
        modified_before,
        "dry-run must not change registry modification time even when checkout is missing"
    );
    assert_eq!(
        fs::read(&registry_path).expect("read registry after dry run"),
        bytes_before,
        "dry-run must not rewrite registry bytes even when checkout is missing"
    );

    let registry_after_dry_run: Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("read registry after dry run"))
            .expect("parse json");
    assert_eq!(
        registry_after_dry_run["workspaces"][0]["status"], "active",
        "dry-run must not persist Active to Invalid status flip"
    );

    // A non-dry-run sweep MUST save the registry when validate_workspaces flips status to invalid.
    let sweep = orbit_json(
        &scheduler_cwd,
        &home,
        None,
        &["--root", root, "run", "ship-sweep", "--json"],
    );
    assert_eq!(sweep["reports"][0]["action"], "skipped", "{sweep}");
    assert_eq!(
        sweep["reports"][0]["skip_reason"], "workspace_inactive",
        "{sweep}"
    );

    let modified_after_sweep = fs::metadata(&registry_path)
        .expect("metadata after non-dry-run sweep")
        .modified()
        .expect("mtime after non-dry-run sweep");
    assert_ne!(
        modified_after_sweep, modified_before,
        "non-dry-run sweep must rewrite the registry when workspace status changes"
    );

    let persisted: Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("read persisted registry"))
            .expect("parse json");
    assert_eq!(
        persisted["workspaces"][0]["status"], "invalid",
        "non-dry-run sweep must persist Invalid status flip"
    );
}

fn assert_workspace(output: &Value, expected_name: &str) {
    assert_eq!(output["dry_run"], true);
    assert_eq!(output["workspaces"], 1, "{output}");
    assert_eq!(
        output["reports"][0]["workspace_name"], expected_name,
        "{output}"
    );
    assert_eq!(output["reports"][0]["action"], "skipped", "{output}");
    assert_eq!(
        output["reports"][0]["skip_reason"], "auto_ship_disabled",
        "{output}"
    );
}

fn orbit_json(cwd: &Path, home: &Path, root_env: Option<&Path>, args: &[&str]) -> Value {
    let output = orbit(cwd, home, root_env, args);
    serde_json::from_slice(&output).expect("ship-sweep JSON output")
}

fn orbit(cwd: &Path, home: &Path, root_env: Option<&Path>, args: &[&str]) -> Vec<u8> {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        // The host-global tracing feed creates HOME/.orbit/state/logs on
        // the first enabled event, including warnings on a loaded host.
        // Keep the empty-HOME guard focused on registry fallback.
        .env("RUST_LOG", "off");
    if let Some(root) = root_env {
        command.env("ORBIT_ROOT", root);
    }
    command
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone()
}

#[test]
fn ship_sweep_contracts() {
    for (name, check) in [
        (
            "ship_sweep_selects_explicit_environment_and_default_registries",
            ship_sweep_selects_explicit_environment_and_default_registries as fn(),
        ),
        (
            "ship_sweep_preserves_unchanged_registry_modification_time",
            ship_sweep_preserves_unchanged_registry_modification_time as fn(),
        ),
        (
            "ship_sweep_dry_run_does_not_persist_invalid_status_flip_for_missing_checkout",
            ship_sweep_dry_run_does_not_persist_invalid_status_flip_for_missing_checkout as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
    }
}
