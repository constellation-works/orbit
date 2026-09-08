#![allow(missing_docs)]

mod format;
mod job;
mod ship;
mod support;

// Content moved from inline #[cfg(test)] mod tests in run/mod.rs per ORB-00221.

use chrono::Utc;
use clap::{Parser, error::ErrorKind};
use orbit_core::runtime::run_audit::RunAuditEvent;
use orbit_core::{OrbitRuntime, V2AuditEventInsertParams};
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};

use crate::command::{Cli, CommandOutput, Commands, Execute};

use super::cancel::RunCancelArgs;
use super::*;

fn parse_run(args: &[&str]) -> RunCommand {
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
fn cancel_requires_confirmation_before_terminalizing_pending_run() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let store = runtime.sqlite_store().expect("store");
    let now = Utc::now();
    let run = JobRun {
        run_id: "jrun-confirm-test".to_string(),
        job_id: "cancel-test".to_string(),
        attempt: 1,
        state: JobRunState::Pending,
        scheduled_at: now,
        started_at: None,
        finished_at: None,
        duration_ms: None,
        created_at: now,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };
    store
        .upsert_job_run_for_workspace(&workspace_id, &run, None)
        .expect("insert pending run");

    let error = RunCancelArgs {
        run_id: run.run_id.clone(),
        json: false,
        confirm: false,
    }
    .execute(&runtime)
    .expect_err("unconfirmed cancellation must refuse");
    assert!(error.to_string().contains("--confirm"));
    assert_eq!(
        store
            .get_job_run_for_workspace(&workspace_id, &run.run_id)
            .expect("read pending run")
            .expect("pending run")
            .state,
        JobRunState::Pending
    );

    RunCancelArgs {
        run_id: run.run_id.clone(),
        json: false,
        confirm: true,
    }
    .execute(&runtime)
    .expect("confirmed cancellation");
    assert_eq!(
        store
            .get_job_run_for_workspace(&workspace_id, &run.run_id)
            .expect("read cancelled run")
            .expect("cancelled run")
            .state,
        JobRunState::Cancelled
    );

    RunCancelArgs {
        run_id: run.run_id.clone(),
        json: true,
        confirm: true,
    }
    .execute(&runtime)
    .expect("duplicate cancellation reports already-terminal success");
}

#[test]
fn parses_ship_auto_mode_defaults() {
    let command = parse_run(&["orbit", "run", "ship"]);
    match command.command {
        RunSubcommand::Ship(args) => {
            assert!(args.task_ids.is_empty());
            assert_eq!(args.mode, None);
            assert_eq!(args.base, None);
        }
        _ => panic!("expected ship"),
    }
}

#[test]
fn parses_workspace_auto_defaults() {
    let command = parse_run(&["orbit", "run", "auto"]);
    match command.command {
        RunSubcommand::Auto(args) => {
            assert!(!args.json);
            assert!(args.claim_token.is_none());
            // No window means one tick, the behavior every caller had before
            // `--for` existed.
            assert_eq!(args.for_duration, None);
            // No crew restriction is the pre-ORB-11242 behavior: every crew.
            assert!(args.allow_crew.is_empty());
            assert!(!args.stop);
        }
        _ => panic!("expected auto"),
    }
}

#[test]
fn parses_workspace_auto_stop() {
    let command = parse_run(&["orbit", "run", "auto", "--stop"]);
    match command.command {
        RunSubcommand::Auto(args) => {
            assert!(args.stop);
            assert_eq!(args.for_duration, None);
            assert!(!args.complete);
        }
        _ => panic!("expected auto"),
    }
}

#[test]
fn workspace_auto_stop_conflicts_with_start_flags() {
    for args in [
        ["orbit", "run", "auto", "--stop", "--for", "30m"].as_slice(),
        ["orbit", "run", "auto", "--stop", "--concurrency", "3"].as_slice(),
        ["orbit", "run", "auto", "--stop", "--complete"].as_slice(),
        ["orbit", "run", "auto", "--stop", "--allow-crew", "luna"].as_slice(),
    ] {
        assert_cli_rejects(args, ErrorKind::ArgumentConflict, "--stop");
    }
}

#[test]
fn auto_stop_with_no_coordinator_is_idle() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    super::auto::AutoCommand {
        low_complexity_crews: None,
        medium_complexity_crews: None,
        hard_complexity_crews: None,
        for_duration: None,
        concurrency: None,
        complete: false,
        allow_crew: Vec::new(),
        grant: None,
        json: true,
        claim_token: None,
        stop: true,
    }
    .execute(&runtime)
    .expect("idle stop succeeds");
}

#[test]
fn parses_workspace_auto_drain_window() {
    let command = parse_run(&["orbit", "run", "auto", "--for", "30m"]);
    match command.command {
        RunSubcommand::Auto(args) => {
            assert_eq!(args.for_duration.as_deref(), Some("30m"));
        }
        _ => panic!("expected auto"),
    }
}

/// [ORB-11242] Both spellings collect into one list, so an operator excluding
/// several crews mid-incident does not have to remember which form the flag
/// takes.
#[test]
fn parses_workspace_auto_crew_allowlist_repeated_and_comma_separated() {
    let command = parse_run(&[
        "orbit",
        "run",
        "auto",
        "--allow-crew",
        "opus,sonnet",
        "--allow-crew",
        "luna",
    ]);
    match command.command {
        RunSubcommand::Auto(args) => {
            assert_eq!(args.allow_crew, vec!["opus", "sonnet", "luna"]);
        }
        _ => panic!("expected auto"),
    }
}

#[test]
fn readiness_previews_the_same_crew_allowlist() {
    let command = parse_run(&["orbit", "run", "readiness", "--allow-crew", "opus,sonnet"]);
    match command.command {
        RunSubcommand::Readiness(args) => {
            assert_eq!(args.allow_crew, vec!["opus", "sonnet"]);
        }
        _ => panic!("expected readiness"),
    }
}

#[test]
fn parses_readiness_selection_and_json_projection() {
    let command = parse_run(&[
        "orbit",
        "run",
        "readiness",
        "TASK-123",
        "TASK-124",
        "--concurrency",
        "8",
        "--limit",
        "20",
        "--json",
    ]);
    match command.command {
        RunSubcommand::Readiness(args) => {
            assert_eq!(args.task_ids, vec!["TASK-123", "TASK-124"]);
            assert_eq!(args.concurrency, Some(8));
            assert_eq!(args.limit, 20);
            assert!(args.json);
            assert!(args.allow_crew.is_empty());
        }
        _ => panic!("expected readiness"),
    }
}

#[test]
fn parses_explicit_ship_defaults() {
    let command = parse_run(&["orbit", "run", "ship", "T1", "T2"]);
    match command.command {
        RunSubcommand::Ship(args) => {
            assert_eq!(args.task_ids, vec!["T1", "T2"]);
            assert_eq!(args.mode, None);
            assert_eq!(args.base, None);
        }
        _ => panic!("expected ship"),
    }
}

#[test]
fn parses_explicit_ship_mode_and_base() {
    let command = parse_run(&["orbit", "run", "ship", "-m", "local", "-b", "main", "T1"]);
    match command.command {
        RunSubcommand::Ship(args) => {
            assert_eq!(args.task_ids, vec!["T1"]);
            assert_eq!(args.mode, Some(super::ship::ShipMode::Local));
            assert_eq!(args.base.as_deref(), Some("main"));
        }
        _ => panic!("expected ship"),
    }
}

#[test]
fn parses_ship_local_as_deprecated_top_level_subcommand() {
    let command = parse_run(&["orbit", "run", "ship-local", "-b", "main", "T1"]);
    match command.command {
        RunSubcommand::ShipLocal(args) => {
            assert_eq!(args.task_ids, vec!["T1"]);
            assert_eq!(args.base.as_deref(), Some("main"));
        }
        _ => panic!("expected ship-local"),
    }
}

#[test]
fn parses_run_job_unchanged() {
    let command = parse_run(&["orbit", "run", "job", "task_auto_pipeline", "--json"]);
    match command.command {
        RunSubcommand::Job(args) => {
            assert_eq!(args.job_id, "task_auto_pipeline");
            assert!(args.json);
        }
        _ => panic!("expected job"),
    }
}

#[test]
fn rejects_positional_job_fallback() {
    assert_cli_rejects(
        &["orbit", "run", "task_auto_pipeline", "--json"],
        ErrorKind::InvalidSubcommand,
        "unrecognized subcommand 'task_auto_pipeline'",
    );
}

#[test]
fn parses_run_history_defaults() {
    let command = parse_run(&["orbit", "run", "history"]);
    match command.command {
        RunSubcommand::History(args) => {
            assert_eq!(args.job_id, None);
            assert_eq!(args.limit, super::history::DEFAULT_HISTORY_LIMIT);
            assert!(!args.json);
        }
        _ => panic!("expected history"),
    }
}

#[test]
fn parses_run_history_job_filter() {
    let command = parse_run(&["orbit", "run", "history", "-j", "task_auto_pipeline"]);
    match command.command {
        RunSubcommand::History(args) => {
            assert_eq!(args.job_id.as_deref(), Some("task_auto_pipeline"));
            assert_eq!(args.limit, super::history::DEFAULT_HISTORY_LIMIT);
        }
        _ => panic!("expected history"),
    }
}

#[test]
fn parses_run_show_latest() {
    let command = parse_run(&["orbit", "run", "show"]);
    match command.command {
        RunSubcommand::Show(args) => {
            assert_eq!(args.run_id, None);
            assert_eq!(args.step_id, None);
        }
        _ => panic!("expected show"),
    }
}

#[test]
fn parses_run_show_run_id() {
    let command = parse_run(&["orbit", "run", "show", "jrun-1"]);
    match command.command {
        RunSubcommand::Show(args) => {
            assert_eq!(args.run_id.as_deref(), Some("jrun-1"));
            assert_eq!(args.step_id, None);
        }
        _ => panic!("expected show"),
    }
}

#[test]
fn run_show_projects_parallel_provider_completion_by_invocation_parent() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let scheduled_at = Utc::now();
    let run = JobRun {
        run_id: "jrun-cli-parallel-provider".to_string(),
        job_id: "task_pr_pipeline".to_string(),
        attempt: 1,
        state: JobRunState::Running,
        scheduled_at,
        started_at: Some(scheduled_at),
        finished_at: None,
        duration_ms: None,
        created_at: scheduled_at,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };
    runtime
        .sqlite_store()
        .expect("store")
        .upsert_job_run_for_workspace(&workspace_id, &run, None)
        .expect("insert run");

    let events = [
        json!({ "event_id": "run", "ts": "2026-09-05T17:26:00Z", "body_kind": "run_started" }),
        json!({ "event_id": "step", "ts": "2026-09-05T17:26:01Z", "parent_event_id": "run", "body_kind": "step_started", "step_id": "pilot" }),
        json!({ "event_id": "finished-invocation", "ts": "2026-09-05T17:26:02Z", "parent_event_id": "step", "body_kind": "activity_started" }),
        json!({ "event_id": "live-invocation", "ts": "2026-09-05T17:26:03Z", "parent_event_id": "step", "body_kind": "activity_started" }),
        json!({ "event_id": "finished-pid", "ts": "2026-09-05T17:26:04Z", "parent_event_id": "finished-invocation", "body_kind": "cli_invocation_process", "pid": u32::MAX - 2 }),
        json!({ "event_id": "live-pid", "ts": "2026-09-05T17:26:05Z", "parent_event_id": "live-invocation", "body_kind": "cli_invocation_process", "pid": u32::MAX - 1 }),
        json!({ "event_id": "finished", "ts": "2026-09-05T17:26:06Z", "parent_event_id": "finished-invocation", "body_kind": "cli_invocation_finished", "exit_code": 0 }),
    ];
    for event in events {
        let ts = event["ts"]
            .as_str()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc))
            .expect("event timestamp");
        runtime
            .insert_v2_audit_event(&V2AuditEventInsertParams {
                workspace_id: workspace_id.clone(),
                event_id: event["event_id"].as_str().expect("event id").to_string(),
                source: "v2_envelope".to_string(),
                schema_version: 1,
                event_type: "test.event".to_string(),
                ts,
                run_id: run.run_id.clone(),
                agent_identity: "codex".to_string(),
                parent_event_id: event
                    .get("parent_event_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                workspace_path: None,
                payload_json: event.to_string(),
            })
            .expect("insert event");
    }

    let output =
        super::run_show_payload(&runtime, Some(&run.run_id), None).expect("show run payload");
    let CommandOutput::Payload(payload) = output else {
        panic!("show should produce a payload");
    };
    let (document, _) = payload.into_view();
    let processes = document["provider_processes"]
        .as_array()
        .expect("provider process projection");
    assert_eq!(processes.len(), 2);
    assert_eq!(processes[0]["pid"], u32::MAX - 2);
    assert_eq!(processes[0]["finished"], true);
    assert_eq!(processes[1]["pid"], u32::MAX - 1);
    assert_eq!(processes[1]["finished"], false);
}

#[test]
fn parses_run_show_step() {
    let command = parse_run(&["orbit", "run", "show", "jrun-1", "-s", "implement_one"]);
    match command.command {
        RunSubcommand::Show(args) => {
            assert_eq!(args.run_id.as_deref(), Some("jrun-1"));
            assert_eq!(args.step_id.as_deref(), Some("implement_one"));
        }
        _ => panic!("expected show"),
    }
}

#[test]
fn parses_run_logs_latest() {
    let command = parse_run(&["orbit", "run", "logs"]);
    match command.command {
        RunSubcommand::Logs(args) => {
            assert_eq!(args.run_id, None);
            assert_eq!(args.step_id, None);
        }
        _ => panic!("expected logs"),
    }
}

#[test]
fn parses_run_logs_run_id() {
    let command = parse_run(&["orbit", "run", "logs", "jrun-1"]);
    match command.command {
        RunSubcommand::Logs(args) => {
            assert_eq!(args.run_id.as_deref(), Some("jrun-1"));
            assert_eq!(args.step_id, None);
        }
        _ => panic!("expected logs"),
    }
}

#[test]
fn parses_run_logs_step() {
    let command = parse_run(&["orbit", "run", "logs", "jrun-1", "-s", "implement_one"]);
    match command.command {
        RunSubcommand::Logs(args) => {
            assert_eq!(args.run_id.as_deref(), Some("jrun-1"));
            assert_eq!(args.step_id.as_deref(), Some("implement_one"));
        }
        _ => panic!("expected logs"),
    }
}

#[test]
fn parses_run_events_latest() {
    let command = parse_run(&["orbit", "run", "events"]);
    match command.command {
        RunSubcommand::Events(args) => {
            assert_eq!(args.run_id, None);
            assert_eq!(args.step_id, None);
            assert_eq!(args.event_type, None);
            assert!(!args.json);
        }
        _ => panic!("expected events"),
    }
}

#[test]
fn parses_run_events_filters() {
    let command = parse_run(&[
        "orbit",
        "run",
        "events",
        "jrun-1",
        "-s",
        "implement_one",
        "--type",
        "cli.invocation.finished",
        "--json",
    ]);
    match command.command {
        RunSubcommand::Events(args) => {
            assert_eq!(args.run_id.as_deref(), Some("jrun-1"));
            assert_eq!(args.step_id.as_deref(), Some("implement_one"));
            assert_eq!(args.event_type.as_deref(), Some("cli.invocation.finished"));
            assert!(args.json);
        }
        _ => panic!("expected events"),
    }
}

#[test]
fn parses_run_trace_latest() {
    let command = parse_run(&["orbit", "run", "trace"]);
    match command.command {
        RunSubcommand::Trace(args) => {
            assert_eq!(args.run_id, None);
            assert!(!args.json);
        }
        _ => panic!("expected trace"),
    }
}

#[test]
fn parses_run_trace_json() {
    let command = parse_run(&["orbit", "run", "trace", "jrun-1", "--json"]);
    match command.command {
        RunSubcommand::Trace(args) => {
            assert_eq!(args.run_id.as_deref(), Some("jrun-1"));
            assert!(args.json);
        }
        _ => panic!("expected trace"),
    }
}

#[test]
fn run_events_filter_by_step_and_type() {
    let events = vec![
        test_audit_event("evt-run", None, "run.started", None),
        test_audit_event(
            "evt-step",
            Some("evt-run"),
            "step.started",
            Some("implement_one"),
        ),
        test_audit_event(
            "evt-cli",
            Some("evt-step"),
            "cli.invocation.finished",
            Some("implement_one"),
        ),
        test_audit_event(
            "evt-review",
            Some("evt-run"),
            "step.started",
            Some("review"),
        ),
    ];

    let filtered = super::events::filter_run_audit_events(
        events,
        Some("implement_one"),
        Some("cli.invocation.finished"),
    );
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].event_id, "evt-cli");
}

#[test]
fn run_trace_tree_nests_children_and_keeps_orphans() {
    let events = vec![
        test_audit_event("evt-run", None, "run.started", None),
        test_audit_event(
            "evt-step",
            Some("evt-run"),
            "step.started",
            Some("implement_one"),
        ),
        test_audit_event(
            "evt-activity",
            Some("evt-step"),
            "activity.started",
            Some("implement_one"),
        ),
        test_audit_event("evt-orphan", Some("evt-missing"), "tool.denied", None),
    ];

    let tree = super::trace::build_trace_tree(&events);
    assert_eq!(tree.roots.len(), 1);
    assert_eq!(tree.roots[0].event.event_id, "evt-run");
    assert_eq!(tree.roots[0].children[0].event.event_id, "evt-step");
    assert_eq!(
        tree.roots[0].children[0].children[0].event.event_id,
        "evt-activity"
    );
    assert_eq!(tree.orphans.len(), 1);
    assert_eq!(tree.orphans[0].event.event_id, "evt-orphan");
}

#[test]
fn resolve_run_step_prefers_audit_step_id() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let yaml_path = runtime.data_root().join("qa_step_id.yaml");
    std::fs::write(
        &yaml_path,
        r#"schemaVersion: 2
kind: Job
metadata:
  name: qa_step_id
spec:
  state: enabled
  kind: workflow
  steps:
    - id: nap
      spec:
        type: deterministic
        action: sleep
        config: {}
"#,
    )
    .expect("write job yaml");
    let result = runtime
        .run_job_v2_from_yaml(&yaml_path, json!({ "seconds": 0 }))
        .expect("run job");
    let run = runtime.show_job_run(&result.run_id).expect("show run");

    let resolved = super::steps::resolve_run_step(&runtime, &run, "nap").expect("resolve step");
    assert_eq!(resolved.target_id, "nap");
    assert_eq!(resolved.target_type, "activity");
}

fn test_audit_event(
    event_id: &str,
    parent_event_id: Option<&str>,
    event_type: &str,
    step_id: Option<&str>,
) -> RunAuditEvent {
    let body_kind = event_type.replace('.', "_");
    let mut raw = json!({
        "schemaVersion": 1,
        "event_type": event_type,
        "event_id": event_id,
        "ts": "2026-04-26T07:00:00Z",
        "run_id": "jrun-test",
        "agent_identity": "codex",
        "body_kind": body_kind,
    });
    if let Some(parent_event_id) = parent_event_id {
        raw.as_object_mut().unwrap().insert(
            "parent_event_id".to_string(),
            Value::String(parent_event_id.to_string()),
        );
    }
    if let Some(step_id) = step_id {
        raw.as_object_mut()
            .unwrap()
            .insert("step_id".to_string(), Value::String(step_id.to_string()));
    }
    RunAuditEvent {
        raw,
        event_id: event_id.to_string(),
        parent_event_id: parent_event_id.map(str::to_string),
        event_type: Some(event_type.to_string()),
        body_kind: Some(body_kind),
        timestamp: None,
        step_id: step_id.map(str::to_string),
    }
}

#[test]
fn workspace_auto_complexity_pools_parse_independently_and_allow_explicit_empty() {
    let command = parse_run(&[
        "orbit",
        "run",
        "auto",
        "--medium-complexity-crews",
        "grok,terra",
        "--low-complexity-crews",
        "luna",
        "--hard-complexity-crews",
    ]);
    let RunSubcommand::Auto(args) = command.command else {
        panic!("auto");
    };
    assert_eq!(
        args.medium_complexity_crews,
        Some(vec!["grok".into(), "terra".into()])
    );
    assert_eq!(args.low_complexity_crews, Some(vec!["luna".into()]));
    assert_eq!(args.hard_complexity_crews, Some(vec![]));
    assert!(
        args.allow_crew.is_empty(),
        "pools must not become an allowlist"
    );
    let RunSubcommand::Auto(defaults) = parse_run(&["orbit", "run", "auto"]).command else {
        panic!("auto");
    };
    assert_eq!(defaults.medium_complexity_crews, None);
    for flag in [
        "--low-complexity-crews",
        "--medium-complexity-crews",
        "--hard-complexity-crews",
    ] {
        assert_cli_rejects(
            &["orbit", "run", "auto", "--stop", flag, "terra"],
            ErrorKind::ArgumentConflict,
            "--stop",
        );
    }
}

#[test]
fn workspace_auto_help_explains_complexity_pools_and_precedence() {
    let error = Cli::try_parse_from(["orbit", "run", "auto", "--help"])
        .err()
        .expect("help");
    let help = error.to_string();
    for text in [
        "--medium-complexity-crews grok,terra",
        "without an explicit crew",
        "replaces its matching workflow pool",
        "retries/resume",
    ] {
        assert!(help.contains(text), "missing {text}: {help}");
    }
}
