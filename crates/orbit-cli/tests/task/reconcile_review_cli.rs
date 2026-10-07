//! Reconciliation records reach operators through the text CLI without losing
//! the identity or the tool-provided next step. Runtime writes are child-isolated.

use orbit_types::workflow::ReviewReconciliation;
use serde_json::{Value, json};

use crate::isolated_cli_fixture::Fixture;
use crate::review_after_landing_cli::{in_isolated_child, open_runtime};

#[test]
fn operator_status_preserves_ids_outcomes_and_next_steps_in_each_row() {
    const TEST: &str =
        "reconcile_review_cli::operator_status_preserves_ids_outcomes_and_next_steps_in_each_row";
    if !in_isolated_child(TEST) {
        return;
    }
    let fixture = Fixture::new();
    let task_id = fixture.json(&["task", "list", "--json"])[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let runtime = open_runtime(&fixture);
    let workspace = runtime.workspace_id().unwrap();
    let now = chrono::Utc::now();
    for (id, outcome) in [
        ("reconciliation-pending", Value::Null),
        (
            "reconciliation-awaiting",
            json!({"kind": "awaiting_disposition", "commands": ["make verify\ncontinued"]}),
        ),
    ] {
        let record: ReviewReconciliation = serde_json::from_value(json!({
            "schema_version": 4, "reconciliation_id": id, "request_key": id,
            "binding": {
                "workspace_id": workspace, "task_id": task_id, "task_meaning_digest": "meaning",
                "execution": {"run_id": "delivery-run", "machine_id": "follower", "claim_id": "claim", "handoff_id": "handoff", "candidate_commit": "candidate"},
                "pull_request": {"number": 42, "url": "https://example.test/pull/42", "repository": "owner/repo", "landing_branch": "agent-main", "merged_head": {"commit": "head", "tree": "head-tree"}, "base": {"commit": "base", "tree": "base-tree"}}
            },
            "binding_digest": "digest",
            "contract": {"accepted_commands": ["make verify"], "required_commands": ["make verify"], "commands_source": "accepted_handoff", "review_crew": "fixture", "review_crew_source": "workspace", "frozen_at": now},
            "requested_by": "fixture", "requested_at": now, "outcome": outcome,
            "revision": 1, "updated_at": now
        })).unwrap();
        runtime
            .review_store()
            .unwrap()
            .review_reconciliation_open(&workspace, &record)
            .unwrap();
    }
    let document = operator_json(
        &fixture,
        &[
            "task",
            "reconcile-review",
            "status",
            &task_id,
            "--format",
            "json",
        ],
    );
    let tool = operator_json(
        &fixture,
        &[
            "tool",
            "run",
            "orbit.task.reconcile_review",
            "--input",
            &json!({"action": "status", "id": task_id}).to_string(),
            "--format",
            "json",
        ],
    );
    assert_eq!(document, tool, "JSON must remain the complete tool result");
    for format in ["auto", "table"] {
        let output = fixture
            .command(&[
                "task",
                "reconcile-review",
                "status",
                &task_id,
                "--format",
                format,
            ])
            .env("ORBIT_OPERATOR", "1")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let text = String::from_utf8(output).unwrap();
        let lines: Vec<_> = text.lines().collect();
        let records = document["reconciliations"].as_array().unwrap();
        assert_eq!(
            lines.len(),
            records.len(),
            "one physical line per reconciliation: {text}"
        );
        for (line, record) in lines.iter().zip(records) {
            assert!(
                line.contains(record["reconciliation_id"].as_str().unwrap()),
                "identity lost: {line}"
            );
            assert!(
                line.contains(record["outcome"].as_str().unwrap_or("pending")),
                "state lost: {line}"
            );
            let next = record["next_step"].as_str().unwrap().replace('\n', "\\n");
            assert!(line.contains(&next), "exact next step lost: {line}");
        }
    }
    let inspect = operator_json(
        &fixture,
        &[
            "task",
            "reconcile-review",
            "inspect",
            &task_id,
            "--format",
            "json",
        ],
    );
    assert_eq!(inspect["eligible"], false);
    let text = fixture
        .command(&["task", "reconcile-review", "inspect", &task_id])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(text).unwrap();
    assert!(
        text.contains("false") && text.contains(inspect["refusal"].as_str().unwrap()),
        "eligibility refusal lost: {text}"
    );
}

fn operator_json(fixture: &Fixture, args: &[&str]) -> Value {
    let output = fixture
        .command(args)
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).unwrap()
}
