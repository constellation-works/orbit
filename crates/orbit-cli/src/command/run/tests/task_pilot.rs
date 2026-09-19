use clap::{Parser, error::ErrorKind};
use orbit_core::OrbitRuntime;
use serde_json::json;

use crate::command::{Cli, CommandOutput, Commands, Execute, operation::RuntimeNeed};

use super::super::RunSubcommand;
use super::super::task_pilot::{TaskPilotCommand, build_task_pilot_input};

fn parse_run(args: &[&str]) -> super::super::RunCommand {
    let cli = Cli::parse_from(args);
    match cli.command {
        Commands::Run(command) => command,
        _ => panic!("expected run command"),
    }
}

fn assert_cli_rejects(args: &[&str], kind: ErrorKind, expected: &str) {
    let error = match Cli::try_parse_from(args.iter().copied()) {
        Ok(_) => panic!("form should be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), kind, "{error}");
    let message = error.to_string();
    assert!(message.contains(expected), "{message}");
}

#[test]
fn build_task_pilot_input_omits_optional_keys_when_unset() {
    let input = build_task_pilot_input(&[], None, None, None).expect("input builds");
    assert_eq!(input, json!({}));
    assert!(input.get("task_ids").is_none());
    assert!(input.get("base_branch").is_none());
    assert!(input.get("max_tasks").is_none());
    assert!(input.get("max_partition_size").is_none());
    assert!(input.get("promotion_authorized").is_none());
    assert!(input.get("ci_sweep_filing").is_none());
    assert!(input.get("state_automation").is_none());
    assert!(input.get("source_revision").is_none());
    assert!(input.get("crew").is_none());
}

#[test]
fn build_task_pilot_input_writes_only_provided_overrides() {
    let ids = vec!["TASK-1".to_string(), "TASK-2".to_string()];
    let input =
        build_task_pilot_input(&ids, Some("agent-main"), Some(10), Some(3)).expect("input builds");
    assert_eq!(input["task_ids"], json!(["TASK-1", "TASK-2"]));
    assert_eq!(input["base_branch"], "agent-main");
    assert_eq!(input["max_tasks"], 10);
    assert_eq!(input["max_partition_size"], 3);
    assert!(input.get("promotion_authorized").is_none());
    assert!(input.get("crew").is_none());
}

#[test]
fn build_task_pilot_input_rejects_empty_and_duplicate_ids() {
    let dup = vec!["TASK-1".to_string(), "TASK-1".to_string()];
    let error = build_task_pilot_input(&dup, None, None, None).expect_err("duplicate");
    assert!(error.to_string().contains("duplicate task id"), "{error}");

    let blank = vec!["  ".to_string()];
    let error = build_task_pilot_input(&blank, None, None, None).expect_err("blank");
    assert!(error.to_string().contains("must not be empty"), "{error}");
}

#[test]
fn parses_task_pilot_automatic_discovery_defaults() {
    let command = parse_run(&["orbit", "run", "task-pilot"]);
    match command.command {
        RunSubcommand::TaskPilot(args) => {
            assert!(args.task_ids.is_empty());
            assert_eq!(args.base_branch, None);
            assert_eq!(args.max_tasks, None);
            assert_eq!(args.max_partition_size, None);
            assert!(!args.json);
            assert!(!args.wait);
        }
        _ => panic!("expected task-pilot"),
    }
}

#[test]
fn parses_task_pilot_explicit_ids_and_optional_flags() {
    let command = parse_run(&[
        "orbit",
        "run",
        "task-pilot",
        "TASK-1",
        "TASK-2",
        "--base-branch",
        "agent-main",
        "--max-tasks",
        "10",
        "--max-partition-size",
        "3",
        "--wait",
        "--json",
    ]);
    match command.command {
        RunSubcommand::TaskPilot(args) => {
            assert_eq!(args.task_ids, vec!["TASK-1", "TASK-2"]);
            assert_eq!(args.base_branch.as_deref(), Some("agent-main"));
            assert_eq!(args.max_tasks, Some(10));
            assert_eq!(args.max_partition_size, Some(3));
            assert!(args.wait);
            assert!(args.json);
        }
        _ => panic!("expected task-pilot"),
    }
}

#[test]
fn rejects_reserved_task_pilot_inputs() {
    for (flag, value) in [
        ("--promotion-authorized", "true"),
        ("--ci-sweep-filing", "x"),
        ("--state-automation", "x"),
        ("--source-revision", "abc"),
        ("--crew", "luna"),
    ] {
        assert_cli_rejects(
            &["orbit", "run", "task-pilot", flag, value],
            ErrorKind::UnknownArgument,
            flag,
        );
    }
}

#[test]
fn task_pilot_is_a_required_workflow_operation() {
    let operation = Cli::parse_from(["orbit", "run", "task-pilot"])
        .command
        .operation();
    assert_eq!(operation.runtime_need, RuntimeNeed::Required);
    let meta = operation.audit_meta.expect("run task-pilot is audited");
    assert_eq!(meta.command, "run");
    assert_eq!(meta.subcommand.as_deref(), Some("task-pilot"));
    assert_eq!(meta.target_type.as_deref(), Some("workflow"));
    assert_eq!(meta.target_id.as_deref(), Some("task-pilot"));
}

#[test]
fn task_pilot_dispatch_submits_task_pilot_pipeline_with_workflow_payload_shape() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let jobs_dir = runtime.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    std::fs::write(
        jobs_dir.join("task_pilot_pipeline.yaml"),
        r#"schemaVersion: 2
kind: Job
metadata:
  name: task_pilot_pipeline
spec:
  state: enabled
  kind: workflow
  steps:
    - id: marker
      spec:
        type: deterministic
        action: sleep
        config:
          seconds: 0
"#,
    )
    .expect("write task_pilot_pipeline fixture");

    let output = TaskPilotCommand {
        task_ids: Vec::new(),
        base_branch: None,
        max_tasks: None,
        max_partition_size: None,
        json: true,
        wait: false,
    }
    .execute(&runtime)
    .expect("dispatch");
    let CommandOutput::Payload(payload) = output else {
        panic!("task-pilot should produce a payload");
    };
    let (document, _) = payload.into_view();
    assert_eq!(document["workflow"], "task-pilot");
    assert_eq!(document["job_id"], "task_pilot_pipeline");
    assert!(
        document["run_id"].as_str().is_some_and(|id| !id.is_empty()),
        "expected a run id: {document}"
    );
    assert!(
        matches!(document["state"].as_str(), Some("submitted" | "queued")),
        "expected submitted/queued: {document}"
    );
    for key in [
        "workflow",
        "job_id",
        "run_id",
        "state",
        "attempt",
        "error_code",
        "error_message",
    ] {
        assert!(document.get(key).is_some(), "missing {key} in {document}");
    }
}
