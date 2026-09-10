use super::*;

#[test]
fn document_update_rewrites_v2_documents_and_envelope() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let mut create = create_params("Original", TaskStatus::Backlog);
    create.required_tools = vec![
        "github.run.list".to_string(),
        "github.auth.status".to_string(),
        "github.run.list".to_string(),
    ];
    store.create_task(create).expect("create task");

    store
        .update_task_document(
            "ORB-00000",
            &TaskDocumentUpdateParams {
                actor: "codex:gpt-5.5".to_string(),
                title: Some("Renamed".to_string()),
                description: Some("Updated description".to_string()),
                acceptance_criteria: Some(vec!["Updated criterion".to_string()]),
                tags: Some(vec!["v2".to_string(), "store".to_string()]),
                plan: Some("1. Updated plan".to_string()),
                execution_summary: Some("Updated summary".to_string()),
                priority: Some(TaskPriority::Low),
                pr_status: Some(Some("approved".to_string())),
                ..Default::default()
            },
        )
        .expect("update document");

    let task = store
        .get_task("ORB-00000")
        .expect("get task")
        .expect("task exists");
    assert_eq!(task.title, "Renamed");
    assert_eq!(task.description, "Updated description");
    assert_eq!(task.acceptance_criteria, vec!["Updated criterion"]);
    assert_eq!(task.tags, vec!["v2", "store"]);
    assert_eq!(
        task.required_tools,
        vec!["github.auth.status", "github.run.list"]
    );
    assert_eq!(task.plan, "1. Updated plan");
    assert_eq!(task.execution_summary, "Updated summary");
    assert_eq!(task.priority, TaskPriority::Low);
    assert_eq!(task.pr_status.as_deref(), Some("approved"));
    let renamed = store
        .get_task_history("ORB-00000")
        .expect("get history")
        .expect("task exists")
        .into_iter()
        .find(|entry| entry.event == "renamed")
        .expect("renamed event");
    // ORB-10311: the rename note carries both the previous and replacement titles.
    let note = renamed.note.expect("renamed note");
    assert!(note.contains("Original"), "{note}");
    assert!(note.contains("Renamed"), "{note}");
    assert_eq!(
        store
            .list_tasks_by_tags(&["task-artifacts".to_string()])
            .expect("old tag should leave generated index")
            .len(),
        0
    );
    assert_eq!(
        store
            .list_tasks_filtered(None, Some(TaskPriority::Low), None, None, None, None)
            .expect("priority filter should use updated generated index")
            .len(),
        1
    );
}

#[test]
fn document_update_sets_and_clears_source_task_id() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let source = store
        .create_task(create_params("Source", TaskStatus::Done))
        .expect("create source");
    store
        .create_task(create_params("Bug", TaskStatus::Backlog))
        .expect("create bug");

    store
        .update_task_document(
            "ORB-00001",
            &TaskDocumentUpdateParams {
                actor: "codex:gpt-5.5".to_string(),
                source_task_id: Some(Some(source.id.clone())),
                ..Default::default()
            },
        )
        .expect("set source task");

    let task = store
        .get_task("ORB-00001")
        .expect("get task")
        .expect("task exists");
    assert_eq!(task.source_task_id(), Some(source.id.as_str()));
    let envelope = store
        .bundle_store
        .read_bundle("ORB-00001")
        .expect("read bundle")
        .envelope;
    assert!(envelope.relations.iter().any(|relation| {
        relation.relation_type == TaskRelationType::RegressionFrom && relation.target == source.id
    }));

    store
        .update_task_document(
            "ORB-00001",
            &TaskDocumentUpdateParams {
                actor: "codex:gpt-5.5".to_string(),
                source_task_id: Some(None),
                ..Default::default()
            },
        )
        .expect("clear source task");

    let task = store
        .get_task("ORB-00001")
        .expect("get task")
        .expect("task exists");
    assert_eq!(task.source_task_id(), None);
    let envelope = store
        .bundle_store
        .read_bundle("ORB-00001")
        .expect("read bundle")
        .envelope;
    assert!(
        envelope
            .relations
            .iter()
            .all(|relation| relation.relation_type != TaskRelationType::RegressionFrom)
    );
}

#[test]
fn history_update_appends_comments_and_status_events() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("History", TaskStatus::Backlog))
        .expect("create task");
    let at = Utc.with_ymd_and_hms(2026, 5, 11, 13, 0, 0).unwrap();

    store
        .update_task_history(
            "ORB-00000",
            &TaskHistoryUpdateParams {
                actor: "codex:gpt-5.5".to_string(),
                status: Some(TaskStatus::InProgress),
                status_note: Some("Starting".to_string()),
                append_history: vec![TaskHistoryEntry {
                    at,
                    by: "codex:gpt-5.5".to_string(),
                    event: "context_pruned".to_string(),
                    note: Some("Dropped missing file".to_string()),
                    from_status: None,
                    to_status: None,
                }],
                append_comments: vec![TaskComment {
                    at,
                    by: "codex:gpt-5.5".to_string(),
                    message: "Working on it".to_string(),
                }],
                ..Default::default()
            },
        )
        .expect("update history");

    let task = store
        .get_task("ORB-00000")
        .expect("get task")
        .expect("task exists");
    assert_eq!(task.status, TaskStatus::InProgress);
    assert_eq!(
        store
            .list_tasks_filtered(Some(TaskStatus::InProgress), None, None, None, None, None,)
            .expect("status filter should use updated generated index")
            .len(),
        1
    );
    let comments = store
        .get_task_comments("ORB-00000")
        .expect("get comments")
        .expect("task exists");
    assert!(
        comments
            .iter()
            .any(|comment| comment.message == "Working on it")
    );
    let history = store
        .get_task_history("ORB-00000")
        .expect("get history")
        .expect("task exists");
    let status_event = history
        .iter()
        .find(|event| event.event == "status_changed")
        .expect("status event");
    assert_eq!(status_event.from_status, Some(TaskStatus::Backlog));
    assert_eq!(status_event.to_status, Some(TaskStatus::InProgress));
    assert_eq!(status_event.note.as_deref(), Some("Starting"));
}

#[test]
fn artifact_update_writes_manifest_and_sorted_text_artifacts() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("Artifacts", TaskStatus::Backlog))
        .expect("create task");

    store
        .upsert_task_artifacts(
            "ORB-00000",
            &TaskArtifactUpdateParams {
                owner_run_id: None,
                actor: "codex:gpt-5.5".to_string(),
                upsert_artifacts: vec![
                    TaskArtifact::from_text("./reports/summary.md", "summary v1\n"),
                    TaskArtifact::from_text("logs/output.txt", "output\n"),
                ],
            },
        )
        .expect("upsert artifacts");

    store
        .upsert_task_artifacts(
            "ORB-00000",
            &TaskArtifactUpdateParams {
                owner_run_id: None,
                actor: "codex:gpt-5.5".to_string(),
                upsert_artifacts: vec![TaskArtifact::from_text(
                    "reports/summary.md",
                    "summary v2\n",
                )],
            },
        )
        .expect("overwrite artifact");

    let artifacts = store
        .get_task_artifacts("ORB-00000")
        .expect("get artifacts")
        .expect("task exists");
    assert_eq!(
        artifacts
            .iter()
            .map(|artifact| artifact.path.as_str())
            .collect::<Vec<_>>(),
        vec!["logs/output.txt", "reports/summary.md"]
    );
    assert_eq!(artifacts[1].text_content(), Some("summary v2\n"));

    let bundle = store
        .bundle_store
        .read_bundle("ORB-00000")
        .expect("read bundle");
    let manifest = bundle.artifact_manifest.expect("manifest");
    let summary = manifest
        .files
        .iter()
        .find(|file| file.path == "reports/summary.md")
        .expect("summary manifest entry");
    assert_eq!(summary.blob, "files/reports/summary.md");
    assert_eq!(summary.sha256.len(), 64);
    assert!(
        summary
            .sha256
            .chars()
            .all(|ch| matches!(ch, '0'..='9' | 'a'..='f'))
    );
    assert_eq!(summary.created_by, "codex:gpt-5.5");

    let err = store
        .upsert_task_artifacts(
            "ORB-00000",
            &TaskArtifactUpdateParams {
                owner_run_id: None,
                actor: "codex:gpt-5.5".to_string(),
                upsert_artifacts: vec![TaskArtifact::from_text("../escape.txt", "")],
            },
        )
        .expect_err("reject unsafe artifact path");
    assert!(err.to_string().contains(".."), "{err}");
}

#[cfg(unix)]
#[test]
fn document_update_on_readonly_bundle_dir_names_path_and_hints_sandbox() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("Original", TaskStatus::Backlog))
        .expect("create task");
    let bundle_dir = store
        .bundle_store
        .bundle_path("ORB-00000")
        .expect("bundle path");
    let _restore = make_readonly(&bundle_dir);

    let err = store
        .update_task_document(
            "ORB-00000",
            &TaskDocumentUpdateParams {
                actor: "codex:gpt-5.5".to_string(),
                title: Some("Renamed".to_string()),
                ..Default::default()
            },
        )
        .expect_err("update must fail on a read-only bundle dir");
    assert_sandbox_write_io(&err, &bundle_dir.display().to_string());
}

/// [ORB-11305] `expected_status` is a compare-and-set: the write is applied
/// against the status persisted at write time, not the one its caller read.
///
/// The caller this exists for is workflow admission, which decides "this task
/// is `backlog`, start it" and can then be overtaken by a human withdrawal
/// before the write lands. Without the guard the withdrawal is overwritten and
/// automation starts work its owner already took back.
#[test]
fn history_update_refuses_a_status_write_whose_expectation_no_longer_holds() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params(
            "Withdrawn under a stale read",
            TaskStatus::Backlog,
        ))
        .expect("create task");

    // The human withdrawal lands first.
    store
        .update_task_history(
            "ORB-00000",
            &TaskHistoryUpdateParams {
                actor: "daniel".to_string(),
                status: Some(TaskStatus::Archived),
                ..Default::default()
            },
        )
        .expect("archive task");

    let events_before = store
        .get_task_history("ORB-00000")
        .expect("read history")
        .expect("history exists")
        .len();

    // The admission write, still holding its `backlog` snapshot, loses.
    let error = store
        .update_task_history(
            "ORB-00000",
            &TaskHistoryUpdateParams {
                actor: "system".to_string(),
                status: Some(TaskStatus::InProgress),
                status_event: Some("started".to_string()),
                expected_status: Some(vec![TaskStatus::Backlog, TaskStatus::InProgress]),
                ..Default::default()
            },
        )
        .expect_err("a stale expectation must not overwrite the newer status");
    let message = error.to_string();
    assert!(
        message.contains("archived"),
        "names what it found: {message}"
    );
    assert!(
        message.contains("backlog"),
        "names what it expected: {message}"
    );

    let task = store
        .get_task("ORB-00000")
        .expect("get task")
        .expect("task exists");
    assert_eq!(task.status, TaskStatus::Archived);
    assert_eq!(
        store
            .get_task_history("ORB-00000")
            .expect("read history")
            .expect("history exists")
            .len(),
        events_before,
        "a refused write must not leave a partial history entry behind"
    );
}

/// The same guard applied to a status that still holds is transparent, and a
/// write with no expectation keeps the unconditional behavior every other
/// caller relies on.
#[test]
fn history_update_applies_when_the_expectation_holds_or_is_absent() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("Still wanted", TaskStatus::Backlog))
        .expect("create task");

    store
        .update_task_history(
            "ORB-00000",
            &TaskHistoryUpdateParams {
                actor: "system".to_string(),
                status: Some(TaskStatus::InProgress),
                status_event: Some("started".to_string()),
                expected_status: Some(vec![TaskStatus::Backlog, TaskStatus::InProgress]),
                ..Default::default()
            },
        )
        .expect("a satisfied expectation applies");
    assert_eq!(
        store
            .get_task("ORB-00000")
            .expect("get task")
            .expect("task exists")
            .status,
        TaskStatus::InProgress
    );

    store
        .update_task_history(
            "ORB-00000",
            &TaskHistoryUpdateParams {
                actor: "daniel".to_string(),
                status: Some(TaskStatus::Archived),
                ..Default::default()
            },
        )
        .expect("an unguarded write is still unconditional");
    assert_eq!(
        store
            .get_task("ORB-00000")
            .expect("get task")
            .expect("task exists")
            .status,
        TaskStatus::Archived
    );
}

#[test]
fn executor_origin_is_store_authored_and_normalized_alias_cannot_forge_it() {
    use orbit_types::workflow::automation::{EVIDENCE_AUTHORITY_ARTIFACT, EvidenceSubmission};
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let task = store
        .create_task(create_params("Coverage", TaskStatus::Backlog))
        .unwrap();
    let params = TaskArtifactUpdateParams {
        actor: "codex".into(),
        owner_run_id: Some("trusted-run".into()),
        upsert_artifacts: vec![TaskArtifact::from_text(
            "./automation-coverage.json",
            "evidence",
        )],
    };
    store.upsert_task_artifacts(&task.id, &params).unwrap();
    let files = store.get_task_artifacts(&task.id).unwrap().unwrap();
    let witness = files
        .iter()
        .find(|a| a.path == EVIDENCE_AUTHORITY_ARTIFACT)
        .unwrap();
    let origin: EvidenceSubmission = serde_json::from_slice(&witness.content).unwrap();
    assert_eq!(origin.run_id, "trusted-run");
    assert_eq!(origin.action_id, task.id);
    for path in [
        EVIDENCE_AUTHORITY_ARTIFACT.to_string(),
        format!("./{EVIDENCE_AUTHORITY_ARTIFACT}"),
    ] {
        assert!(
            store
                .upsert_task_artifacts(
                    &task.id,
                    &TaskArtifactUpdateParams {
                        actor: "attacker".into(),
                        owner_run_id: None,
                        upsert_artifacts: vec![TaskArtifact::from_text(&path, "forged")]
                    }
                )
                .is_err()
        );
    }
    assert_eq!(store.get_task_artifacts(&task.id).unwrap().unwrap(), files);
}

fn assert_pre_transition(store: &TaskV2Store) {
    let task = store
        .get_task("ORB-00000")
        .expect("get task")
        .expect("task exists");
    assert_eq!(task.status, TaskStatus::Backlog);
    let listed = store.list_tasks().expect("list tasks");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].status, TaskStatus::Backlog);
}

fn transition_to_in_progress(store: &TaskV2Store) -> Result<(), orbit_common::OrbitError> {
    store.update_task_history(
        "ORB-00000",
        &TaskHistoryUpdateParams {
            actor: "codex:gpt-5.5".to_string(),
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        },
    )
}

/// After jsonl append, before envelope publish: abort restores the pre-call
/// bundle so listing stays on the old status.
#[test]
fn history_update_aborts_when_jsonl_append_is_followed_by_injected_failure() {
    use crate::driver::file::task_bundle::{BundleWriteFault, inject_bundle_write_faults};

    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("Abort after append", TaskStatus::Backlog))
        .expect("create task");
    inject_bundle_write_faults(&[BundleWriteFault::AfterJsonlAppend]);
    let error = transition_to_in_progress(&store).expect_err("injected after append");
    assert!(error.to_string().contains("AfterJsonlAppend"), "{error}");
    assert_pre_transition(&store);
}

/// Envelope is staged but not renamed: abort still yields the pre-call status.
#[test]
fn history_update_aborts_when_envelope_stage_fails_before_rename() {
    use crate::driver::file::task_bundle::{BundleWriteFault, inject_bundle_write_faults};

    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("Abort after stage", TaskStatus::Backlog))
        .expect("create task");
    inject_bundle_write_faults(&[BundleWriteFault::AfterEnvelopeStage]);
    let error = transition_to_in_progress(&store).expect_err("injected after stage");
    assert!(error.to_string().contains("AfterEnvelopeStage"), "{error}");
    assert_pre_transition(&store);
}

/// Compensation itself fails: pending evidence remains, listing still serves
/// the pre-call view, and reindex retries recovery to a consistent bundle.
#[test]
fn reindex_recovers_an_incomplete_write_left_by_failed_compensation() {
    use orbit_types::task::TASK_EVENTS_FILE_NAME;

    use crate::driver::file::task_bundle::{
        BundleWriteFault, PENDING_WRITE_FILE_NAME, inject_bundle_write_faults,
    };
    use crate::workflow::task::reindex_workspace;

    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("Interrupted", TaskStatus::Backlog))
        .expect("create task");
    let bundle_dir = store
        .bundle_store
        .bundle_path("ORB-00000")
        .expect("bundle path");
    let events_before =
        std::fs::read(bundle_dir.join(TASK_EVENTS_FILE_NAME)).expect("events before");

    inject_bundle_write_faults(&[
        BundleWriteFault::AfterJsonlAppend,
        BundleWriteFault::DuringCompensation,
    ]);
    transition_to_in_progress(&store).expect_err("injected incomplete write");
    assert!(
        bundle_dir.join(PENDING_WRITE_FILE_NAME).is_file(),
        "failed compensation must retain pending-write evidence"
    );
    assert_ne!(
        std::fs::read(bundle_dir.join(TASK_EVENTS_FILE_NAME)).expect("events after inject"),
        events_before,
        "the status event was appended before compensation failed"
    );
    assert_pre_transition(&store);

    inject_bundle_write_faults(&[BundleWriteFault::DuringRecovery]);
    reindex_workspace(&store.registry, &store.workspace_id).expect_err("injected recovery failure");
    assert!(bundle_dir.join(PENDING_WRITE_FILE_NAME).is_file());
    assert_pre_transition(&store);

    inject_bundle_write_faults(&[]);
    reindex_workspace(&store.registry, &store.workspace_id).expect("reindex recovers");
    assert!(
        !bundle_dir.join(PENDING_WRITE_FILE_NAME).is_file(),
        "recovery removes pending-write evidence"
    );
    assert_eq!(
        std::fs::read(bundle_dir.join(TASK_EVENTS_FILE_NAME)).expect("events after recover"),
        events_before
    );
    assert_pre_transition(&store);
}

#[test]
fn document_update_aborts_description_when_envelope_stage_fails() {
    use crate::driver::file::task_bundle::{BundleWriteFault, inject_bundle_write_faults};

    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("Docs", TaskStatus::Backlog))
        .expect("create task");
    inject_bundle_write_faults(&[BundleWriteFault::AfterEnvelopeStage]);
    store
        .update_task_document(
            "ORB-00000",
            &TaskDocumentUpdateParams {
                actor: "codex:gpt-5.5".to_string(),
                description: Some("should not stick".to_string()),
                ..Default::default()
            },
        )
        .expect_err("injected envelope stage failure");
    let task = store
        .get_task("ORB-00000")
        .expect("get task")
        .expect("task exists");
    assert_eq!(task.description, "Detailed task description");
    assert_eq!(task.status, TaskStatus::Backlog);
}

#[test]
fn atomic_task_mutation_commits_receipt_event_and_envelope_together() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("Atomic", TaskStatus::Backlog))
        .expect("create task");
    let params = AtomicTaskMutationParams {
        actor: "task-pilot".to_string(),
        operation_id: "operation-one".to_string(),
        expected_context_files: vec!["docs/design/task-artifacts/1_overview.md".to_string()],
        expected_status: TaskStatus::Backlog,
        expected_complexity: None,
        context_files: vec!["file:src/lib.rs".to_string()],
        status: TaskStatus::Backlog,
        complexity: TaskComplexity::Hard,
        event_type: "task_pilot_applied".to_string(),
        event_note: "task-pilot atomic application".to_string(),
        audit_note: r#"{"assessment_rationale":"cross-component repair"}"#.to_string(),
    };

    assert_eq!(
        store
            .apply_atomic_task_mutation("ORB-00000", &params)
            .expect("apply mutation"),
        AtomicTaskMutationOutcome::Applied
    );
    assert_eq!(
        store
            .apply_atomic_task_mutation("ORB-00000", &params)
            .expect("replay mutation"),
        AtomicTaskMutationOutcome::AlreadyApplied
    );
    let task = store.get_task("ORB-00000").unwrap().unwrap();
    assert_eq!(task.context_files, vec!["file:src/lib.rs"]);
    assert_eq!(task.complexity, Some(TaskComplexity::Hard));
    let history = store.get_task_history("ORB-00000").unwrap().unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event == "task_pilot_applied")
            .count(),
        1
    );
    let note = history
        .iter()
        .find(|event| event.event == "task_pilot_applied")
        .and_then(|event| event.note.as_deref())
        .expect("task-pilot audit note");
    assert!(note.starts_with("operation_id=operation-one\n"), "{note}");
    assert!(note.contains("assessment_rationale"), "{note}");
}

#[test]
fn atomic_task_mutation_refuses_a_changed_complexity_without_partial_audit() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("Atomic stale", TaskStatus::Backlog))
        .expect("create task");
    store
        .update_task_document(
            "ORB-00000",
            &TaskDocumentUpdateParams {
                actor: "operator".to_string(),
                complexity: Some(TaskComplexity::Hard),
                ..Default::default()
            },
        )
        .expect("operator changes complexity");
    let history_before = store.get_task_history("ORB-00000").unwrap().unwrap();

    let outcome = store
        .apply_atomic_task_mutation(
            "ORB-00000",
            &AtomicTaskMutationParams {
                actor: "task-pilot".to_string(),
                operation_id: "stale-complexity".to_string(),
                expected_context_files: vec![
                    "docs/design/task-artifacts/1_overview.md".to_string(),
                ],
                expected_status: TaskStatus::Backlog,
                expected_complexity: None,
                context_files: vec!["file:src/lib.rs".to_string()],
                status: TaskStatus::Backlog,
                complexity: TaskComplexity::Low,
                event_type: "task_pilot_applied".to_string(),
                event_note: "task-pilot atomic application".to_string(),
                audit_note: "stale assessment".to_string(),
            },
        )
        .expect("stale mutation is a structured outcome");

    assert_eq!(outcome, AtomicTaskMutationOutcome::Stale);
    let task = store.get_task("ORB-00000").unwrap().unwrap();
    assert_eq!(task.complexity, Some(TaskComplexity::Hard));
    assert_eq!(
        task.context_files,
        vec!["docs/design/task-artifacts/1_overview.md"]
    );
    assert_eq!(
        store.get_task_history("ORB-00000").unwrap().unwrap(),
        history_before
    );
}

#[test]
fn atomic_task_mutation_rolls_back_receipt_when_envelope_publish_fails() {
    use crate::driver::file::task_bundle::{BundleWriteFault, inject_bundle_write_faults};

    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("Atomic fault", TaskStatus::Backlog))
        .expect("create task");
    let history_before = store.get_task_history("ORB-00000").unwrap().unwrap();
    inject_bundle_write_faults(&[BundleWriteFault::AfterEnvelopeStage]);

    store
        .apply_atomic_task_mutation(
            "ORB-00000",
            &AtomicTaskMutationParams {
                actor: "task-pilot".to_string(),
                operation_id: "operation-fault".to_string(),
                expected_context_files: vec![
                    "docs/design/task-artifacts/1_overview.md".to_string(),
                ],
                expected_status: TaskStatus::Backlog,
                expected_complexity: None,
                context_files: vec!["file:src/lib.rs".to_string()],
                status: TaskStatus::Backlog,
                complexity: TaskComplexity::Hard,
                event_type: "task_pilot_applied".to_string(),
                event_note: "task-pilot atomic application".to_string(),
                audit_note: r#"{"assessment_rationale":"fault fixture"}"#.to_string(),
            },
        )
        .expect_err("injected publish failure");

    let task = store.get_task("ORB-00000").unwrap().unwrap();
    assert_eq!(
        task.context_files,
        vec!["docs/design/task-artifacts/1_overview.md"]
    );
    assert_eq!(task.complexity, None);
    assert_eq!(
        store.get_task_history("ORB-00000").unwrap().unwrap(),
        history_before
    );
}
