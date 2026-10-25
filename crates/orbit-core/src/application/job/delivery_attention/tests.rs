use chrono::Utc;
use orbit_types::task::{ExternalRef, Task, TaskHistoryEntry, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{JobRun, JobRunState};

use super::delivery_attention_value;

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
