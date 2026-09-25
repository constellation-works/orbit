mod files;
mod generate;
mod highlights;
mod overlay;
mod types;

// Migrated from file/scoreboard/scoreboard_summary.rs per ORB-00231

use chrono::Utc;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{JobRun, JobRunState};
use std::fs;

fn test_task(
    id: &str,
    status: TaskStatus,
    created_by: &str,
    planned_by: &str,
) -> orbit_types::task::Task {
    let mut task = test_task_no_attrib(id, status);
    task.created_by = Some(created_by.to_string());
    task.planned_by = Some(planned_by.to_string());
    task
}

fn test_task_no_attrib(id: &str, status: TaskStatus) -> orbit_types::task::Task {
    use orbit_types::task::{Task, TaskPriority, TaskType};
    Task {
        job_run_machine: None,
        id: id.to_string(),
        title: id.to_string(),
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
        task_type: TaskType::Chore,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: None,
        crew: None,
        orchestrator: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn test_job_run(
    run_id: &str,
    job_id: &str,
    state: JobRunState,
    finished_at: chrono::DateTime<Utc>,
) -> JobRun {
    JobRun {
        executed_on: None,
        run_id: run_id.to_string(),
        job_id: job_id.to_string(),
        attempt: 1,
        state,
        scheduled_at: finished_at,
        started_at: Some(finished_at),
        finished_at: Some(finished_at),
        duration_ms: Some(0),
        created_at: finished_at,
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

/// Helper: write `pr.json` + `tokens.json` snapshots into a tempdir.
fn write_snapshot_fixtures(dir: &std::path::Path) {
    fs::create_dir_all(dir).expect("create scoreboard dir");
    fs::write(
        dir.join("pr.json"),
        r#"{
              "pr-review-comments": { "codex": 4 },
              "pr-count-without-revision": { "codex": 2 },
              "pr-count-with-revision": { "claude": 1 }
            }"#,
    )
    .expect("write pr.json");
    fs::write(
        dir.join("tokens.json"),
        r#"{
              "agents": [
                {
                  "agent": "claude",
                  "model": "claude-opus-4-7",
                  "total_tokens": 1000,
                  "total_output_tokens": 250,
                  "total_tool_calls": 7
                }
              ]
            }"#,
    )
    .expect("write tokens.json");
}
