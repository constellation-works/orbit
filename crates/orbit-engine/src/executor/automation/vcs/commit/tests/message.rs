use chrono::Utc;
use orbit_types::task::{ExternalRef, Task, TaskPriority, TaskStatus, TaskType};

use super::super::message::{batch_commit_message, finalize_commit_message, task_commit_message};

#[test]
fn batch_commit_subject_uses_task_type() {
    let cases = [
        (TaskType::Feature, "feat"),
        (TaskType::Bug, "fix"),
        (TaskType::Refactor, "refactor"),
        (TaskType::Chore, "chore"),
    ];

    for (task_type, expected_type) in cases {
        let task = task_with_type(task_type, "Ship better commit messages");

        assert_eq!(
            batch_commit_message(&task),
            format!("{expected_type}: Ship better commit messages [ORB-00107]")
        );
    }
}

#[test]
fn batch_commit_subject_keeps_short_title_unchanged() {
    let task = task_with_type(TaskType::Feature, "Short title");

    assert_eq!(batch_commit_message(&task), "feat: Short title [ORB-00107]");
}

#[test]
fn batch_commit_subject_truncates_long_title_with_ellipsis() {
    let title = "a".repeat(145);
    let task = task_with_type(TaskType::Feature, &title);
    let message = batch_commit_message(&task);
    let subject = message.lines().next().expect("message has subject");
    let typed_subject = subject
        .split(" [")
        .next()
        .expect("subject includes task tag");

    assert_eq!(typed_subject.chars().count(), 72);
    assert_eq!(typed_subject, format!("feat: {}{}", "a".repeat(65), '…'));
}

#[test]
fn batch_commit_body_includes_full_title_only_when_truncated() {
    let short_task = task_with_type(TaskType::Chore, "Short title");
    assert_eq!(
        batch_commit_message(&short_task),
        "chore: Short title [ORB-00107]"
    );

    let title = "b".repeat(145);
    let long_task = task_with_type(TaskType::Feature, &title);
    assert_eq!(
        batch_commit_message(&long_task),
        format!("feat: {}{} [ORB-00107]\n\n{}", "b".repeat(65), '…', title)
    );
}

#[test]
fn batch_commit_subject_appends_external_refs_in_declaration_order() {
    let mut task = task_with_type(TaskType::Chore, "Wire external refs");
    task.external_refs = vec![
        external_ref("eng", "1234"),
        external_ref("jira", "CORE-987"),
    ];

    assert_eq!(
        batch_commit_message(&task),
        "chore: Wire external refs [ORB-00107] [ENG-1234] [JIRA-CORE-987]"
    );
}

#[test]
fn batch_commit_body_includes_execution_summary_when_present() {
    let mut task = task_with_type(TaskType::Feature, "Summarize the work");
    task.execution_summary =
        "## Summary\n- Added deterministic batch commit messages.\n\n## Validation\n- cargo test"
            .to_string();

    assert_eq!(
        batch_commit_message(&task),
        "feat: Summarize the work [ORB-00107]\n\nAdded deterministic batch commit messages."
    );
}

#[test]
fn batch_commit_body_omits_execution_summary_when_absent() {
    let mut task = task_with_type(TaskType::Feature, "No summary available");
    task.execution_summary = "## Validation\n- cargo test".to_string();

    assert_eq!(
        batch_commit_message(&task),
        "feat: No summary available [ORB-00107]"
    );
}

#[test]
fn batch_commit_body_orders_full_title_before_execution_summary() {
    let title = "c".repeat(145);
    let mut task = task_with_type(TaskType::Bug, &title);
    task.execution_summary = "## Summary\n- Preserved the full task title.".to_string();

    assert_eq!(
        batch_commit_message(&task),
        format!(
            "fix: {}{} [ORB-00107]\n\n{}\n\nPreserved the full task title.",
            "c".repeat(66),
            '…',
            title
        )
    );
}

#[test]
fn batch_commit_trailers_include_raw_planner_and_implementer() {
    let mut task = task_with_type(TaskType::Refactor, "Record attribution");
    task.planned_by = Some("codex".to_string());
    task.implemented_by = Some("claude".to_string());

    assert_eq!(
        batch_commit_message(&task),
        "refactor: Record attribution [ORB-00107]\n\nPlanned-By: codex\nImplemented-By: claude"
    );
}

#[test]
fn batch_commit_trailers_omit_missing_fields() {
    let mut planned_only = task_with_type(TaskType::Refactor, "Planner only");
    planned_only.planned_by = Some("codex".to_string());
    assert_eq!(
        batch_commit_message(&planned_only),
        "refactor: Planner only [ORB-00107]\n\nPlanned-By: codex"
    );

    let mut implemented_only = task_with_type(TaskType::Refactor, "Implementer only");
    implemented_only.implemented_by = Some("claude".to_string());
    assert_eq!(
        batch_commit_message(&implemented_only),
        "refactor: Implementer only [ORB-00107]\n\nImplemented-By: claude"
    );

    let neither = task_with_type(TaskType::Refactor, "No trailers");
    assert_eq!(
        batch_commit_message(&neither),
        "refactor: No trailers [ORB-00107]"
    );
}

#[test]
fn orchestration_trailer_preserves_an_explicit_model_shaped_orchestrator() {
    let mut task = task_with_type(TaskType::Feature, "Attribute orchestration");
    task.orchestrator = Some("gpt-5.6-sol".to_string());
    task.created_by = Some("gpt-5.6-terra".to_string());
    task.implemented_by = Some("gpt-5.6-terra".to_string());

    assert_eq!(
        batch_commit_message(&task),
        "feat: Attribute orchestration [ORB-00107]\n\nImplemented-By: gpt-5.6-terra\nOrchestrated-By: gpt-5.6-sol"
    );
}

#[test]
fn orchestration_trailer_uses_the_explicit_alias_after_pre_execution_reassignment() {
    let mut task = task_with_type(TaskType::Feature, "Respect reassigned orchestration");
    task.created_by = Some("gpt-5.6-sol".to_string());
    task.orchestrator = Some("terra".to_string());

    assert_eq!(
        batch_commit_message(&task),
        "feat: Respect reassigned orchestration [ORB-00107]\n\nOrchestrated-By: terra"
    );
}

#[test]
fn orchestration_trailer_uses_alias_without_a_model_and_omits_unattributed_tasks() {
    let mut alias_only = task_with_type(TaskType::Feature, "Keep legacy attribution truthful");
    alias_only.orchestrator = Some("terra".to_string());
    alias_only.created_by = Some("codex".to_string());

    assert_eq!(
        batch_commit_message(&alias_only),
        "feat: Keep legacy attribution truthful [ORB-00107]\n\nOrchestrated-By: terra"
    );

    let mut no_orchestration = task_with_type(TaskType::Feature, "Do not infer orchestration");
    no_orchestration.created_by = Some("gpt-5.6-sol".to_string());
    assert_eq!(
        batch_commit_message(&no_orchestration),
        "feat: Do not infer orchestration [ORB-00107]"
    );
}

#[test]
fn orchestration_trailers_are_deterministic_and_preserved_on_task_and_finalize_messages() {
    let mut first = task_with_type(TaskType::Feature, "First task");
    first.id = "ORB-00108".to_string();
    first.orchestrator = Some("sol".to_string());
    first.created_by = Some("gpt-5.6-sol".to_string());

    let mut second = task_with_type(TaskType::Feature, "Second task");
    second.id = "ORB-00109".to_string();
    second.orchestrator = Some("terra".to_string());
    second.created_by = Some("gpt-5.6-terra".to_string());

    let mut duplicate = task_with_type(TaskType::Feature, "Duplicate orchestrator");
    duplicate.id = "ORB-00110".to_string();
    duplicate.orchestrator = Some("another-sol-alias".to_string());
    duplicate.created_by = Some("gpt-5.6-sol".to_string());

    assert_eq!(
        task_commit_message(&first),
        "[ORB-00108] First task\n\nOrchestrated-By: sol"
    );
    assert_eq!(
        finalize_commit_message(&[second.clone(), duplicate, first]),
        "fix: finalize ship batch [ORB-00109, ORB-00110, ORB-00108]\n\n- ORB-00109: Second task\n- ORB-00110: Duplicate orchestrator\n- ORB-00108: First task\n\nOrchestrated-By: another-sol-alias, sol, terra"
    );
}

#[test]
fn orchestration_trailer_never_allows_a_recorded_value_to_add_a_new_trailer_line() {
    let mut task = task_with_type(TaskType::Feature, "Protect trailer formatting");
    task.orchestrator = Some("terra\nInjected-By: attacker".to_string());
    task.created_by = Some("gpt-5.6-sol\nInjected-By: attacker".to_string());

    assert_eq!(
        batch_commit_message(&task),
        "feat: Protect trailer formatting [ORB-00107]"
    );
}

fn task_with_type(task_type: TaskType, title: &str) -> Task {
    let now = Utc::now();
    Task {
        id: "ORB-00107".to_string(),
        title: title.to_string(),
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
        status: TaskStatus::InProgress,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: None,
        crew: None,
        orchestrator: None,
        created_at: now,
        updated_at: now,
    }
}

fn external_ref(system: &str, id: &str) -> ExternalRef {
    ExternalRef::try_new(system.to_string(), id.to_string(), None)
        .expect("external ref fixture is valid")
}
