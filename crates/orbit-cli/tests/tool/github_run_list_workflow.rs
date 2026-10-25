#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! `github.run.list` maps a `gh` workflow-resolution miss to invalid input.
//!
//! A `gh run list --workflow` value that matches more than one workflow, or
//! no workflow at all, means the caller did not identify a workflow. The tool
//! refuses it as invalid input with file-name guidance instead of reporting
//! an execution failure, and it never guesses a match [ORB-18196].

use std::path::PathBuf;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;

struct Lane {
    _temp: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
    bin: PathBuf,
}

impl Lane {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("fixture tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        let bin = temp.path().join("bin");
        for dir in [&home, &work, &bin] {
            std::fs::create_dir_all(dir).expect("create fixture dir");
        }
        crate::git_repo::seal_lookup_boundary(&work);
        Self {
            _temp: temp,
            home,
            work,
            bin,
        }
    }

    /// A stand-in `gh` that reproduces the two workflow-resolution failures
    /// without credentials or network: an ambiguous value, an unknown value,
    /// and one unrelated failure that must stay an execution failure.
    fn install_gh(&self) {
        let script = r#"#!/bin/sh
args="$*"
case "$args" in
*"codeql.yml"*)
printf '[{"databaseId":7,"number":3,"workflowName":"codeql","displayTitle":"fixture","status":"completed","conclusion":"success","event":"push","headBranch":"main","headSha":"abc123","createdAt":"2026-10-24T00:00:00Z","startedAt":"2026-10-24T00:00:00Z","updatedAt":"2026-10-24T00:01:00Z","url":"https://example.invalid/run/7"}]\n'
exit 0
;;
*"codeql"*)
printf 'could not resolve to a unique workflow; found: codeql codeql.yml\n' >&2
exit 1
;;
*"Windows Compile Check"*)
printf 'could not find any workflows named Windows Compile Check\n' >&2
exit 1
;;
*"boom"*)
printf 'HTTP 500: internal server error\n' >&2
exit 1
;;
*)
printf '[]\n'
exit 0
;;
esac
"#;
        let gh = self.bin.join("gh");
        std::fs::write(&gh, script).expect("write gh stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&gh).expect("stub metadata").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&gh, permissions).expect("mark stub executable");
        }
    }

    fn run_list(&self, input: &str) -> std::process::Output {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", &self.bin)
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .args(["tool", "run", "github.run.list", "--input", input])
            .output()
            .expect("run github.run.list")
    }

    fn tool_list_json(&self) -> Value {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let output = command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", &self.bin)
            .args(["tool", "list", "--json"])
            .output()
            .expect("list tools");
        assert!(
            output.status.success(),
            "tool list must succeed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("tool list JSON")
    }
}

fn tool_error(output: &std::process::Output) -> Value {
    assert!(
        !output.status.success(),
        "the miss must fail: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "tool errors stay on stderr: {output:?}"
    );
    serde_json::from_slice(&output.stderr).expect("JSON tool error")
}

#[test]
fn ambiguous_workflow_is_invalid_input_that_names_the_matches() {
    let lane = Lane::new();
    lane.install_gh();

    let error = tool_error(&lane.run_list(r#"{"workflow":"codeql"}"#));
    assert_eq!(error["code"], "invalid_input", "{error}");
    let message = error["error"].as_str().expect("error string");
    assert!(message.contains("invalid input:"), "{message}");
    assert!(!message.contains("execution failed"), "{message}");
    for phrase in [
        "workflow file name",
        "numeric workflow id",
        "job display name is not a workflow name",
    ] {
        assert!(
            message.to_lowercase().contains(phrase),
            "ambiguous message must tell callers {phrase}: {message}"
        );
    }
    for name in ["codeql", "codeql.yml"] {
        assert!(message.contains(name), "{message}");
    }
    assert!(
        message.contains("does not choose one"),
        "the tool must not guess a match: {message}"
    );
}

#[test]
fn unknown_workflow_is_invalid_input_not_an_execution_failure() {
    let lane = Lane::new();
    lane.install_gh();

    let error = tool_error(&lane.run_list(r#"{"workflow":"Windows Compile Check"}"#));
    assert_eq!(error["code"], "invalid_input", "{error}");
    let message = error["error"].as_str().expect("error string");
    assert!(message.contains("invalid input:"), "{message}");
    assert!(!message.contains("execution failed"), "{message}");
    for phrase in [
        "workflow file name",
        "numeric workflow id",
        "job display name is not a workflow name",
    ] {
        assert!(
            message.to_lowercase().contains(phrase),
            "unknown-workflow message must tell callers {phrase}: {message}"
        );
    }
}

#[test]
fn unrelated_gh_failure_stays_an_execution_failure() {
    let lane = Lane::new();
    lane.install_gh();

    let error = tool_error(&lane.run_list(r#"{"workflow":"boom"}"#));
    assert_eq!(error["code"], "execution_failed", "{error}");
    let message = error["error"].as_str().expect("error string");
    assert!(message.contains("execution failed:"), "{message}");
    assert!(
        message.contains("gh run list failed:"),
        "other failures keep the shared execution-error path: {message}"
    );
    assert!(!message.contains("invalid input:"), "{message}");
}

#[test]
fn workflow_parameter_points_at_file_names_not_job_names() {
    let lane = Lane::new();
    lane.install_gh();

    let listed = lane.tool_list_json();
    let tool = listed
        .as_array()
        .expect("tool list array")
        .iter()
        .find(|tool| tool["name"] == "github.run.list")
        .expect("github.run.list is listed");
    let workflow = tool["parameters"]
        .as_array()
        .expect("parameters array")
        .iter()
        .find(|param| param["name"] == "workflow")
        .expect("workflow parameter");
    let description = workflow["description"].as_str().expect("description");
    for phrase in [
        "workflow file name",
        "numeric workflow id",
        "job display name",
    ] {
        assert!(
            description.to_lowercase().contains(phrase),
            "workflow description must tell callers {phrase}: {description}"
        );
    }
    assert!(
        description.to_lowercase().contains("not accepted")
            || description.to_lowercase().contains("not a workflow"),
        "workflow description must say a job name is not accepted: {description}"
    );
}
