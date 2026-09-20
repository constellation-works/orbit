//! Lint findings for task context declarations. After [ORB-12490] a missing
//! target is a warning that keeps the declaration; an empty or unusable
//! surface is an advisory warning because legacy v2 admission permits it.

use chrono::Utc;
use orbit_types::task::{TaskHistoryEntry, TaskStatus, TaskType};

use super::super::lint::context_entry_covers_path;
use super::test_runtime;
use crate::adapter::tool_host::test_support::create_context_task;
use crate::application::task::{TaskAddParams, TaskLintSeverity, TaskRecordUpdateParams};

#[test]
fn a_missing_declared_target_warns_without_asking_for_its_removal() {
    let (root, runtime) = test_runtime();
    let repo_root = root.path().join("repo");
    std::fs::create_dir_all(repo_root.join("src")).expect("create src");
    std::fs::write(repo_root.join("src/lib.rs"), b"pub fn run() {}\n").expect("write lib.rs");
    let task = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::Backlog,
        &["file:src/lib.rs", "file:src/future.rs"],
    );

    let report = runtime.lint_task(task.id.as_str()).expect("lint the task");
    let finding = report
        .findings
        .iter()
        .find(|finding| finding.check == "context_target_missing")
        .expect("a missing-target finding");
    assert_eq!(finding.severity, TaskLintSeverity::Warning);
    assert!(
        finding.message.contains("file:src/future.rs"),
        "{finding:?}"
    );
    assert!(
        !finding.fix_it.to_lowercase().contains("remove"),
        "the remedy must not advise deleting declared scope: {finding:?}"
    );
    assert!(
        runtime
            .get_task(task.id.as_str())
            .expect("re-read task")
            .context_files
            .contains(&"file:src/future.rs".to_string()),
        "linting must not drop the declaration"
    );
}

#[test]
fn an_empty_surface_is_an_advisory_warning_that_names_each_admission_rule() {
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Declares nothing".to_string(),
            task_type: Some(TaskType::Feature),
            ..Default::default()
        })
        .expect("add task succeeds");

    let report = runtime.lint_task(task.id.as_str()).expect("lint the task");
    let finding = report
        .findings
        .iter()
        .find(|finding| finding.check == "context_surface")
        .expect("an empty-surface finding");
    assert_eq!(finding.severity, TaskLintSeverity::Warning);
    assert!(finding.message.contains("legacy v2 admission permits"));
    assert!(
        finding
            .message
            .contains("operator task-scope reservation refuses")
    );
    assert!(
        finding
            .message
            .contains("distributed pull admission will exclude")
    );
    assert!(finding.fix_it.contains("orbit task update --context"));
    assert!(
        finding
            .fix_it
            .contains("before claiming an operator task-scope reservation")
    );
    assert!(!finding.fix_it.contains("--restore-pruned"));

    // With pruning evidence, the same finding points at the auditable repair.
    runtime
        .stores()
        .task_records()
        .update(
            task.id.as_str(),
            TaskRecordUpdateParams {
                actor: "test".to_string(),
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "test".to_string(),
                    event: "context_files_pruned".to_string(),
                    note: Some(
                        "dropped: file:src/gone.rs (selector anchor not found in workspace)"
                            .to_string(),
                    ),
                    from_status: None,
                    to_status: None,
                }],
                ..Default::default()
            },
        )
        .expect("append pruning history");

    let report = runtime.lint_task(task.id.as_str()).expect("re-lint");
    let finding = report
        .findings
        .iter()
        .find(|finding| finding.check == "context_surface")
        .expect("an empty-surface finding");
    assert!(finding.fix_it.contains("--restore-pruned"), "{finding:?}");
}

/// A chore that declares nothing receives the same advisory finding because
/// legacy v2 admission permits an empty surface regardless of task type.
#[test]
fn an_empty_chore_surface_is_reported_as_a_warning() {
    let (root, runtime) = test_runtime();
    let repo_root = root.path().join("repo");
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &[]);

    let report = runtime.lint_task(task.id.as_str()).expect("lint the task");
    let finding = report
        .findings
        .iter()
        .find(|finding| finding.check == "context_surface")
        .expect("an empty-surface finding");
    assert_eq!(finding.severity, TaskLintSeverity::Warning);
}

#[test]
fn an_out_of_workspace_selector_is_a_path_validity_error() {
    let (root, runtime) = test_runtime();
    let repo_root = root.path().join("repo");
    let task = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::Backlog,
        &["file:../outside.rs"],
    );

    let report = runtime.lint_task(task.id.as_str()).expect("lint the task");
    let finding = report
        .findings
        .iter()
        .find(|finding| finding.check == "path_validity")
        .expect("an invalid-selector finding");
    assert_eq!(finding.severity, TaskLintSeverity::Error);
    assert!(
        finding.message.contains("file:../outside.rs"),
        "{finding:?}"
    );
}

#[test]
fn context_entry_covers_file_line_mentions() {
    assert!(context_entry_covers_path(
        "file:crates/orbit-cli/src/command/ship.rs",
        "crates/orbit-cli/src/command/ship.rs:274"
    ));
    assert!(context_entry_covers_path(
        "symbol:crates/x.rs#run:function",
        "crates/x.rs:42"
    ));
    assert!(context_entry_covers_path("dir:src", "src/lib.rs"));
    assert!(!context_entry_covers_path(
        "file:src/lib.rs",
        "tests/lib.rs"
    ));
}
