//! Behavior checks for flags in CLI help examples.

use std::fs;
use std::path::Path;

use crate::task_workspace::{TaskSeed, TestWorkspace};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use tempfile::tempdir;

#[test]
fn run_job_help_examples_are_accepted_by_the_documented_command() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("fixture home");
    fs::create_dir_all(&work).expect("fixture work");

    for surface in [&["run"][..], &["run", "job"][..]] {
        let mut help_command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            help_command.env_remove(name);
        });
        let help_output = help_command
            .current_dir(&work)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .args(surface)
            .arg("--help")
            .output()
            .expect("run orbit run --help");
        assert!(
            help_output.status.success(),
            "orbit run --help failed: {}",
            String::from_utf8_lossy(&help_output.stderr)
        );
        let help = String::from_utf8(help_output.stdout).expect("help is UTF-8");
        let examples: Vec<_> = help
            .lines()
            .filter_map(|line| line.trim().strip_prefix("orbit run job "))
            .collect();
        assert!(
            !examples.is_empty(),
            "orbit run --help should contain a run-job usage example"
        );

        for example in examples {
            let arguments = parse_example_arguments(example);
            let job = &arguments[0];
            if Path::new(job).extension().is_some_and(|ext| ext == "yaml") {
                let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../..")
                    .join(job);
                let yaml = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("documented job {job} must exist: {error}"));
                let document: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("job YAML");
                let _: orbit_types::workflow::JobV2 =
                    serde_yaml::from_value(document["spec"].clone()).expect("documented job spec");
            }
            let mut command = cargo_bin_cmd!("orbit");
            test_env::clear_inherited_authority(|name| {
                command.env_remove(name);
            });
            let output = command
                .current_dir(&work)
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .args(["run", "job"])
                .args(&arguments)
                .arg("--help")
                .output()
                .expect("parse documented run-job example");
            assert!(
                output.status.success(),
                "orbit run job rejected help example `{example}`: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn run_cancel_help_synopsis_reaches_run_lookup() {
    if crate::isolated_test::run_in_child(
        "help_examples::run_cancel_help_synopsis_reaches_run_lookup",
    ) {
        return;
    }
    let fixture = TestWorkspace::new("help-examples", TaskSeed::new("unused"));
    let output = fixture.run(&["run", "--help"], "run help");
    let help = String::from_utf8(output.stdout).expect("UTF-8 help");
    let synopsis = help
        .lines()
        .find_map(|line| line.trim().strip_prefix("orbit run cancel "))
        .expect("cancel synopsis");
    let arguments: Vec<_> = synopsis
        .split_whitespace()
        .map(|token| {
            if token == "<run_id>" {
                "jrun-help-example-missing"
            } else {
                token
            }
        })
        .collect();
    let mut args = vec!["run", "cancel"];
    args.extend(arguments);
    args.push("--json");
    let output = fixture.run_raw(&args);
    assert!(!output.status.success(), "the fixture run does not exist");
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).expect("JSON error");
    assert_eq!(
        error["code"], "job_run_not_found",
        "the documented synopsis must pass confirmation and reach run lookup: {error}"
    );
}

fn parse_example_arguments(example: &str) -> Vec<String> {
    example
        .split_whitespace()
        .filter_map(|token| {
            let token = token.trim_start_matches('[').trim_end_matches(']');
            if token == "..." {
                None
            } else if token == "<job_id>" {
                Some("task_auto_pipeline".to_owned())
            } else {
                Some(token.to_owned())
            }
        })
        .collect()
}
