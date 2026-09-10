#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::tempdir;

/// An `orbit` invocation with no inherited authority.
///
/// ORB-11300: every fixture here writes durable host identity and workspace
/// registry state. Clearing `ORBIT_ROOT` alone left the inherited
/// `ORBIT_REGISTRY_ROOT`/`ORBIT_WORKSPACE` pair in place, which outranks
/// `HOME` and routes writes at the live registry.
fn orbit(cwd: &std::path::Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command.current_dir(cwd);
    command
}

/// The same isolation, with `HOME` pinned at the fixture's own global root.
fn orbit_at_home(cwd: &std::path::Path, home: &std::path::Path) -> assert_cmd::Command {
    let mut command = orbit(cwd);
    command.env("HOME", home).env("USERPROFILE", home);
    command
}

fn initialized_workspace() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = home.join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(work.join(".git")).expect("create work repo");

    orbit_at_home(&work, &home)
        .args([
            "init",
            "--non-interactive",
            "--host-name",
            "local",
            "--task-prefix",
            "DE",
        ])
        .assert()
        .success();
    orbit_at_home(&work, &home)
        .args(["workspace", "init", "--name", "local-workspace"])
        .assert()
        .success();
    (temp, home, work)
}

#[test]
fn fleet_administration_and_inventory_are_absent_from_cli_and_tool_registry() {
    let (_temp, home, work) = initialized_workspace();

    for subcommand in ["register", "list", "retire"] {
        orbit_at_home(&work, &home)
            .args(["host", subcommand])
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }

    orbit_at_home(&work, &home)
        .args(["workspace", "link"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));

    orbit_at_home(&work, &home)
        .args(["tool", "list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("orbit.host.").not())
        .stdout(predicate::str::contains("register-spoke").not());
}

#[test]
fn rename_updates_host_and_local_owner_names_without_changing_stable_identity() {
    let (_temp, home, work) = initialized_workspace();
    let identity_path = home.join(".orbit/host.toml");
    let registry_path = home.join(".orbit/workspaces.json");
    let before: toml::Value =
        toml::from_str(&std::fs::read_to_string(&identity_path).expect("read host identity"))
            .expect("parse host identity");
    let machine_id = before["machine_id"]
        .as_str()
        .expect("machine id")
        .to_string();
    assert_eq!(before["task_prefix"].as_str(), Some("DE"));

    orbit_at_home(&work, &home)
        .args(["host", "rename", "local", "renamed"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 local workspace owner record"));

    let after: toml::Value =
        toml::from_str(&std::fs::read_to_string(&identity_path).expect("read renamed identity"))
            .expect("parse renamed identity");
    assert_eq!(after["machine_id"].as_str(), Some(machine_id.as_str()));
    assert_eq!(after["host_id"].as_str(), Some("renamed"));
    assert_eq!(after["task_prefix"].as_str(), Some("DE"));

    let registry: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&registry_path).expect("read workspace registry"),
    )
    .expect("parse workspace registry");
    assert_eq!(registry["owner_host_ids"][&machine_id], "renamed");
    assert_eq!(
        registry["workspaces"][0]["owner_machine_id"], machine_id,
        "rename must not change stable owner binding"
    );

    orbit_at_home(&work, &home)
        .args(["host", "rename", "local", "again"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "current host.toml names 'renamed'",
        ));
}

#[test]
fn show_reports_the_persisted_identity_outside_a_workspace_without_writing() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("orbit-root");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).expect("create outside directory");
    let root_arg = root.to_str().expect("utf8 root");
    std::fs::create_dir_all(&root).expect("create root");
    std::fs::write(
        root.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_0123456789abcdef0123456789abcdef\"\nhost_id = \"operator-host\"\ntask_prefix = \"DE\"\n",
    )
    .expect("write identity");

    let identity_path = root.join("host.toml");
    let before = std::fs::read(&identity_path).expect("read identity before show");
    let identity: toml::Value =
        toml::from_str(std::str::from_utf8(&before).expect("identity is utf8"))
            .expect("parse identity");
    let machine_id = identity["machine_id"]
        .as_str()
        .expect("machine id")
        .to_string();
    let human = orbit(&outside)
        .args(["--root", root_arg, "host", "show"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        String::from_utf8(human).expect("human output is utf8"),
        format!("machine_id: {machine_id}\nhost_id: operator-host\ntask_prefix: DE\n")
    );

    let json_output = orbit(&outside)
        .args(["--root", root_arg, "host", "show", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let shown: Value = serde_json::from_slice(&json_output).expect("valid host JSON");
    let object = shown.as_object().expect("host JSON object");
    assert_eq!(object.len(), 3);
    assert_eq!(object["machine_id"], machine_id);
    assert_eq!(object["host_id"], "operator-host");
    assert_eq!(object["task_prefix"], "DE");
    assert_eq!(
        std::fs::read(&identity_path).expect("read identity after show"),
        before
    );
}

#[test]
fn show_rejects_invalid_host_identities_without_replacing_them() {
    let cases = [
        ("absent", None, "no host identity"),
        (
            "malformed",
            Some("schema_version = [\n"),
            "invalid host identity",
        ),
        (
            "incomplete",
            Some("schema_version = 2\nhost_id = \"operator-host\"\n"),
            "incomplete",
        ),
        (
            "future",
            Some(
                "schema_version = 99\nmachine_id = \"hm_0123456789abcdef0123456789abcdef\"\nhost_id = \"operator-host\"\ntask_prefix = \"DE\"\n",
            ),
            "unsupported schema_version",
        ),
    ];

    for (name, contents, error_text) in cases {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join(name);
        std::fs::create_dir_all(&root).expect("create root");
        let identity_path = root.join("host.toml");
        if let Some(contents) = contents {
            std::fs::write(&identity_path, contents).expect("write identity fixture");
        }
        let before = std::fs::read(&identity_path).ok();

        orbit(temp.path())
            .args(["--root", root.to_str().expect("utf8 root"), "host", "show"])
            .assert()
            .failure()
            .stderr(predicate::str::contains(error_text));

        assert_eq!(std::fs::read(&identity_path).ok(), before);
    }
}

#[test]
fn host_show_is_listed_in_help() {
    orbit(std::path::Path::new("."))
        .args(["host", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("show"));
    orbit(std::path::Path::new("."))
        .args(["host", "show", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("machine_id"))
        .stdout(predicate::str::contains("host_id"))
        .stdout(predicate::str::contains("task_prefix"));
}
