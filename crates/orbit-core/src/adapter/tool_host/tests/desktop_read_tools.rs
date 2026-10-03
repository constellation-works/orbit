use chrono::Utc;
use orbit_types::task::TaskStatus;
use orbit_types::tool::ToolSessionContext;
use serde_json::{Value, json};

use super::super::desktop_read_tools::read;
use super::super::test_support::{create_task, test_runtime, unmanaged_tool_env_guard};

fn request(scope: &str) -> Value {
    json!({"workspace":"test-workspace", "scope":scope})
}

#[test]
fn task_filters_apply_before_pagination_and_report_actual_total() {
    if !isolated("task_filters_apply_before_pagination_and_report_actual_total") {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    create_task(&runtime, &repo, "Needle first", "", TaskStatus::Review, &[]);
    create_task(
        &runtime,
        &repo,
        "Needle second",
        "",
        TaskStatus::Review,
        &[],
    );
    create_task(
        &runtime,
        &repo,
        "Needle excluded status",
        "",
        TaskStatus::Proposed,
        &[],
    );
    create_task(
        &runtime,
        &repo,
        "Unrelated",
        "Needle body is outside search",
        TaskStatus::Review,
        &[],
    );
    let session = ToolSessionContext::default();
    let mut input = request("tasks");
    input["search"] = json!("nEeDlE");
    input["status"] = json!("review");
    input["priority"] = json!("medium");
    input["limit"] = json!(1);
    let first = read(&runtime, &session, input.clone()).expect("first page");
    assert_eq!(first["total"], 2);
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    assert_eq!(first["pagination"]["next_offset"], 1);
    assert_eq!(first["pagination"]["truncated"], true);
    assert_eq!(first["search_scope"], "task key and title");
    input["offset"] = json!(1);
    let second = read(&runtime, &session, input.clone()).expect("second page");
    assert_eq!(second["total"], 2);
    assert_eq!(second["pagination"]["truncated"], false);
    assert!(second["pagination"]["next_offset"].is_null());
    assert_ne!(first["items"][0]["id"], second["items"][0]["id"]);
    input["offset"] = json!(99);
    let beyond = read(&runtime, &session, input).expect("past end");
    assert_eq!(beyond["total"], 2);
    assert_eq!(beyond["items"], json!([]));
}

#[test]
fn invalid_page_requests_fail_instead_of_silently_expanding_or_wrapping() {
    if !isolated("invalid_page_requests_fail_instead_of_silently_expanding_or_wrapping") {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, _) = test_runtime();
    let session = ToolSessionContext::default();
    for limit in [json!(0), json!(51), json!(-1), json!("25"), json!(1.5)] {
        let mut input = request("tasks");
        input["limit"] = limit;
        assert!(read(&runtime, &session, input).is_err());
    }
    for (scope, field, value) in [
        ("tasks", "offset", json!(-1)),
        ("runs", "offset", json!(10_000)),
        ("run", "log_offset", json!(200)),
    ] {
        let mut input = request(scope);
        input[field] = value;
        input["id"] = json!("unused");
        assert!(read(&runtime, &session, input).is_err());
    }
    assert!(read(&runtime, &session, json!({"scope":"tasks"})).is_err());
}

#[test]
fn run_projection_excludes_input_and_preserves_observed_pending_state() {
    if !isolated("run_projection_excludes_input_and_preserves_observed_pending_state") {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, _) = test_runtime();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_auto_pipeline",
            1,
            Utc::now(),
            Some(json!({"private_input":"not for the desktop"})),
            None,
        )
        .expect("seed run");
    let session = ToolSessionContext::default();
    let mut input = request("run");
    input["id"] = json!(run.run_id);
    let detail = read(&runtime, &session, input).expect("observed detail");
    assert_eq!(detail["run"]["state"], "pending");
    assert!(detail["run"].get("input").is_none());
    assert!(detail["run"].get("pid").is_none());
    assert_eq!(detail["usage"]["state"], "unavailable");
    assert_eq!(detail["logs"]["items"], json!([]));
    assert_eq!(
        runtime.show_job_run_observed(&run.run_id).unwrap().state,
        run.state
    );
    let list = read(&runtime, &session, request("runs")).expect("run list");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["run_id"], run.run_id);
    assert!(list["items"][0].get("input").is_none());
}

// A temporary store alone does not clear an inherited managed worker authority.
fn isolated(name: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_DESKTOP_READ_CHILD";
    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap();
    let exact = format!("{module}::{name}");
    if std::env::var_os(MARKER).is_some_and(|value| value == exact.as_str()) {
        return true;
    }
    let home = tempfile::tempdir().expect("fixture home");
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command
        .args(["--exact", &exact, "--nocapture", "--test-threads=1"])
        .env_remove("ORBIT_WORKER_CONTEXT_REQUIRED")
        .env(MARKER, &exact)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .expect("isolated fixture child");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"));
    false
}

#[test]
fn detail_pages_comments_history_and_artifact_metadata_independently() {
    if !isolated("detail_pages_comments_history_and_artifact_metadata_independently") {
        return;
    }
    use crate::application::task::TaskRecordUpdateParams;
    use orbit_types::task::{TaskArtifact, TaskComment, TaskHistoryEntry};
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    let task = create_task(
        &runtime,
        &repo,
        "Paged task",
        "Full editable description",
        TaskStatus::Review,
        &[],
    );
    runtime
        .stores()
        .task_records()
        .update(
            &task.id,
            TaskRecordUpdateParams {
                actor: "test".into(),
                append_comments: (0..3)
                    .map(|i| TaskComment {
                        at: Utc::now(),
                        by: "test".into(),
                        message: format!("Comment {i}"),
                    })
                    .collect(),
                append_history: (0..3)
                    .map(|i| TaskHistoryEntry {
                        at: Utc::now(),
                        by: "test".into(),
                        event: format!("event_{i}"),
                        note: None,
                        from_status: None,
                        to_status: None,
                    })
                    .collect(),
                upsert_artifacts: vec![
                    TaskArtifact::from_text("one.txt", "private file body"),
                    TaskArtifact::from_text("two.txt", "another body"),
                ],
                ..Default::default()
            },
        )
        .expect("seed sidecars");
    let session = ToolSessionContext::default();
    let mut input = request("task");
    input["id"] = json!(task.id);
    input["limit"] = json!(1);
    input["comments_offset"] = json!(1);
    input["history_offset"] = json!(2);
    input["artifacts_offset"] = json!(1);
    let output = read(&runtime, &session, input).expect("detail page");
    assert_eq!(output["task"]["description"], "Full editable description");
    assert_eq!(output["comments_total"], 3);
    assert_eq!(output["comments"][0]["message"], "Comment 1");
    assert_eq!(output["comments_pagination"]["next_offset"], 2);
    assert_eq!(output["history_pagination"]["offset"], 2);
    assert_eq!(output["history"].as_array().unwrap().len(), 1);
    assert_eq!(output["artifacts_total"], 2);
    assert_eq!(output["artifacts"].as_array().unwrap().len(), 1);
    assert!(output["artifacts"][0].get("content").is_none());
    assert_eq!(output["artifacts_pagination"]["truncated"], false);
    assert!(
        output["revision"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(output["actions"]["complete"]["enabled"], false);
}

#[test]
fn run_logs_page_audited_invocations_with_bounded_utf8_excerpts() {
    if !isolated("run_logs_page_audited_invocations_with_bounded_utf8_excerpts") {
        return;
    }
    use orbit_common::storage::blob_store::BlobStore;
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, _) = test_runtime();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    let blob_store = BlobStore::new(runtime.data_root().join("state/audit/blobs"));
    let blob = blob_store.write("界".repeat(3000).as_bytes()).unwrap();
    for index in 0..3 {
        let event_id = format!("desktop-log-{index}");
        let at = Utc::now();
        let payload = json!({"schemaVersion":1,"event_id":event_id,"event_type":"test.event",
            "body_kind":"cli_invocation_finished","run_id":run.run_id,"agent_identity":"test",
            "ts":at.to_rfc3339(),"stdout_blob_ref":blob,"exit_code":index});
        runtime
            .insert_v2_audit_event(&crate::V2AuditEventInsertParams {
                workspace_id: runtime.workspace_id().unwrap(),
                event_id,
                source: "v2_envelope".into(),
                schema_version: 1,
                event_type: "test.event".into(),
                ts: at,
                run_id: run.run_id.clone(),
                agent_identity: "test".into(),
                parent_event_id: None,
                workspace_path: None,
                payload_json: payload.to_string(),
            })
            .unwrap();
    }
    let mut input = request("run");
    input["id"] = json!(run.run_id);
    input["log_offset"] = json!(1);
    input["limit"] = json!(1);
    let output = read(&runtime, &ToolSessionContext::default(), input).unwrap();
    let logs = &output["logs"];
    assert_eq!(logs["total"], 3);
    assert_eq!(logs["pagination"]["next_offset"], 2);
    assert_eq!(logs["items"].as_array().unwrap().len(), 1);
    assert_eq!(logs["items"][0]["event_id"], "desktop-log-1");
    assert!(logs["items"][0]["stdout"].as_str().unwrap().len() <= 4096);
    assert_eq!(logs["items"][0]["stdout_truncated"], true);
    assert!(logs["items"][0].get("stdout_blob_ref").is_none());
}

#[test]
fn legacy_task_list_text_and_relation_collections_are_explicitly_bounded() {
    if !isolated("legacy_task_list_text_and_relation_collections_are_explicitly_bounded") {
        return;
    }
    use super::super::test_support::create_task_with_crew;
    use crate::application::task::TaskRecordUpdateParams;
    use orbit_types::task::{TaskRelation, TaskRelationType};
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    let dependencies = (0..51)
        .map(|i| {
            let task = create_task(
                &runtime,
                &repo,
                &format!("Dependency {i}"),
                "",
                TaskStatus::Done,
                &[],
            );
            TaskRelation {
                relation_type: TaskRelationType::BlockedBy,
                target: task.id,
            }
        })
        .collect();
    let credential = format!("ghp_{}", "a".repeat(36));
    let task = create_task_with_crew(
        &runtime,
        &repo,
        &format!("Legacy target {credential} {}", "界".repeat(500)),
        "",
        TaskStatus::Review,
        &[],
        Some(&"c".repeat(500)),
    );
    runtime
        .stores()
        .task_records()
        .update(
            &task.id,
            TaskRecordUpdateParams {
                actor: "test".into(),
                relations: Some(dependencies),
                ..Default::default()
            },
        )
        .unwrap();
    let mut input = request("tasks");
    input["search"] = json!("Legacy target");
    let output = read(&runtime, &ToolSessionContext::default(), input).unwrap();
    let row = &output["items"][0];
    assert!(row["title"].as_str().unwrap().len() <= 512);
    assert_eq!(row["title_truncated"], true);
    assert!(!row["title"].as_str().unwrap().contains(&credential));
    assert!(row["crew"].as_str().unwrap().len() <= 128);
    assert_eq!(row["crew_truncated"], true);
    assert_eq!(row["relations"].as_array().unwrap().len(), 50);
    assert_eq!(row["relations_total"], 51);
    assert_eq!(row["relations_truncated"], true);
    assert_eq!(row["dependencies_total"], 51);
    assert_eq!(row["dependencies_truncated"], true);
    assert_eq!(row["id"], task.id);
    assert_eq!(output["total"], 1);
}

#[test]
fn omitted_artifact_addresses_do_not_strand_following_detail_pages() {
    if !isolated("omitted_artifact_addresses_do_not_strand_following_detail_pages") {
        return;
    }
    use crate::application::task::TaskRecordUpdateParams;
    use orbit_types::task::TaskArtifact;
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    let task = create_task(
        &runtime,
        &repo,
        "Artifact pages",
        "",
        TaskStatus::Review,
        &[],
    );
    let long_path = format!("a/{}/result.txt", vec!["x".repeat(200); 11].join("/"));
    runtime
        .stores()
        .task_records()
        .update(
            &task.id,
            TaskRecordUpdateParams {
                actor: "test".into(),
                upsert_artifacts: vec![
                    TaskArtifact::from_text(long_path, "omitted metadata"),
                    TaskArtifact::from_text("z-result.txt", "visible metadata"),
                ],
                ..Default::default()
            },
        )
        .unwrap();
    let mut input = request("task");
    input["id"] = json!(task.id);
    input["limit"] = json!(1);
    let first = read(&runtime, &ToolSessionContext::default(), input.clone()).unwrap();
    assert_eq!(first["artifacts_total"], 2);
    assert_eq!(first["artifacts"], json!([]));
    assert_eq!(first["artifacts_pagination"]["next_offset"], 1);
    assert_eq!(first["artifacts_pagination"]["truncated"], true);
    input["artifacts_offset"] = json!(1);
    let next = read(&runtime, &ToolSessionContext::default(), input).unwrap();
    assert_eq!(next["artifacts_total"], 2);
    assert_eq!(next["artifacts"][0]["path"], "z-result.txt");
    assert!(next["artifacts_pagination"]["next_offset"].is_null());
}
