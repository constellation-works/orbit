#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Isolated-HOME coverage for invalid optional crew `effort` [ORB-12720].
//!
//! A mistyped `effort = "hard"` (task complexity vocabulary) must not fail
//! `orbit task list`. The warning goes to stderr through tracing so `--json`
//! stdout stays a parseable payload.

use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

fn orbit_at_home(work: &Path, home: &Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("RUST_LOG");
    command
}

fn run_orbit(work: &Path, home: &Path, args: &[&str], label: &str) -> Output {
    let output = orbit_at_home(work, home)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run {label}: {error}"));
    assert!(
        output.status.success(),
        "{label} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn inject_invalid_astra_effort(config_path: &Path) {
    let body = fs::read_to_string(config_path).unwrap_or_default();
    let updated = if body.contains("[crews.astra]") {
        body.replacen("[crews.astra]", "[crews.astra]\neffort = \"hard\"", 1)
    } else {
        let mut body = body;
        if !body.contains("[workflow]") {
            body.push_str("\n[workflow]\ndefault_crew = \"astra\"\n");
        }
        body.push_str(
            "\n[crews.astra]\nmodel = \"gpt-6-astra\"\nprovider = \"codex\"\neffort = \"hard\"\n",
        );
        body
    };
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).expect("create config dir");
    }
    fs::write(config_path, updated).expect("write invalid optional effort");
}

#[test]
fn task_list_succeeds_when_astra_effort_is_hard() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("fixture home");
    fs::create_dir_all(work.join(".git")).expect("fixture work repo");

    run_orbit(
        &work,
        &home,
        &["workspace", "init", "--name", "effort-admission"],
        "initialize workspace",
    );

    let workspace_config = work.join(".orbit").join("config.toml");
    let global_config = home.join(".orbit").join("config.toml");
    let config_path = if workspace_config.exists() {
        workspace_config
    } else {
        global_config
    };
    inject_invalid_astra_effort(&config_path);

    let output = run_orbit(
        &work,
        &home,
        &["task", "list", "--json"],
        "task list with invalid optional effort",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let parsed: Value = serde_json::from_str(&stdout).unwrap_or_else(|error| {
        panic!("--json stdout must stay parseable JSON ({error}): {stdout}")
    });
    assert!(
        parsed.is_array() || parsed.get("tasks").is_some(),
        "task list JSON payload: {parsed}"
    );
    assert!(
        !stdout.contains("ignoring"),
        "--json stdout must not carry the warning: {stdout}"
    );
    assert!(
        stderr.contains("ignoring [crews.astra].effort"),
        "warning must name the ignored property on stderr: {stderr}"
    );
    assert!(
        stderr.contains("hard"),
        "warning must name the offending value: {stderr}"
    );
    assert!(
        stderr.contains("low, medium, high, xhigh, max") || stderr.contains("accepted"),
        "warning must name accepted values: {stderr}"
    );
}
