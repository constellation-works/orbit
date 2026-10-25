//! Dashboard task writes run the core add/update pre-flight and return its
//! warnings, as the CLI and the task tools do. Warnings are identified by the
//! record they name, never by their wording.

use orbit_core::JobRunState;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use super::support::Server;

use super::support::{Fixture, isolated, json_ok};

const WORKSPACE: &str = "?workspace=ws_http_fixture";

/// The `warnings` a write returned, empty when the key is absent.
fn warnings(body: &Value) -> Vec<String> {
    body.get("warnings")
        .map(|warnings| {
            warnings
                .as_array()
                .expect("warnings array")
                .iter()
                .map(|warning| warning.as_str().expect("warning text").to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn seed(fixture: &Fixture, title: &str, status: TaskStatus) -> String {
    fixture
        .runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}."),
            status: Some(status),
            ..Default::default()
        })
        .expect("seed task")
        .id
}

/// A disabled required tool, a parent or dependency this host cannot read
/// and a non-chore task with no `context_files` warn on add; an unreadable
/// dependency warns on update; the latest task-pilot receipt's blockers warn
/// on approval.
#[test]
fn dashboard_task_writes_return_the_core_preflight_warnings() {
    isolated(
        "task_writes::dashboard_task_writes_return_the_core_preflight_warnings",
        || {
            let fixture = Fixture::new();
            let required_tool = "orbit.search";
            fixture
                .runtime
                .disable_tool(required_tool)
                .expect("disable required tool");
            let server = fixture.server(true);

            let created = json_ok(server.send(
                "POST",
                &format!("/api/tasks{WORKSPACE}"),
                json!({
                    "title": "Feature filed without scope",
                    "description": "Exercises the add pre-flight.",
                    "complexity": "low",
                    "task_type": "feature",
                    "required_tools": [required_tool],
                    "parent_id": "EXT-99999",
                    "dependencies": ["EXT-99998"],
                }),
            ));
            let added = warnings(&created);
            assert_eq!(added.len(), 4, "{created}");
            for named in [required_tool, "EXT-99999", "EXT-99998", "context_files"] {
                assert!(
                    added.iter().any(|warning| warning.contains(named)),
                    "a warning names `{named}`: {created}"
                );
            }

            let chore_without_scope = json_ok(server.send(
                "POST",
                &format!("/api/tasks{WORKSPACE}"),
                json!({
                    "title": "Chore filed without scope",
                    "description": "Exercises missing-context warnings for the default task type.",
                    "complexity": "low",
                }),
            ));
            let chore_warnings = warnings(&chore_without_scope);
            assert_eq!(chore_warnings.len(), 1, "{chore_without_scope}");
            assert!(
                chore_warnings[0].contains("context_files"),
                "a default chore warns about missing context: {chore_without_scope}"
            );

            let clean = json_ok(server.send(
                "POST",
                &format!("/api/tasks{WORKSPACE}"),
                json!({
                    "title": "No-diff chore without scope",
                    "description": "Exercises a clean add that needs no file scope.",
                    "complexity": "low",
                    "tags": ["no-diff-expected"],
                }),
            ));
            assert!(clean.get("warnings").is_none(), "{clean}");
            let id = clean["id"].as_str().expect("task id").to_string();

            let updated = json_ok(server.send(
                "PATCH",
                &format!("/api/tasks/{id}{WORKSPACE}"),
                json!({ "dependencies": ["EXT-99997"] }),
            ));
            let update_warnings = warnings(&updated);
            assert_eq!(update_warnings.len(), 1, "{updated}");
            assert!(update_warnings[0].contains("EXT-99997"), "{updated}");

            let blocker = "Await operator credentials before executing the repair";
            fixture
                .runtime
                .update_task_as_human(
                    &id,
                    TaskUpdateParams {
                        comment: Some(format!(
                            "operation_id={}\n{}",
                            "a".repeat(64),
                            json!({"assessment": {"blocked_by": [blocker]}})
                        )),
                        ..Default::default()
                    },
                    "task-pilot".to_string(),
                )
                .expect("record pilot receipt");
            let approved = json_ok(server.send(
                "POST",
                &format!("/api/tasks/{id}/approve{WORKSPACE}"),
                json!({}),
            ));
            assert_eq!(approved["status"], json!("backlog"), "{approved}");
            assert!(
                warnings(&approved)
                    .iter()
                    .any(|warning| warning.contains(blocker)),
                "approval returns the pilot blocker: {approved}"
            );
        },
    );
}

/// A creation or readiness-field write that leaves the task in the
/// backlog with no footprint warns on the dashboard too: the warning is
/// the task's own backlog readiness gap with its fix. Metadata writes do
/// not repeat it.
#[test]
fn dashboard_backlog_writes_without_context_warn_about_the_missing_file_lock() {
    isolated(
        "task_writes::dashboard_backlog_writes_without_context_warn_about_the_missing_file_lock",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            let footprint_warning = |task: &Value| -> Value {
                let gaps = task["readiness"]["gaps"]
                    .as_array()
                    .expect("readiness gaps");
                let gap = gaps
                    .iter()
                    .find(|gap| gap["code"] == json!("missing_context_files"))
                    .unwrap_or_else(|| panic!("missing_context_files gap: {task}"));
                json!([format!(
                    "{} Fix: {}.",
                    gap["message"].as_str().expect("gap message"),
                    gap["fix"].as_str().expect("gap fix")
                )])
            };

            let created = json_ok(server.send(
                "POST",
                &format!("/api/tasks{WORKSPACE}"),
                json!({
                    "title": "Backlog work",
                    "description": "Filed straight into the backlog.",
                    "complexity": "low",
                    "status": "backlog",
                }),
            ));
            assert_eq!(
                created["warnings"],
                footprint_warning(&created),
                "{created}"
            );

            let id = created["id"].as_str().expect("task id");
            let updated = json_ok(server.send(
                "PATCH",
                &format!("/api/tasks/{id}{WORKSPACE}"),
                json!({ "priority": "high" }),
            ));
            assert!(updated.get("warnings").is_none(), "{updated}");
            assert_eq!(updated["readiness"], created["readiness"], "{updated}");

            // A subsequent readiness-field write still warns; unrelated
            // writes must not hide a later edit that keeps the
            // missing-context gap.
            let edited = json_ok(server.send(
                "PATCH",
                &format!("/api/tasks/{id}{WORKSPACE}"),
                json!({ "complexity": "medium" }),
            ));
            assert_eq!(edited["warnings"], footprint_warning(&edited), "{edited}");
        },
    );
}

/// `force` stays the dashboard's human override: the lifecycle table refuses
/// reopening a done task, the override applies it and records it, and the
/// write still returns its warnings.
#[test]
fn dashboard_force_still_overrides_the_lifecycle_table() {
    isolated(
        "task_writes::dashboard_force_still_overrides_the_lifecycle_table",
        || {
            let fixture = Fixture::new();
            let id = seed(&fixture, "Delivered work", TaskStatus::Done);
            let server = fixture.server(true);
            let uri = format!("/api/tasks/{id}{WORKSPACE}");

            let refused = server.send("PATCH", &uri, json!({ "status": "backlog" }));
            assert_eq!(refused.status().as_u16(), 400);

            let forced =
                json_ok(server.send("PATCH", &uri, json!({ "status": "backlog", "force": true })));
            assert_eq!(forced["status"], json!("backlog"), "{forced}");
            assert_eq!(warnings(&forced).len(), 1, "{forced}");
            assert!(
                forced["history"]
                    .as_array()
                    .expect("history")
                    .iter()
                    .any(|entry| entry["event"] == json!("forced")),
                "the override is recorded: {forced}"
            );
        },
    );
}

/// The owning-run relaxation of the update context check needs a caller
/// bound to the task's run. A dashboard request carries no run binding, so a
/// task a run owns still gets the strict check, with its escape named.
#[test]
fn dashboard_update_of_a_run_owned_task_keeps_the_strict_context_check() {
    isolated(
        "task_writes::dashboard_update_of_a_run_owned_task_keeps_the_strict_context_check",
        || {
            let fixture = Fixture::new();
            let id = seed(&fixture, "Run-owned task", TaskStatus::Proposed);
            fixture
                .runtime
                .update_task_with_identity(
                    &id,
                    TaskUpdateParams {
                        job_run_id: Some(Some("jrun-dashboard-owner".to_string())),
                        ..Default::default()
                    },
                    None,
                    None,
                )
                .expect("bind task to a run");
            let server = fixture.server(true);

            let refused = server.send(
                "PATCH",
                &format!("/api/tasks/{id}{WORKSPACE}"),
                json!({ "context_files": ["file:src/not_yet.rs"] }),
            );
            assert_eq!(refused.status().as_u16(), 400);
            let refused: Value = refused.json().expect("error body");
            let error = refused["error"].as_str().expect("error message");
            assert!(
                error.contains("file:src/not_yet.rs") && error.contains("allow_missing_context"),
                "{refused}"
            );
            assert!(
                fixture
                    .runtime
                    .get_task(&id)
                    .expect("task")
                    .context_files
                    .is_empty()
            );
        },
    );
}

/// Operator approval refuses while a non-terminal local run lists the task in
/// `input.task_ids`, and leaves status, comments, and history unchanged. The
/// task has no `job_run_id` and no execution claim. Once that run is terminal,
/// the same action approves.
#[test]
fn dashboard_approval_refuses_while_a_local_run_owns_the_task() {
    isolated(
        "task_writes::dashboard_approval_refuses_while_a_local_run_owns_the_task",
        || {
            let fixture = Fixture::new();
            let proposed = seed(
                &fixture,
                "Proposed while a run owns it",
                TaskStatus::Proposed,
            );
            let review = seed(&fixture, "Review while a run owns it", TaskStatus::Backlog);
            fixture
                .runtime
                .update_task_as_human(
                    &review,
                    TaskUpdateParams {
                        status: Some(TaskStatus::InProgress),
                        ..Default::default()
                    },
                    "dashboard-fixture".to_string(),
                )
                .expect("move the review fixture to in-progress");
            fixture
                .runtime
                .update_task_as_human(
                    &review,
                    TaskUpdateParams {
                        status: Some(TaskStatus::Review),
                        execution_summary: Some("Delivered for review.".to_string()),
                        ..Default::default()
                    },
                    "dashboard-fixture".to_string(),
                )
                .expect("move the review fixture to review");

            let mut proposed_run = own_pending(&fixture, &proposed);
            let mut review_run = own_pending(&fixture, &review);
            for id in [&proposed, &review] {
                let task = fixture.runtime.get_task(id).expect("task");
                assert!(task.job_run_id.is_none(), "{id}");
                assert!(
                    fixture
                        .runtime
                        .inspect_execution_claims()
                        .expect("claims")
                        .iter()
                        .all(|inspection| inspection.claim.task_id != *id),
                    "{id} has an execution claim"
                );
            }

            let server = fixture.server(true);
            assert_approval_refused(&server, &proposed, &proposed_run.run_id);
            assert_approval_refused(&server, &review, &review_run.run_id);

            proposed_run.state = JobRunState::Cancelled;
            review_run.state = JobRunState::Success;
            fixture.save_run(&proposed_run);
            fixture.save_run(&review_run);

            let approved = json_ok(server.send(
                "POST",
                &format!("/api/tasks/{proposed}/approve{WORKSPACE}"),
                json!({}),
            ));
            assert_eq!(approved["status"], json!("backlog"), "{approved}");
            let done = json_ok(server.send(
                "POST",
                &format!("/api/tasks/{review}/approve{WORKSPACE}"),
                json!({}),
            ));
            assert_eq!(done["status"], json!("done"), "{done}");
        },
    );
}

fn own_pending(fixture: &Fixture, task_id: &str) -> orbit_core::JobRun {
    let mut run = fixture.seed_run(
        &format!("jrun-owner-{task_id}"),
        "fixture",
        JobRunState::Pending,
    );
    run.input = Some(json!({ "task_ids": [task_id] }));
    fixture.save_run(&run);
    run
}

fn assert_approval_refused(server: &Server, id: &str, run_id: &str) {
    let path = format!("/api/tasks/{id}{WORKSPACE}");
    let before = json_ok(server.get(&path));
    let refused = server.send(
        "POST",
        &format!("/api/tasks/{id}/approve{WORKSPACE}"),
        json!({
            "comment": "must not persist",
            "note": "must not persist",
        }),
    );
    assert_eq!(refused.status().as_u16(), 400);
    let body: Value = refused.json().expect("error body");
    let error = body["error"].as_str().expect("error message");
    assert!(
        error.contains(run_id),
        "dashboard approval must name owning run {run_id}: {body}"
    );
    let after = json_ok(server.get(&path));
    assert_eq!(after["status"], before["status"], "{after}");
    assert_eq!(after["comments"], before["comments"], "{after}");
    assert_eq!(after["history"], before["history"], "{after}");
}

/// PATCH rejects an unknown decision atomically and shares the core storage contract.
#[test]
fn dashboard_pr_status_admission() {
    isolated("task_writes::dashboard_pr_status_admission", || {
        let fixture = Fixture::new();
        let id = seed(&fixture, "Review decision admission", TaskStatus::Proposed);
        let server = fixture.server(true);
        let uri = format!("/api/tasks/{id}{WORKSPACE}");
        let before = json_ok(server.get(&uri));
        let refused = server.send(
            "PATCH",
            &uri,
            json!({
                "pr_status": "aproved", "title": "must not persist", "comment": "must not persist"
            }),
        );
        assert_eq!(refused.status().as_u16(), 400);
        let body: Value = refused.json().unwrap();
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("unknown PR status 'aproved'"),
            "{body}"
        );
        let after = json_ok(server.get(&uri));
        for field in ["title", "pr_status", "comments", "history"] {
            assert_eq!(after[field], before[field], "{field}");
        }
        for value in [
            "approve",
            "approved",
            "request-changes",
            "request_changes",
            "changes-requested",
            "changes_requested",
            " APPROVED ",
            " ApPrOvEd ",
            " ReQuEsT-ChAnGeS ",
            "",
            "   ",
        ] {
            if value.trim().is_empty() {
                json_ok(server.send("PATCH", &uri, json!({"pr_status": "approve"})));
            }
            json_ok(server.send("PATCH", &uri, json!({"pr_status": value})));
            let expected = (!value.trim().is_empty()).then_some(value.trim());
            assert_eq!(
                json_ok(server.get(&uri))["pr_status"],
                json!(expected),
                "{value:?}"
            );
        }
        json_ok(server.send("PATCH", &uri, json!({"pr_status": "approve"})));
        json_ok(server.send("PATCH", &uri, json!({"pr_status": null})));
        assert!(json_ok(server.get(&uri))["pr_status"].is_null());
    });
}

/// The dashboard lists a task's open decision request and answers it
/// through the same core apply step as the CLI: the chosen block-triage
/// option lands as a human comment, the task returns to backlog, and the
/// request reads `answered`. A second answer is refused.
#[test]
fn dashboard_answers_a_decision_request_as_human() {
    isolated(
        "task_writes::dashboard_answers_a_decision_request_as_human",
        || {
            use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
            use orbit_types::task::{
                DecisionAnswer, DecisionBinding, DecisionGate, DecisionRequest,
            };

            let fixture = Fixture::new();
            let id = seed(&fixture, "Blocked awaiting a decision", TaskStatus::Blocked);
            let history = fixture.runtime.get_task_history(&id).unwrap();
            let blocked_at = history
                .iter()
                .rfind(|entry| entry.to_status == Some(TaskStatus::Blocked))
                .expect("seeded blocked task records its block")
                .at;
            let request = DecisionRequest::new(
                id.clone(),
                DecisionGate::BlockTriage,
                vec![
                    DecisionAnswer {
                        id: "1".to_string(),
                        label: "Drop criterion 2".to_string(),
                        consequence: "the old API stays retired".to_string(),
                    },
                    DecisionAnswer {
                        id: "dismiss".to_string(),
                        label: "Dismiss".to_string(),
                        consequence: "Leave blocked, stop asking for this episode.".to_string(),
                    },
                ],
                DecisionBinding::BlockEpisode {
                    episode: blocked_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
                },
                "block_triage".to_string(),
                chrono::Utc::now(),
            );
            let request_id = request.id.clone();
            fixture
                .runtime
                .apply_task_automation_update(
                    &id,
                    TaskAutomationUpdate {
                        append_history: vec![request.history_entry("system")],
                        ..Default::default()
                    },
                )
                .expect("record the decision request");

            let server = fixture.server(true);
            let listed = json_ok(server.get(&format!("/api/decisions{WORKSPACE}")));
            assert_eq!(listed["total"], 1, "{listed}");
            assert_eq!(listed["items"][0]["id"], request_id);
            assert_eq!(listed["items"][0]["state"], "open");

            let detail = json_ok(server.get(&format!("/api/tasks/{id}{WORKSPACE}")));
            assert_eq!(detail["open_decisions"][0]["id"], request_id, "{detail}");
            assert_eq!(detail["open_decisions"][0]["gate"], "block_triage");

            let answered = json_ok(server.send(
                "POST",
                &format!("/api/decisions/{request_id}/answer{WORKSPACE}"),
                json!({"answer": "1", "note": "Criterion 2 goes."}),
            ));
            assert_eq!(answered["request"]["state"], "answered", "{answered}");
            assert_eq!(answered["request"]["resolution"]["answer_id"], "1");
            assert_eq!(answered["request"]["resolution"]["answered_by"], "human");

            let after = json_ok(server.get(&format!("/api/tasks/{id}{WORKSPACE}")));
            assert_eq!(after["status"], "backlog", "{after}");
            assert!(
                after["open_decisions"].is_null(),
                "the answered request leaves open_decisions: {after}"
            );
            assert!(
                after["comments"].as_array().unwrap().iter().any(|comment| {
                    comment["by"] == "human"
                        && comment["message"]
                            .as_str()
                            .unwrap_or_default()
                            .starts_with("Decision answer:")
                }),
                "the answer lands as a human comment: {after}"
            );

            let refused = server.send(
                "POST",
                &format!("/api/decisions/{request_id}/answer{WORKSPACE}"),
                json!({"answer": "1"}),
            );
            assert!(
                !refused.status().is_success(),
                "a second answer is refused: {}",
                refused.status()
            );
            let still = json_ok(server.get(&format!("/api/tasks/{id}{WORKSPACE}")));
            assert_eq!(
                still["comments"], after["comments"],
                "the refusal changed nothing"
            );
            assert_eq!(
                still["history"], after["history"],
                "the refusal changed nothing"
            );
        },
    );
}
