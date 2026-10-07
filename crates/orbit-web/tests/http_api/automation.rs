use std::collections::BTreeMap;
use std::fs;

use chrono::{Duration, Utc};
use orbit_core::AutoTaskAddParams;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::automation::members::{
    MemberAssessment, MemberAttempt, MemberState, StateMember, StateTriggerKind,
};
use orbit_types::workflow::automation::{AutomationState, SourceRevision};
use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

fn state(fixture: &Fixture, kind: &str, name: &str) -> AutomationState {
    let revision = SourceRevision {
        commit: "a".repeat(40),
        tree: "b".repeat(40),
    };
    AutomationState {
        consumer: orbit_core::application::automation::consumer_key(&fixture.runtime, kind, name)
            .unwrap(),
        epoch: "retained-definition".into(),
        repository: "fixture".into(),
        branch: "agent-main".into(),
        generation: 1,
        baseline: revision.clone(),
        observed: revision.clone(),
        covered: revision,
        trigger: None,
        members: None,
        active: None,
        stall: None,
        pending: vec![],
        pending_commits: vec![],
        waived: vec![],
        excluded: vec![],
        unresolved: BTreeMap::new(),
        associations: BTreeMap::new(),
        lookup_retries: Default::default(),
    }
}

// Seed retained historical state directly; normal commits require the receipts
// that created its assessments. The HTTP reader must neither rewrite nor ship it.
fn seed(fixture: &Fixture, state: &AutomationState) {
    let encoded = serde_json::to_string(state).unwrap();
    fixture.runtime.sqlite_store().unwrap().with_transaction(|tx| {
        tx.connection().execute(
            "INSERT INTO automation_consumers (consumer, generation, state_json) VALUES (?1, 1, ?2)",
            [state.consumer.as_str(), encoded.as_str()],
        ).map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
        Ok(())
    }).unwrap();
}

#[test]
fn automation_lists_bound_inventories_and_full_state_is_explicit_and_scoped() {
    isolated(
        "automation::automation_lists_bound_inventories_and_full_state_is_explicit_and_scoped",
        || {
            let fixture = Fixture::new();
            fixture.job("task_pilot_pipeline");
            let routines = fixture.work.join("routines");
            fs::create_dir_all(&routines).unwrap();
            fs::write(routines.join("pilot.yaml"), format!(
            "schemaVersion: 1\nname: pilot\nenabled: false\ntarget: job:task_pilot_pipeline\ntrigger:\n  state:\n    kind: preparation_eligible\n    owner_machine: {}\n    branch: agent-main\n    debounce_minutes: 2\n    max_wait_minutes: 10\n    max_items: 50\n    retries: 1\n    deadline_minutes: 90\n",
            fixture.runtime.automation_machine_identity().unwrap()
        )).unwrap();
            let mut routine = state(&fixture, "routine", "pilot");
            let now = Utc::now();
            let mut members = MemberState::default();
            for i in 0..1000 {
                let key = format!("member-{i:04}");
                let member = StateMember {
                    key: key.clone(),
                    task_ids: vec![key.clone()],
                    fingerprint: "input".into(),
                    source: routine.baseline.clone(),
                    evidence: json!({"material": "x".repeat(512)}),
                    first_seen: now,
                    changed_at: now,
                    crew: None,
                };
                members.pending.insert(key.clone(), member.clone());
                members
                    .withheld
                    .insert(key.clone(), format!("Waiting for dependency {i}"));
                if i < 500 {
                    members.assessed.insert(
                        key.clone(),
                        MemberAssessment {
                            input_fingerprint: "input".into(),
                            resulting_fingerprint: if i < 250 { "input" } else { "stale" }.into(),
                            ready: i % 2 == 0,
                            receipt_id: format!("receipt-{i}"),
                        },
                    );
                }
                let attempt = MemberAttempt {
                    consumer: routine.consumer.clone(),
                    kind: StateTriggerKind::PreparationEligible,
                    id: format!("attempt-{i}"),
                    member,
                    members: vec![],
                    attempt: 1,
                    max_attempts: 2,
                    deadline: now + Duration::hours(1),
                    retry_after: now,
                    action_key: format!("action-{i}"),
                    action_id: None,
                    exhausted: i < 200,
                };
                if i < 200 {
                    members.failed.insert(key, attempt.clone());
                }
                if i == 999 {
                    members.active = Some(attempt);
                }
            }
            // Assessments absent from pending remain fresh; stale overlapping ones do not.
            members.pending.remove("member-0499");
            members.scan_after = Some("member-0999".into());
            routine.members = Some(members);
            seed(&fixture, &routine);

            fixture
                .runtime
                .auto_task_add(AutoTaskAddParams {
                    name: "review".into(),
                    description: "delivery diagnostic fixture".into(),
                    schedule: serde_json::from_value::<AutoTaskSchedule>(
                        json!({"deliveries_landed": {
                            "branch": "agent-main", "threshold": 1, "max_wait_minutes": 10,
                            "coverage": "landed_code_review_v1", "max_items": 50, "retries": 0,
                        }}),
                    )
                    .unwrap(),
                    template: AutoTaskTemplate {
                        title: "Review landings".into(),
                        description: String::new(),
                        acceptance_criteria: vec![],
                        task_type: TaskType::Chore,
                        tags: vec![],
                        required_tools: vec![],
                        context_files: vec![],
                        priority: TaskPriority::Medium,
                        complexity: None,
                        crew: None,
                        status: TaskStatus::Backlog,
                    },
                    dedupe: DedupePolicy::SkipIfOpen,
                })
                .unwrap();
            let mut auto_task = state(&fixture, "auto-task", "review");
            for i in 0..1000 {
                let commit = format!("{i:040x}");
                auto_task.pending_commits.push(commit.clone());
                auto_task
                    .unresolved
                    .insert(commit.clone(), format!("Evidence unavailable {i}"));
                auto_task.associations.insert(commit, None);
            }
            seed(&fixture, &auto_task);
            let server = fixture.server(false);
            for (path, field, name) in [
                (
                    "/api/routines?workspace=ws_http_fixture",
                    "routines",
                    "pilot",
                ),
                (
                    "/api/auto-tasks?workspace=ws_http_fixture",
                    "definitions",
                    "review",
                ),
            ] {
                let response = server.get(path);
                assert_eq!(response.status(), 200);
                let bytes = response.bytes().unwrap();
                assert!(
                    bytes.len() < 50_000,
                    "poll response must stay under 50 KB: {} bytes",
                    bytes.len()
                );
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                let diagnostic = &body[field]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["name"] == name)
                    .unwrap()["automation"]["state"];
                if field == "routines" {
                    let members = &diagnostic["members"];
                    assert_eq!(
                        members["counts"],
                        json!({"pending":999,"fresh":251,"ready":125,"withheld":1000,"failed":200})
                    );
                    assert_eq!(members["withheld"].as_object().unwrap().len(), 20);
                    assert_eq!(
                        members["withheld"]["member-0000"],
                        "Waiting for dependency 0"
                    );
                    assert_eq!(members["failed"].as_object().unwrap().len(), 20);
                    assert_eq!(members["active"]["member"]["key"], "member-0999");
                    assert!(members.get("pending").is_none() && members.get("assessed").is_none());
                } else {
                    assert_eq!(diagnostic["counts"]["pending_commits"], 1000);
                    assert_eq!(diagnostic["counts"]["unresolved"], 1000);
                    assert_eq!(diagnostic["unresolved"].as_object().unwrap().len(), 20);
                    assert!(
                        diagnostic.get("pending_commits").is_none()
                            && diagnostic.get("associations").is_none()
                    );
                }
            }
            for (kind, name, expected) in [
                ("routine", "pilot", &routine),
                ("auto-task", "review", &auto_task),
            ] {
                let path = format!("/api/automation/{kind}/{name}/state");
                assert_eq!(server.get(&path).status(), 400);
                assert_eq!(
                    server.get(&format!("{path}?workspace=unknown")).status(),
                    404
                );
                let full = json_ok(server.get(&format!("{path}?workspace=ws_http_fixture")));
                assert_eq!(
                    full["state"],
                    json!(expected),
                    "full retained state is unchanged and on demand"
                );
            }
            assert_eq!(
                server
                    .get("/api/automation/invalid/pilot/state?workspace=ws_http_fixture")
                    .status(),
                404
            );
            assert_eq!(
                server
                    .get("/api/automation/routine/missing/state?workspace=ws_http_fixture")
                    .status(),
                404
            );
        },
    );
}
