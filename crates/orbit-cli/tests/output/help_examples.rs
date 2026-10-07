//! Behavior checks for flags in CLI help examples.

use std::fs;

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

    let mut help_command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        help_command.env_remove(name);
    });
    let help_output = help_command
        .current_dir(&work)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .args(["run", "--help"])
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
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let output = command
            .current_dir(&work)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .args(["run", "job"])
            .args(parse_example_arguments(example))
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
