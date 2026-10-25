//! Task writes scrub prose and reject invalid artifacts atomically [ORB-14345].

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::task::{TaskArtifact, TaskComplexity, TaskStatus};
use orbit_types::workflow::REVIEW_REPORT_HISTORY_ARTIFACT;
use orbit_types::workflow::automation::{COVERAGE_ARTIFACT, EVIDENCE_AUTHORITY_ARTIFACT};

fn activity_updates_refuse_replica_writes_and_preserve_owner_updates() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
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

fn guarded_start_and_activity_notes_redact_before_persistence() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
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

fn rejected_artifacts_leave_status_document_and_history_unchanged() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
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

fn automation_persists_returned_summary_crew_and_run_history_without_status_change() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
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

fn pr_status_updates_share_admission_and_trim_before_storage() {
    use serde_json::json;

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Review decision admission".into(),
            ..Default::default()
        })
        .unwrap();
    let before = runtime.get_task(&task.id).unwrap();
    let history = runtime.get_task_history(&task.id).unwrap();
    let comments = runtime.get_task_comments(&task.id).unwrap();
    let error = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id, "pr_status": "aproved", "title": "must not persist",
                "comment": "must not persist", "model": "codex"
            }),
            None,
            Some("codex".into()),
        )
        .expect_err("MCP typo must be refused atomically");
    assert!(matches!(error, OrbitError::InvalidInput(_)), "{error}");
    assert!(
        error.to_string().contains("unknown PR status 'aproved'"),
        "{error}"
    );
    assert_eq!(runtime.get_task(&task.id).unwrap(), before);
    assert_eq!(runtime.get_task_history(&task.id).unwrap(), history);
    assert_eq!(runtime.get_task_comments(&task.id).unwrap(), comments);

    for (value, normalized) in [
        ("approve", "APPROVED"),
        ("approved", "APPROVED"),
        ("request-changes", "CHANGES_REQUESTED"),
        ("request_changes", "CHANGES_REQUESTED"),
        ("changes-requested", "CHANGES_REQUESTED"),
        ("changes_requested", "CHANGES_REQUESTED"),
        (" APPROVED ", "APPROVED"),
        (" ApPrOvEd ", "APPROVED"),
        (" ReQuEsT-ChAnGeS ", "CHANGES_REQUESTED"),
        ("", ""),
        ("   ", ""),
    ] {
        assert_eq!(
            orbit_engine::normalize_review_decision(value),
            normalized,
            "{value:?}"
        );
        if value.trim().is_empty() {
            runtime
                .update_task_with_identity(
                    &task.id,
                    TaskUpdateParams {
                        pr_status: Some(Some("approve".into())),
                        ..Default::default()
                    },
                    None,
                    None,
                )
                .unwrap();
        }
        runtime
            .execute_tool_command(
                "orbit.task.update",
                json!({
                    "id": task.id, "pr_status": value, "model": "codex"
                }),
                None,
                Some("codex".into()),
            )
            .unwrap();
        let expected = (!value.trim().is_empty()).then(|| value.trim().to_string());
        assert_eq!(
            runtime.get_task(&task.id).unwrap().pr_status,
            expected,
            "MCP {value:?}"
        );
        // The direct runtime boundary must not depend on adapter parsing.
        if value.trim().is_empty() {
            runtime
                .update_task_with_identity(
                    &task.id,
                    TaskUpdateParams {
                        pr_status: Some(Some("approve".into())),
                        ..Default::default()
                    },
                    None,
                    None,
                )
                .unwrap();
        }
        runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    pr_status: Some(Some(value.into())),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            runtime.get_task(&task.id).unwrap().pr_status,
            expected,
            "runtime {value:?}"
        );
    }
    runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id, "pr_status": "approve", "model": "codex"
            }),
            None,
            Some("codex".into()),
        )
        .unwrap();
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                pr_status: Some(None),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    assert_eq!(runtime.get_task(&task.id).unwrap().pr_status, None);
}

fn muse_identity_is_accepted_and_mismatches_refused() {
    use serde_json::json;

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Muse provenance fixture".into(),
            ..Default::default()
        })
        .unwrap();

    for (input, message) in [
        (
            json!({"id": task.id, "comment": "bare family", "model": "muse"}),
            "bare family",
        ),
        (
            json!({"id": task.id, "comment": "full model", "model": "muse-wonder-1.0"}),
            "full model",
        ),
    ] {
        runtime
            .execute_tool_command("orbit.task.update", input, None, None)
            .unwrap_or_else(|error| panic!("muse identity must be accepted: {error}"));
        let comments = runtime.get_task_comments(&task.id).unwrap();
        let comment = comments
            .iter()
            .find(|comment| comment.message == message)
            .expect("muse comment must persist");
        // Bare family records `muse`; a full model string keeps its model
        // label, which must still resolve to the muse family.
        let expected_by = if message == "bare family" {
            "muse"
        } else {
            "muse-wonder-1.0"
        };
        assert_eq!(comment.by, expected_by, "{message}");
        assert_eq!(
            orbit_types::identity::agent_from_model(&comment.by),
            Some("muse"),
            "{message} must record the muse family"
        );
    }

    // The matching agent/model pair reaches the same validator through the
    // direct runtime identity API (the tool surface carries only `model`).
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                comment: Some("matching pair".into()),
                ..Default::default()
            },
            Some("muse".into()),
            Some("muse-wonder-1.0".into()),
        )
        .expect("matching muse agent/model pair must be accepted");
    assert!(
        runtime
            .get_task_comments(&task.id)
            .unwrap()
            .iter()
            .any(|comment| comment.message == "matching pair"
                && orbit_types::identity::agent_from_model(&comment.by) == Some("muse"))
    );

    let before = runtime.get_task_comments(&task.id).unwrap();
    let mismatch = runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                comment: Some("must not persist".into()),
                ..Default::default()
            },
            Some("muse".into()),
            Some("claude".into()),
        )
        .expect_err("muse agent with a claude model must be refused");
    assert!(
        mismatch.to_string().contains("does not match"),
        "{mismatch}"
    );
    let unknown = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({"id": task.id, "comment": "must not persist", "model": "llama"}),
            None,
            None,
        )
        .expect_err("unknown family must be refused");
    assert!(
        unknown
            .to_string()
            .contains("not a canonical agent family (codex, claude, gemini, grok, muse)"),
        "{unknown}"
    );
    assert_eq!(runtime.get_task_comments(&task.id).unwrap(), before);
}

fn tag_changes_record_previous_and_new_tags_in_history() {
    use orbit_types::task::{TAGS_CHANGED_EVENT, TagsChanged, tags_at};

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Tag history fixture".into(),
            tags: vec!["audit-work".into()],
            ..Default::default()
        })
        .unwrap();
    // A write that leaves the tags alone records no tag change.
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                comment: Some("no retag".into()),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                tags: Some(vec!["no-diff-expected".into()]),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    let persisted = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let history = persisted.get_task_history(&task.id).unwrap();
    let changes: Vec<TagsChanged> = history
        .iter()
        .filter(|entry| entry.event == TAGS_CHANGED_EVENT)
        .map(|entry| {
            TagsChanged::from_entry(entry)
                .unwrap()
                .expect("tags_changed entry carries its lists")
        })
        .collect();
    assert_eq!(
        changes,
        [TagsChanged {
            previous: vec!["audit-work".into()],
            tags: vec!["no-diff-expected".into()],
        }],
        "one retag records exactly one previous/new pair"
    );
    let current = persisted.get_task(&task.id).unwrap().tags;
    let filed_at = history.first().unwrap().at;
    assert_eq!(
        tags_at(&history, &current, filed_at),
        ["audit-work"],
        "replay before the retag reads the filed tags"
    );
    assert_eq!(
        tags_at(&history, &current, chrono::Utc::now()),
        ["no-diff-expected"],
        "replay after the retag reads the new tags"
    );
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    activity_updates_refuse_replica_writes_and_preserve_owner_updates_scenarios,
    activity_updates_refuse_replica_writes_and_preserve_owner_updates,
    guarded_start_and_activity_notes_redact_before_persistence,
    rejected_artifacts_leave_status_document_and_history_unchanged,
    automation_persists_returned_summary_crew_and_run_history_without_status_change,
    pr_status_updates_share_admission_and_trim_before_storage,
    muse_identity_is_accepted_and_mismatches_refused,
    tag_changes_record_previous_and_new_tags_in_history
);
