use chrono::Utc;
use orbit_types::task::{ExternalRef, Task, TaskHistoryEntry, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{JobRun, JobRunState};

use super::{delivery_attention_value, landing_review_gate_facts};

fn task(status: TaskStatus, with_pr: bool) -> Task {
    Task {
        id: "TASK-1".to_string(),
        title: "Fixture".to_string(),
        description: String::new(),
        acceptance_criteria: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
        execution_summary: String::new(),
        context_files: Vec::new(),
        created_by: None,
        planned_by: None,
        implemented_by: None,
        status,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Feature,
        pr_status: None,
        external_refs: if with_pr {
            vec![ExternalRef {
                system: "github-pr".to_string(),
                id: "42".to_string(),
                url: Some("https://github.com/o/r/pull/42".to_string()),
            }]
        } else {
            Vec::new()
        },
        relations: Vec::new(),
        job_run_id: Some("jrun-1".to_string()),
        job_run_machine: None,
        crew: None,
        crew_source: None,
        orchestrator: None,
        orchestrator_model: None,
        orchestrator_model_source: None,
        origin: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn run(state: JobRunState) -> JobRun {
    let now = Utc::now();
    JobRun {
        executed_on: None,
        run_id: "jrun-1".to_string(),
        job_id: "task_pr_pipeline".to_string(),
        attempt: 1,
        state,
        scheduled_at: now,
        started_at: Some(now),
        finished_at: Some(now),
        duration_ms: None,
        created_at: now,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    }
}

fn attempt_history(blocker: &str) -> Vec<TaskHistoryEntry> {
    vec![TaskHistoryEntry {
        at: Utc::now(),
        by: "system".to_string(),
        event: "pr_auto_fix_admitted".to_string(),
        note: Some(format!(
            r#"{{"pr_number":"42","head_sha":"abc123","blocker":"{blocker}","attempt":1,"budget":3,"run_id":"jrun-1","crew":"c","crew_source":"s"}}"#
        )),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
    }]
}

#[test]
fn failed_run_with_failed_checks_attempt_is_flagged() {
    let value = delivery_attention_value(
        &task(TaskStatus::Review, true),
        &attempt_history("failed_checks"),
        Some(&run(JobRunState::Failed)),
        Some("complete_pr"),
        false,
    )
    .expect("attention");
    assert_eq!(value["run_state"], "failed");
    assert_eq!(value["failed_step_id"], "complete_pr");
    assert_eq!(value["blocker"], "failed_checks");
    assert_eq!(value["head_sha"], "abc123");
    assert_eq!(value["attempt"], 1);
    assert_eq!(value["budget"], 3);
    assert_eq!(value["repairing"], false);
    assert_eq!(value["pr_number"], "42");
}

#[test]
fn timeout_and_interrupted_runs_and_conflict_are_flagged() {
    for state in [JobRunState::Timeout, JobRunState::Interrupted] {
        let value = delivery_attention_value(
            &task(TaskStatus::Blocked, true),
            &attempt_history("conflict"),
            Some(&run(state)),
            Some("complete_pr"),
            true,
        )
        .expect("attention");
        assert_eq!(value["blocker"], "conflict");
        assert_eq!(value["repairing"], true);
    }
}

#[test]
fn healthy_and_ineligible_tasks_have_no_attention() {
    assert!(
        delivery_attention_value(
            &task(TaskStatus::Review, true),
            &[],
            Some(&run(JobRunState::Success)),
            None,
            false,
        )
        .is_none()
    );
    assert!(
        delivery_attention_value(&task(TaskStatus::Review, true), &[], None, None, false,)
            .is_none()
    );
    // Blocked without a recorded PR is out of scope.
    assert!(
        delivery_attention_value(
            &task(TaskStatus::Blocked, false),
            &attempt_history("failed_checks"),
            Some(&run(JobRunState::Failed)),
            Some("complete_pr"),
            false,
        )
        .is_none()
    );
    // A backlog task is not flagged even with a failed run.
    assert!(
        delivery_attention_value(
            &task(TaskStatus::Backlog, true),
            &attempt_history("failed_checks"),
            Some(&run(JobRunState::Failed)),
            Some("complete_pr"),
            false,
        )
        .is_none()
    );
}

#[test]
fn abandoned_landing_review_settle_reports_abandoned_verdict() {
    // Exact step-error text built for `Settled::Abandoned` in
    // `application/review/gate/settle.rs`: it settles nothing, so the row
    // must show the gate outcome, not the provisional `(verdict incomplete,`
    // parenthetical it quotes.
    let message = format!(
        "review_abandoned: the reviewer exited successfully but its only report \
         is the initial provisional one (verdict incomplete, no escalation, finding or \
         validation record, never updated); attempt attempt-1 was released without a \
         verdict, so the candidate's review is not spent and a reviewer is admitted again \
         within its remaining minutes; {}",
        orbit_types::workflow::REVIEW_LANDING_DECISION_PENDING,
    );
    let facts = landing_review_gate_facts(None, Some(&message));
    assert_eq!(facts["availability"], "read");
    assert_eq!(facts["verdict"], "abandoned");
    assert!(facts["finding_count"].is_null());
}

#[test]
fn abandoned_landing_review_settle_step_error_reports_abandoned_verdict() {
    // The recorded step error is the refusal's `Display`, which names the
    // refused action before the settlement text.
    let message = format!(
        "deterministic action `review_gate_settle` refused: review_abandoned: the reviewer \
         exited successfully but its only report is the initial provisional one (verdict \
         incomplete, no escalation, finding or validation record, never updated); attempt \
         attempt-1 was released without a verdict, so the candidate's review is not spent and \
         a reviewer is admitted again within its remaining minutes; {}",
        orbit_types::workflow::REVIEW_LANDING_DECISION_PENDING,
    );
    let facts = landing_review_gate_facts(None, Some(&message));
    assert_eq!(facts["availability"], "read");
    assert_eq!(facts["verdict"], "abandoned");
    assert!(facts["finding_count"].is_null());
}

#[test]
fn blocked_landing_review_settle_still_parses_verdict_and_count() {
    let message = format!(
        "review_gate_blocked: verdict reject (decide); 1 finding(s) recorded; {}",
        orbit_types::workflow::REVIEW_LANDING_DECISION_PENDING,
    );
    let facts = landing_review_gate_facts(None, Some(&message));
    assert_eq!(facts["availability"], "read");
    assert_eq!(facts["verdict"], "reject");
    assert_eq!(facts["finding_count"], 1);
}

#[test]
fn blocked_settlement_quoting_abandoned_marker_still_parses_verdict_and_count() {
    // A blocked settlement embeds the reviewers free-text escalation verbatim,
    // so the escalation may quote the abandoned-settlement marker. The message
    // still leads with `review_gate_blocked:`, so it must parse the recorded
    // verdict and finding count instead of reporting abandoned.
    let message = format!(
        "review_gate_blocked: verdict reject (quoted review_abandoned: in escalation); \
         2 finding(s) recorded; {}",
        orbit_types::workflow::REVIEW_LANDING_DECISION_PENDING,
    );
    let facts = landing_review_gate_facts(None, Some(&message));
    assert_eq!(facts["availability"], "read");
    assert_eq!(facts["verdict"], "reject");
    assert_eq!(facts["finding_count"], 2);
}
