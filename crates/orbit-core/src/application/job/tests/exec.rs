//! Deterministic final-summary faults at the shared crate-private finalizer
//! (unit admission criterion 2). Targeting this seam keeps worker checkpoint
//! writes available and exercises unsuccessful outcomes without a provider.

use chrono::Utc;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    JobRunState, PipelineState, ReviewEvidenceHold, ReviewEvidenceKind, ReviewEvidenceRequirement,
};
use serde_json::{Value, json};

use super::super::exec::{V2JobRunResult, V2RunFinalizationOptions};
use crate::OrbitRuntime;

#[derive(Clone, Copy, Debug)]
enum SummaryFault {
    State,
    Diagnostic,
}

#[test]
fn held_and_unsuccessful_runs_finalize_despite_summary_store_faults() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &held_and_unsuccessful_runs_finalize_despite_summary_store_faults,
    )) {
        return;
    }
    for options in [
        V2RunFinalizationOptions::DIRECT,
        V2RunFinalizationOptions::DETACHED_WORKER,
    ] {
        for final_state in [JobRunState::Held, JobRunState::Failed] {
            for fault in [SummaryFault::State, SummaryFault::Diagnostic] {
                let root = tempfile::tempdir().unwrap();
                let global = root.path().join("global");
                let workspace = root.path().join("repo/.orbit");
                std::fs::create_dir_all(&global).unwrap();
                std::fs::create_dir_all(&workspace).unwrap();
                let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
                let started = Utc::now();
                let input = json!({});
                let run = runtime
                    .stores()
                    .jobs()
                    .insert_job_run(
                        "finalization_fixture",
                        1,
                        started,
                        Some(input.clone()),
                        None,
                    )
                    .unwrap();
                runtime
                    .stores()
                    .jobs()
                    .mark_job_run_running(&run.run_id, started, std::process::id())
                    .unwrap();
                let seeded =
                    PipelineState::new(run.run_id.clone(), run.job_id.clone(), input.clone());
                runtime.write_run_state(&run.run_id, &seeded).unwrap();
                let held = final_state == JobRunState::Held;
                let hold = held.then(|| ReviewEvidenceHold {
                    schema_version: 1,
                    attempt_id: "fixture-attempt".into(),
                    lineage_key: "fixture-lineage".into(),
                    run_id: run.run_id.clone(),
                    candidate: SourceRevision {
                        commit: "fixture-commit".into(),
                        tree: "fixture-tree".into(),
                    },
                    task_meaning_digest: "fixture-meaning".into(),
                    task_spec_digest: None,
                    requirements: vec![ReviewEvidenceRequirement {
                        kind: ReviewEvidenceKind::NativeOs,
                        name: "Native fixture check".into(),
                        command: "native fixture".into(),
                        artifact: "evidence/native.json".into(),
                        os: None,
                    }],
                });
                let message = "candidate validation failed with its original cause";
                let pipeline = if held {
                    json!({"inspect": {"gate": "awaiting_evidence", "evidence_hold": hold}})
                } else {
                    json!({"inspect": {"message": message}})
                };
                let result = V2JobRunResult {
                    run_id: run.run_id.clone(),
                    job_name: run.job_id.clone(),
                    success: false,
                    pipeline,
                    evidence_hold: hold,
                    forge_hold: None,
                    message: (!held).then(|| message.into()),
                    events_emitted: 0,
                };
                let database = orbit_config::resolved_audit_db_path(
                    &orbit_config::ConfigRoots::new(&global, &workspace),
                )
                .unwrap();
                let connection = rusqlite::Connection::open(database).unwrap();
                // Run state lives in its own table, so the terminal write on
                // `job_runs` is unaffected. Reject only the summary change.
                connection
                    .execute_batch(match fault {
                        SummaryFault::State => {
                            "CREATE TRIGGER fail_summary BEFORE UPDATE OF pipeline_state_json \
                             ON job_run_states \
                             WHEN NEW.pipeline_state_json IS NOT OLD.pipeline_state_json \
                             BEGIN SELECT RAISE(ABORT, 'injected state failure'); END;"
                        }
                        SummaryFault::Diagnostic => {
                            "CREATE TRIGGER fail_summary BEFORE INSERT ON job_run_steps \
                             BEGIN SELECT RAISE(ABORT, 'injected diagnostic failure'); END;"
                        }
                    })
                    .unwrap();
                let log_path = root.path().join("warnings.jsonl");
                let log = std::fs::File::create(&log_path).unwrap();
                let subscriber = tracing_subscriber::fmt()
                    .json()
                    .without_time()
                    .with_max_level(tracing::Level::WARN)
                    .with_writer(move || log.try_clone().unwrap())
                    .finish();
                tracing::subscriber::with_default(subscriber, || {
                    runtime.finalize_v2_pipeline_run(
                        &run,
                        &input,
                        started,
                        Utc::now(),
                        Ok(&result),
                        options,
                    )
                })
                .expect("ORB-14525: a summary fault must not escape before terminal finalization");

                let stored = runtime.get_job_run_backend(&run.run_id).unwrap().unwrap();
                assert_eq!(stored.state, final_state, "{options:?}, {fault:?}");
                assert!(stored.finished_at.is_some());
                match fault {
                    SummaryFault::State => {
                        assert_eq!(
                            runtime.read_run_state(&run.run_id).unwrap().unwrap(),
                            seeded
                        );
                        assert_eq!(stored.steps.len(), 1);
                        assert_eq!(stored.steps[0].state, final_state);
                        if held {
                            assert_eq!(
                                stored.steps[0].error_code.as_deref(),
                                Some("review_awaiting_evidence")
                            );
                            assert!(
                                stored.steps[0]
                                    .error_message
                                    .as_ref()
                                    .is_some_and(|detail| !detail.is_empty())
                            );
                        } else {
                            assert_eq!(stored.steps[0].error_message.as_deref(), Some(message));
                        }
                    }
                    SummaryFault::Diagnostic => {
                        assert!(stored.steps.is_empty(), "the diagnostic fault must fire");
                        assert_eq!(
                            runtime
                                .read_run_state(&run.run_id)
                                .unwrap()
                                .unwrap()
                                .pipeline,
                            result.pipeline
                        );
                    }
                }
                let (operation, error) = match (held, fault) {
                    (true, SummaryFault::State) => {
                        ("persist held run state", "injected state failure")
                    }
                    (false, SummaryFault::State) => {
                        ("persist failed run state", "injected state failure")
                    }
                    (true, SummaryFault::Diagnostic) => {
                        ("record held step", "injected diagnostic failure")
                    }
                    (false, SummaryFault::Diagnostic) => {
                        ("record failure step", "injected diagnostic failure")
                    }
                };
                let events: Vec<Value> = std::fs::read_to_string(&log_path)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                assert!(
                    events.iter().any(|event| {
                        event["level"] == "WARN"
                            && event["target"] == "orbit.core.job_run"
                            && event["fields"]["run_id"] == run.run_id
                            && event["fields"]["operation"] == operation
                            && event["fields"]["error"]
                                .as_str()
                                .is_some_and(|detail| detail.contains(error))
                    }),
                    "the rejected summary must emit a warning correlated with its run: {events:?}"
                );
            }
        }
    }
}
