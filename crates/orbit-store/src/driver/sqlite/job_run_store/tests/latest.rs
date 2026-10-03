use chrono::{TimeZone, Utc};

use super::super::SqliteJobRunStore;
use crate::Store;
use crate::contracts::{JobRunQuery, JobRunStoreBackend};

thread_local! {
    static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn count_reads(sql: &str) {
    if sql.contains("FROM job_runs") || sql.contains("FROM job_run_steps") {
        READS.with(|count| count.set(count.get() + 1));
    }
}

fn insert_run(store: &Store, workspace: &str, job: &str, run: &str, second: i64) {
    let at = Utc
        .timestamp_opt(1_700_000_000 + second, 0)
        .unwrap()
        .to_rfc3339();
    store
        .conn
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO job_runs(workspace_id,job_id,run_id,attempt,state,scheduled_at,created_at)
         VALUES (?1,?2,?3,1,'success',?4,?4)",
            rusqlite::params![workspace, job, run, at],
        )
        .unwrap();
}

#[test]
fn latest_job_runs_batches_reads_and_preserves_exact_runs() {
    let store = Store::open_in_memory().unwrap();
    let backend = SqliteJobRunStore::new(store.clone(), "ws");
    let jobs = (0..8)
        .map(|index| format!("job-{index}"))
        .collect::<Vec<_>>();
    for job in &jobs {
        insert_run(&store, "ws", job, &format!("{job}-old"), 0);
        insert_run(&store, "ws", job, &format!("{job}-new-z"), 1);
        insert_run(&store, "ws", job, &format!("{job}-new-a"), 1);
        insert_run(&store, "other", job, &format!("{job}-other"), 2);
    }
    // A latest run must retain step details rather than only its summary.
    let at = Utc::now();
    backend
        .complete_job_run_step(
            "job-2-new-a",
            &crate::contracts::JobRunStepParams {
                step_index: 0,
                target_type: orbit_types::workflow::JobTargetType::Activity,
                target_id: "test".into(),
                started_at: at,
                finished_at: at,
                duration_ms: Some(3),
                exit_code: Some(0),
                agent_response_json: Some(serde_json::json!({"ok":true})),
                state: orbit_types::workflow::JobRunState::Success,
                error_code: None,
                error_message: None,
            },
        )
        .unwrap();
    let mut requested = jobs.clone();
    requested.reverse();
    requested.extend(["missing".into(), jobs[0].clone()]);
    store.conn.lock().unwrap().trace(Some(count_reads));
    READS.with(|count| count.set(0));
    let actual = backend.latest_job_runs(&requested).unwrap();
    let reads = READS.with(std::cell::Cell::get);
    store.conn.lock().unwrap().trace(None);
    assert_eq!(
        reads, 2,
        "one query for latest runs and one for their steps, independent of job count"
    );
    let expected = jobs
        .iter()
        .map(|job| {
            backend
                .list_job_runs_filtered(&JobRunQuery {
                    job_id: Some(job.clone()),
                    limit: Some(1),
                    ..Default::default()
                })
                .unwrap()
                .remove(0)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual, expected,
        "same workspace, tie ordering, metadata and steps as individual reads"
    );
    assert_eq!(actual[2].steps.len(), 1);
}

#[test]
fn latest_job_runs_empty_input_never_reads_history() {
    let store = Store::open_in_memory().unwrap();
    let backend = SqliteJobRunStore::new(store.clone(), "ws");
    store.conn.lock().unwrap().trace(Some(count_reads));
    READS.with(|count| count.set(0));
    assert!(backend.latest_job_runs(&[]).unwrap().is_empty());
    assert_eq!(READS.with(std::cell::Cell::get), 0);
    store.conn.lock().unwrap().trace(None);
}

#[test]
fn latest_job_runs_batches_large_catalogs_without_decoding_old_history() {
    let store = Store::open_in_memory().unwrap();
    let backend = SqliteJobRunStore::new(store.clone(), "ws");
    let jobs = (0..1_001)
        .map(|index| format!("job-{index:04}"))
        .collect::<Vec<_>>();
    for job in &jobs {
        insert_run(&store, "ws", job, &format!("{job}-old"), 0);
        insert_run(&store, "ws", job, &format!("{job}-new"), 1);
    }
    store
        .conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE job_runs SET input_json='invalid-json' WHERE run_id LIKE '%-old'",
            [],
        )
        .unwrap();
    let actual = backend.latest_job_runs(&jobs).unwrap();
    assert_eq!(actual.len(), jobs.len());
    for (run, job) in actual.iter().zip(jobs) {
        assert_eq!(run.run_id, format!("{job}-new"));
    }
}
