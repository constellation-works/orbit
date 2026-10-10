//! Run finalization through foreground replay and the detached worker's public
//! entrypoint. SQLite faults target final summaries, leaving execution and the
//! required terminal-state write available [ORB-14361].

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use orbit_core::OrbitRuntime;
use orbit_store::contracts::JobRunStoreBackend;
use orbit_types::workflow::{JobRun, JobRunState, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
    job: PathBuf,
    database: PathBuf,
}

impl Fixture {
    fn new(with_step: bool) -> Self {
        let root = TempDir::new().unwrap();
        let global = root.path().join("global");
        let workspace = root.path().join("repo/.orbit");
        let jobs_dir = global.join("resources/jobs");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            workspace.join("config.toml"),
            "[workflow]\ndefault_crew = \"fixture\"\n\n\
             [crews.fixture]\nprovider = \"codex\"\nmodel = \"fixture-model\"\n",
        )
        .unwrap();
        let job = jobs_dir.join("finalization_fixture.yaml");
        let steps = if with_step {
            json!([{"id": "inspect", "spec": {
                "type": "deterministic", "action": "list_backlog_tasks", "config": {}
            }}])
        } else {
            json!([])
        };
        std::fs::write(
            &job,
            serde_json::to_string(&json!({
                "schemaVersion": 2, "kind": "Job",
                "metadata": {"name": "finalization_fixture"},
                "spec": {"state": "enabled", "steps": steps}
            }))
            .unwrap(),
        )
        .unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        let database = orbit_config::resolved_audit_db_path(&orbit_config::ConfigRoots::new(
            &global, &workspace,
        ))
        .unwrap();
        Self {
            _root: root,
            runtime,
            jobs,
            job,
            database,
        }
    }

    fn queued(&self, input: Value) -> JobRun {
        self.jobs
            .insert_job_run("finalization_fixture", 1, Utc::now(), Some(input), None)
            .unwrap()
    }
}

#[test]
fn foreground_replay_setup_failure_finalizes_with_its_original_cause() {
    if !super::dispatch_admission::isolated(
        "job_finalization::foreground_replay_setup_failure_finalizes_with_its_original_cause",
    ) {
        return;
    }
    let fixture = Fixture::new(false);
    // A historical run can retain a crew removed from today's configuration.
    let source = fixture.queued(json!({"crew": "removed-crew"}));
    fixture
        .jobs
        .mark_job_run_running(&source.run_id, Utc::now(), std::process::id())
        .unwrap();
    fixture
        .jobs
        .finalize_job_run(&source.run_id, JobRunState::Success, Utc::now(), Some(0))
        .unwrap();

    let error = fixture.runtime.replay_job_run(&source.run_id).unwrap_err();
    assert!(error.to_string().contains("removed-crew"), "{error}");
    let runs = fixture.runtime.list_job_runs(Default::default()).unwrap();
    let replay = runs
        .iter()
        .find(|run| run.retry_source_run_id.as_deref() == Some(source.run_id.as_str()))
        .expect("replay persisted before setup");
    let replay = fixture.jobs.get_job_run(&replay.run_id).unwrap().unwrap();
    assert_eq!(replay.state, JobRunState::Failed);
    assert!(replay.started_at.is_some());
    assert!(replay.finished_at.is_some());
    assert_eq!(replay.steps.len(), 1);
    assert_eq!(replay.steps[0].state, JobRunState::Failed);
    assert_eq!(replay.steps[0].error_message, Some(error.to_string()));
    assert_eq!(
        fixture
            .jobs
            .get_job_run(&source.run_id)
            .unwrap()
            .unwrap()
            .state,
        JobRunState::Success
    );
}

#[derive(Clone, Copy, Debug)]
enum SummaryFault {
    Pipeline,
    Step,
}

#[test]
fn successful_runs_stay_successful_when_final_summary_writes_fail() {
    if !super::dispatch_admission::isolated(
        "job_finalization::successful_runs_stay_successful_when_final_summary_writes_fail",
    ) {
        return;
    }
    // Empty workers exercise the synthetic fallback; a real activity exercises
    // the audit-derived worker summary. Pipeline faults use empty jobs so no
    // execution checkpoint is affected.
    for (worker, with_step, fault) in [
        (false, false, SummaryFault::Pipeline),
        (false, false, SummaryFault::Step),
        (true, false, SummaryFault::Pipeline),
        (true, false, SummaryFault::Step),
        (true, true, SummaryFault::Step),
    ] {
        let fixture = Fixture::new(with_step);
        let queued = worker.then(|| {
            let run = fixture.queued(json!({}));
            let state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
            fixture.jobs.write_run_state(&run.run_id, &state).unwrap();
            (run, state)
        });
        let connection = rusqlite::Connection::open(&fixture.database).unwrap();
        connection
            .execute_batch(match fault {
                SummaryFault::Pipeline => {
                    "CREATE TRIGGER fail_final_summary BEFORE UPDATE ON job_run_states \
                     WHEN NEW.pipeline_state_json IS NOT OLD.pipeline_state_json \
                     AND (SELECT state FROM job_runs WHERE workspace_id = NEW.workspace_id \
                          AND run_id = NEW.run_id) = 'running' \
                     BEGIN SELECT RAISE(ABORT, 'injected pipeline summary failure'); END;"
                }
                SummaryFault::Step => {
                    "CREATE TRIGGER fail_final_summary BEFORE INSERT ON job_run_steps \
                     WHEN NEW.state = 'success' \
                     BEGIN SELECT RAISE(ABORT, 'injected step summary failure'); END;"
                }
            })
            .unwrap();

        let run_id = if let Some((run, _)) = &queued {
            fixture
                .runtime
                .execute_pipeline_run_worker(&run.run_id)
                .unwrap();
            run.run_id.clone()
        } else {
            let result = fixture
                .runtime
                .run_job_v2_from_yaml(&fixture.job, json!({}))
                .unwrap();
            assert!(result.success);
            result.run_id
        };
        let stored = fixture.jobs.get_job_run(&run_id).unwrap().unwrap();
        assert_eq!(
            stored.state,
            JobRunState::Success,
            "worker={worker}, {fault:?}"
        );
        assert!(stored.finished_at.is_some());
        match fault {
            SummaryFault::Pipeline => {
                let state = fixture.jobs.read_run_state(&run_id).unwrap().unwrap();
                if let Some((_, seeded)) = queued {
                    assert_eq!(state, seeded, "the worker's final snapshot write was lost");
                } else {
                    assert!(
                        state.step_states.is_empty(),
                        "the legacy summary write was lost"
                    );
                }
                assert_eq!(
                    stored.steps.len(),
                    1,
                    "step summary still attempted after snapshot failure"
                );
            }
            SummaryFault::Step => {
                assert!(
                    stored.steps.is_empty(),
                    "the fault must actually prevent summary storage"
                );
                if with_step {
                    let state = fixture.jobs.read_run_state(&run_id).unwrap().unwrap();
                    assert_eq!(state.step_states.get(&0), Some(&JobRunState::Success));
                    assert!(state.pipeline.get("inspect").is_some());
                }
            }
        }
    }
}
