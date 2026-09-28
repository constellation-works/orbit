#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Binary-level coverage for explicit workspace managed-artifact convergence.

use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

fn orbit(cwd: &Path, home: &Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("ORBIT_HOME");
    command
}

fn write_machine_identity(home: &Path) {
    let global = home.join(".orbit");
    std::fs::create_dir_all(&global).expect("create global root");
    std::fs::write(
        global.join("config.toml"),
        "[machine]\nid = \"hm_workspace_sync\"\nname = \"sync-machine\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("write machine identity");
}

fn read(path: impl Into<PathBuf>) -> Vec<u8> {
    std::fs::read(path.into()).expect("read snapshot path")
}

fn read_optional(path: impl Into<PathBuf>) -> Option<Vec<u8>> {
    std::fs::read(path.into()).ok()
}

#[test]
fn workspace_sync_creates_missing_defaults_preserves_operator_content_and_is_idempotent() {
    let home = tempdir().expect("home tempdir");
    let repo = home.path().join("workspace");
    std::fs::create_dir_all(repo.join(".git")).expect("create workspace repo");
    write_machine_identity(home.path());
    orbit(&repo, home.path())
        .args(["workspace", "init"])
        .assert()
        .success();

    // `workspace sync` reports each action's path as the process resolves it,
    // so fixtures compared against those paths have to resolve the same way.
    // A macOS temp dir arrives as `/var/...` and resolves to `/private/var/...`.
    let repo_root = std::fs::canonicalize(&repo).expect("canonical workspace root");
    let workspace_root = repo_root.join(".orbit");
    let auto_tasks = workspace_root.join("auto_tasks");
    let missing = auto_tasks.join("code-review.yaml");
    std::fs::remove_file(&missing).expect("remove a shipped auto-task");

    let locally_modified = auto_tasks.join("security-review.yaml");
    let local_body = format!(
        "{}# operator edit\n",
        std::fs::read_to_string(&locally_modified).expect("read managed auto-task")
    );
    std::fs::write(&locally_modified, &local_body).expect("edit managed auto-task");

    let collision = auto_tasks.join("qa-sweep.yaml");
    let collision_body = "operator-authored definition using a bundled file name\n";
    std::fs::write(&collision, collision_body).expect("write colliding auto-task");
    let manifest_path = auto_tasks.join(".orbit-managed-assets.json");
    let mut manifest: Value =
        serde_json::from_slice(&read(&manifest_path)).expect("parse manifest");
    manifest["assets"]
        .as_object_mut()
        .expect("assets object")
        .remove("qa-sweep");
    std::fs::write(
        &manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).expect("serialize manifest")
        ),
    )
    .expect("remove collision provenance");

    let registry = home.path().join(".orbit/workspaces.json");
    let identity = workspace_root.join("config.yaml");
    let registry_before = read(&registry);
    let identity_before = read(&identity);
    let gitignore_before = read_optional(repo.join(".gitignore"));

    let check = orbit(&repo, home.path())
        .args(["workspace", "sync", "--check", "--json"])
        .assert()
        .code(3)
        .get_output()
        .stdout
        .clone();
    let checked: Value = serde_json::from_slice(&check).expect("parse check JSON");
    assert!(checked["check"].as_bool().expect("check flag"));
    assert!(
        checked["actions"]
            .as_array()
            .expect("actions")
            .iter()
            .any(|action| {
                action["outcome"] == "created"
                    && action["kind"] == "auto_task"
                    && action["path"]
                        .as_str()
                        .is_some_and(|path| path.ends_with("code-review.yaml"))
            })
    );
    assert!(
        !missing.exists(),
        "--check must not create the missing file"
    );
    assert_eq!(read(&registry), registry_before);
    assert_eq!(read(&identity), identity_before);
    assert_eq!(read_optional(repo.join(".gitignore")), gitignore_before);

    let applied = orbit(&repo, home.path())
        .args(["workspace", "sync", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let applied: Value = serde_json::from_slice(&applied).expect("parse apply JSON");
    let actions = applied["actions"].as_array().expect("actions");
    assert!(actions.iter().any(|action| {
        action["outcome"] == "preserved"
            && action["path"] == locally_modified.to_string_lossy().as_ref()
    }));
    assert!(actions.iter().any(|action| {
        action["outcome"] == "preserved" && action["path"] == collision.to_string_lossy().as_ref()
    }));
    assert!(
        missing.exists(),
        "apply creates the missing shipped definition"
    );
    assert_eq!(
        std::fs::read_to_string(&locally_modified).expect("read local edit"),
        local_body
    );
    assert_eq!(
        std::fs::read_to_string(&collision).expect("read collision"),
        collision_body
    );
    assert_eq!(read(&registry), registry_before);
    assert_eq!(read(&identity), identity_before);
    assert_eq!(read_optional(repo.join(".gitignore")), gitignore_before);

    let managed_after = read(&missing);
    orbit(&repo, home.path())
        .args(["workspace", "sync", "--check", "--json"])
        .assert()
        .success();
    assert_eq!(
        read(&missing),
        managed_after,
        "second run is byte-for-byte inert"
    );
}

#[test]
fn workspace_sync_outside_registered_workspace_fails_before_writing() {
    let parent = tempdir().expect("parent tempdir");
    let home = parent.path().join("home");
    let repo = home.join("outside-workspace");
    let uninitialized_root = home.join("uninitialized-root");
    std::fs::create_dir_all(parent.path().join(".git")).expect("create parent repo");
    std::fs::create_dir_all(repo.join(".git")).expect("create outside repo");
    std::fs::create_dir_all(&uninitialized_root).expect("create uninitialized root");
    write_machine_identity(&home);
    orbit(parent.path(), &home)
        .args(["workspace", "init", "--name", "initialized-parent"])
        .assert()
        .success();
    let parent_state = read(parent.path().join(".orbit/config.yaml"));
    let before: Vec<_> = std::fs::read_dir(&repo).expect("read empty repo").collect();
    orbit(&repo, &home)
        .args([
            "workspace",
            "sync",
            "--root",
            uninitialized_root.to_str().expect("utf8 root"),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("orbit workspace init"));
    let after: Vec<_> = std::fs::read_dir(&repo)
        .expect("reread empty repo")
        .collect();
    assert_eq!(after.len(), before.len());
    assert!(!repo.join(".orbit").exists());
    assert_eq!(read(parent.path().join(".orbit/config.yaml")), parent_state);
}

/// Exact shipped jobs whose manifest cannot be written: both the JSON report
/// and the human summary name the skipped provenance write, and neither
/// claims the catalog converged or its provenance was recorded.
#[cfg(unix)]
#[test]
fn workspace_sync_reports_a_denied_manifest_write_without_claiming_convergence() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempdir().expect("home tempdir");
    let repo = home.path().join("workspace");
    std::fs::create_dir_all(repo.join(".git")).expect("create workspace repo");
    write_machine_identity(home.path());
    orbit(&repo, home.path())
        .args(["workspace", "init"])
        .assert()
        .success();
    let jobs = home.path().join(".orbit/resources/jobs");
    let manifest = jobs.join(".orbit-managed-assets.json");
    std::fs::remove_file(&manifest).expect("drop the job manifest");

    std::fs::set_permissions(&jobs, std::fs::Permissions::from_mode(0o555))
        .expect("make the job catalog read-only");
    let json = orbit(&repo, home.path())
        .args(["workspace", "sync", "--json"])
        .output()
        .expect("run JSON sync");
    let human = orbit(&repo, home.path())
        .args(["workspace", "sync"])
        .output()
        .expect("run human sync");
    std::fs::set_permissions(&jobs, std::fs::Permissions::from_mode(0o755))
        .expect("restore job catalog permissions");

    assert!(json.status.success(), "{json:?}");
    assert!(!manifest.exists(), "the denied write left no manifest");
    let report: Value = serde_json::from_slice(&json.stdout).expect("parse sync JSON");
    assert!(
        report["warnings"]
            .as_array()
            .expect("warnings array")
            .iter()
            .any(|warning| warning.as_str().is_some_and(|warning| {
                warning.contains("could not write managed job asset manifest")
            })),
        "{report}"
    );
    let migrated: Vec<_> = report["actions"]
        .as_array()
        .expect("actions")
        .iter()
        .filter(|action| action["kind"] == "job" && action["outcome"] == "migrated")
        .collect();
    assert!(!migrated.is_empty(), "{report}");
    for action in migrated {
        assert!(
            action["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("not recorded")),
            "{action}"
        );
    }

    assert!(human.status.success(), "{human:?}");
    let stdout = String::from_utf8(human.stdout).expect("utf8 human output");
    assert!(
        stdout.contains("warning: could not write managed job asset manifest"),
        "{stdout}"
    );
    assert!(stdout.contains("not fully converged"), "{stdout}");
    assert!(!stdout.contains("managed artifacts converged"), "{stdout}");
    assert!(!stdout.contains("already converged"), "{stdout}");
}
