//! Exercise agent input errors through the built `orbit tool run` entry point.

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
