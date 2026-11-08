//! A detached leaf stays `submitted` with no status until the drain's
//! admission pass sees the child run end. The pass then records `terminal`
//! and the child's final status. Before that record, the same assertion
//! sees `phase=submitted` and no status.

use chrono::{DateTime, Utc};
use orbit_core::DrainAdmissionsStopRequest;
use orbit_types::workflow::ChildDispatchPhase;

use super::*;

fn leaf_dispatch(workspace: &Workspace, drain: &str, leaf: &str) -> ChildDispatch {
    workspace
        .runtime
        .read_run_state(drain)
        .unwrap()
        .unwrap()
        .child_dispatches
        .into_iter()
        .find(|child| child.child_run_id == leaf)
        .unwrap_or_else(|| panic!("drain {drain} has no dispatch for {leaf}"))
}

fn pass_recorded_at(workspace: &Workspace, drain: &str) -> Option<DateTime<Utc>> {
    workspace
        .runtime
        .read_run_state(drain)
        .unwrap()
        .and_then(|state| {
            state
                .pull_drain
                .drain_last_pass
                .map(|pass| pass.recorded_at)
        })
}

/// The next admission pass whose record is strictly after `after`.
///
/// A pass that started before the leaf finished can record its timestamp
/// afterwards without having seen the terminal run. The caller waits for
/// two such records so the second pass started after the finish.
fn wait_for_later_pass(workspace: &Workspace, drain: &str, after: DateTime<Utc>) -> DateTime<Utc> {
    workspace.wait_for(drain, "an admission pass after the leaf finished", || {
        pass_recorded_at(workspace, drain).filter(|recorded_at| *recorded_at > after)
    })
}

#[test]
fn a_finished_leaf_is_recorded_terminal_on_the_drain() {
    if !crate::dispatch_admission::isolated(
        "drain_approval::leaf_dispatch::a_finished_leaf_is_recorded_terminal_on_the_drain",
    ) {
        return;
    }
    let workspace = Workspace::with_shipped_jobs();
    let release = workspace._root.path().join("release-workers");
    let mut owned_workers =
        crate::worker_fixture::install_owned_workers(&release, workspace.repo.clone());
    let task = workspace.backlog_task("leaf-dispatch");

    let drain = workspace
        .runtime
        .submit_pipeline_run(
            "workspace_auto_pipeline",
            json!({
                "for_seconds": 3600,
                "max_active_leaf_runs": 1,
                "completion": "review",
                "approve_proposed": false,
                "poll_sleep_seconds": 1,
                "idle_sleep_seconds": 1,
            }),
            None,
            None,
        )
        .unwrap()
        .run_id;
    let executor = {
        let runtime = workspace.runtime.clone();
        let drain = drain.clone();
        std::thread::spawn(move || runtime.execute_pipeline_run_worker(&drain))
    };

    let leaf = workspace.wait_for(&drain, "the admitted leaf", || {
        workspace.leaf_for(&drain, &task)
    });
    let open = leaf_dispatch(&workspace, &drain, &leaf);
    assert_eq!(
        open.phase,
        ChildDispatchPhase::Submitted,
        "a live leaf is still submitted"
    );
    assert_eq!(open.child_status, None, "a live leaf has no status yet");
    assert!(
        !workspace.run(&leaf).state.is_terminal(),
        "the leaf must still be running when the open dispatch is observed"
    );

    let finished_at = Utc::now();
    workspace
        .runtime
        .apply_task_automation_update(
            &task.id,
            orbit_engine::TaskAutomationUpdate {
                status: Some(TaskStatus::Archived),
                ..Default::default()
            },
        )
        .unwrap();
    workspace.finish(&leaf, JobRunState::Failed);
    let first_pass = wait_for_later_pass(&workspace, &drain, finished_at);
    let _second_pass = wait_for_later_pass(&workspace, &drain, first_pass);
    let settled = leaf_dispatch(&workspace, &drain, &leaf);
    assert_eq!(
        settled.phase,
        ChildDispatchPhase::Terminal,
        "a finished leaf stays phase=submitted with no status until the admission pass records it: {settled:?}"
    );
    assert_eq!(
        settled.child_status.as_deref(),
        Some("failed"),
        "the dispatch status is the child's final status: {settled:?}"
    );

    workspace
        .runtime
        .stop_workspace_auto_admissions(DrainAdmissionsStopRequest {
            actor: "operator",
            source: "fixture",
            reason: None,
            claim_token: None,
            force: false,
        })
        .unwrap();
    executor.join().unwrap().unwrap();
    let mut expected_workers = vec![drain.clone()];
    expected_workers.extend(
        workspace
            .runtime
            .read_run_state(&drain)
            .unwrap()
            .unwrap()
            .child_dispatches
            .iter()
            .map(|child| child.child_run_id.clone()),
    );
    owned_workers.release_and_wait(&expected_workers);
}
