//! Deterministic final-summary faults at the shared crate-private finalizer
//! (unit admission criterion 2). Targeting this seam keeps worker checkpoint
//! writes available and exercises unsuccessful outcomes without a provider.
//! The same seam finalizes a claimed leaf, which the boundary executes only
//! under an owner-bound worker binding it refuses to fabricate (criterion 3).

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
                        hosted_check: None,
                    }],
                    published_ref: None,
                    candidate_carry: None,
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
                    child_cancelled: false,
                    message: (!held).then(|| message.into()),
                    failure_code: None,
                    parked: None,
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

/// [ORB-15517] A claimed leaf its provider's usage limit stopped holds on
/// the provider and its reset, through either finalization branch, and
/// records the deferral Incidents lists. Owner-local provider holds have
/// public-worker coverage in the runtime integration tests. Targets the
/// finalizer because only an owner-bound worker can execute a claimed leaf,
/// which no crate-root fixture can stand up.
#[test]
fn a_claimed_leaf_provider_limit_holds_and_records_a_deferral() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &a_claimed_leaf_provider_limit_holds_and_records_a_deferral,
    )) {
        return;
    }
    let limit = "step `implement` failed: [provider_limit] provider=codex \
                 resets_at=2026-10-09T22:42:00Z cli subprocess exited with code 1: You've hit \
                 your usage limit.";
    for job in ["task_claimed_local_pipeline", "task_claimed_pr_pipeline"] {
        for as_error in [false, true] {
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
                .insert_job_run(job, 1, started, Some(input.clone()), None)
                .unwrap();
            runtime
                .stores()
                .jobs()
                .mark_job_run_running(&run.run_id, started, std::process::id())
                .unwrap();
            let seeded = PipelineState::new(run.run_id.clone(), run.job_id.clone(), input.clone());
            runtime.write_run_state(&run.run_id, &seeded).unwrap();
            let result = V2JobRunResult {
                run_id: run.run_id.clone(),
                job_name: run.job_id.clone(),
                success: false,
                pipeline: json!({}),
                evidence_hold: None,
                forge_hold: None,
                child_cancelled: false,
                message: Some(limit.into()),
                failure_code: None,
                parked: None,
                events_emitted: 0,
            };
            let error = crate::OrbitError::Execution(limit.into());
            let outcome = if as_error { Err(&error) } else { Ok(&result) };
            runtime
                .finalize_v2_pipeline_run(
                    &run,
                    &input,
                    started,
                    Utc::now(),
                    outcome,
                    V2RunFinalizationOptions::DETACHED_WORKER,
                )
                .unwrap();

            let stored = runtime.get_job_run_backend(&run.run_id).unwrap().unwrap();
            let case = format!("{job}, as_error={as_error}: {:?}", stored.steps);
            let deferred = runtime
                .audit_failure_incidents(&orbit_store::contracts::FailureIncidentQuery::default())
                .unwrap()
                .incidents
                .into_iter()
                .filter(|incident| {
                    incident.surface == orbit_store::contracts::RUN_DEFERRED_SURFACE
                        && incident.run_ids.contains(&run.run_id)
                })
                .map(|incident| incident.class)
                .collect::<Vec<_>>();
            assert_eq!(stored.state, JobRunState::Held, "{case}");
            let step = stored.steps.last().expect("held diagnostic step");
            assert_eq!(step.error_code.as_deref(), Some("provider_limit"), "{case}");
            let text = step.error_message.as_deref().unwrap_or_default();
            assert!(
                text.contains("`codex`") && text.contains("2026-10-09T22:42:00Z"),
                "the hold names the provider and its reset: {case}"
            );
            assert_eq!(
                orbit_types::workflow::failed_provider(text),
                Some("codex"),
                "claim settlement still reads the provider: {case}"
            );
            assert_eq!(
                deferred,
                [orbit_store::contracts::FailureClass::Deferred],
                "{case}"
            );
        }
    }
}

#[test]
fn code_only_hold_is_audited_held_and_finalizes_held() {
    use orbit_common::RemoteToolCode;
    use orbit_engine::JobOutcome;
    use orbit_types::workflow::StepFailureCode;

    use super::super::exec::audit_terminal_outcome;

    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &code_only_hold_is_audited_held_and_finalizes_held,
    )) {
        return;
    }
    let message = "the operator has not recorded approval";
    let outcome = JobOutcome {
        success: false,
        evidence_hold: None,
        forge_hold: None,
        child_cancelled: false,
        pipeline: json!({}),
        message: Some(message.into()),
        failure_code: Some(StepFailureCode::OwnerSignoffRequired),
        audit_failures: 0,
        degraded_audit: false,
        telemetry_failures: 0,
        degraded_telemetry: false,
        parked: None,
    };
    let (label, _) = audit_terminal_outcome("task_pr_pipeline", &Ok(outcome));
    assert_eq!(label, "held", "a typed hold code is audited held");

    let error = crate::OrbitError::RemoteTool {
        code: RemoteToolCode::Other("baseline_red".into()),
        message: "required validation failed on this candidate".into(),
        payload: json!({}),
    };
    let (label, _) = audit_terminal_outcome("task_pr_pipeline", &Err(error));
    assert_eq!(label, "held", "a typed error code is audited held");

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
        .insert_job_run("task_pr_pipeline", 1, started, Some(input.clone()), None)
        .unwrap();
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, started, std::process::id())
        .unwrap();
    runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id.clone(), input.clone()),
        )
        .unwrap();
    let result = V2JobRunResult {
        run_id: run.run_id.clone(),
        job_name: run.job_id.clone(),
        success: false,
        pipeline: json!({}),
        evidence_hold: None,
        forge_hold: None,
        child_cancelled: false,
        message: Some(message.into()),
        failure_code: Some(StepFailureCode::OwnerSignoffRequired),
        parked: None,
        events_emitted: 0,
    };
    runtime
        .finalize_v2_pipeline_run(
            &run,
            &input,
            started,
            Utc::now(),
            Ok(&result),
            V2RunFinalizationOptions::DETACHED_WORKER,
        )
        .unwrap();
    let stored = runtime.get_job_run_backend(&run.run_id).unwrap().unwrap();
    assert_eq!(stored.state, JobRunState::Held);
}

#[test]
fn detached_worker_persists_unrecognized_audit_step_outcomes_as_failed() {
    use orbit_engine::V2AuditWriter;
    use orbit_types::workflow::activity_job::V2AuditEventKind;

    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &detached_worker_persists_unrecognized_audit_step_outcomes_as_failed,
    )) {
        return;
    }
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
        .insert_job_run("task_pr_pipeline", 1, started, Some(input.clone()), None)
        .unwrap();
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, started, std::process::id())
        .unwrap();
    runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id.clone(), input.clone()),
        )
        .unwrap();
    let writer = V2AuditWriter::with_disk_sinks(
        &runtime.paths().audit_dir,
        runtime.v2_audit_store().unwrap(),
        runtime.workspace_id().unwrap(),
        &run.run_id,
        "fixture",
        Some(runtime.paths().repo_root.as_path()),
    )
    .unwrap();
    let emit = |kind| writer.emit(kind).unwrap();
    emit(V2AuditEventKind::StepStarted {
        step_id: "implement".into(),
    });
    emit(V2AuditEventKind::StepFinished {
        step_id: "implement".into(),
        lock_wait_ms: None,
        outcome: "not-a-state".into(),
        error_diagnostic: None,
        error_message: None,
    });
    emit(V2AuditEventKind::StepStarted {
        step_id: "check".into(),
    });
    emit(V2AuditEventKind::StepFinished {
        step_id: "check".into(),
        lock_wait_ms: None,
        outcome: "finished".into(),
        error_diagnostic: None,
        error_message: None,
    });
    emit(V2AuditEventKind::StepStarted {
        step_id: "hanging".into(),
    });
    let result = V2JobRunResult {
        run_id: run.run_id.clone(),
        job_name: run.job_id.clone(),
        success: true,
        pipeline: json!({}),
        evidence_hold: None,
        forge_hold: None,
        child_cancelled: false,
        message: None,
        failure_code: None,
        parked: None,
        events_emitted: 0,
    };
    runtime
        .finalize_v2_pipeline_run(
            &run,
            &input,
            started,
            Utc::now(),
            Ok(&result),
            V2RunFinalizationOptions::DETACHED_WORKER,
        )
        .unwrap();
    let stored = runtime.get_job_run_backend(&run.run_id).unwrap().unwrap();
    let state_of = |id: &str| {
        stored
            .steps
            .iter()
            .find(|step| step.target_id == id)
            .unwrap_or_else(|| panic!("missing step {id}: {:?}", stored.steps))
            .state
    };
    assert_eq!(state_of("implement"), JobRunState::Failed);
    assert_eq!(state_of("check"), JobRunState::Success);
    assert_eq!(state_of("hanging"), JobRunState::Failed);
}
