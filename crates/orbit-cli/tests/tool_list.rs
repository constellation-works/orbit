#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use predicates::prelude::*;
use tempfile::tempdir;

const INACTIVE_TOOL_NAMES: &[&str] = &[
    // ORB-10798: auto-task authoring is human/admin work on the CLI surface.
    "orbit.auto_task.add",
    "orbit.auto_task.show",
    "orbit.auto_task.toggle",
    "orbit.auto_task.update",
    "orbit.friction.show",
    "orbit.friction.tags",
    "orbit.docs.index",
    "orbit.docs.migrate",
    "orbit.docs.add",
    "orbit.docs.list",
    "orbit.docs.show",
    "orbit.task.locks",
    "orbit.task.locks.release",
    "orbit.task.locks.reserve",
    "orbit.semantic.index",
    "orbit.semantic.install",
    "orbit.semantic.stats",
    "orbit.friction.stats",
];

/// An `orbit` invocation pinned to the fixture's own home, with no inherited
/// authority.
///
/// ORB-11300: clearing `ORBIT_ROOT` alone left the inherited
/// `ORBIT_REGISTRY_ROOT`/`ORBIT_WORKSPACE` pair in place, and that pair
/// outranks `HOME` — a suite launched from inside a managed Orbit run listed
/// and initialized against the live workspace instead of this temp one.
fn orbit_at_home(work: &std::path::Path, home: &std::path::Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}

#[test]
fn tool_list_all_shows_inactive_lock_reservation_with_required_input_shape() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");

    orbit_at_home(&work, &home)
        // `--format table` because `assert_cmd` captures through a pipe, where
        // `auto` resolves to the plain form and suppresses the header
        // (`specs/output-modes.md` §2). The STATUS *column* is what this test
        // is about, and naming the mode is how a piped caller asks for it.
        .args(["tool", "list", "--all", "--format", "table"])
        .assert()
        .success()
        .stdout(predicate::str::contains("orbit.task.locks.reserve"))
        .stdout(predicate::str::contains("STATUS"))
        .stdout(predicate::str::contains("inactive"))
        .stdout(predicate::str::contains(
            "Exactly one of `task_ids` or `files`",
        ));
}

#[test]
fn tool_list_json_hides_inactive_tools_by_default() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");

    let output = orbit_at_home(&work, &home)
        .args(["tool", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let tools: Vec<serde_json::Value> = serde_json::from_slice(&output).expect("tool list JSON");
    let names = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name").to_string())
        .collect::<BTreeSet<_>>();

    for inactive in INACTIVE_TOOL_NAMES {
        assert!(
            !names.contains(*inactive),
            "inactive tool must not be visible through default `orbit tool list`: {inactive}"
        );
    }
}

#[test]
fn tool_list_json_all_includes_inactive_tools_with_status() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");

    let output = orbit_at_home(&work, &home)
        .args(["tool", "list", "--json", "--all"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let tools: Vec<serde_json::Value> = serde_json::from_slice(&output).expect("tool list JSON");

    for inactive in INACTIVE_TOOL_NAMES {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == *inactive)
            .unwrap_or_else(|| panic!("inactive tool missing from --all: {inactive}"));
        assert_eq!(tool["status"], "inactive");
        assert_eq!(tool["active"], false);
    }
}

#[test]
fn tool_list_json_all_includes_parameter_schema_for_inactive_tools() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");

    let output = orbit_at_home(&work, &home)
        .args(["tool", "list", "--json", "--all"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let tools: Vec<serde_json::Value> = serde_json::from_slice(&output).expect("tool list JSON");
    let mint = tools
        .iter()
        .find(|tool| tool["name"] == "orbit.auto_task.mint")
        .expect("mint tool");
    assert_eq!(mint["status"], "active");
    let reserve = tools
        .iter()
        .find(|tool| tool["name"] == "orbit.task.locks.reserve")
        .expect("reserve tool");
    let parameters = reserve["parameters"].as_array().expect("parameters array");
    assert!(parameters.iter().any(|param| {
        param["name"] == "task_ids"
            && param["param_type"] == "string_list"
            && param["required"] == false
    }));
    assert!(parameters.iter().any(|param| {
        param["name"] == "files"
            && param["param_type"] == "string_list"
            && param["required"] == false
    }));
}

#[test]
fn tool_list_json_includes_task_show_context_parameters() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");

    let output = orbit_at_home(&work, &home)
        .args(["tool", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let tools: Vec<serde_json::Value> = serde_json::from_slice(&output).expect("tool list JSON");
    let task_show = tools
        .iter()
        .find(|tool| tool["name"] == "orbit.task.show")
        .expect("task show tool");
    let parameters = task_show["parameters"]
        .as_array()
        .expect("parameters array");
    assert!(parameters.iter().any(|param| {
        param["name"] == "with_context"
            && param["param_type"] == "boolean"
            && param["required"] == false
    }));
    assert!(parameters.iter().any(|param| {
        param["name"] == "max_docs"
            && param["param_type"] == "integer"
            && param["required"] == false
    }));
    assert!(parameters.iter().any(|param| {
        param["name"] == "workspace"
            && param["param_type"] == "string"
            && param["required"] == false
            && param["description"]
                .as_str()
                .is_some_and(|description| description.contains("resolved globally by default"))
    }));
}

#[test]
fn tool_show_displays_lock_reservation_shapes() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");

    orbit_at_home(&work, &home)
        .args(["tool", "show", "orbit.task.locks.reserve"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Status:"))
        .stdout(predicate::str::contains("inactive"))
        .stdout(predicate::str::contains("task_ids"))
        .stdout(predicate::str::contains("files"))
        .stdout(predicate::str::contains("optional"))
        .stdout(predicate::str::contains(
            "Exactly one of `task_ids` or `files`",
        ));
}

#[test]
fn tool_run_rejects_inactive_tools() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");

    orbit_at_home(&work, &home)
        .args([
            "tool",
            "run",
            "orbit.semantic.uninstall",
            "--input",
            "{\"model\":\"codex\"}",
        ])
        .assert()
        .failure()
        // The JSON error payload moved to stderr [ORB-10570]; stdout carries
        // the payload and nothing else.
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("inactive"));
}
