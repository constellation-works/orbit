//! Task writes scrub prose and reject invalid artifacts atomically [ORB-14345].

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::task::{TaskArtifact, TaskComplexity, TaskStatus};
use orbit_types::workflow::REVIEW_REPORT_HISTORY_ARTIFACT;
use orbit_types::workflow::automation::{COVERAGE_ARTIFACT, EVIDENCE_AUTHORITY_ARTIFACT};

#[test]
fn activity_updates_refuse_replica_writes_and_preserve_owner_updates() {
    if !super::dispatch_admission::isolated(
        "task_update::activity_updates_refuse_replica_writes_and_preserve_owner_updates",
    ) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let owner = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = owner
        .add_task(TaskAddParams {
            title: "Activity coordination fence".into(),
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .unwrap();
    let before = owner.get_task(&task.id).unwrap();
    let history = owner.get_task_history(&task.id).unwrap();
    let comments = owner.get_task_comments(&task.id).unwrap();
    let replica = owner
        .clone()
        .with_coordination_write_owner(Some("owner-machine".into()));
    let ordinary_refusal = replica
        .update_task_with_identity(&task.id, TaskUpdateParams::default(), None, None)
        .expect_err("ordinary task writes must refuse the replica");
    let update = orbit_engine::TaskActivityUpdate {
        status: TaskStatus::Review,
        expected_status: TaskStatus::InProgress,
        execution_summary: Some("Activity summary".into()),
        comment: Some("Activity comment".into()),
        note: Some("Activity transition".into()),
        agent: None,
        model: Some("codex".into()),
        calling_run_id: None,
    };
    for result in [
        replica.update_task_from_activity(&task.id, update.clone()),
        orbit_engine::RuntimeHost::update_task_from_activity(&replica, &task.id, update.clone()),
    ] {
        let error = result.expect_err("activity updates must refuse the replica");
        let OrbitError::CapabilityRefused(reason) = &error else {
            panic!("expected the replica capability refusal, got {error:?}");
        };
        assert!(
            reason.contains("owner-machine"),
            "refusal must name the owner"
        );
        assert_eq!(error.to_string(), ordinary_refusal.to_string());
    }
    let persisted = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    assert_eq!(persisted.get_task(&task.id).unwrap(), before);
    assert_eq!(persisted.get_task_history(&task.id).unwrap(), history);
    assert_eq!(persisted.get_task_comments(&task.id).unwrap(), comments);

    orbit_engine::RuntimeHost::update_task_from_activity(&owner, &task.id, update)
        .expect("the owner host can apply the same activity update");
    let persisted = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let updated = persisted.get_task(&task.id).unwrap();
    assert_eq!(updated.status, TaskStatus::Review);
    assert_eq!(updated.execution_summary, "Activity summary");
    assert_eq!(
        persisted.get_task_comments(&task.id).unwrap().len(),
        comments.len() + 1
    );
    assert!(
        persisted
            .get_task_history(&task.id)
            .unwrap()
            .iter()
            .any(|event| event.to_status == Some(TaskStatus::Review)
                && event.note.as_deref() == Some("Activity transition"))
    );
}

#[test]
fn guarded_start_and_activity_notes_redact_before_persistence() {
    if !super::dispatch_admission::isolated(
        "task_update::guarded_start_and_activity_notes_redact_before_persistence",
    ) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Guarded start redaction".into(),
            complexity: TaskComplexity::Low,
            ..Default::default()
        })
        .unwrap();
    let token = format!("glpat-{}", "c".repeat(20));
    let text = format!("diagnostic {token}");
    let safe = "diagnostic [REDACTED_SECRET]";
    runtime
        .start_task_with_identity_and_crew(
            &task.id,
            Some(text.clone()),
            Some(text.clone()),
            None,
            Some("codex".into()),
            None,
            Some(text.clone()),
            TaskUpdateParams {
                title: Some(text.clone()),
                description: Some(text.clone()),
                acceptance_criteria: Some(vec![text.clone()]),
                execution_summary: Some(text.clone()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    runtime
        .update_task_from_activity(
            &task.id,
            orbit_engine::TaskActivityUpdate {
                status: TaskStatus::Review,
                expected_status: TaskStatus::InProgress,
                execution_summary: Some(text.clone()),
                comment: Some(text.clone()),
                note: Some(text),
                agent: None,
                model: Some("codex".into()),
                calling_run_id: None,
            },
        )
        .unwrap();
    let persisted = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let updated = persisted.get_task(&task.id).unwrap();
    assert_eq!(updated.status, TaskStatus::Review);
    assert_eq!(updated.title, safe);
    assert_eq!(updated.description, safe);
    assert_eq!(updated.plan, safe);
    assert_eq!(updated.execution_summary, safe);
    assert_eq!(updated.acceptance_criteria, [safe]);
    let comments = persisted.get_task_comments(&task.id).unwrap();
    assert_eq!(comments.len(), 2);
    assert!(comments.iter().all(|comment| comment.message == safe));
    let history = persisted.get_task_history(&task.id).unwrap();
    for target in [TaskStatus::Backlog, TaskStatus::Review] {
        assert!(history.iter().any(|event| event.to_status == Some(target)
            && event.note.as_deref() == Some(safe)), "transition note must be scrubbed");
    }
}

#[test]
fn rejected_artifacts_leave_status_document_and_history_unchanged() {
    if !super::dispatch_admission::isolated(
        "task_update::rejected_artifacts_leave_status_document_and_history_unchanged",
    ) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Artifact rejection fixture".into(),
            description: "Original document".into(),
            acceptance_criteria: vec!["Rejected updates persist nothing.".into()],
            plan: "Exercise task updates.".into(),
            complexity: TaskComplexity::Low,
            status: Some(TaskStatus::Review),
            ..Default::default()
        })
        .unwrap();
    let before = runtime.get_task(&task.id).unwrap();
    let history = runtime.get_task_history(&task.id).unwrap();
    let comments = runtime.get_task_comments(&task.id).unwrap();
    let payload = TaskUpdateParams {
        status: Some(TaskStatus::Done),
        description: Some("Replacement document".into()),
        execution_summary: Some("Completed the fixture.".into()),
        comment: Some("Record completion.".into()),
        ..Default::default()
    };

    for path in [
        "../escape.txt",
        "/absolute.txt",
        EVIDENCE_AUTHORITY_ARTIFACT,
        REVIEW_REPORT_HISTORY_ARTIFACT,
        COVERAGE_ARTIFACT,
    ] {
        let mut rejected = payload.clone();
        rejected.upsert_artifacts = vec![
            TaskArtifact::from_text("notes/valid.txt", "valid first artifact"),
            TaskArtifact::from_text(path, "not json"),
        ];
        let error = runtime
            .update_task_with_identity(&task.id, rejected, None, Some("codex".into()))
            .expect_err("invalid artifact must refuse the update");
        assert!(
            matches!(error, OrbitError::InvalidInput(_)),
            "{path}: {error}"
        );

        // Reopen the public runtime to check durable state, including metadata,
        // status, document fields, events, comments and the artifact manifest.
        let persisted = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        assert_eq!(persisted.get_task(&task.id).unwrap(), before, "{path}");
        assert_eq!(
            persisted.get_task_history(&task.id).unwrap(),
            history,
            "{path}"
        );
        assert_eq!(
            persisted.get_task_comments(&task.id).unwrap(),
            comments,
            "{path}"
        );
        assert!(
            persisted
                .get_task_artifact_manifest(&task.id)
                .unwrap()
                .is_empty(),
            "{path}"
        );
    }

    // The same transition and document/history edits succeed with valid
    // artifacts, proving the refusals above reach artifact validation.
    let mut accepted = payload;
    accepted.upsert_artifacts = vec![TaskArtifact::from_text(
        "notes/valid.txt",
        "accepted artifact",
    )];
    runtime
        .update_task_with_identity(&task.id, accepted, None, Some("codex".into()))
        .expect("valid artifacts permit the combined update");
    let persisted = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let updated = persisted.get_task(&task.id).unwrap();
    assert_eq!(updated.status, TaskStatus::Done);
    assert_eq!(updated.description, "Replacement document");
    assert_eq!(updated.execution_summary, "Completed the fixture.");
    assert!(persisted.get_task_history(&task.id).unwrap().len() > history.len());
    assert_eq!(
        persisted.get_task_comments(&task.id).unwrap().len(),
        comments.len() + 1
    );
    assert_eq!(
        persisted
            .get_task_artifact(&task.id, "notes/valid.txt")
            .unwrap()
            .unwrap()
            .content,
        b"accepted artifact",
    );
}

#[test]
fn automation_persists_returned_summary_crew_and_run_history_without_status_change() {
    if !super::dispatch_admission::isolated(
        "task_update::automation_persists_returned_summary_crew_and_run_history_without_status_change",
    ) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Returned summary persistence".into(),
            plan: "Validate the unchanged tree.".into(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();
    orbit_engine::RuntimeHost::apply_task_automation_update(
        &runtime,
        &task.id,
        orbit_engine::TaskAutomationUpdate {
            expected_status: Some(TaskStatus::Backlog),
            execution_summary: Some("Outcome: success\nValidated the unchanged tree.".into()),
            implemented_by: Some("fixture-crew".into()),
            status_event: Some("execution_summary_persisted".into()),
            status_note: Some("persisted returned summary from run fixture-run".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let persisted = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let updated = persisted.get_task(&task.id).unwrap();
    assert_eq!(updated.status, TaskStatus::Backlog);
    assert_eq!(
        updated.execution_summary,
        "Outcome: success\nValidated the unchanged tree."
    );
    assert_eq!(updated.implemented_by.as_deref(), Some("fixture-crew"));
    assert!(
        persisted
            .get_task_history(&task.id)
            .unwrap()
            .iter()
            .any(|event| event.event == "execution_summary_persisted"
                && event.note.as_deref()
                    == Some("persisted returned summary from run fixture-run"))
    );
}
