use std::process::Command;

/// Remove v72/v73/v77 tables and the v75 counter with its triggers before fixtures
/// replay their migrations from an older schema: a `job_runs` trigger names
/// `run_resource_minutes`, which those fixtures drop.
pub fn drop_replayed_migration_tables(connection: &rusqlite::Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "DROP TRIGGER IF EXISTS delivery_runs_stamp_run_insert;
         DROP TRIGGER IF EXISTS delivery_runs_stamp_run_delete;
         DROP TRIGGER IF EXISTS delivery_runs_stamp_run_update;
         DROP TRIGGER IF EXISTS delivery_runs_stamp_minute_insert;
         DROP TRIGGER IF EXISTS delivery_runs_stamp_minute_delete;
         DROP TRIGGER IF EXISTS delivery_runs_stamp_minute_rekey;
         DROP TABLE IF EXISTS delivery_runs_stamp;
         DROP TABLE IF EXISTS worker_resource_limits;
         DROP TABLE IF EXISTS assistance_attempts;
         DROP TABLE IF EXISTS assistance_slots;
         DROP TABLE IF EXISTS assistance_budgets;
         DROP TABLE IF EXISTS assistance_work;
         DROP TABLE IF EXISTS automation_condition_audit;
         DROP TABLE IF EXISTS automation_condition_states;",
    )
}

pub fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_STORE_ADMISSION_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let root = tempfile::tempdir_in(orbit_common::test_env::canonical_temp_dir()).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .current_dir(root.path());
    let output = orbit_common::test_env::run_child_test(&mut command, test, root.path());
    orbit_common::test_env::assert_child_test_passed(
        test,
        output.status,
        output.stdout,
        output.stderr,
    );
    false
}

use chrono::{TimeZone, Utc};
use orbit_store::contracts::TaskCreateParams;
use orbit_types::task::{ExternalRef, TaskComment, TaskPriority, TaskStatus, TaskType};
// Shared by task and publication fixtures; file-store cases only use isolation.
#[allow(dead_code)]
pub fn create_params(title: &str, status: TaskStatus) -> TaskCreateParams {
    let now = Utc.with_ymd_and_hms(2026, 5, 11, 12, 0, 0).unwrap();
    TaskCreateParams {
        granted_tools: Vec::new(),
        requires_labels: Vec::new(),
        actor: "codex:gpt-5.5".to_string(),
        parent_id: None,
        title: title.to_string(),
        description: "Detailed task description".to_string(),
        acceptance_criteria: vec![
            "First criterion".to_string(),
            "Second criterion".to_string(),
        ],
        dependencies: Vec::new(),
        relations: Vec::new(),
        tags: vec!["task-artifacts".to_string(), "v2".to_string()],
        required_tools: Vec::new(),
        plan: "1. Do the work".to_string(),
        execution_summary: String::new(),
        context_files: vec!["docs/design/task-artifacts/1_overview.md".to_string()],
        repo_root: None,
        created_by: Some("codex:gpt-5.5".to_string()),
        planned_by: None,
        implemented_by: None,
        status,
        priority: TaskPriority::High,
        complexity: None,
        task_type: TaskType::Feature,
        external_refs: vec![
            ExternalRef::try_new("linear".to_string(), "ENG-123".to_string(), None).unwrap(),
        ],
        source_task_id: None,
        crew: None,
        crew_source: None,
        orchestrator: None,
        orchestrator_model: None,
        orchestrator_model_source: None,
        created_by_model: None,
        origin: None,
        rule_fingerprint: None,
        comments: vec![TaskComment {
            at: now,
            by: "daniel".to_string(),
            message: "Please build this.".to_string(),
            operation_id: None,
        }],
        context_creation: Vec::new(),
        creation_decision: None,
        not_before: None,
    }
}
