//! An atomic task mutation writes its idempotency receipt once, as the typed
//! `operation_id` of its history event and audit comment, and replays on
//! that field. Rows written before the field kept the receipt in text and
//! still replay; nothing writes that form now, so the fixture appends the old
//! rows to the bundle as raw JSON lines.

use orbit_store::contracts::{AtomicTaskMutationOutcome, AtomicTaskMutationParams};
use orbit_types::task::{TaskComment, TaskHistoryEntry};

use super::*;

fn mutation(owner: &Coordinated, id: &str, operation_id: &str) -> AtomicTaskMutationParams {
    let task = owner.backends.task.task.get_task(id).unwrap().unwrap();
    AtomicTaskMutationParams {
        actor: "task-pilot".to_string(),
        operation_id: operation_id.to_string(),
        expected_context_files: task.context_files.clone(),
        expected_status: task.status,
        expected_complexity: task.complexity,
        expected_crew: task.crew.clone(),
        expected_crew_source: task.crew_source.clone(),
        crew: task.crew.clone(),
        crew_source: task.crew_source.clone(),
        expected_context_creation: None,
        context_files: vec!["file:src/lib.rs".to_string()],
        add_tags: Vec::new(),
        add_dependencies: Vec::new(),
        status: task.status,
        complexity: TaskComplexity::Low,
        event_type: "task_pilot_applied".to_string(),
        event_note: "pilot".to_string(),
        history_summary: "selectors (high)".to_string(),
        audit_note: "{\"assessment\":{}}".to_string(),
        append_history: Vec::new(),
        decision: None,
    }
}

fn apply(
    owner: &Coordinated,
    id: &str,
    params: &AtomicTaskMutationParams,
) -> AtomicTaskMutationOutcome {
    owner
        .backends
        .task
        .task
        .apply_atomic_task_mutation(id, params)
        .unwrap()
}

fn history(owner: &Coordinated, id: &str) -> Vec<TaskHistoryEntry> {
    owner
        .backends
        .task
        .history
        .get_task_history(id)
        .unwrap()
        .unwrap()
}

fn comments(owner: &Coordinated, id: &str) -> Vec<TaskComment> {
    owner
        .backends
        .task
        .history
        .get_task_comments(id)
        .unwrap()
        .unwrap()
}

#[test]
fn a_mutation_receipt_is_a_typed_field_and_replays_on_it() {
    if !allocation_admission_isolated("mutation_receipts::a_mutation_receipt_is_a_typed_field_and_replays_on_it") {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    let task = owner.create_task("Typed receipt");
    let params = mutation(&owner, &task.id, "op-typed");

    assert_eq!(
        apply(&owner, &task.id, &params),
        AtomicTaskMutationOutcome::Applied
    );
    let events = history(&owner, &task.id);
    let event = events
        .iter()
        .find(|entry| entry.event == "task_pilot_applied")
        .expect("receipt event");
    assert_eq!(event.operation_id.as_deref(), Some("op-typed"));
    assert_eq!(event.receipt(), Some("op-typed"));
    assert_eq!(
        event.note.as_deref(),
        Some("selectors (high)"),
        "the summary carries no text receipt"
    );
    let written = comments(&owner, &task.id);
    let audit = written.last().expect("audit comment");
    assert_eq!(audit.operation_id.as_deref(), Some("op-typed"));
    assert_eq!(
        audit.message, params.audit_note,
        "the audit body is unprefixed"
    );
    let receipt = audit.receipt().expect("comment receipt");
    assert_eq!(
        (receipt.operation_id, receipt.body),
        ("op-typed", params.audit_note.as_str())
    );

    assert_eq!(
        apply(&owner, &task.id, &params),
        AtomicTaskMutationOutcome::AlreadyApplied,
        "the same operation replays on its typed receipt"
    );
    assert_eq!(history(&owner, &task.id), events);
    assert_eq!(comments(&owner, &task.id), written);
}

#[test]
fn a_receipt_written_as_text_before_the_typed_field_still_replays() {
    if !allocation_admission_isolated(
        "mutation_receipts::a_receipt_written_as_text_before_the_typed_field_still_replays",
    ) {
        return;
    }
    for note in [
        "selectors (high) (operation_id=op-legacy)",
        "operation_id=op-legacy\nselectors (high)",
    ] {
        let root = TempDir::new().unwrap();
        let owner = Coordinated::open(root.path());
        let task = owner.create_task("Legacy receipt");
        let bundle = owner
            .registry
            .canonical_task_bundle_path(PARTITION_ID, &task.id)
            .unwrap();
        let events_path = bundle.join(TASK_EVENTS_FILE_NAME);
        let comments_path = bundle.join(TASK_COMMENTS_FILE_NAME);
        let event_id = format!(
            "EV-{:04}",
            jsonl_rows::<TaskEventRowV2>(&events_path).len() + 1
        );
        let comment_id = format!(
            "C-{:04}",
            std::fs::metadata(&comments_path)
                .map_or(0, |_| jsonl_rows::<TaskCommentRowV2>(&comments_path).len())
                + 1
        );
        let at = Utc::now();
        let append = |path: &Path, row: serde_json::Value| {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(file, "{row}").unwrap();
        };
        append(
            &events_path,
            serde_json::json!({
                "schema_version": TASK_ARTIFACT_SCHEMA_VERSION, "event_id": event_id, "at": at,
                "by": "task-pilot", "type": "task_pilot_applied", "note": note,
            }),
        );
        append(
            &comments_path,
            serde_json::json!({
                "schema_version": TASK_ARTIFACT_SCHEMA_VERSION, "comment_id": comment_id, "at": at,
                "by": "task-pilot", "body": "operation_id=op-legacy\n{\"assessment\":{}}",
            }),
        );
        let before = history(&owner, &task.id);
        assert_eq!(before.last().unwrap().receipt(), Some("op-legacy"));
        let legacy = comments(&owner, &task.id);
        let receipt = legacy
            .last()
            .unwrap()
            .receipt()
            .expect("legacy comment receipt");
        assert_eq!(
            (receipt.operation_id, receipt.body),
            ("op-legacy", "{\"assessment\":{}}")
        );

        assert_eq!(
            apply(&owner, &task.id, &mutation(&owner, &task.id, "op-legacy")),
            AtomicTaskMutationOutcome::AlreadyApplied,
            "legacy receipt `{note}` replays"
        );
        assert_eq!(history(&owner, &task.id), before);
        assert_eq!(
            apply(&owner, &task.id, &mutation(&owner, &task.id, "op-other")),
            AtomicTaskMutationOutcome::Applied,
            "another operation is not a replay of `{note}`"
        );
    }
}
