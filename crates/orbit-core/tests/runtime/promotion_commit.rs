//! A promotion that also records attribution publishes one durable commit.

use chrono::Utc;
use orbit_core::OrbitRuntime;
use orbit_core::application::task::TaskAddParams;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{NO_DIFF_EXPECTED_TAG, TaskStatus};
use serde_json::json;

/// [ORB-22424] Attribution and the review transition were two commits, doubling
/// the fsyncs of every no-diff promotion. Sharing one commit keeps both facts
/// and their history order, and gives both events the commit's timestamp.
#[test]
fn no_diff_promotion_records_attribution_and_review_in_one_commit() {
    if !super::dispatch_admission::isolated(
        "promotion_commit::no_diff_promotion_records_attribution_and_review_in_one_commit",
    ) {
        return;
    }
    let root =
        tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).expect("tempdir");
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(repo.join(".orbit")).expect("workspace");
    crate::fixture_template::seed_store(&global);
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).expect("runtime");
    let task = runtime
        .add_task(TaskAddParams {
            title: "No diff".into(),
            description: "d".into(),
            acceptance_criteria: vec!["c".into()],
            plan: "p".into(),
            tags: vec![NO_DIFF_EXPECTED_TAG.into()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("task");
    let run = RuntimeHost::insert_job_run(
        &runtime,
        "task_pr_pipeline",
        1,
        Utc::now(),
        Some(json!({"task_ids": [task.id]})),
        None,
    )
    .expect("run");
    RuntimeHost::apply_task_automation_update(
        &runtime,
        &task.id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::InProgress),
            job_run_id: Some(run.run_id.clone()),
            execution_summary: Some("Outcome: success\nNo change needed.".into()),
            ..TaskAutomationUpdate::default()
        },
    )
    .expect("start");

    let output = orbit_engine::execute_deterministic_action(
        &runtime,
        "pr_promote",
        &json!({}),
        &json!({
            "job_run_id": run.run_id,
            "completed_task_ids": [task.id],
            "workspace_path": repo,
            "no_diff_expected": true,
            "already_landed_checkpoint": {"decision": "skipped_no_diff_expected"},
        }),
        false,
        &Default::default(),
        None,
    )
    .expect("promote");
    assert_eq!(output["decision"], "performed");

    let promoted = runtime.get_task(&task.id).expect("promoted");
    assert_eq!(promoted.status, TaskStatus::Review);
    assert!(promoted.implemented_by.is_some(), "attribution is recorded");
    let history = runtime.get_task_history(&task.id).expect("history");
    let [.., fields, status] = history.as_slice() else {
        panic!("promotion records attribution and status events: {history:?}");
    };
    assert_eq!(fields.event, "updated");
    assert_eq!(status.event, "status_changed");
    assert_eq!(status.to_status, Some(TaskStatus::Review));
    assert_eq!(
        fields.at, status.at,
        "attribution and the transition must share one durable commit"
    );
}
