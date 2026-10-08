//! A run's pipeline state lives beside its listing row, never in it. Every
//! run listing reads `job_runs` alone, so a multi-megabyte checkpoint must
//! leave that table without a single overflow page, whichever path wrote it.

#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]

use chrono::{TimeZone, Utc};
use orbit_store::Store;
use orbit_store::compose::workspace_job_run_store;
use orbit_store::contracts::{
    ChildJobRunAdmissionOutcome, ChildJobRunAdmissionParams, JobRunCompletion, JobRunQuery,
};
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::json;

fn count(store: &Store, sql: &str) -> i64 {
    store
        .connection()
        .lock()
        .unwrap()
        .query_row(sql, [], |row| row.get(0))
        .expect(sql)
}

#[test]
fn large_pipeline_state_never_overflows_the_listing_row() {
    let store = Store::open_in_memory().expect("memory store");
    let jobs = workspace_job_run_store(store.clone(), "ws_a");
    let payload = "x".repeat(2 * 1024 * 1024);

    // Initialize, overwrite, and read-modify-write a parent's state.
    let parent = jobs
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .expect("insert parent");
    let mut state = PipelineState::new(
        parent.run_id.clone(),
        parent.job_id.clone(),
        json!({ "payload": payload }),
    );
    assert!(jobs.initialize_run_state(&parent.run_id, &state).unwrap());
    state
        .rebase_recovery_checkpoints
        .insert("first".into(), json!(payload));
    jobs.write_run_state(&parent.run_id, &state).unwrap();
    jobs.update_run_state(&parent.run_id, &mut |_, state| {
        state
            .rebase_recovery_checkpoints
            .insert("second".into(), json!(payload));
        Ok(())
    })
    .unwrap();
    jobs.mark_job_run_running(&parent.run_id, Utc::now(), std::process::id())
        .unwrap();

    // Admit a child, which writes its state and the parent's in one step.
    let ChildJobRunAdmissionOutcome::Admitted(child) = jobs
        .admit_child_job_run(&ChildJobRunAdmissionParams {
            parent_run_id: parent.run_id.clone(),
            parent_step_id: None,
            job_id: "task_pr_pipeline".into(),
            action: "dispatch".into(),
            blocking: false,
            attempt: 1,
            scheduled_at: Utc::now(),
            input: Some(json!({ "task_ids": ["T-1"] })),
        })
        .unwrap()
    else {
        panic!("child admission refused");
    };
    // Row-only rewrites keep the state they did not touch.
    jobs.finalize_job_run(&child.run_id, JobRunState::Cancelled, Utc::now(), None)
        .unwrap();

    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM dbstat WHERE name = 'job_runs' AND pagetype = 'overflow'"
        ),
        0,
        "job_runs gained overflow pages: run listings would walk every checkpoint again"
    );
    let parent_state = jobs.read_run_state(&parent.run_id).unwrap().unwrap();
    assert_eq!(parent_state.rebase_recovery_checkpoints.len(), 2);
    assert_eq!(parent_state.child_dispatches.len(), 1);
    assert!(jobs.read_run_state(&child.run_id).unwrap().is_some());
    let listed = jobs
        .list_job_runs_filtered(&Default::default())
        .expect("list runs");
    assert_eq!(listed.len(), 2);

    // Deleting a run deletes its state.
    jobs.delete_job_run(&child.run_id).unwrap();
    assert!(jobs.read_run_state(&child.run_id).unwrap().is_none());
    assert_eq!(count(&store, "SELECT COUNT(*) FROM job_run_states"), 1);
}

#[test]
fn run_completions_keep_the_filter_and_fall_back_to_creation_time() {
    let store = Store::open_in_memory().expect("memory store");
    let jobs = workspace_job_run_store(store.clone(), "ws_a");
    let finished = Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0).unwrap();
    let insert = |job_id: &str| {
        let run = jobs
            .insert_job_run(job_id, 1, finished, None, None)
            .expect("insert run");
        jobs.mark_job_run_running(&run.run_id, finished, std::process::id())
            .expect("start run");
        run
    };
    let done = insert("task_auto_pipeline");
    let legacy = insert("task_pr_pipeline");
    let failed = insert("task_pr_pipeline");
    jobs.finalize_job_run(&done.run_id, JobRunState::Success, finished, None)
        .unwrap();
    jobs.finalize_job_run(&legacy.run_id, JobRunState::Success, finished, None)
        .unwrap();
    jobs.finalize_job_run(&failed.run_id, JobRunState::Failed, finished, None)
        .unwrap();
    // A run finalized before `finished_at` was recorded.
    store
        .connection()
        .lock()
        .unwrap()
        .execute(
            "UPDATE job_runs SET finished_at = NULL WHERE run_id = ?1",
            [&legacy.run_id],
        )
        .unwrap();

    let mut completions = jobs
        .list_job_run_completions_filtered(&JobRunQuery {
            state: Some(JobRunState::Success),
            limit: Some(1),
            ..JobRunQuery::default()
        })
        .expect("list completions");
    completions.sort_by(|a, b| a.job_id.cmp(&b.job_id));

    assert_eq!(
        completions,
        vec![
            JobRunCompletion {
                job_id: "task_auto_pipeline".into(),
                completed_at: finished,
            },
            JobRunCompletion {
                job_id: "task_pr_pipeline".into(),
                completed_at: legacy.created_at,
            },
        ],
        "the scoreboard counts every successful run by when it finished, ignoring `limit`"
    );
}
