//! Fulfilment attempt decisions survive the newest-run scan window.

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Duration, Utc};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::JobRunQuery;
use orbit_types::workflow::{JobRun, JobRunState, PipelineState, ReviewEvidenceHold};
use serde_json::{Value, json};

use super::{CODEQL, Fixture, REVIEW_EVIDENCE_FULFILMENT_JOB, Stub, UNREACHABLE_MIB};
use super::{assert_still_held, configure_job, fulfil_once, held};

fn hold_key(hold: &ReviewEvidenceHold) -> String {
    format!("{}:{}", hold.attempt_id, hold.candidate.commit)
}

/// Seed a persisted run and optional outcome, without starting a worker.
fn seed_run(fixture: &Fixture, input: Value, at: DateTime<Utc>, output: Option<Value>) {
    let mut run = fixture
        .runtime
        .insert_job_run(
            REVIEW_EVIDENCE_FULFILMENT_JOB,
            1,
            at,
            Some(input.clone()),
            None,
        )
        .unwrap();
    run.created_at = at;
    run.finished_at = Some(at);
    run.state = if output.is_some() {
        JobRunState::Success
    } else {
        JobRunState::Interrupted
    };
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), input);
    if let Some(output) = output {
        state.pipeline["fulfil"] = output;
    }
    fixture
        .runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&fixture.runtime.workspace_id().unwrap(), &run, Some(&state))
        .unwrap();
}

fn recent_runs(fixture: &Fixture) -> Vec<JobRun> {
    fixture
        .runtime
        .sqlite_store()
        .unwrap()
        .list_job_runs_for_workspace(
            &fixture.runtime.workspace_id().unwrap(),
            &JobRunQuery {
                job_id: Some(REVIEW_EVIDENCE_FULFILMENT_JOB.into()),
                limit: Some(200),
                include_steps: false,
                ..Default::default()
            },
        )
        .unwrap()
}

/// Explicit timestamps age the hold out, without sleeps or competing work.
/// Include different holds of this same task to guard against substituting
/// a bounded task history for the bounded job history. One transaction writes
/// the whole window; the ids stay outside the minted `jrun-` sequence.
fn age_out(fixture: &Fixture, key: &str) {
    static BATCH: AtomicU64 = AtomicU64::new(0);
    let batch = BATCH.fetch_add(1, Ordering::Relaxed);
    let now = Utc::now();
    let job_id = REVIEW_EVIDENCE_FULFILMENT_JOB.to_string();
    let mut runs = Vec::with_capacity(201);
    let mut states = Vec::with_capacity(201);
    for index in 0..201 {
        let at = now + Duration::milliseconds(index);
        let input = json!({
            "task_id": fixture.task_id,
            "hold_key": format!("other-hold-{batch}-{index}"),
        });
        let run_id = format!("age-out-{batch}-{index:04}");
        let mut state = PipelineState::new(run_id.clone(), job_id.clone(), input.clone());
        state.pipeline["fulfil"] = json!({"retryable": false, "reason": "findings_reported"});
        runs.push(JobRun {
            build: None,
            executed_on: None,
            run_id,
            job_id: job_id.clone(),
            attempt: 1,
            state: JobRunState::Success,
            scheduled_at: at,
            started_at: None,
            finished_at: Some(at),
            duration_ms: None,
            created_at: at,
            pid: None,
            pid_start_time: None,
            input: Some(input),
            retry_source_run_id: None,
            knowledge_metrics: None,
            resolved_crew: None,
            crew_model: None,
            steps: Vec::new(),
        });
        states.push(state);
    }
    let paired: Vec<_> = runs
        .iter()
        .zip(states.iter())
        .map(|(run, state)| (run, Some(state)))
        .collect();
    let workspace_id = fixture.runtime.workspace_id().unwrap();
    fixture
        .runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_runs_for_workspace(&workspace_id, &paired)
        .unwrap();
    let window = recent_runs(fixture);
    assert_eq!(window.len(), 200);
    assert!(
        window
            .iter()
            .all(|run| run.input.as_ref().unwrap()["hold_key"] != key)
    );
}

fn a_final_fulfilment_outcome_stays_closed_after_aging_out() {
    let probe = orbit_exec::probe_bwrap();
    if !probe.available {
        orbit_exec::report_bwrap_deferral(
            "a_final_fulfilment_outcome_stays_closed_after_aging_out",
            &probe.detail,
        );
        return;
    }
    let (fixture, hold) = held(Stub::Clean, CODEQL);
    configure_job(&fixture, 1, None);
    let key = hold_key(&hold);
    seed_run(
        &fixture,
        json!({"task_id": fixture.task_id, "hold_key": key, "attempt_key": format!("{key}#1")}),
        Utc::now() - Duration::hours(1),
        Some(json!({"fulfilled": false, "retryable": false, "reason": "findings_reported"})),
    );
    age_out(&fixture, &key);
    let before = recent_runs(&fixture);
    let tick = fixture
        .runtime
        .run_review_evidence_fulfilment_tick(Utc::now())
        .unwrap();
    assert!(tick.skipped.is_none(), "{tick:?}");
    assert!(
        tick.dispatched.is_empty(),
        "a final outcome cannot reopen: {tick:?}"
    );
    assert_eq!(recent_runs(&fixture), before, "the tick inserts no run");
    assert_still_held(&fixture, "the aged-out final outcome");
}

fn retryable_and_missing_outcomes_keep_their_budget_after_aging_out() {
    let probe = orbit_exec::probe_bwrap();
    if !probe.available {
        orbit_exec::report_bwrap_deferral(
            "retryable_and_missing_outcomes_keep_their_budget_after_aging_out",
            &probe.detail,
        );
        return;
    }
    // `candidate_unreachable` recorded a different reason string and otherwise
    // repeated this row. The job still reports `disk_insufficient`.
    for output in [
        Some(json!({"retryable": true, "reason": "disk_insufficient"})),
        None,
    ] {
        let (fixture, hold) = held(Stub::Clean, CODEQL);
        configure_job(&fixture, 1, Some(UNREACHABLE_MIB));
        let key = hold_key(&hold);
        seed_run(
            &fixture,
            json!({"task_id": fixture.task_id, "hold_key": key, "attempt_key": format!("{key}#1")}),
            Utc::now() - Duration::hours(1),
            output.clone(),
        );
        for attempt in 2..=3 {
            age_out(&fixture, &key);
            let (run_id, result) = fulfil_once(&fixture);
            let run = fixture.runtime.get_job_run(&run_id).unwrap().unwrap();
            assert_eq!(
                run.input.unwrap()["attempt_key"],
                format!("{key}#{attempt}"),
                "{output:?}"
            );
            assert_eq!(result["reason"], "disk_insufficient");
            assert_eq!(result["retryable"], true);
        }
        age_out(&fixture, &key);
        let tick = fixture
            .runtime
            .run_review_evidence_fulfilment_tick(Utc::now())
            .unwrap();
        assert!(tick.skipped.is_none(), "{tick:?}");
        assert!(
            tick.dispatched.is_empty(),
            "three attempts exhaust {output:?}: {tick:?}"
        );
        assert_still_held(&fixture, "the aged-out exhausted budget");
    }
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_final_fulfilment_outcome_stays_closed_after_aging_out_scenarios,
    a_final_fulfilment_outcome_stays_closed_after_aging_out,
    retryable_and_missing_outcomes_keep_their_budget_after_aging_out
);
