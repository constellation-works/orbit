//! Exercise agent input errors through the built `orbit tool run` entry point.

use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;

use crate::{git_repo, tool_input_hint_cases};

#[test]
fn tool_run_input_misses_return_actionable_suggestions() {
    let root = tempfile::tempdir().expect("fixture");
    let home = root.path().join("home");
    let work = root.path().join("work");
    std::fs::create_dir_all(&home).expect("fixture home");
    git_repo::init(&work);
    let command = || {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&work)
            .env("HOME", &home)
            .env("USERPROFILE", &home);
        command
    };
    command()
        .args(["workspace", "init", "--name", "input-hints"])
        .assert()
        .success();

    for case in tool_input_hint_cases::cases() {
        let output = command()
            .args([
                "tool",
                "run",
                case.tool,
                "--input",
                &case.input.to_string(),
                "--format",
                "json",
            ])
            .assert()
            .failure()
            .get_output()
            .clone();
        assert!(output.stdout.is_empty(), "{output:?}");
        let error: Value = serde_json::from_slice(&output.stderr).expect("JSON tool error");
        tool_input_hint_cases::assert_hint(&case, &error, "error");
        if case.tool == "orbit.search" {
            for suggestion in case.suggestions {
                let mut corrected = case.input.clone();
                corrected["status"] = suggestion.into();
                command()
                    .args(["tool", "run", case.tool, "--input", &corrected.to_string()])
                    .assert()
                    .success();
            }
        }
    }
}

/// Malformed `--input` and an unreadable `--input-file` fail before workspace
/// bootstrap, including from a directory that is not an Orbit workspace.
#[test]
fn tool_run_unusable_input_from_an_uninitialized_directory() {
    let root = tempfile::tempdir_in(non_workspace_parent()).expect("fixture");
    let home = root.path().join("home");
    let work = root.path().join("work");
    std::fs::create_dir_all(&home).expect("fixture home");
    std::fs::create_dir_all(&work).expect("fixture work");
    let missing = root.path().join("missing-input.json");

    let json_error = run_tool(&work, &home, &["orbit.task.show", "--input", "not-json"]);
    assert!(
        json_error.contains("invalid JSON input"),
        "the error must identify the invalid JSON: {json_error}"
    );
    assert_input_error_precedes_bootstrap(&json_error);

    let file_error = run_tool(
        &work,
        &home,
        &[
            "orbit.task.list",
            "--input-file",
            missing.to_str().expect("missing path"),
        ],
    );
    assert!(
        file_error.contains("cannot read input file"),
        "the error must name the unreadable input file: {file_error}"
    );
    assert!(
        file_error.contains(&missing.display().to_string()),
        "the error must include the missing path: {file_error}"
    );
    assert_input_error_precedes_bootstrap(&file_error);
}

/// A checkout with a pending layout migration stays unmigrated when tool input
/// is already unusable. A later well-formed call still opens the runtime.
#[test]
fn tool_run_unusable_input_does_not_migrate_a_pending_layout() {
    let root = tempfile::tempdir_in(non_workspace_parent()).expect("fixture");
    let home = root.path().join("home");
    let work = root.path().join("work");
    std::fs::create_dir_all(&home).expect("fixture home");
    git_repo::init(&work);
    let mut init = orbit_command(&work, &home);
    init.args(["workspace", "init", "--name", "unusable-input"])
        .assert()
        .success();

    let marker = work.join(".orbit/state/layout.version");
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent).expect("layout marker directory");
    }
    // Workspace init can leave the marker absent (version 0). Record version 1
    // so a later runtime open has a pending migration to apply.
    std::fs::write(&marker, "1\n").expect("rewind layout marker");

    let json_error = run_tool(&work, &home, &["orbit.task.show", "--input", "not-json"]);
    assert!(json_error.contains("invalid JSON input"), "{json_error}");
    assert_input_error_precedes_bootstrap(&json_error);
    assert_eq!(
        std::fs::read_to_string(&marker).expect("marker after invalid JSON"),
        "1\n",
        "invalid JSON must not apply the pending layout migration"
    );

    let missing = root.path().join("missing-input.json");
    let file_error = run_tool(
        &work,
        &home,
        &[
            "orbit.task.list",
            "--input-file",
            missing.to_str().expect("missing path"),
        ],
    );
    assert!(
        file_error.contains("cannot read input file"),
        "{file_error}"
    );
    assert_input_error_precedes_bootstrap(&file_error);
    assert_eq!(
        std::fs::read_to_string(&marker).expect("marker after missing input file"),
        "1\n",
        "an unreadable input file must not apply the pending layout migration"
    );

    let mut listed = orbit_command(&work, &home);
    listed
        .args(["tool", "run", "orbit.task.list", "--input", "{}"])
        .assert()
        .success();
    assert_ne!(
        std::fs::read_to_string(&marker).expect("marker after a well-formed call"),
        "1\n",
        "a well-formed call must still open the runtime and apply the pending migration"
    );
}

fn non_workspace_parent() -> PathBuf {
    // A managed run points TMPDIR at the checkout, and walk-up from there finds
    // that checkout's workspace. `/tmp` is not an ancestor of this repo.
    PathBuf::from("/tmp")
}

fn orbit_command(work: &Path, home: &Path) -> assert_cmd::Command {
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

fn run_tool(work: &Path, home: &Path, args: &[&str]) -> String {
    let mut command = orbit_command(work, home);
    let output = command
        .arg("tool")
        .arg("run")
        .args(args)
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(
        output.stdout.is_empty(),
        "tool errors stay on stderr: {output:?}"
    );
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON tool error");
    error["error"].as_str().expect("error string").to_string()
}

fn assert_input_error_precedes_bootstrap(error: &str) {
    let lowered = error.to_ascii_lowercase();
    assert!(
        !lowered.contains("orbit init"),
        "workspace bootstrap must not replace the input error: {error}"
    );
    assert!(
        !lowered.contains("no workspace") && !lowered.contains("no initialized"),
        "workspace bootstrap must not replace the input error: {error}"
    );
}
