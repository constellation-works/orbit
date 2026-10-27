//! Completion refusals through the real provider adapter, worker and finalizer.

use std::os::unix::fs::PermissionsExt;

use chrono::{Duration, Utc};
use orbit_core::TaskStatus;
use orbit_core::application::job::JobRunListParams;
use orbit_core::metrics::reliability::ReliabilityWindow;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::workflow::{DeliveryRefusal, JobRunState, PipelineState};
use serde_json::{Value, json};

use super::provider_failure_hold::fixture;

#[test]
fn completion_refusals_hold_the_run_and_pass_its_parents() {
    if !super::dispatch_admission::isolated(
        "delivery_holds::completion_refusals_hold_the_run_and_pass_its_parents",
    ) {
        return;
    }
    for (case, patch, refusal, diagnostic) in [
        (
            "head_inprogress",
            json!({"headRefOid":"other-head"}),
            Some(DeliveryRefusal::HeadChanged),
            "reports headRefOid",
        ),
        (
            "head_changed",
            json!({"headRefOid": "other-head"}),
            Some(DeliveryRefusal::HeadChanged),
            "reports headRefOid",
        ),
        (
            "draft",
            json!({"isDraft": true}),
            Some(DeliveryRefusal::PullRequestDraft),
            "still a draft",
        ),
        (
            "closed",
            json!({"state": "CLOSED"}),
            Some(DeliveryRefusal::PullRequestClosed),
            "closed without being merged",
        ),
        (
            "review",
            json!({"mergeStateStatus": "BLOCKED", "reviewDecision": "REVIEW_REQUIRED", "statusCheckRollup": []}),
            Some(DeliveryRefusal::RequirementsUnsatisfied),
            "review is required",
        ),
        (
            "not_promotable",
            json!({}),
            Some(DeliveryRefusal::TaskNotPromotable),
            "not promotable",
        ),
        (
            "no_head",
            json!({"headRefOid": null}),
            None,
            "did not report headRefOid",
        ),
        (
            "forge_read",
            json!({}),
            None,
            "error connecting to api.github.com",
        ),
        (
            "candidate_red",
            json!({"mergeStateStatus": "BLOCKED", "reviewDecision": "APPROVED", "statusCheckRollup": [{"name": "test", "status": "COMPLETED", "conclusion": "FAILURE"}]}),
            None,
            "status check 'test' failed",
        ),
        (
            "candidate_red_review",
            json!({"mergeStateStatus":"BLOCKED", "reviewDecision":"REVIEW_REQUIRED", "statusCheckRollup":[{"name":"test", "status":"COMPLETED", "conclusion":"FAILURE"}]}),
            None,
            "review is required",
        ),
        (
            "contradictory",
            json!({"mergedAt": "2026-10-25T00:00:00Z"}),
            None,
            "contradictory merge state",
        ),
        (
            "merge_wait",
            json!({"mergeStateStatus": "PENDING", "statusCheckRollup": [{"name": "test", "status": "IN_PROGRESS", "conclusion": null}]}),
            Some(DeliveryRefusal::CompletionBudgetExpired),
            "timed out after",
        ),
        (
            "deadline_parked",
            json!({"mergeStateStatus": "BLOCKED", "reviewDecision": "APPROVED", "statusCheckRollup": [{"name": "test", "status": "IN_PROGRESS", "conclusion": null}]}),
            Some(DeliveryRefusal::CompletionBudgetExpired),
            "completion deadline expired",
        ),
    ] {
        completion_outcome(case, patch, refusal, diagnostic);
    }
}

fn completion_outcome(
    case: &str,
    patch: Value,
    refusal: Option<DeliveryRefusal>,
    diagnostic: &str,
) {
    let fx = fixture(
        "[workflow]\ndefault_crew = \"fixture\"\n\n[crews.fixture]\nprovider = \"codex\"\nmodel = \"fixture\"\n",
    );
    let task = fx.task("fixture");
    let bin = std::path::PathBuf::from(std::env::var("HOME").unwrap()).join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let mut paths = vec![bin.clone()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    // This fixture runs alone in an isolated child.
    unsafe {
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    }
    let status = fx.repo.join("status.json");
    let mut observation =
        json!({"state": "OPEN", "mergeStateStatus": "CLEAN", "headRefOid": "published-head"});
    observation
        .as_object_mut()
        .unwrap()
        .extend(patch.as_object().unwrap().clone());
    std::fs::write(&status, observation.to_string()).unwrap();
    let gh = bin.join("gh");
    std::fs::write(&gh, format!(
        "#!/bin/sh\nset -eu\ncase \"$1 $2\" in\n'pr view') touch '{}'; cat '{}' ;;\n*) echo \"unexpected forge call: $*\" >&2; exit 1 ;;\nesac\n",
        fx.repo.join("deadline-hit").display(),
        status.display(),
    )).unwrap();
    if case == "forge_read" {
        std::fs::write(
            &gh,
            "#!/bin/sh\necho 'error connecting to api.github.com' >&2\nexit 1\n",
        )
        .unwrap();
    }
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let jobs = fx.runtime.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    let job = jobs.join("delivery_hold_fixture.yaml");
    std::fs::write(&job, json!({
        "schemaVersion": 2, "kind": "Job", "metadata": {"name": "delivery_hold_fixture"},
        "spec": {"state": "enabled", "steps": [{
            "id": "complete", "spec": {"type": "deterministic", "action": "pr_complete", "config": {}},
            "default_input": {
                "workspace_path": "{{ input.workspace_path }}",
                "completed_task_ids": "{{ input.task_ids }}", "pr_number": "42",
                "published_head_sha": "published-head",
                "max_wait_seconds": if case == "deadline_parked" { 600 } else { 0 },
            },
        }]},
    }).to_string()).unwrap();
    let task_status = match case {
        "not_promotable" => TaskStatus::Blocked,
        "head_inprogress" => TaskStatus::InProgress,
        _ => TaskStatus::Review,
    };
    let input = json!({"workspace_path": fx.repo, "task_ids": [task]});
    let run = fx
        .runtime
        .insert_job_run(
            "delivery_hold_fixture",
            1,
            Utc::now(),
            Some(input.clone()),
            None,
        )
        .unwrap();
    fx.runtime
        .apply_task_automation_update(
            &task,
            TaskAutomationUpdate {
                status: Some(task_status),
                job_run_id: Some(run.run_id.clone()),
                execution_summary: Some(
                    "Outcome: success\nCandidate validated and published.".into(),
                ),
                ..Default::default()
            },
        )
        .unwrap();
    fx.runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id, input),
        )
        .unwrap();
    if case == "deadline_parked" {
        let hit = fx.repo.join("deadline-hit");
        let _ = orbit_engine::test_support::with_prerequisite_clock(
            move || {
                let now = u64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis(),
                )
                .unwrap();
                if hit.exists() { now + 601_000 } else { now }
            },
            |_, _| {},
            || fx.runtime.execute_pipeline_run_worker(&run.run_id),
        );
    } else {
        let _ = fx.runtime.execute_pipeline_run_worker(&run.run_id);
    }
    let stored = fx.runtime.show_job_run(&run.run_id).unwrap();
    let expected = if refusal.is_some() {
        JobRunState::Held
    } else {
        JobRunState::Failed
    };
    assert_eq!(stored.state, expected, "{case}: {:?}", stored.steps);
    assert!(
        stored
            .steps
            .last()
            .unwrap()
            .error_message
            .as_deref()
            .unwrap()
            .contains(diagnostic),
        "{case}: {:?}",
        stored.steps
    );
    assert_eq!(
        stored.steps.last().unwrap().error_code.as_deref(),
        refusal.map(DeliveryRefusal::error_code)
    );
    assert_eq!(
        fx.runtime.get_task(&task).unwrap().status,
        task_status,
        "{case}: refusal preserves the task state"
    );
    if refusal.is_some() {
        let waited = fx
            .runtime
            .wait_pipeline_runs(std::slice::from_ref(&run.run_id), 1, 1, None)
            .unwrap();
        let parent = jobs.join("delivery_hold_parent.yaml");
        std::fs::write(&parent, json!({
        "schemaVersion": 2, "kind": "Job", "metadata": {"name": "delivery_hold_parent"},
        "spec": {"state": "enabled", "steps": [{
            "id": "require_child_success", "spec": {"type": "deterministic", "action": "pipeline_success_guard", "config": {}},
            "default_input": {"result": "{{ input.result }}"},
        }]},
    }).to_string()).unwrap();
        let mut child = serde_json::to_value(&waited.results[0]).unwrap();
        for _ in 0..2 {
            let result = fx
                .runtime
                .run_job_v2_from_yaml(&parent, json!({"result": child}))
                .unwrap();
            assert!(result.success, "{:?}", result.message);
            assert_eq!(
                fx.runtime.show_job_run(&result.run_id).unwrap().state,
                JobRunState::Success
            );
            child = json!({"status": "success", "run_id": result.run_id});
        }
    }
    assert!(
        fx.runtime
            .list_job_runs(JobRunListParams {
                state: Some(expected),
                ..Default::default()
            })
            .unwrap()
            .iter()
            .any(|row| row.run_id == run.run_id)
    );
    let metrics = fx
        .runtime
        .pipeline_reliability(&ReliabilityWindow::ending_at(
            "fixture",
            Utc::now() + Duration::seconds(1),
            Duration::minutes(10),
        ))
        .unwrap();
    assert_eq!(
        metrics.job_runs.overall.held,
        u64::from(refusal.is_some()),
        "{case}"
    );
    assert_eq!(
        metrics.job_runs.overall.failed,
        u64::from(refusal.is_none()),
        "{case}"
    );
}
