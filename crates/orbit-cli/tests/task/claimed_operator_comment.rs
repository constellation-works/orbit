//! [ORB-16723] An operator's comment reaches a task an execution claim
//! protects, so mid-run steering can deliver it to the claimed leaf. Every
//! other unscoped write stays refused by the claim fence: a comment combined
//! with any other change, and any write from an agent identity.

use std::path::Path;

use orbit_core::{ActorIdentity, OrbitRuntime};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use crate::isolated_cli_fixture::Fixture;

const CLAIMING_RUN: &str = "jrun-follower-claim";
const FENCE: &str = "active execution claim requires a claim-scoped mutation";

/// Arguments added to a comment update, and the environment it runs under.
type Refusal<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)]);

/// Put `id` under a follower's live execution claim, as the owner's pull
/// admission leaves it: in progress, `pulled_by` the claim, with the claim
/// row in the owner's coordination journal.
fn claim(runtime: &OrbitRuntime, id: &str) {
    let claim_id = format!("claim-{id}");
    let request_id = format!("request-{id}");
    runtime
        .apply_task_automation_update(
            id,
            TaskAutomationUpdate {
                expected_status: Some(TaskStatus::Proposed),
                status: Some(TaskStatus::InProgress),
                status_event: Some("pulled_by".into()),
                status_note: Some(
                    json!({
                        "machine_id": "follower-mac",
                        "run_context": {"run_id": CLAIMING_RUN},
                        "claim_id": claim_id,
                        "request_id": request_id,
                    })
                    .to_string(),
                ),
                ..Default::default()
            },
        )
        .unwrap();
    let claim = json!({
        "claim_id": claim_id,
        "task_id": id,
        "request_id": request_id,
        "executed_on": {"machine_id": "follower-mac", "machine_name": null},
        "run_context": {
            "run_id": CLAIMING_RUN,
            "job_name": "task_claimed_pr_pipeline",
            "machine_name": null
        },
        "footprint": [],
        "reservation_id": format!("reservation-{id}"),
        "reservation_expires_at": "2099-01-01T00:00:00Z",
        "phase": "claimed"
    });
    rusqlite::Connection::open(runtime.global_root().join("orbit.db"))
        .unwrap()
        .execute(
            "INSERT INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at)
             VALUES (?1, 'distributed-execution-claim-v1', ?2, ?3, 'fixture', ?4)",
            rusqlite::params![
                runtime.workspace_id().unwrap(),
                claim_id,
                claim.to_string(),
                chrono::Utc::now().to_rfc3339()
            ],
        )
        .unwrap();
}

/// Every coordination row the owner holds: claims, receipts, reservations.
fn coordination(runtime: &OrbitRuntime) -> Vec<(String, String, String)> {
    let connection = rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT kind, row_id, payload_json FROM task_coordination_rows
             WHERE workspace_id = ?1 ORDER BY kind, row_id",
        )
        .unwrap();
    statement
        .query_map([runtime.workspace_id().unwrap()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The task projection with the fields a comment is allowed to change taken
/// out: its comment log and the envelope's `updated_at`.
fn without_comments(mut task: Value) -> Value {
    let object = task.as_object_mut().unwrap();
    object.remove("comments");
    object.remove("updated_at");
    task
}

/// `args` under `env` keeps the claim fence's refusal and writes nothing.
fn assert_fenced(fixture: &Fixture, id: &str, args: &[&str], env: &[(&str, &str)], before: &Value) {
    let mut command = fixture.command(args);
    for (name, value) in env {
        command.env(name, value);
    }
    let refused = command.output().unwrap();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success() && stderr.contains(FENCE) && stderr.contains(CLAIMING_RUN),
        "{args:?} {env:?} must keep the claim fence's refusal: {stderr}"
    );
    assert_eq!(
        fixture.json(&["task", "show", id, "--json"]),
        *before,
        "{args:?} {env:?} wrote nothing"
    );
}

#[test]
fn an_operator_comment_lands_under_an_execution_claim_and_nothing_else_does() {
    if crate::isolated_test::run_in_child(
        "claimed_operator_comment::an_operator_comment_lands_under_an_execution_claim_and_nothing_else_does",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let runtime = OrbitRuntime::from_roots(
        &fixture.root,
        Path::new(shown["orbit_root"].as_str().unwrap()),
    )
    .unwrap()
    .with_actor(ActorIdentity::human("claim-fixture"));
    let id = fixture.json(&["task", "list", "--json"])[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    fixture.json(&[
        "task",
        "update",
        &id,
        "--plan",
        "Run under the claim.",
        "--json",
    ]);
    claim(&runtime, &id);
    let journal = coordination(&runtime);
    let before = fixture.json(&["task", "show", &id, "--json"]);
    assert_eq!(before["status"], "in-progress");
    let comments_before = before["comments"].as_array().unwrap().len();

    // A comment that also changes the task, or any write from an agent
    // identity or the agent tool route, is refused by the fence unchanged.
    let refused: [Refusal; 5] = [
        (&["--tag", "late"], &[]),
        (&["--title", "Renamed"], &[]),
        (&["--status", "review"], &[]),
        (&[], &[("ORBIT_AGENT_NAME", "claude")]),
        (&[], &[("ORBIT_TASK_ACTOR_KIND", "agent")]),
    ];
    for (extra, env) in refused {
        let mut args = vec!["task", "update", &id, "--comment", "must not persist"];
        args.extend_from_slice(extra);
        assert_fenced(&fixture, &id, &args, env, &before);
    }
    let input = json!({"id": id, "comment": "must not persist"}).to_string();
    assert_fenced(
        &fixture,
        &id,
        &["tool", "run", "orbit.task.update", "--input", &input],
        &[],
        &before,
    );

    // The operator's comment alone lands: exactly one comment, nothing else
    // on the task or in the claim journal.
    let steer = "STEER: keep the flag name stable.";
    fixture.json(&["task", "update", &id, "--comment", steer, "--json"]);
    let after = fixture.json(&["task", "show", &id, "--json"]);
    let comments = after["comments"].as_array().unwrap();
    assert_eq!(comments.len(), comments_before + 1, "{after}");
    assert_eq!(comments.last().unwrap()["message"], steer);
    assert_eq!(
        without_comments(after.clone()),
        without_comments(before),
        "a comment changes no other task field, history or status"
    );
    assert_eq!(
        coordination(&runtime),
        journal,
        "a comment leaves the claim, its receipt and reservation untouched"
    );
}
