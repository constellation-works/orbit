//! Operator-authorized reconciliation output through the built CLI. Store
//! fixtures cover settled and unsettled results without launching review jobs.

use orbit_types::workflow::{REVIEW_RECONCILIATION_SCHEMA_VERSION, ReviewReconciliation};
use serde_json::{Value, json};

use crate::isolated_cli_fixture::Fixture;
use crate::review_after_landing_cli::{in_isolated_child, open_runtime};

fn text(fixture: &Fixture, args: &[&str]) -> String {
    String::from_utf8(
        fixture
            .command(args)
            // Fixture::command scrubbed inherited authority; explicitly grant
            // only this disposable operator invocation its capability.
            .env("ORBIT_OPERATOR", "1")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
}

fn operator_json(fixture: &Fixture, args: &[&str]) -> Value {
    serde_json::from_str(&text(fixture, args)).unwrap()
}

#[test]
fn operator_reconciliation_output_preserves_ids_outcomes_and_exact_next_steps() {
    const TEST: &str = "reconcile_review::operator_reconciliation_output_preserves_ids_outcomes_and_exact_next_steps";
    if !in_isolated_child(TEST) {
        return;
    }
    let fixture = Fixture::new();
    let tasks = fixture.json(&["task", "list", "--format", "json"]);
    let task = tasks[0]["id"].as_str().unwrap();
    let runtime = open_runtime(&fixture);

    let inspect = operator_json(
        &fixture,
        &[
            "task",
            "reconcile-review",
            "inspect",
            task,
            "--format",
            "json",
        ],
    );
    assert_eq!(inspect["eligible"], false);
    let human = text(&fixture, &["task", "reconcile-review", "inspect", task]);
    assert!(
        human.contains("false"),
        "the eligibility verdict is visible"
    );
    assert!(
        human.contains(inspect["refusal"].as_str().unwrap()),
        "the tool's entire refusal, including its corrective guidance, is visible: {human}"
    );
    let empty = operator_json(
        &fixture,
        &[
            "task",
            "reconcile-review",
            "status",
            task,
            "--format",
            "json",
        ],
    );
    assert_eq!(empty["reconciliations"], json!([]));
    assert!(
        !text(&fixture, &["task", "reconcile-review", "status", task])
            .trim()
            .is_empty(),
        "an empty status has an explicit human response"
    );

    let workspace = runtime.workspace_id().unwrap();
    let store = runtime.review_store().unwrap();
    for (index, outcome) in [
        Value::Null,
        json!({"kind": "accepted"}),
        json!({"kind": "awaiting_disposition", "commands": ["make check"]}),
        json!({"kind": "refused", "reason": "fixture finding", "next_step": "fix the fixture finding"}),
        json!({"kind": "accepted_with_disposition"}),
    ]
    .into_iter()
    .enumerate()
    {
        let timestamp = format!("2026-10-07T12:00:0{index}Z");
        let record: ReviewReconciliation = serde_json::from_value(json!({
            "schema_version": REVIEW_RECONCILIATION_SCHEMA_VERSION,
            "reconciliation_id": format!("rrc-fixture-{index}"),
            "request_key": format!("fixture-{index}"),
            "binding": {
                "workspace_id": workspace,
                "task_id": task,
                "task_meaning_digest": "fixture-meaning",
                "execution": {
                    "run_id": "follower-run", "machine_id": "follower-host",
                    "claim_id": "fixture-claim", "handoff_id": "fixture-handoff",
                    "candidate_commit": "candidate"
                },
                "pull_request": {
                    "number": 42, "url": "https://github.com/owner/repository/pull/42",
                    "repository": "owner/repository", "landing_branch": "agent-main",
                    "merged_head": {"commit": "head", "tree": "head-tree"},
                    "base": {"commit": "base", "tree": "base-tree"}
                }
            },
            "binding_digest": "fixture-binding",
            "contract": {
                "accepted_commands": ["make check"], "required_commands": ["make check"],
                "commands_source": "accepted_handoff", "review_crew": "fixture-reviewer",
                "review_crew_source": "workspace", "frozen_at": timestamp
            },
            "requested_by": "fixture-operator", "requested_at": timestamp,
            "attempts": [{"attempt": 1, "run_id": "fixture-run", "admitted_by": "fixture-operator", "admitted_at": timestamp}],
            "outcome": outcome, "revision": 0, "updated_at": timestamp
        }))
        .unwrap();
        store.review_reconciliation_open(&workspace, &record).unwrap();
    }

    let input = json!({"action": "status", "id": task, "workspace": fixture.repo}).to_string();
    let tool = operator_json(
        &fixture,
        &[
            "tool",
            "run",
            "orbit.task.reconcile_review",
            "--input",
            &input,
            "--format",
            "json",
        ],
    );
    let machine = operator_json(
        &fixture,
        &[
            "task",
            "reconcile-review",
            "status",
            task,
            "--format",
            "json",
        ],
    );
    assert_eq!(machine, tool, "JSON remains the unchanged tool document");
    let human = text(&fixture, &["task", "reconcile-review", "status", task]);
    let lines: Vec<_> = human.lines().collect();
    let records = machine["reconciliations"].as_array().unwrap();
    assert_eq!(lines.len(), records.len(), "one line per reconciliation");
    for (line, record) in lines.iter().zip(records) {
        for field in ["reconciliation_id", "outcome", "run_id", "next_step"] {
            if let Some(value) = record[field].as_str() {
                assert!(line.contains(value), "{field} is visible verbatim: {line}");
            }
        }
    }
    let selected = records[2]["reconciliation_id"].as_str().unwrap();
    let filtered = text(
        &fixture,
        &[
            "task",
            "reconcile-review",
            "status",
            task,
            "--reconciliation",
            selected,
        ],
    );
    assert_eq!(filtered.lines().count(), 1);
    assert!(filtered.contains(selected));
    assert!(filtered.contains(records[2]["next_step"].as_str().unwrap()));
}
