#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_core::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest,
};
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{ActorIdentity, OrbitRuntime};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate, blocked_workflow_failure_update};
use orbit_types::task::{ExecutionLocation, ExternalRef, TaskComplexity, TaskStatus};
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};

use crate::isolated_cli_fixture;
use isolated_cli_fixture::Fixture;

fn local_read_projections_return_registered_keys_task_flow_and_artifact_manifest() {
    let fixture = Fixture::new();
    let keys = fixture.json(&["config", "keys", "--json"]);
    let supported = keys["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == "automation.stall_window_minutes")
        .unwrap();
    assert!(
        supported["description"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    fixture
        .command(&[
            "config",
            "set",
            "--global",
            "automation.stall_window_minutes",
            "45",
        ])
        .assert()
        .success();
    assert_eq!(
        fixture.json(&["config", "get", "automation.stall_window_minutes", "--json"])["value"],
        45
    );

    let tasks = fixture.json(&["task", "list", "--json"]);
    let id = tasks.as_array().unwrap()[0]["id"].as_str().unwrap();
    let flow = fixture.json(&["task", "flow", "--window", "1d", "--buckets", "2", "--json"]);
    assert_eq!(flow["buckets"].as_array().unwrap().len(), 2);
    assert_eq!(flow["totals"]["filed"], 1);
    assert_eq!(flow["totals"]["open_now"], 1);
    assert_eq!(flow["totals"]["net"], 1);
    let filtered = fixture.json(&["task", "flow", "--tag", "absent-fixture-tag", "--json"]);
    assert_eq!(filtered["totals"]["filed"], 0);
    assert_eq!(filtered["totals"]["open_now"], 0);
    assert!(filtered["verdict"].as_str().unwrap().starts_with("no data"));

    let before_invalid_flow = fixture.json(&["task", "show", id, "--json"]);
    fixture
        .command(&["task", "flow", "--window", "invalid-duration", "--json"])
        .assert()
        .failure();
    assert_eq!(
        fixture.json(&["task", "show", id, "--json"]),
        before_invalid_flow
    );

    let source = fixture.repo.join("report.txt");
    fs::write(&source, "fixture artifact\n").unwrap();
    let stored = fixture.json(&[
        "task",
        "artifact",
        "put",
        id,
        source.to_str().unwrap(),
        "--path",
        "reports/fixture.txt",
        "--json",
    ]);
    let before = fixture.json(&["task", "show", id, "--json"]);
    let manifest = fixture.json(&["artifacts", id, "--task", "--json"]);
    assert_eq!(manifest[0]["path"], stored["artifacts"][0]["path"]);
    assert_eq!(
        manifest[0]["media_type"],
        stored["artifacts"][0]["media_type"]
    );
    assert_eq!(
        manifest[0]["created_by"],
        stored["artifacts"][0]["created_by"]
    );
    assert_eq!(manifest[0]["size"], stored["artifacts"][0]["size_bytes"]);
    assert_eq!(manifest.as_array().unwrap().len(), 1);
    assert_eq!(manifest[0]["path"], "reports/fixture.txt");
    assert_eq!(manifest[0]["size"], 17);
    assert_eq!(fixture.json(&["task", "show", id, "--json"]), before);
}

fn task_show_formats_status_history_and_whole_second_timestamps_without_changing_json() {
    let fixture = Fixture::new();
    let id = fixture.json(&["task", "list", "--json"])[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let changed = fixture.json(&["task", "update", &id, "--status", "backlog", "--json"]);
    assert!(changed["history"].as_array().unwrap().iter().any(|entry| {
        entry["event"] == "status_changed"
            && entry["from_status"] == "proposed"
            && entry["to_status"] == "backlog"
    }));
    fixture.json(&[
        "task",
        "update",
        &id,
        "--comment",
        "timestamp comment",
        "--json",
    ]);

    let before = fixture
        .command(&["task", "show", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = fixture
        .command(&["task", "show", &id])
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = String::from_utf8(human).unwrap();
    assert!(human.contains("status proposed → backlog"), "{human}");
    assert!(human.contains("timestamp comment"), "{human}");
    assert!(
        human
            .lines()
            .any(|line| line.starts_with("Created: ") && line.ends_with('Z'))
    );
    assert!(
        human
            .lines()
            .any(|line| line.starts_with("Updated: ") && line.ends_with('Z'))
    );
    assert!(
        human
            .lines()
            .any(|line| line.starts_with("  [") && line.contains("timestamp comment"))
    );
    for line in human.lines() {
        if let Some(rest) = line.trim_start().strip_prefix('[')
            && let Some((stamp, _)) = rest.split_once(']')
        {
            assert_eq!(
                stamp.len(),
                20,
                "timestamp must have seconds and a zone: {line}"
            );
            assert!(
                !stamp.contains('.'),
                "timestamp must not have fractions: {line}"
            );
            assert!(
                stamp.ends_with('Z'),
                "timestamp must include a zone: {line}"
            );
        }
        for label in ["Created: ", "Updated: "] {
            if let Some(stamp) = line.strip_prefix(label) {
                assert_eq!(
                    stamp.len(),
                    20,
                    "timestamp must have seconds and a zone: {line}"
                );
                assert!(
                    !stamp.contains('.'),
                    "timestamp must not have fractions: {line}"
                );
                assert!(
                    stamp.ends_with('Z'),
                    "timestamp must include a zone: {line}"
                );
            }
        }
    }

    let after = fixture
        .command(&["task", "show", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        before, after,
        "human rendering must leave the JSON bytes unchanged"
    );
}

#[test]
fn task_show_summarizes_active_run_and_limits_artifacts() {
    if crate::isolated_test::run_in_child(
        "local_read_projections_cli::task_show_summarizes_active_run_and_limits_artifacts",
    ) {
        return;
    }

    let fixture = Fixture::new();
    let shown_workspace = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown_workspace["orbit_root"].as_str().unwrap());
    let run_id = "jrun-show-summary";
    let now = Utc::now();
    let id = {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("task-show-fixture"));
        let run = JobRun {
            run_id: run_id.into(),
            job_id: "local-run-fixture".into(),
            attempt: 1,
            state: JobRunState::Running,
            scheduled_at: now,
            created_at: now,
            started_at: Some(now),
            finished_at: None,
            duration_ms: None,
            pid: None,
            pid_start_time: None,
            input: None,
            retry_source_run_id: None,
            knowledge_metrics: None,
            resolved_crew: None,
            crew_model: None,
            steps: Vec::new(),
            executed_on: Some(ExecutionLocation {
                machine_id: "machine-show-fixture".into(),
                machine_name: Some("show-fixture-host".into()),
            }),
        };
        runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_for_workspace(&runtime.workspace_id().unwrap(), &run, None)
            .unwrap();
        let task = runtime
            .add_task(TaskAddParams {
                title: "Run summary fixture".into(),
                acceptance_criteria: vec!["Show the active run".into()],
                complexity: TaskComplexity::Low,
                status: Some(TaskStatus::InProgress),
                external_refs: vec![ExternalRef::github_pr("42").unwrap()],
                ..Default::default()
            })
            .unwrap();
        runtime
            .update_task_as_human(
                &task.id.to_string(),
                TaskUpdateParams {
                    pr_status: Some(Some("approve".into())),
                    ..Default::default()
                },
                "task-show-fixture".into(),
            )
            .unwrap();
        runtime
            .apply_task_automation_update(
                &task.id.to_string(),
                TaskAutomationUpdate {
                    job_run_id: Some(run_id.into()),
                    ..Default::default()
                },
            )
            .unwrap();
        task.id.to_string()
    };

    let source = fixture.repo.join("show-summary.txt");
    for index in 0..7 {
        fs::write(&source, format!("artifact {index}\n")).unwrap();
        let source_path = source.to_str().unwrap();
        let artifact_path = format!("reports/show-summary-{index}.txt");
        fixture
            .command(&[
                "task",
                "artifact",
                "put",
                &id,
                source_path,
                "--path",
                &artifact_path,
            ])
            .assert()
            .success();
    }

    let json_before = fixture.json(&["task", "show", &id, "--json"]);
    assert_eq!(json_before["job_run_id"], run_id);
    assert_eq!(json_before["pr_status"], "approve");
    assert_eq!(json_before["artifacts"].as_array().unwrap().len(), 7);
    assert_eq!(
        json_before["artifacts"][6]["path"],
        "reports/show-summary-6.txt"
    );

    let plain = fixture
        .command(&["task", "show", &id])
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let plain = String::from_utf8(plain).unwrap();
    assert!(
        plain.contains("Run: jrun-show-summary on show-fixture-host (running)"),
        "{plain}"
    );
    assert!(plain.contains("PR: #42 (open)"));
    assert!(!plain.contains("PR Status:"));
    assert!(!plain.contains("github-pr: 42"));
    assert!(plain.contains("Artifacts: 7 total (showing first 5)"));
    let artifact_lines = plain
        .lines()
        .filter(|line| line.starts_with("  - reports/show-summary-"))
        .collect::<Vec<_>>();
    assert_eq!(artifact_lines.len(), 5, "{plain}");
    for index in 0..5 {
        assert!(plain.contains(&format!("reports/show-summary-{index}.txt")));
    }
    assert!(!plain.contains("reports/show-summary-5.txt"));
    assert!(!plain.contains("reports/show-summary-6.txt"));

    let fields = fixture
        .command(&[
            "task",
            "show",
            &id,
            "--fields",
            "job_run_id,job_run_machine,external_refs,pr_status",
        ])
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let fields = String::from_utf8(fields).unwrap();
    assert!(fields.contains("Field: job_run_id\njrun-show-summary"));
    assert!(fields.contains("Field: pr_status\napprove"));
    assert!(!fields.contains("Run:"));
    assert!(!fields.contains("PR: #42"));
    assert_eq!(
        fixture.json(&["task", "show", &id, "--json"]),
        json_before,
        "text and --fields rendering must not mutate the JSON projection"
    );
}

/// `orbit task show --json` and `task list --json` serve the same task JSON as
/// the tool host's `orbit.task.show` and `orbit.task.list`: one base
/// projection (including `job_run_machine`) and, for the full record, the same
/// sidecars, artifact manifest and crew enrichment. A tool-host write response
/// keeps the base record without the sidecars.
fn task_show_and_list_json_match_the_tool_host_projection() {
    let fixture = Fixture::new();
    let id = fixture.json(&["task", "list", "--json"])[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let dependency = fixture.json(&[
        "task",
        "add",
        "--title",
        "Parity dependency",
        "--complexity",
        "low",
        "--acceptance-criteria",
        "Exist",
        "--json",
    ])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let update = serde_json::json!({"id": id, "dependencies": [dependency]}).to_string();
    let written = fixture.json(&[
        "tool",
        "run",
        "orbit.task.update",
        "--full",
        "--input",
        &update,
    ]);
    for sidecar in ["comments", "history", "artifacts", "review"] {
        assert!(
            written.get(sidecar).is_none(),
            "a write response omits `{sidecar}`: {written}"
        );
    }
    assert!(written.as_object().unwrap().contains_key("job_run_machine"));
    let source = fixture.repo.join("parity.txt");
    fs::write(&source, "parity artifact\n").unwrap();
    fixture
        .command(&[
            "task",
            "artifact",
            "put",
            &id,
            source.to_str().unwrap(),
            "--path",
            "reports/parity.txt",
        ])
        .assert()
        .success();

    // `workspace` is lookup-owner metadata both CLI paths attach; not task data.
    let without_workspace = |mut doc: serde_json::Value| {
        doc.as_object_mut().unwrap().remove("workspace");
        doc
    };
    let shown = without_workspace(fixture.json(&["task", "show", &id, "--json"]));
    let input = serde_json::json!({"id": id}).to_string();
    let tool_shown = without_workspace(fixture.json(&[
        "tool",
        "run",
        "orbit.task.show",
        "--full",
        "--input",
        &input,
    ]));
    assert_eq!(shown, tool_shown, "CLI and tool host serve one task record");
    assert_eq!(shown["job_run_machine"], serde_json::Value::Null);
    assert_eq!(
        shown["resolved_dependencies"],
        serde_json::json!([format!("{dependency} [proposed]")])
    );
    assert_eq!(shown["artifacts"][0]["path"], "reports/parity.txt");
    assert_eq!(shown["artifacts"][0]["size_bytes"], 16);
    assert!(shown["artifacts"][0]["sha256"].is_string());

    let listed = fixture.json(&["task", "list", "--json"]);
    let tool_listed = fixture.json(&["tool", "run", "orbit.task.list", "--full", "--input", "{}"]);
    let by_id = |rows: &serde_json::Value| {
        rows.as_array()
            .unwrap()
            .iter()
            .map(|row| (row["id"].as_str().unwrap().to_string(), row.clone()))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(
        by_id(&listed),
        by_id(&tool_listed["tasks"]),
        "CLI and tool host list the same rows"
    );
    for row in listed.as_array().unwrap() {
        assert!(
            row.as_object().unwrap().contains_key("job_run_machine"),
            "every list row carries job_run_machine: {row}"
        );
    }
}

/// A run-blocked task with an escalation recorded without another status change.
fn block_with_final_recovery(runtime: &OrbitRuntime, repo: &Path, id: &str, run_id: &str) {
    runtime
        .apply_task_automation_update(
            id,
            TaskAutomationUpdate {
                job_run_id: Some(run_id.into()),
                ..blocked_workflow_failure_update(
                    "task_pr_pipeline",
                    run_id,
                    Some("STEP_FAILED"),
                    Some("step `implement` completed with success=false"),
                )
            },
        )
        .unwrap();
    let task = runtime.get_task(id).unwrap();
    let observed = runtime.final_recovery_revision(&task).unwrap();
    let outcome = runtime
        .apply_final_recovery(
            &FinalRecoveryRequest {
                task_id: id.to_string(),
                run_id: run_id.to_string(),
                observed,
                repo_root: repo.to_path_buf(),
                base_ref: "main".into(),
                completion: FinalRecoveryCompletion::Review,
                requeue_bound: Default::default(),
            },
            Some(&json!({
                "decision": "escalate",
                "diagnosis": "the failure needs an operator",
                "human_action": "move the task back to backlog after fixing the cause",
            })),
        )
        .unwrap();
    assert_eq!(outcome, FinalRecoveryOutcome::Escalated { reason: None });
}

fn guidance_lines(id: &str, run_id: &str, at: &str) -> (String, String) {
    let formatted = DateTime::parse_from_rfc3339(at)
        .unwrap()
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    (
        format!(
            "Next: move the task back to backlog with `orbit task update {id} --status backlog` to run it again"
        ),
        format!("Final Recovery: escalate (escalated) by run {run_id} at {formatted}"),
    )
}

/// Local `orbit task show` text, its `--json`, and `orbit.task.show` carry the
/// same blocked guidance. List rows omit it. A task with neither key prints
/// neither line.
fn task_show_prints_blocked_guidance_on_cli_and_tool() {
    let fixture = Fixture::new();
    let blocked = fixture.json(&["task", "list", "--json"])[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let plain = fixture.json(&[
        "task",
        "add",
        "--title",
        "Plain show",
        "--complexity",
        "low",
        "--acceptance-criteria",
        "Stay quiet",
        "--json",
    ])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let run_id = "jrun-show-guidance";
    // A managed run exports worker authority. The fixture runtime must not
    // inherit it: the marker fails construction when this temp root has no
    // binding, and the other variables would point the write at the live store.
    {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("show-guidance-fixture"));
        block_with_final_recovery(&runtime, &fixture.repo, &blocked, run_id);
    }

    let without_workspace = |mut doc: Value| {
        doc.as_object_mut().unwrap().remove("workspace");
        doc
    };
    let cli = without_workspace(fixture.json(&["task", "show", &blocked, "--json"]));
    let input = json!({"id": blocked}).to_string();
    let tool = without_workspace(fixture.json(&[
        "tool",
        "run",
        "orbit.task.show",
        "--full",
        "--input",
        &input,
    ]));
    assert_eq!(cli, tool, "CLI and tool host serve one show record");
    assert_eq!(
        cli["next_step"],
        json!({
            "run_id": run_id,
            "resume_command": null,
            "requeue_command": format!("orbit task update {blocked} --status backlog"),
        })
    );
    assert_eq!(cli["final_recovery"]["run_id"], run_id);
    assert_eq!(cli["final_recovery"]["decision"], "escalate");
    assert_eq!(cli["final_recovery"]["outcome"], "escalated");
    assert!(cli["final_recovery"]["at"].as_str().unwrap().contains('T'));
    for extra in ["status_transitions", "job_run_navigable"] {
        assert!(
            cli.get(extra).is_none(),
            "the tool show does not add dashboard extra `{extra}`"
        );
    }

    let human = fixture
        .command(&["task", "show", &blocked])
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = String::from_utf8(human).unwrap();
    let (next_line, recovery_line) = guidance_lines(
        &blocked,
        run_id,
        cli["final_recovery"]["at"].as_str().unwrap(),
    );
    assert!(
        human.lines().any(|line| line == next_line),
        "local show Next: line:\n{human}"
    );
    assert!(
        human.lines().any(|line| line == recovery_line),
        "local show Final Recovery: line:\n{human}"
    );

    let quiet = fixture
        .command(&["task", "show", &plain])
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let quiet = String::from_utf8(quiet).unwrap();
    assert!(
        !quiet.lines().any(|line| line.starts_with("Next:")),
        "an unblocked task prints no Next: line:\n{quiet}"
    );
    assert!(
        !quiet
            .lines()
            .any(|line| line.starts_with("Final Recovery:")),
        "a task with no decision prints no Final Recovery: line:\n{quiet}"
    );
    let quiet_json = fixture.json(&["task", "show", &plain, "--json"]);
    assert!(quiet_json.get("next_step").is_none());
    assert!(quiet_json.get("final_recovery").is_none());

    let listed = fixture.json(&["task", "list", "--json"]);
    let tool_listed = fixture.json(&["tool", "run", "orbit.task.list", "--full", "--input", "{}"]);
    for rows in [
        listed.as_array().unwrap(),
        tool_listed["tasks"].as_array().unwrap(),
    ] {
        let row = rows.iter().find(|row| row["id"] == blocked).unwrap();
        assert!(
            row.get("next_step").is_none() && row.get("final_recovery").is_none(),
            "a list row omits show guidance: {row}"
        );
    }
}

#[test]
fn local_read_projection_contracts() {
    for (name, check) in [
        (
            "local_read_projections_return_registered_keys_task_flow_and_artifact_manifest",
            local_read_projections_return_registered_keys_task_flow_and_artifact_manifest as fn(),
        ),
        (
            "task_show_formats_status_history_and_whole_second_timestamps_without_changing_json",
            task_show_formats_status_history_and_whole_second_timestamps_without_changing_json
                as fn(),
        ),
        (
            "task_show_and_list_json_match_the_tool_host_projection",
            task_show_and_list_json_match_the_tool_host_projection as fn(),
        ),
        (
            "task_show_prints_blocked_guidance_on_cli_and_tool",
            task_show_prints_blocked_guidance_on_cli_and_tool as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
        orbit_common::test_env::record_scenario_pass(name);
    }
}

/// The public show record explains the current block, including an escalation
/// recorded while the task was already blocked, without leaking stale reasons.
#[test]
fn task_show_explains_latest_block() {
    if crate::isolated_test::run_in_child(
        "local_read_projections_cli::task_show_explains_latest_block",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let id = fixture.json(&["task", "list", "--json"])[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let _authority = orbit_common::test_env::unset(
        orbit_common::test_env::INHERITED_AUTHORITY_ENV
            .iter()
            .copied(),
    );
    let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
        .unwrap()
        .with_actor(ActorIdentity::human("block-readout-fixture"));
    runtime
        .apply_task_automation_update(
            &id,
            blocked_workflow_failure_update(
                "ship",
                "jrun-first-block",
                Some("STEP_FAILED"),
                Some("Required check failed"),
            ),
        )
        .unwrap();
    let failed = fixture.json(&["task", "show", &id, "--json"]);
    assert_eq!(failed["block"]["reason"], "Required check failed");
    assert_eq!(failed["block"]["cause"], "run");
    assert_eq!(failed["block"]["run_id"], "jrun-first-block");
    assert!(failed["block"]["at"].as_str().is_some());

    block_with_final_recovery(&runtime, &fixture.repo, &id, "jrun-latest-escalation");
    let escalated = fixture.json(&["task", "show", &id, "--json"]);
    assert_eq!(
        escalated["block"]["reason"],
        "the failure needs an operator"
    );
    assert_eq!(
        escalated["block"]["next_action"],
        "move the task back to backlog after fixing the cause"
    );
    assert_eq!(escalated["block"]["run_id"], "jrun-latest-escalation");
    assert_eq!(escalated["next_step"]["run_id"], "jrun-latest-escalation");
    assert_eq!(escalated["block"]["at"], escalated["final_recovery"]["at"]);
    let index = escalated["block"]["comment_index"].as_u64().unwrap() as usize;
    assert!(
        escalated["comments"][index]["message"]
            .as_str()
            .unwrap()
            .contains("human_action:")
    );
    assert_eq!(
        escalated,
        fixture.json(&["task", "show", &id, "--json"]),
        "show is read-only"
    );
    let input = json!({"id": id}).to_string();
    assert_eq!(
        escalated["block"],
        fixture.json(&[
            "tool",
            "run",
            "orbit.task.show",
            "--full",
            "--input",
            &input
        ])["block"]
    );

    runtime
        .apply_task_automation_update(
            &id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
        )
        .unwrap();
    let queued = fixture.json(&["task", "show", &id, "--json"]);
    assert!(queued.get("block").is_none());
    assert!(queued.get("next_step").is_none());
    runtime
        .apply_task_automation_update(
            &id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Blocked),
                status_event: Some("status_changed".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let unknown = fixture.json(&["task", "show", &id, "--json"]);
    assert!(
        unknown["block"]["reason"].is_null(),
        "a newer unexplained block must not reuse the escalation"
    );
    assert!(unknown["block"]["run_id"].is_null());
    assert!(unknown["block"]["comment_index"].is_null());
}

/// [ORB-17206] An escalated block with a local run in failed, timeout or
/// interrupted state offers the resume route in `next_step`.
#[test]
fn task_show_offers_resume_for_resumable_escalated_run() {
    if crate::isolated_test::run_in_child(
        "local_read_projections_cli::task_show_offers_resume_for_resumable_escalated_run",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let id = fixture.json(&["task", "list", "--json"])[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let _authority = orbit_common::test_env::unset(
        orbit_common::test_env::INHERITED_AUTHORITY_ENV
            .iter()
            .copied(),
    );
    let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
        .unwrap()
        .with_actor(ActorIdentity::human("escalation-resume-fixture"));
    let run_id = "jrun-escalated-failed";
    let now = Utc::now();
    let run = JobRun {
        run_id: run_id.into(),
        job_id: "task_pr_pipeline".into(),
        attempt: 1,
        state: JobRunState::Failed,
        scheduled_at: now,
        created_at: now,
        started_at: Some(now),
        finished_at: Some(now),
        duration_ms: Some(1000),
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
        executed_on: None,
    };
    runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&runtime.workspace_id().unwrap(), &run, None)
        .unwrap();
    block_with_final_recovery(&runtime, &fixture.repo, &id, run_id);

    let cli = fixture.json(&["task", "show", &id, "--json"]);
    assert_eq!(
        cli["next_step"]["resume_command"],
        json!(format!("orbit job resume {run_id}"))
    );
    assert_eq!(
        cli["next_step"]["requeue_command"],
        format!("orbit task update {id} --status backlog")
    );
    assert_eq!(cli["next_step"]["run_id"], run_id);

    let human = fixture
        .command(&["task", "show", &id])
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = String::from_utf8(human).unwrap();
    assert!(
        human.lines().any(|line| line == format!(
            "Next: run `orbit job resume {run_id}` to retry the failed work, or move the task back to backlog with `orbit task update {id} --status backlog`"
        )),
        "resumable Next line:\n{human}"
    );

    let nonlocal_id = fixture.json(&[
        "task",
        "add",
        "--title",
        "Nonlocal escalated",
        "--complexity",
        "low",
        "--acceptance-criteria",
        "Stay unresumed",
        "--json",
    ])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let nonlocal_run_id = "jrun-escalated-nonlocal";
    let nonlocal_run = JobRun {
        run_id: nonlocal_run_id.into(),
        job_id: "task_pr_pipeline".into(),
        attempt: 1,
        state: JobRunState::Failed,
        scheduled_at: now,
        created_at: now,
        started_at: Some(now),
        finished_at: Some(now),
        duration_ms: Some(1000),
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
        executed_on: Some(ExecutionLocation {
            machine_id: "machine-other".into(),
            machine_name: Some("other-host".into()),
        }),
    };
    runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&runtime.workspace_id().unwrap(), &nonlocal_run, None)
        .unwrap();
    block_with_final_recovery(&runtime, &fixture.repo, &nonlocal_id, nonlocal_run_id);
    let nonlocal_cli = fixture.json(&["task", "show", &nonlocal_id, "--json"]);
    assert_eq!(nonlocal_cli["next_step"]["resume_command"], Value::Null);
    assert_eq!(
        nonlocal_cli["next_step"]["requeue_command"],
        format!("orbit task update {nonlocal_id} --status backlog")
    );
    assert_eq!(nonlocal_cli["next_step"]["run_id"], nonlocal_run_id);
    let nonlocal_human = fixture
        .command(&["task", "show", &nonlocal_id])
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let nonlocal_human = String::from_utf8(nonlocal_human).unwrap();
    assert!(
        nonlocal_human.lines().any(|line| line == format!(
            "Next: move the task back to backlog with `orbit task update {nonlocal_id} --status backlog` to run it again"
        )),
        "nonlocal Next line:\n{nonlocal_human}"
    );
    assert!(
        !nonlocal_human.contains("cannot be resumed"),
        "nonlocal run must not claim it cannot be resumed:\n{nonlocal_human}"
    );

    let nonresumable_id = fixture.json(&[
        "task",
        "add",
        "--title",
        "Nonresumable escalated",
        "--complexity",
        "low",
        "--acceptance-criteria",
        "Stay unresumed",
        "--json",
    ])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let nonresumable_run_id = "jrun-escalated-succeeded";
    let nonresumable_run = JobRun {
        run_id: nonresumable_run_id.into(),
        job_id: "task_pr_pipeline".into(),
        attempt: 1,
        state: JobRunState::Success,
        scheduled_at: now,
        created_at: now,
        started_at: Some(now),
        finished_at: Some(now),
        duration_ms: Some(1000),
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
        executed_on: None,
    };
    runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&runtime.workspace_id().unwrap(), &nonresumable_run, None)
        .unwrap();
    block_with_final_recovery(
        &runtime,
        &fixture.repo,
        &nonresumable_id,
        nonresumable_run_id,
    );
    let nonresumable_cli = fixture.json(&["task", "show", &nonresumable_id, "--json"]);
    assert_eq!(nonresumable_cli["next_step"]["resume_command"], Value::Null);
    assert_eq!(
        nonresumable_cli["next_step"]["requeue_command"],
        format!("orbit task update {nonresumable_id} --status backlog")
    );
    assert_eq!(nonresumable_cli["next_step"]["run_id"], nonresumable_run_id);
    let nonresumable_human = fixture
        .command(&["task", "show", &nonresumable_id])
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let nonresumable_human = String::from_utf8(nonresumable_human).unwrap();
    assert!(
        nonresumable_human.lines().any(|line| line == format!(
            "Next: move the task back to backlog with `orbit task update {nonresumable_id} --status backlog` to run it again"
        )),
        "nonresumable Next line:\n{nonresumable_human}"
    );
    assert!(
        !nonresumable_human.contains("cannot be resumed"),
        "nonresumable run must not claim it cannot be resumed:\n{nonresumable_human}"
    );
}
