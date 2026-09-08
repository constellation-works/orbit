use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use orbit_types::workflow::{JobRun, JobRunState, JobRunStep, JobTargetType, PipelineState};
use serde_json::json;

use crate::Store;
use crate::contracts::{JobRunOrder, JobRunQuery, JobRunStoreBackend};
use crate::driver::sqlite::job_run_store::SqliteJobRunStore;

fn at(minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 1, 0, minute, 0)
        .single()
        .expect("valid timestamp")
}

fn run_with_steps(run_id: &str, state: JobRunState, created: DateTime<Utc>, steps: u32) -> JobRun {
    JobRun {
        run_id: run_id.to_string(),
        job_id: "task_pr_pipeline".to_string(),
        attempt: 1,
        state,
        scheduled_at: created,
        started_at: Some(created),
        finished_at: state.is_terminal().then_some(created),
        duration_ms: state.is_terminal().then_some(u64::from(steps) * 100),
        created_at: created,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: (0..steps)
            .map(|index| JobRunStep {
                step_index: index,
                target_type: JobTargetType::Activity,
                target_id: format!("step-{index}"),
                state,
                started_at: Some(created),
                finished_at: Some(created),
                duration_ms: Some(100),
                exit_code: Some(0),
                error_code: None,
                error_message: None,
                agent_response_json: None,
            })
            .collect(),
    }
}

/// The run upsert persists the run row only; steps are written per step.
fn insert_run_with_steps(store: &Store, workspace_id: &str, run: &JobRun) {
    store
        .upsert_job_run_for_workspace(workspace_id, run, None)
        .expect("insert run");
    for step in &run.steps {
        store
            .upsert_job_run_step_for_workspace(workspace_id, &run.run_id, step)
            .expect("insert step");
    }
}

/// Steps come back for every run on a page whatever its size: the page is
/// hydrated with one query per id chunk, not one per run, so a long history
/// must not lose or cross-wire any run's steps.
#[test]
fn listing_hydrates_every_runs_steps_across_id_chunks() {
    let store = Store::open_in_memory().expect("open store");
    let total = 1_100_u32;
    for index in 0..total {
        let run = run_with_steps(
            &format!("jrun-{index:05}"),
            JobRunState::Success,
            at(index % 60),
            index % 4,
        );
        insert_run_with_steps(&store, "ws", &run);
    }

    let runs = store
        .list_job_runs_for_workspace("ws", &JobRunQuery::default())
        .expect("list runs");
    assert_eq!(runs.len(), total as usize);
    for run in &runs {
        let index: u32 = run.run_id["jrun-".len()..].parse().expect("fixture id");
        assert_eq!(run.steps.len(), (index % 4) as usize, "{}", run.run_id);
        for (position, step) in run.steps.iter().enumerate() {
            assert_eq!(step.step_index, position as u32, "{}", run.run_id);
            assert_eq!(step.target_id, format!("step-{position}"), "{}", run.run_id);
        }
    }
}

/// [ORB-11625] A list page loads pipeline state in one query per id chunk.
/// Missing runs stay absent; unreadable JSON degrades to `None` rather than
/// failing the page.
#[test]
fn reading_run_states_hydrates_a_page_and_isolates_unreadable_rows() {
    let store = Store::open_in_memory().expect("open store");
    let mut expected = Vec::new();
    for index in 0..4_u32 {
        let run_id = format!("jrun-state-{index}");
        let run = run_with_steps(&run_id, JobRunState::Running, at(index), 0);
        let mut state = PipelineState::new(run_id.clone(), run.job_id.clone(), json!({}));
        state.record_child_dispatch(
            orbit_types::workflow::ChildDispatch::submitted(
                format!("jrun-child-{index}"),
                "task_auto_pipeline".to_string(),
                "invoke_and_wait".to_string(),
                true,
                false,
                at(index),
            )
            .with_parent_step_id(Some("ship_leaves".to_string())),
        );
        store
            .upsert_job_run_for_workspace("ws", &run, Some(&state))
            .expect("insert run with state");
        expected.push(run_id);
    }
    let unreadable = run_with_steps("jrun-bad-json", JobRunState::Pending, at(8), 0);
    store
        .upsert_job_run_for_workspace("ws", &unreadable, None)
        .expect("insert run without state");
    store
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE job_runs SET pipeline_state_json = '{not-json' \
                     WHERE workspace_id = ?1 AND run_id = ?2",
                    rusqlite::params!["ws", "jrun-bad-json"],
                )
                .map_err(|e| orbit_common::OrbitError::Store(e.to_string()))?;
            Ok(())
        })
        .expect("seed unreadable state");

    let mut ids = expected.clone();
    ids.push("jrun-bad-json".to_string());
    ids.push("jrun-missing".to_string());
    let states = store
        .read_job_run_states_for_workspace("ws", &ids)
        .expect("batch read");

    assert_eq!(states.len(), 5);
    for (index, run_id) in expected.iter().enumerate() {
        let state = states
            .get(run_id)
            .expect("run present")
            .as_ref()
            .expect("readable state");
        assert_eq!(
            state.child_dispatches[0].child_run_id,
            format!("jrun-child-{index}")
        );
    }
    assert!(
        states
            .get("jrun-bad-json")
            .expect("unreadable row is present")
            .is_none()
    );
    assert!(!states.contains_key("jrun-missing"));
}

/// Counting and duration reads apply the list filter but ignore its limit.
#[test]
fn count_and_durations_ignore_the_page_limit() {
    let store = Store::open_in_memory().expect("open store");
    for index in 0..30_u32 {
        let state = if index % 3 == 0 {
            JobRunState::Failed
        } else if index % 3 == 1 {
            JobRunState::Success
        } else {
            JobRunState::Running
        };
        let run = run_with_steps(&format!("jrun-{index:03}"), state, at(index), 1 + index % 2);
        insert_run_with_steps(&store, "ws", &run);
    }
    // Another workspace's rows must not leak into the count.
    let other = run_with_steps("jrun-other", JobRunState::Failed, at(5), 1);
    store
        .upsert_job_run_for_workspace("other-ws", &other, None)
        .expect("insert other run");

    let failed = JobRunQuery {
        state: Some(JobRunState::Failed),
        limit: Some(2),
        ..JobRunQuery::default()
    };
    assert_eq!(
        store
            .list_job_runs_for_workspace("ws", &failed)
            .expect("page")
            .len(),
        2
    );
    assert_eq!(
        store
            .count_job_runs_for_workspace("ws", &failed)
            .expect("count"),
        10
    );

    let terminal_since = JobRunQuery {
        terminal_only: true,
        created_since: Some(at(15)),
        limit: Some(1),
        ..JobRunQuery::default()
    };
    let mut durations = store
        .list_job_run_durations_for_workspace("ws", &terminal_since)
        .expect("durations");
    durations.sort_unstable();
    // Terminal runs created at minute >= 15: indexes 15..30 with index % 3 != 2
    // → 10 runs; durations are 100 * (1 + index % 2).
    assert_eq!(durations.len(), 10);
    assert!(durations.iter().all(|d| *d == 100 || *d == 200));
    assert_eq!(
        store
            .count_job_runs_for_workspace("ws", &terminal_since)
            .expect("count"),
        10
    );
}

/// [ORB-11251] `CreatedAt` orders and truncates by `created_at`, so a run
/// created earlier but still running the longest can be limited away before
/// its later `finished_at` is ever considered. `Recency` orders and
/// truncates by `finished_at`/`started_at`/`created_at` instead, so the same
/// bounded query surfaces the run that most recently finished — the
/// ordering the dashboard displays — rather than dropping it under `LIMIT`.
#[test]
fn recency_order_truncates_by_finish_time_not_creation_time() {
    let store = Store::open_in_memory().expect("open store");
    let mut old_long_running =
        run_with_steps("jrun-old-long-running", JobRunState::Success, at(0), 1);
    old_long_running.finished_at = Some(at(50));
    let mut new_short_running =
        run_with_steps("jrun-new-short-running", JobRunState::Success, at(10), 1);
    new_short_running.finished_at = Some(at(20));
    insert_run_with_steps(&store, "ws", &old_long_running);
    insert_run_with_steps(&store, "ws", &new_short_running);

    let by_created_at = JobRunQuery {
        limit: Some(1),
        ..JobRunQuery::default()
    };
    let top_by_created_at = store
        .list_job_runs_for_workspace("ws", &by_created_at)
        .expect("list by created_at");
    assert_eq!(
        top_by_created_at[0].run_id, "jrun-new-short-running",
        "created_at DESC ranks the newer-created run first, even though it finished earlier"
    );

    let by_recency = JobRunQuery {
        limit: Some(1),
        order_by: JobRunOrder::Recency,
        ..JobRunQuery::default()
    };
    let top_by_recency = store
        .list_job_runs_for_workspace("ws", &by_recency)
        .expect("list by recency");
    assert_eq!(
        top_by_recency[0].run_id, "jrun-old-long-running",
        "recency ordering must select the most-recently-finished run before LIMIT applies"
    );
}

fn insert_named_run(
    store: &Store,
    workspace_id: &str,
    run_id: &str,
    job_id: &str,
    state: JobRunState,
    created: DateTime<Utc>,
    steps: u32,
) {
    let mut run = run_with_steps(run_id, state, created, steps);
    run.job_id = job_id.to_string();
    for step in &mut run.steps {
        step.target_id = format!("{run_id}-{}", step.step_index);
        if state.is_terminal() {
            step.agent_response_json = Some(json!({ "payload": "x".repeat(256) }));
        }
    }
    insert_run_with_steps(store, workspace_id, &run);
}

fn process_cpu_ms() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/self/stat").ok()?;
    let after_comm = text.rsplit_once(')')?.1;
    let mut fields = after_comm.split_whitespace();
    for _ in 0..11 {
        fields.next()?;
    }
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some((utime.saturating_add(stime)) * 10)
}

fn elapsed_of(op: impl FnOnce()) -> (Duration, Option<u64>) {
    let cpu_before = process_cpu_ms();
    let started = Instant::now();
    op();
    let wall = started.elapsed();
    let cpu = match (cpu_before, process_cpu_ms()) {
        (Some(before), Some(after)) => Some(after.saturating_sub(before)),
        _ => None,
    };
    (wall, cpu)
}

fn hydrated_work(runs: &[JobRun]) -> (usize, usize) {
    (
        runs.len(),
        runs.iter().map(|run| run.steps.len()).sum::<usize>(),
    )
}

/// [ORB-11762] `list_job_runs_for_workspace` hydrates steps only for the SQL
/// result set. `active_only` is applied in SQL, so a long terminal history is
/// never selected and therefore never hydrated. The job-scoped and
/// workspace-scoped admission helpers must return that same filtered page.
#[test]
fn active_only_list_skips_terminal_history_before_hydration() {
    let store = Store::open_in_memory().expect("open store");
    let job_id = "task_auto_pipeline";
    let historical = 800_u32;
    for index in 0..historical {
        insert_named_run(
            &store,
            "ws",
            &format!("jrun-term-{index:04}"),
            job_id,
            JobRunState::Success,
            at(index % 50),
            4,
        );
    }
    insert_named_run(
        &store,
        "ws",
        "jrun-active-running",
        job_id,
        JobRunState::Running,
        at(50),
        1,
    );
    insert_named_run(
        &store,
        "ws",
        "jrun-active-pending-b",
        job_id,
        JobRunState::Pending,
        at(40),
        1,
    );
    insert_named_run(
        &store,
        "ws",
        "jrun-active-pending-a",
        job_id,
        JobRunState::Pending,
        at(40),
        1,
    );
    insert_named_run(
        &store,
        "ws",
        "jrun-skipped",
        job_id,
        JobRunState::Skipped,
        at(45),
        1,
    );
    insert_named_run(
        &store,
        "ws",
        "jrun-retrying",
        job_id,
        JobRunState::Retrying,
        at(46),
        1,
    );
    insert_named_run(
        &store,
        "ws",
        "jrun-other-job",
        "other_pipeline",
        JobRunState::Pending,
        at(51),
        1,
    );
    insert_named_run(
        &store,
        "other-ws",
        "jrun-other-ws",
        job_id,
        JobRunState::Pending,
        at(52),
        1,
    );

    let active_query = JobRunQuery {
        job_id: Some(job_id.to_string()),
        active_only: true,
        ..JobRunQuery::default()
    };
    let page = store
        .list_job_runs_for_workspace("ws", &active_query)
        .expect("active-only page");
    let ids = page
        .iter()
        .map(|run| run.run_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        [
            "jrun-active-running",
            "jrun-active-pending-a",
            "jrun-active-pending-b",
        ],
        "SQL must return pending/running for this job in created_at DESC, run_id ASC"
    );
    assert!(
        page.iter().all(|run| {
            matches!(run.state, JobRunState::Pending | JobRunState::Running)
                && run.job_id == job_id
                && run.steps.len() == 1
                && run.steps[0].target_id.starts_with(&run.run_id)
        }),
        "hydrated steps must belong only to the active rows SQL returned"
    );
    assert_eq!(
        page.iter().map(|run| run.steps.len()).sum::<usize>(),
        3,
        "terminal history steps must not be hydrated onto the active page"
    );

    let backend = SqliteJobRunStore::new(store.clone(), "ws");
    let job_scoped = backend
        .list_pending_or_running_job_runs(job_id)
        .expect("job-scoped active list");
    assert_eq!(
        job_scoped
            .iter()
            .map(|run| run.run_id.as_str())
            .collect::<Vec<_>>(),
        ids
    );
    let workspace_scoped = backend
        .list_all_pending_or_running_runs()
        .expect("workspace-scoped active list");
    assert_eq!(
        workspace_scoped
            .iter()
            .map(|run| run.run_id.as_str())
            .collect::<Vec<_>>(),
        [
            "jrun-other-job",
            "jrun-active-running",
            "jrun-active-pending-a",
            "jrun-active-pending-b",
        ]
    );
}

/// [ORB-11762] Isolated-store before/after of the queued-worker admission
/// scan: unfiltered list-then-retain versus SQL `active_only`. Records the
/// historical/active/queued-worker counts plus CPU and hydration work.
#[test]
fn active_lookup_benchmark_records_queued_worker_hydration_cost() {
    let store = Store::open_in_memory().expect("open store");
    let job_id = "task_auto_pipeline";
    let historical = 400_usize;
    let active = 3_usize;
    let queued_workers = 8_usize;
    for index in 0..historical {
        insert_named_run(
            &store,
            "ws",
            &format!("jrun-hist-{index:04}"),
            job_id,
            JobRunState::Failed,
            at((index % 50) as u32),
            4,
        );
    }
    for index in 0..active {
        insert_named_run(
            &store,
            "ws",
            &format!("jrun-live-{index}"),
            job_id,
            if index == 0 {
                JobRunState::Running
            } else {
                JobRunState::Pending
            },
            at(50 + index as u32),
            1,
        );
    }

    let unfiltered = JobRunQuery {
        job_id: Some(job_id.to_string()),
        ..JobRunQuery::default()
    };

    let mut before_page = Vec::new();
    let (before_wall, before_cpu_ms) = elapsed_of(|| {
        before_page = store
            .list_job_runs_for_workspace("ws", &unfiltered)
            .expect("unfiltered list");
    });
    let (before_rows, before_steps) = hydrated_work(&before_page);
    before_page.retain(|run| matches!(run.state, JobRunState::Pending | JobRunState::Running));

    let backend = SqliteJobRunStore::new(store.clone(), "ws");
    let mut after_runs = Vec::new();
    let (after_wall, after_cpu_ms) = elapsed_of(|| {
        after_runs = backend
            .list_pending_or_running_job_runs(job_id)
            .expect("active list");
    });
    let (after_rows, after_steps) = hydrated_work(&after_runs);

    let mut queued_last = Vec::new();
    let (queued_wall, queued_cpu_ms) = elapsed_of(|| {
        for _ in 0..queued_workers {
            queued_last = backend
                .list_pending_or_running_job_runs(job_id)
                .expect("queued worker scan");
        }
    });

    assert_eq!(after_runs.len(), active);
    assert_eq!(after_rows, active);
    assert_eq!(after_steps, active);
    assert_eq!(queued_last.len(), active);
    assert_eq!(before_page.len(), active);
    assert_eq!(before_rows, historical + active);
    assert_eq!(before_steps, historical * 4 + active);
    assert!(
        after_rows < before_rows,
        "active-only SQL must hydrate fewer run rows than list-then-retain ({after_rows} < {before_rows})"
    );
    assert!(
        after_steps < before_steps,
        "active-only SQL must hydrate fewer steps than list-then-retain ({after_steps} < {before_steps})"
    );

    let record = json!({
        "schema_version": 1,
        "task_id": "ORB-11762",
        "historical_row_count": historical,
        "active_count": active,
        "queued_worker_count": queued_workers,
        "before": {
            "algorithm": "list_job_runs_for_workspace unfiltered then retain pending/running",
            "rows_read": before_rows,
            "steps_hydrated": before_steps,
            "wall_ms": before_wall.as_secs_f64() * 1000.0,
            "cpu_ms": before_cpu_ms,
        },
        "after": {
            "algorithm": "JobRunQuery.active_only SQL filter before hydration",
            "rows_read": after_rows,
            "steps_hydrated": after_steps,
            "wall_ms": after_wall.as_secs_f64() * 1000.0,
            "cpu_ms": after_cpu_ms,
        },
        "queued_workers": {
            "scans": queued_workers,
            "wall_ms": queued_wall.as_secs_f64() * 1000.0,
            "cpu_ms": queued_cpu_ms,
        },
    });
    if let Ok(path) = std::env::var("ORB_11762_BENCH_PATH") {
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&record).expect("bench json"),
        )
        .expect("write bench record");
    }
    assert_eq!(record["historical_row_count"], json!(historical));
    assert_eq!(record["active_count"], json!(active));
    assert_eq!(record["queued_worker_count"], json!(queued_workers));
}

/// [ORB-11253] The transactional run-control seam: the run's own state, the
/// mutation, and the write are one operation, so a caller can refuse a run that
/// has terminalized and can never half-apply a change it aborts.
mod run_state_update {
    use std::sync::{Arc, Barrier, mpsc};
    use std::thread;
    use std::time::Duration;

    use chrono::Utc;
    use orbit_common::OrbitError;
    use orbit_types::workflow::{ChildDispatchPhase, JobRunState, PipelineState, RunStateUpdate};

    use crate::Store;
    use crate::contracts::{
        ChildJobRunAdmissionOutcome, ChildJobRunAdmissionParams, JobRunStoreBackend,
    };
    use crate::driver::sqlite::job_run_store::SqliteJobRunStore;

    fn started_run_with_state(backend: &SqliteJobRunStore, job_id: &str) -> String {
        let run = backend
            .insert_job_run(job_id, 1, Utc::now(), None, None)
            .expect("insert run");
        backend
            .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .expect("start run");
        let state = PipelineState::new(
            run.run_id.clone(),
            job_id.to_string(),
            serde_json::json!({ "max_active_leaf_runs": 5 }),
        );
        backend
            .write_run_state(&run.run_id, &state)
            .expect("write state");
        run.run_id
    }

    fn child_admission(parent_run_id: &str) -> ChildJobRunAdmissionParams {
        ChildJobRunAdmissionParams {
            parent_run_id: parent_run_id.to_string(),
            parent_step_id: Some("leaf_invoke".to_string()),
            job_id: "task_auto_pipeline".to_string(),
            action: "invoke_detached".to_string(),
            blocking: false,
            attempt: 1,
            scheduled_at: Utc::now(),
            input: Some(serde_json::json!({ "task_ids": ["ORB-1"] })),
            authority: None,
        }
    }

    #[test]
    fn a_live_run_applies_the_update() {
        let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
        let run_id = started_run_with_state(&backend, "workspace_auto_pipeline");

        let outcome = backend
            .update_run_state(&run_id, &mut |run_state, state| {
                assert_eq!(run_state, JobRunState::Running);
                state.set_drain_worker_limit(7, 5, "operator".to_string(), None, Some(0));
                Ok(())
            })
            .expect("update live state");

        assert_eq!(outcome, RunStateUpdate::Updated);
        let stored = backend
            .read_run_state(&run_id)
            .expect("read state")
            .expect("state exists");
        assert_eq!(stored.effective_max_active_leaf_runs(5), 7);
        assert_eq!(stored.drain_worker_limit_revision(), 1);
    }

    /// The closure sees the run's real state inside the transaction, which is
    /// the only place a "do not mutate a finished run" rule can be enforced
    /// without racing the worker that finishes it.
    #[test]
    fn a_terminal_run_is_refused_without_writing() {
        let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
        let run_id = started_run_with_state(&backend, "workspace_auto_pipeline");
        backend
            .finalize_job_run(&run_id, JobRunState::Success, Utc::now(), Some(1))
            .expect("finalize");

        let error = backend
            .update_run_state(&run_id, &mut |run_state, state| {
                if run_state.is_terminal() {
                    return Err(OrbitError::JobValidation(format!("run is {run_state}")));
                }
                state.set_drain_worker_limit(7, 5, "operator".to_string(), None, None);
                Ok(())
            })
            .expect_err("a terminal run is refused by the caller's own rule");

        assert!(matches!(error, OrbitError::JobValidation(_)), "{error:?}");
        let stored = backend
            .read_run_state(&run_id)
            .expect("read state")
            .expect("state exists");
        assert!(stored.drain_worker_limit.is_none());
    }

    #[test]
    fn a_missing_run_and_a_stateless_run_are_distinguishable() {
        let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
        let pending = backend
            .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
            .expect("insert run");

        assert_eq!(
            backend
                .update_run_state("jrun-missing", &mut |_, _| Ok(()))
                .expect("missing run"),
            RunStateUpdate::NotFound
        );
        assert_eq!(
            backend
                .update_run_state(&pending.run_id, &mut |_, _| Ok(()))
                .expect("stateless run"),
            RunStateUpdate::NoState
        );
    }

    #[test]
    fn an_update_that_errors_rolls_back() {
        let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
        let run_id = started_run_with_state(&backend, "workspace_auto_pipeline");

        let error = backend
            .update_run_state(&run_id, &mut |_, state| {
                state.set_drain_worker_limit(7, 5, "operator".to_string(), None, None);
                Err(OrbitError::JobRunControlConflict("superseded".to_string()))
            })
            .expect_err("closure error propagates");

        assert!(matches!(error, OrbitError::JobRunControlConflict(_)));
        let stored = backend
            .read_run_state(&run_id)
            .expect("read state")
            .expect("state exists");
        assert!(stored.drain_worker_limit.is_none());
    }

    /// Two operators reading the same revision must not both succeed: the
    /// compare-and-set is evaluated inside the write transaction, so the loser
    /// is refused rather than silently overwriting the winner.
    #[test]
    fn concurrent_compare_and_set_updates_admit_exactly_one_winner() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let db_path = temp.path().join("orbit.db");
        let seed = SqliteJobRunStore::new(Store::open(&db_path).expect("seed store"), "ws_a");
        let run_id = started_run_with_state(&seed, "workspace_auto_pipeline");
        drop(seed);

        let barrier = Arc::new(Barrier::new(2));
        let writers = [7_u32, 2_u32]
            .into_iter()
            .map(|requested| {
                let db_path = db_path.clone();
                let run_id = run_id.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let backend =
                        SqliteJobRunStore::new(Store::open(&db_path).expect("store"), "ws_a");
                    barrier.wait();
                    backend.update_run_state(&run_id, &mut |_, state| {
                        thread::sleep(Duration::from_millis(20));
                        if !state.set_drain_worker_limit(
                            requested,
                            5,
                            format!("operator-{requested}"),
                            None,
                            Some(0),
                        ) {
                            return Err(OrbitError::JobRunControlConflict(format!(
                                "revision moved under {requested}"
                            )));
                        }
                        Ok(())
                    })
                })
            })
            .collect::<Vec<_>>();
        let outcomes = writers
            .into_iter()
            .map(|writer| writer.join().expect("writer thread"))
            .collect::<Vec<_>>();

        let winners = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Ok(RunStateUpdate::Updated)))
            .count();
        assert_eq!(winners, 1, "exactly one writer may win: {outcomes:?}");
        assert!(
            outcomes
                .iter()
                .any(|outcome| matches!(outcome, Err(OrbitError::JobRunControlConflict(_)))),
            "the loser is refused as a conflict: {outcomes:?}"
        );
        let stored = SqliteJobRunStore::new(Store::open(&db_path).expect("store"), "ws_a")
            .read_run_state(&run_id)
            .expect("read state")
            .expect("state exists");
        assert_eq!(stored.drain_worker_limit_revision(), 1);
    }

    /// [ORB-11310] Reproduce the stale-observation interleaving without a
    /// timing sleep: admission first observes the parent as eligible, then a
    /// stop transaction takes the SQLite writer lock and pauses before commit,
    /// and an independent connection attempts child admission while that lock
    /// is held. Stop commits first, so the serialized admission must observe
    /// the flag and create nothing.
    #[test]
    fn stop_acknowledgement_wins_over_a_stale_child_admission_observation() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let db_path = temp.path().join("orbit.db");
        let seed = SqliteJobRunStore::new(Store::open(&db_path).expect("seed store"), "ws_a");
        let parent_run_id = started_run_with_state(&seed, "workspace_auto_pipeline");
        assert!(
            !seed
                .read_run_state(&parent_run_id)
                .expect("initial eligibility read")
                .expect("parent state")
                .admissions_stopped(),
            "the admission path must first observe an eligible parent"
        );
        drop(seed);

        let (stop_holds_lock_tx, stop_holds_lock_rx) = mpsc::channel();
        let (release_stop_tx, release_stop_rx) = mpsc::channel();
        let stop_path = db_path.clone();
        let stop_parent = parent_run_id.clone();
        let stop = thread::spawn(move || {
            let backend =
                SqliteJobRunStore::new(Store::open(&stop_path).expect("stop store"), "ws_a");
            backend.update_run_state(&stop_parent, &mut |_, state| {
                state.set_drain_admissions_stop("operator".to_string(), None);
                stop_holds_lock_tx.send(()).expect("signal stop lock");
                release_stop_rx.recv().expect("release stop commit");
                Ok(())
            })
        });
        stop_holds_lock_rx.recv().expect("stop holds writer lock");

        let (admission_attempted_tx, admission_attempted_rx) = mpsc::channel();
        let admission_path = db_path.clone();
        let admission_parent = parent_run_id.clone();
        let admission = thread::spawn(move || {
            let backend = SqliteJobRunStore::new(
                Store::open(&admission_path).expect("admission store"),
                "ws_a",
            );
            admission_attempted_tx
                .send(())
                .expect("signal admission attempt");
            backend.admit_child_job_run(&child_admission(&admission_parent))
        });
        admission_attempted_rx
            .recv()
            .expect("admission attempted after stale read");

        release_stop_tx.send(()).expect("let stop acknowledge");
        assert_eq!(
            stop.join().expect("stop thread").expect("stop update"),
            RunStateUpdate::Updated
        );
        assert_eq!(
            admission
                .join()
                .expect("admission thread")
                .expect("admission result"),
            ChildJobRunAdmissionOutcome::AdmissionsStopped
        );

        let stored = SqliteJobRunStore::new(Store::open(&db_path).expect("verify store"), "ws_a");
        assert!(
            stored
                .list_job_runs("task_auto_pipeline")
                .expect("list children")
                .is_empty(),
            "no child may become durable after stop acknowledges"
        );
        let parent = stored
            .read_run_state(&parent_run_id)
            .expect("read parent")
            .expect("parent state");
        assert!(parent.admissions_stopped());
        assert!(parent.child_dispatches.is_empty());
    }

    #[test]
    fn a_child_admitted_before_stop_is_linked_and_left_running() {
        let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
        let parent_run_id = started_run_with_state(&backend, "workspace_auto_pipeline");

        let child = match backend
            .admit_child_job_run(&child_admission(&parent_run_id))
            .expect("admit child")
        {
            ChildJobRunAdmissionOutcome::Admitted(child) => child,
            ChildJobRunAdmissionOutcome::AdmissionsStopped
            | ChildJobRunAdmissionOutcome::Refused { .. } => panic!("parent was admitting"),
        };
        backend
            .update_run_state(&parent_run_id, &mut |_, state| {
                state.set_drain_admissions_stop("operator".to_string(), None);
                Ok(())
            })
            .expect("stop after admission");

        let parent = backend
            .read_run_state(&parent_run_id)
            .expect("read parent")
            .expect("parent state");
        assert!(parent.admissions_stopped());
        assert_eq!(parent.child_dispatches.len(), 1);
        assert_eq!(parent.child_dispatches[0].child_run_id, child.run_id);
        assert_eq!(
            parent.child_dispatches[0].phase,
            ChildDispatchPhase::Submitted
        );
        assert_eq!(
            backend
                .get_job_run(&child.run_id)
                .expect("read child")
                .expect("child run")
                .state,
            JobRunState::Pending,
            "stop is not cancellation"
        );
    }

    #[test]
    fn a_refused_terminal_parent_rolls_back_and_releases_the_writer_lock() {
        let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
        let parent_run_id = started_run_with_state(&backend, "workspace_auto_pipeline");
        backend
            .finalize_job_run(&parent_run_id, JobRunState::Success, Utc::now(), Some(1))
            .expect("finish parent");

        let error = backend
            .admit_child_job_run(&child_admission(&parent_run_id))
            .expect_err("terminal parent refuses admission");
        assert!(matches!(error, OrbitError::JobValidation(_)), "{error:?}");
        assert!(
            backend
                .list_job_runs("task_auto_pipeline")
                .expect("list children")
                .is_empty(),
            "the failed transaction must not leave a child"
        );

        backend
            .insert_job_run("unrelated_pipeline", 1, Utc::now(), None, None)
            .expect("a later writer proves the admission lock was released");
    }
}
