//! History-backed restoration of context declarations an earlier prune
//! removed. Restoration must cite evidence, stay idempotent, and never invent
//! or inherit scope ([ORB-12490]).

use chrono::Utc;
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};

use super::test_runtime;
use crate::OrbitRuntime;
use crate::adapter::tool_host::test_support::create_context_task;
use crate::application::task::TaskRecordUpdateParams;

/// Write the history entry the retired pruning path used to record.
fn record_pruned_history(runtime: &OrbitRuntime, task: &Task, note: &str) {
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
                    note: Some(note.to_string()),
                    from_status: None,
                    to_status: None,
                }],
                ..Default::default()
            },
        )
        .expect("append pruning history");
}

#[test]
fn restores_selectors_a_pruning_entry_recorded() {
    let (root, runtime) = test_runtime();
    let repo_root = root.path().join("repo");
    let task = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::Backlog,
        &["file:src/lib.rs"],
    );
    record_pruned_history(
        &runtime,
        &task,
        "dropped: file:src/gone.rs, symbol:src/gone.rs#run:function (selector anchor not found in workspace)",
    );

    let planned = runtime
        .plan_context_file_restore(task.id.as_str())
        .expect("plan the restoration");
    assert_eq!(
        planned.restored,
        vec![
            "file:src/gone.rs".to_string(),
            "symbol:src/gone.rs#run:function".to_string()
        ]
    );
    assert!(planned.unrestorable.is_empty(), "{planned:?}");
    assert!(!planned.applied, "a plan must not write");
    assert_eq!(
        runtime
            .get_task(task.id.as_str())
            .expect("re-read task")
            .context_files,
        ["file:src/lib.rs"],
        "planning must leave the declaration untouched"
    );

    let (updated, applied) = runtime
        .restore_pruned_context_files(task.id.as_str())
        .expect("apply the restoration");
    assert!(applied.applied);
    assert_eq!(
        updated.context_files,
        [
            "file:src/lib.rs",
            "file:src/gone.rs",
            "symbol:src/gone.rs#run:function"
        ]
    );

    // The repair is auditable: history names exactly what came back.
    let restored_note = runtime
        .get_task_history(task.id.as_str())
        .expect("read history")
        .into_iter()
        .filter(|entry| entry.event == "context_files_restored")
        .filter_map(|entry| entry.note)
        .next()
        .expect("a restore history entry");
    assert!(
        restored_note.contains("file:src/gone.rs"),
        "{restored_note}"
    );
    assert!(
        restored_note.contains("symbol:src/gone.rs#run:function"),
        "{restored_note}"
    );

    // Idempotent: a second pass has nothing left to restore and writes nothing.
    let (_, again) = runtime
        .restore_pruned_context_files(task.id.as_str())
        .expect("second restoration pass");
    assert!(again.restored.is_empty(), "{again:?}");
    assert!(!again.applied);
}

/// A task whose declaration is empty and whose history holds no pruning
/// evidence gets no scope back. Guessing one — from a parent, a sibling, or the
/// description — is the invention this path refuses.
#[test]
fn invents_no_scope_without_pruning_evidence() {
    let (root, runtime) = test_runtime();
    let repo_root = root.path().join("repo");
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &[]);

    let (updated, restoration) = runtime
        .restore_pruned_context_files(task.id.as_str())
        .expect("restore with no evidence");
    assert!(restoration.is_empty(), "{restoration:?}");
    assert!(!restoration.applied);
    assert!(updated.context_files.is_empty());
}

/// Evidence naming a selector this workspace cannot canonicalize is reported
/// for operator repair rather than rewritten into something plausible.
#[test]
fn reports_recorded_selectors_that_cannot_be_canonicalized() {
    let (root, runtime) = test_runtime();
    let repo_root = root.path().join("repo");
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &[]);
    record_pruned_history(
        &runtime,
        &task,
        "dropped: file:../outside.rs (selector anchor not found in workspace)",
    );

    let (updated, restoration) = runtime
        .restore_pruned_context_files(task.id.as_str())
        .expect("restore with out-of-workspace evidence");
    assert!(restoration.restored.is_empty(), "{restoration:?}");
    assert_eq!(
        restoration.unrestorable,
        vec!["file:../outside.rs".to_string()]
    );
    assert!(updated.context_files.is_empty());
}

/// An unparseable note carries no evidence, so it restores nothing at all.
#[test]
fn ignores_history_notes_that_do_not_name_selectors() {
    let (root, runtime) = test_runtime();
    let repo_root = root.path().join("repo");
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &[]);
    record_pruned_history(&runtime, &task, "context pruned during a migration");

    let restoration = runtime
        .plan_context_file_restore(task.id.as_str())
        .expect("plan the restoration");
    assert!(restoration.is_empty(), "{restoration:?}");
}

/// A selector an operator re-declared by hand is not added twice.
#[test]
fn skips_selectors_the_task_already_declares() {
    let (root, runtime) = test_runtime();
    let repo_root = root.path().join("repo");
    let task = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::Backlog,
        &["file:src/gone.rs"],
    );
    record_pruned_history(
        &runtime,
        &task,
        "dropped: file:src/gone.rs (selector anchor not found in workspace)",
    );

    let restoration = runtime
        .plan_context_file_restore(task.id.as_str())
        .expect("plan the restoration");
    assert!(restoration.is_empty(), "{restoration:?}");
}
