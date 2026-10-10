use std::collections::BTreeMap;
use std::fs;

use chrono::{DateTime, Duration, Utc};
use orbit_core::AutoTaskAddParams;
use orbit_store::contracts::{RoutineFireIntentParams, RoutineFireState, RoutineStoreBackend};
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::automation::members::{
    MemberAssessment, MemberAttempt, MemberState, StateMember, StateTriggerKind,
};
use orbit_types::workflow::automation::{AutomationState, SourceRevision};
use orbit_types::workflow::{
    AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, JobRunState, JobRunTrigger, PipelineState,
};
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

/// A state-triggered routine fires through its automation, not the cron fire
/// store, so the latest run it admitted is its last fire once that is newer
/// than any cron fire recorded before the routine became state-triggered.
#[test]
fn routine_last_fire_reports_the_newer_state_triggered_run() {
    isolated(
        "automation::routine_last_fire_reports_the_newer_state_triggered_run",
        || {
            let fixture = Fixture::new();
            fixture.job("task_pilot_pipeline");
            let routines = fixture.work.join("routines");
            fs::create_dir_all(&routines).unwrap();
            fs::write(routines.join("pilot.yaml"), format!(
                "schemaVersion: 1\nname: pilot\nenabled: false\ntarget: job:task_pilot_pipeline\ntrigger:\n  state:\n    kind: preparation_eligible\n    owner_machine: {}\n    branch: agent-main\n    debounce_minutes: 2\n    max_wait_minutes: 10\n    max_items: 50\n    retries: 1\n    deadline_minutes: 90\n",
                fixture.runtime.automation_machine_identity().unwrap()
            )).unwrap();
            let store =
                orbit_store::compose::routine_store(&fixture.global.join("orbit.db")).unwrap();
            let slot = "2026-09-20T23:00:00+00:00";
            store
                .routine_record_fire_intent(&RoutineFireIntentParams {
                    routine_name: "pilot".into(),
                    slot: slot.into(),
                    attempt: 1,
                    source_workspace: "fixture".into(),
                })
                .unwrap();
            store
                .routine_mark_fire_dispatched("pilot", slot, 1, "jrun-cron-fire")
                .unwrap();
            store
                .routine_mark_fire_outcome("pilot", slot, 1, RoutineFireState::Succeeded, None)
                .unwrap();
            let server = fixture.server(false);
            let last_fire = || {
                json_ok(server.get("/api/routines?workspace=ws_http_fixture"))["routines"][0]["last_fire"].clone()
            };
            assert_eq!(last_fire()["run_id"], "jrun-cron-fire");

            // The run is created after the cron fire, as a later state-triggered fire is.
            std::thread::sleep(std::time::Duration::from_millis(1100));
            let mut run = fixture.seed_run(
                "jrun-state-fire",
                "task_pilot_pipeline",
                JobRunState::Success,
            );
            run.created_at = Utc::now();
            let mut pipeline =
                PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
            pipeline.trigger = Some(JobRunTrigger::state_routine("pilot", "consumer-fixture"));
            fixture
                .runtime
                .sqlite_store()
                .unwrap()
                .upsert_job_run_for_workspace(
                    &fixture.runtime.workspace_id().unwrap(),
                    &run,
                    Some(&pipeline),
                )
                .unwrap();
            // An unrelated routine's run in the same job never becomes this routine's fire.
            let other = fixture.seed_run(
                "jrun-other-fire",
                "task_pilot_pipeline",
                JobRunState::Failed,
            );
            let mut other_pipeline =
                PipelineState::new(other.run_id.clone(), other.job_id.clone(), json!({}));
            other_pipeline.trigger = Some(JobRunTrigger::state_routine(
                "someone-else",
                "consumer-other",
            ));
            fixture
                .runtime
                .sqlite_store()
                .unwrap()
                .upsert_job_run_for_workspace(
                    &fixture.runtime.workspace_id().unwrap(),
                    &other,
                    Some(&other_pipeline),
                )
                .unwrap();

            let fire = last_fire();
            assert_eq!(fire["run_id"], "jrun-state-fire", "{fire}");
            assert_eq!(fire["state"], "succeeded", "{fire}");
            assert_eq!(
                DateTime::parse_from_rfc3339(fire["started_at"].as_str().unwrap()).unwrap(),
                run.created_at,
                "the fire is timed by its run: {fire}"
            );
        },
    );
}

// Public HTTP projection: a fire is a scheduled slot, not each retry attempt.
// Timestamps are seeded explicitly so chronology never depends on host timing.
fn record_fire(
    store: &dyn RoutineStoreBackend,
    fixture: &Fixture,
    name: &str,
    at: DateTime<Utc>,
    attempt: u32,
    state: RoutineFireState,
) {
    let slot = at.to_rfc3339();
    store
        .routine_record_fire_intent(&RoutineFireIntentParams {
            routine_name: name.into(),
            slot: slot.clone(),
            attempt,
            source_workspace: "fixture".into(),
        })
        .unwrap();
    if state != RoutineFireState::Error {
        store
            .routine_mark_fire_dispatched(
                name,
                &slot,
                attempt,
                &format!("jrun-{name}-{}-{attempt}", at.timestamp()),
            )
            .unwrap();
    }
    store
        .routine_mark_fire_outcome(name, &slot, attempt, state, None)
        .unwrap();
    let global = orbit_store::Store::open(&fixture.global.join("orbit.db")).unwrap();
    global.with_transaction(|tx| {
        tx.connection().execute(
            "UPDATE routine_fires SET created_at = ?1, updated_at = ?2 WHERE routine_name = ?3 AND slot = ?1 AND attempt = ?4",
            (&slot, (at + Duration::minutes(1)).to_rfc3339(), name, attempt),
        ).map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
        Ok(())
    }).unwrap();
}

#[test]
fn routine_fire_history_counts_slots_and_stops_at_non_failures() {
    isolated(
        "automation::routine_fire_history_counts_slots_and_stops_at_non_failures",
        || {
            let fixture = Fixture::new();
            fixture.job("watch_pipeline");
            let routines = fixture.work.join("routines");
            fs::create_dir_all(&routines).unwrap();
            for name in ["watch", "empty"] {
                fs::write(routines.join(format!("{name}.yaml")), format!(
                    "schemaVersion: 1\nname: {name}\nenabled: true\ntarget: job:watch_pipeline\ntrigger: {{cron: '*/20 * * * *'}}\n"
                )).unwrap();
            }
            let store =
                orbit_store::compose::routine_store(&fixture.global.join("orbit.db")).unwrap();
            let start = DateTime::parse_from_rfc3339("2026-10-10T06:00:00Z")
                .unwrap()
                .with_timezone(&Utc);
            for i in 0..13 {
                let outcome = match i {
                    0 => RoutineFireState::Succeeded,
                    11 => RoutineFireState::TimedOut,
                    12 => RoutineFireState::Error,
                    _ => RoutineFireState::Failed,
                };
                record_fire(
                    store.as_ref(),
                    &fixture,
                    "watch",
                    start + Duration::minutes(i * 20),
                    1,
                    outcome,
                );
            }
            let server = fixture.server(false);
            let status = || {
                let payload = json_ok(server.get("/api/routines?workspace=ws_http_fixture"));
                payload["routines"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["name"] == "watch")
                    .unwrap()
                    .clone()
            };
            let row = status();
            assert_eq!(row["recent_fires"].as_array().unwrap().len(), 10);
            assert_eq!(row["recent_fires"][0]["state"], "error");
            assert_eq!(row["recent_fires"][0]["run_id"], Value::Null);
            assert_eq!(row["recent_fires"][1]["state"], "timed_out");
            assert_eq!(row["recent_fires"][1]["ok"], false);
            assert_eq!(row["recent_fires"][1]["duration_ms"], 60_000);
            assert_eq!(row["recent_fires"][0], row["last_fire"]);
            assert_eq!(
                row["failure_streak"],
                json!({
                    "count": 12, "since": (start + Duration::minutes(20)).to_rfc3339(), "truncated": false
                })
            );
            let empty = json_ok(server.get("/api/routines?workspace=ws_http_fixture"))["routines"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["name"] == "empty")
                .unwrap()
                .clone();
            assert_eq!(empty["recent_fires"], json!([]));
            assert_eq!(empty["failure_streak"]["count"], 0);
            assert!(empty["failure_streak"]["since"].is_null());

            // An older slot's successful retry breaks the streak without adding a dot.
            record_fire(
                store.as_ref(),
                &fixture,
                "watch",
                start + Duration::minutes(200),
                2,
                RoutineFireState::Succeeded,
            );
            let row = status();
            assert_eq!(row["failure_streak"]["count"], 2);
            assert_eq!(row["recent_fires"][2]["attempt"], 2);
            assert_eq!(row["recent_fires"][2]["state"], "succeeded");
            assert_eq!(
                row["recent_fires"][3]["slot"],
                (start + Duration::minutes(180)).to_rfc3339()
            );

            for (i, outcome) in [
                RoutineFireState::Dispatched,
                RoutineFireState::Skipped,
                RoutineFireState::Succeeded,
            ]
            .into_iter()
            .enumerate()
            {
                record_fire(
                    store.as_ref(),
                    &fixture,
                    "watch",
                    start + Duration::minutes(260 + i as i64 * 20),
                    1,
                    outcome,
                );
                assert_eq!(
                    status()["failure_streak"],
                    json!({"count": 0, "since": null, "truncated": false})
                );
            }
        },
    );
}

#[test]
fn routine_streak_merges_automation_history_and_marks_bounded_history() {
    isolated(
        "automation::routine_streak_merges_automation_history_and_marks_bounded_history",
        || {
            let fixture = Fixture::new();
            fixture.job("review_pipeline");
            let routines = fixture.work.join("routines");
            fs::create_dir_all(&routines).unwrap();
            fs::write(routines.join("review.yaml"),
                "schemaVersion: 1\nname: review\nenabled: false\ntarget: job:review_pipeline\ntrigger:\n  deliveries_landed: {branch: agent-main, threshold: 1, max_wait_minutes: 10, coverage: landed_code_review_v1, max_items: 20, retries: 0}\n"
            ).unwrap();
            let store =
                orbit_store::compose::routine_store(&fixture.global.join("orbit.db")).unwrap();
            let start = DateTime::parse_from_rfc3339("2026-10-10T06:00:00Z")
                .unwrap()
                .with_timezone(&Utc);
            record_fire(
                store.as_ref(),
                &fixture,
                "review",
                start,
                1,
                RoutineFireState::Failed,
            );
            record_fire(
                store.as_ref(),
                &fixture,
                "review",
                start,
                2,
                RoutineFireState::Failed,
            );
            // The workspace run query can also return retained cron attempts.
            // Neither the superseded attempt nor its current run is another fire.
            for attempt in 1..=2 {
                let mut run = fixture.seed_run(
                    &format!("jrun-review-{}-{attempt}", start.timestamp()),
                    "review_pipeline",
                    JobRunState::Failed,
                );
                run.created_at = start;
                let mut pipeline =
                    PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
                pipeline.trigger = Some(JobRunTrigger::routine("review", start.to_rfc3339()));
                fixture
                    .runtime
                    .sqlite_store()
                    .unwrap()
                    .upsert_job_run_for_workspace(
                        &fixture.runtime.workspace_id().unwrap(),
                        &run,
                        Some(&pipeline),
                    )
                    .unwrap();
            }
            for i in 1..4 {
                let mut run = fixture.seed_run(
                    &format!("jrun-delivery-{i}"),
                    "review_pipeline",
                    JobRunState::Failed,
                );
                run.created_at = start + Duration::minutes(i * 20);
                run.started_at = Some(run.created_at);
                run.finished_at = Some(run.created_at + Duration::minutes(1));
                let mut pipeline =
                    PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
                pipeline.trigger =
                    Some(JobRunTrigger::state_routine("review", "delivery-consumer"));
                fixture
                    .runtime
                    .sqlite_store()
                    .unwrap()
                    .upsert_job_run_for_workspace(
                        &fixture.runtime.workspace_id().unwrap(),
                        &run,
                        Some(&pipeline),
                    )
                    .unwrap();
            }
            // Same job, different routine: must never break this routine's streak.
            let other = fixture.seed_run(
                "jrun-other-success",
                "review_pipeline",
                JobRunState::Success,
            );
            let mut pipeline =
                PipelineState::new(other.run_id.clone(), other.job_id.clone(), json!({}));
            pipeline.trigger = Some(JobRunTrigger::state_routine("other", "other-consumer"));
            fixture
                .runtime
                .sqlite_store()
                .unwrap()
                .upsert_job_run_for_workspace(
                    &fixture.runtime.workspace_id().unwrap(),
                    &other,
                    Some(&pipeline),
                )
                .unwrap();
            let server = fixture.server(false);
            let status = || {
                json_ok(server.get("/api/routines?workspace=ws_http_fixture"))["routines"][0]
                    .clone()
            };
            let row = status();
            assert_eq!(
                row["failure_streak"],
                json!({"count": 4, "since": start.to_rfc3339(), "truncated": false})
            );
            assert_eq!(row["recent_fires"].as_array().unwrap().len(), 4);
            assert_eq!(row["recent_fires"][0]["run_id"], "jrun-delivery-3");
            assert_eq!(row["recent_fires"][0], row["last_fire"]);

            // Bound the read without presenting the retained count as exact, or
            // extending it past unseen cron history with older automation runs.
            for i in 4..105 {
                record_fire(
                    store.as_ref(),
                    &fixture,
                    "review",
                    start + Duration::minutes(i * 20),
                    1,
                    RoutineFireState::Failed,
                );
            }
            let row = status();
            assert_eq!(row["recent_fires"].as_array().unwrap().len(), 10);
            assert_eq!(
                row["failure_streak"],
                json!({
                    "count": 100, "since": (start + Duration::minutes(100)).to_rfc3339(), "truncated": true
                })
            );
        },
    );
}
