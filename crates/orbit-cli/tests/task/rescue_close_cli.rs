//! An operator closes out a rescued blocked task through the built binary
//! while another run holds an execution claim on the same files.

use std::fs;
use std::path::Path;

use chrono::Utc;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{ActorIdentity, OrbitRuntime, TaskComplexity, TaskStatus};
use serde_json::{Value, json};

use crate::{fixture_crew, isolated_cli_fixture};
use isolated_cli_fixture::Fixture;

#[test]
fn operator_closes_rescued_blocked_task_without_force_while_another_run_claims_its_files() {
    let fixture = Fixture::new();
    fixture_crew::configure_sol(&fixture.root);
    fs::write(fixture.repo.join("shared.txt"), "fixture\n").unwrap();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
        .unwrap()
        .with_actor(ActorIdentity::human("rescue-close-fixture"));
    let add = |title: &str, status| {
        runtime
            .add_task(TaskAddParams {
                title: title.to_string(),
                description: format!("Fixture task: {title}"),
                acceptance_criteria: vec!["Fixture task is observable.".to_string()],
                plan: "Fixture plan.".to_string(),
                context_files: vec!["file:shared.txt".to_string()],
                complexity: TaskComplexity::Low,
                status: Some(status),
                ..Default::default()
            })
            .unwrap()
            .id
    };
    let holder = add("claim holder", TaskStatus::InProgress);
    let claim = json!({
        "claim_id": "claim-active",
        "task_id": holder,
        "request_id": "req-active",
        "executed_on": {"machine_id": "machine-1", "machine_name": null},
        "run_context": {"run_id": "jrun-holder-42", "job_name": "test-job", "machine_name": null},
        "footprint": ["file:shared.txt"],
        "reservation_id": "res-active",
        "reservation_expires_at": (Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
        "phase": "running",
    });
    rusqlite::Connection::open(runtime.global_root().join("orbit.db"))
        .unwrap()
        .execute(
            "INSERT INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at)
             VALUES (?1, 'distributed-execution-claim-v1', 'claim-active', ?2, 'fixture', ?3)",
            rusqlite::params![
                runtime.workspace_id().unwrap(),
                claim.to_string(),
                Utc::now().to_rfc3339()
            ],
        )
        .unwrap();

    let summary = "rescued work already landed by hand";
    let tool_update = |input: Value, operator: bool, extra: &[&str]| {
        let input = input.to_string();
        let mut args = vec!["tool", "run", "orbit.task.update", "--input", &input];
        args.extend_from_slice(extra);
        let mut command = fixture.command(&args);
        if operator {
            command.env("ORBIT_OPERATOR", "1");
        }
        command.output().unwrap()
    };
    let status = |id: &str| fixture.json(&["task", "show", id, "--json"])["status"].clone();

    // Through `orbit tool run`, a caller that is not an operator, or that
    // names an agent model, starts work on the claimed files and is refused
    // with the claim named.
    let tool_task = add("rescued through the tool", TaskStatus::Blocked);
    let close = json!({"id": tool_task, "status": "in-progress", "execution_summary": summary});
    for (case, operator, extra) in [
        ("an unidentified shell", false, &[][..]),
        (
            "an operator naming an agent model",
            true,
            &["--model", "claude"][..],
        ),
    ] {
        let refused = tool_update(close.clone(), operator, extra);
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(
            !refused.status.success()
                && stderr.contains("task footprint overlaps an execution claim")
                && stderr.contains(&holder)
                && stderr.contains("jrun-holder-42"),
            "{case}: the start must be refused naming the claim's task and run: {stderr}"
        );
        assert_eq!(status(&tool_task), "blocked", "{case}");
    }

    // The operator's close-out through `orbit tool run`, without force.
    for (input, expected) in [
        (close.clone(), "in-progress"),
        (json!({"id": tool_task, "status": "review"}), "review"),
        (json!({"id": tool_task, "status": "done"}), "done"),
    ] {
        let written = tool_update(input, true, &[]);
        assert!(
            written.status.success(),
            "operator close to {expected}: {}",
            String::from_utf8_lossy(&written.stderr)
        );
        assert_eq!(status(&tool_task), expected);
    }

    // `orbit task update` from an agent shell starts work too: a backlog or
    // blocked task is refused on the claimed files with the claim named, with
    // or without the close-out summary.
    let cli_task = add("rescued through the CLI", TaskStatus::Blocked);
    let backlog_task = add("started through the CLI", TaskStatus::Backlog);
    for (task, from, extra) in [
        (&cli_task, "blocked", &["--execution-summary", summary][..]),
        (&backlog_task, "backlog", &[][..]),
    ] {
        let mut args = vec!["task", "update", task.as_str(), "--status", "in-progress"];
        args.extend_from_slice(extra);
        let refused = fixture
            .command(&args)
            .env("ORBIT_AGENT_NAME", "claude")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(
            !refused.status.success()
                && stderr.contains("task footprint overlaps an execution claim")
                && stderr.contains(&holder)
                && stderr.contains("jrun-holder-42"),
            "agent CLI start from {from} must be refused naming the claim's task and run: {stderr}"
        );
        assert_eq!(status(task), from);
    }

    // The same close-out through `orbit task update`, without --force.
    for (args, expected) in [
        (
            vec!["--status", "in-progress", "--execution-summary", summary],
            "in-progress",
        ),
        (vec!["--status", "review"], "review"),
        (vec!["--status", "done"], "done"),
    ] {
        let mut argv = vec!["task", "update", cli_task.as_str()];
        argv.extend(args);
        argv.push("--json");
        assert_eq!(fixture.json(&argv)["status"], expected);
    }
}
