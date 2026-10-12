//! Dashboard task writes run the core add/update pre-flight and return its
//! warnings, as the CLI and the task tools do. Warnings are identified by the
//! record they name, never by their wording.

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

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

            let clean = json_ok(server.send(
                "POST",
                &format!("/api/tasks{WORKSPACE}"),
                json!({
                    "title": "Chore with nothing to warn about",
                    "description": "Exercises a clean add.",
                    "complexity": "low",
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

/// [ORB-15191] A write that leaves the task in the backlog with no
/// footprint warns on the dashboard too: the warning is the task's own
/// backlog readiness gap with its fix.
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
            assert_eq!(
                updated["warnings"],
                footprint_warning(&updated),
                "{updated}"
            );
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
