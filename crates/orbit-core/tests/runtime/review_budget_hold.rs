//! Exhausted review admission is an operator hold, with no second reservation.

use chrono::{Duration, Utc};
use orbit_core::metrics::reliability::ReliabilityWindow;
use orbit_engine::{ReviewReleaseRequest, ReviewerInvocationRequest, RuntimeHost};
use orbit_store::contracts::ReviewSettlement;
use orbit_types::workflow::{
    JobRunState, REVIEW_MANIFEST_ARTIFACT, ReviewLedger, ReviewVerdict, ReviewerInvocationEvent,
};
use serde_json::json;

use super::review_gate_audit::Fixture;

#[test]
fn exhausted_review_admission_holds_the_run_without_reserving_again() {
    if !super::dispatch_admission::isolated(
        "review_budget_hold::exhausted_review_admission_holds_the_run_without_reserving_again",
    ) {
        return;
    }
    // Exercise both admission refusals and the early minutes preflight. The
    // initial admission in each case is also the non-exhausted control.
    for (reviewed, preflight) in [(true, false), (false, false), (false, true)] {
        let mut fixture = Fixture::new_with_review_minutes(1);
        fixture.admit();
        let runtime = &fixture.runtime;
        let admission = &fixture.input["admission"];
        assert_eq!(admission["decision"], "admitted");
        assert_eq!(admission["attempt_index"], 1);
        let workspace = runtime.workspace_id().unwrap();
        let lineage = admission["lineage_key"].as_str().unwrap();
        let attempt = admission["attempt_id"].as_str().unwrap();
        let run_id = fixture.input["job_run_id"].as_str().unwrap();
        for event in [
            ReviewerInvocationEvent::Started,
            ReviewerInvocationEvent::Finished {
                runtime_seconds: if reviewed { 10 } else { 60 },
            },
        ] {
            RuntimeHost::record_reviewer_invocation(
                runtime,
                &ReviewerInvocationRequest {
                    run_id: run_id.into(),
                    lineage_key: lineage.into(),
                    attempt_id: attempt.into(),
                    event,
                },
            )
            .unwrap();
        }
        let store = runtime.review_store().unwrap();
        if reviewed {
            store
                .review_settle(
                    &workspace,
                    &ReviewSettlement {
                        lineage_key: lineage,
                        attempt_id: attempt,
                        verdict: ReviewVerdict::Accept,
                        now: Utc::now(),
                    },
                )
                .unwrap();
        } else {
            RuntimeHost::release_review_attempt(
                runtime,
                &ReviewReleaseRequest {
                    run_id: run_id.into(),
                    lineage_key: lineage.into(),
                    attempt_id: attempt.into(),
                },
            )
            .unwrap();
        }
        let ledger =
            || -> ReviewLedger { store.review_ledger(&workspace, lineage).unwrap().unwrap() };
        let before = ledger();
        let manifest = runtime
            .get_task_artifact(&fixture.task_id, REVIEW_MANIFEST_ARTIFACT)
            .unwrap()
            .unwrap();
        let status = runtime.get_task(&fixture.task_id).unwrap().status;
        let mut input = fixture.input.clone();
        input["preflight"] = json!(preflight);
        let refusal = runtime
            .run_deterministic("review_gate_admit", &json!({}), &input, Default::default())
            .expect_err("exhaustion stops admission");
        let reason = if reviewed {
            "review_candidate_reviewed"
        } else {
            "review_minutes_exhausted"
        };
        let message = refusal.to_string();
        assert!(
            message.contains(&format!("review_budget_exhausted: {reason}")),
            "{message}"
        );
        let recovery = format!(
            "an operator can run orbit task review-reset {} --lineage '{lineage}' \
             --reason '<decision>' before resuming, or dispatch a fresh delivery run",
            fixture.task_id
        );
        assert!(message.ends_with(&recovery), "{message}");

        // Run the very same admission through the persisted worker, so this
        // checks terminal run accounting rather than only the error variant.
        let jobs = runtime.paths().global_dir.join("resources/jobs");
        std::fs::create_dir_all(&jobs).unwrap();
        std::fs::write(
            jobs.join("task_pr_pipeline.yaml"),
            json!({
                "schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_pr_pipeline"},
                "spec": {"state": "enabled", "kind": "workflow", "steps": [{
                    "id": "review_gate_admit", "default_input": input,
                    "spec": {"type": "deterministic", "action": "review_gate_admit", "config": {}},
                }]},
            })
            .to_string(),
        )
        .unwrap();
        let executed = runtime.execute_pipeline_run_worker(run_id);
        let run = runtime.show_job_run(run_id).unwrap();
        assert_eq!(
            run.state,
            JobRunState::Held,
            "{reason}, preflight={preflight}: {executed:?}"
        );
        assert_eq!(
            run.steps.last().unwrap().error_code.as_deref(),
            Some("review_budget_exhausted")
        );
        assert!(
            run.steps
                .last()
                .unwrap()
                .error_message
                .as_deref()
                .unwrap()
                .contains(&message)
        );
        assert_eq!(ledger(), before, "no new review or budget reset");
        assert_eq!(
            runtime
                .get_task_artifact(&fixture.task_id, REVIEW_MANIFEST_ARTIFACT)
                .unwrap()
                .unwrap()
                .content,
            manifest.content,
            "no new review manifest"
        );
        assert_eq!(runtime.get_task(&fixture.task_id).unwrap().status, status);
        let metrics = runtime
            .pipeline_reliability(&ReliabilityWindow::ending_at(
                "fixture",
                Utc::now() + Duration::seconds(1),
                Duration::minutes(10),
            ))
            .unwrap();
        assert_eq!(metrics.job_runs.overall.held, 1);
        assert_eq!(metrics.job_runs.overall.failed, 0);

        #[cfg(unix)]
        {
            // The operator's existing recovery must still resume this held
            // local lineage. Substitute workers are owned and reaped before
            // the disposable runtime is dropped.
            store
                .review_reset(
                    &workspace,
                    &orbit_store::contracts::ReviewResetRequest {
                        lineage_key: lineage,
                        task_id: &fixture.task_id,
                        actor: "human:operator",
                        reason: "Authorize another review of this candidate",
                        budget: None,
                        now: Utc::now(),
                    },
                )
                .unwrap();
            let workers = super::worker_fixture::install_owned_workers(
                &fixture.repo.join("resume-worker-release"),
                fixture.repo.clone(),
            );
            let resumed = runtime
                .submit_resume_run(run_id, Some("operator"), None)
                .expect("review-reset then resume remains usable for a held local run");
            runtime
                .execute_pipeline_run_worker(&resumed.run_id)
                .unwrap();
            let next = runtime.show_job_run(&resumed.run_id).unwrap();
            assert_eq!(next.state, JobRunState::Success, "{:?}", next.steps);
            if !preflight {
                assert_eq!(ledger().attempts.len(), 2);
            }
            drop(workers);
        }
    }
}
