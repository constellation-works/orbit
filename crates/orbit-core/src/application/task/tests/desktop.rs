use super::{enter_isolated_child, test_runtime};
use crate::{
    ActorIdentity,
    application::task::{TaskAddParams, TaskUpdateParams},
};
use orbit_types::{
    desktop::*,
    task::TaskStatus,
    tool::{McpCapability, ToolSessionContext},
};
fn session(operator: bool) -> ToolSessionContext {
    let mut session = ToolSessionContext::default();
    session.effective_capabilities.insert(if operator {
        McpCapability::Operator
    } else {
        McpCapability::Agent
    });
    session
}
fn create(request_id: &str) -> DesktopTaskRequest {
    DesktopTaskRequest {
        request_id: request_id.into(),
        operation: DesktopTaskOperation::Create {
            title: "Desktop fixture".into(),
            description: "bounded".into(),
            acceptance_criteria: vec!["verified behavior".into()],
            priority: orbit_types::task::TaskPriority::Medium,
            crew: None,
        },
    }
}
#[test]
fn desktop_create_retries_once_and_refuses_changed_payload() {
    if !enter_isolated_child(
        module_path!(),
        "desktop_create_retries_once_and_refuses_changed_payload",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let session = session(false);
    let first = runtime
        .desktop_task_write(create("create-once"), None, None, &session)
        .unwrap();
    let replay = runtime
        .desktop_task_write(create("create-once"), None, None, &session)
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(first.snapshot.revision, replay.snapshot.revision);
    assert_eq!(first.snapshot.task.status, TaskStatus::Proposed);
    assert_eq!(runtime.list_tasks().unwrap().len(), 1);
    let mut changed = create("create-once");
    if let DesktopTaskOperation::Create { title, .. } = &mut changed.operation {
        *title = "changed".into();
    }
    assert!(
        runtime
            .desktop_task_write(changed, None, None, &session)
            .is_err()
    );
}
#[test]
fn desktop_comment_retry_and_competing_edit_are_guarded() {
    if !enter_isolated_child(
        module_path!(),
        "desktop_comment_retry_and_competing_edit_are_guarded",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let session = session(false);
    let first = runtime
        .desktop_task_write(create("create"), None, None, &session)
        .unwrap()
        .snapshot;
    let comment = DesktopTaskRequest {
        request_id: "comment".into(),
        operation: DesktopTaskOperation::Comment {
            id: first.task.id.clone(),
            expected_revision: first.revision.clone(),
            comment: "one comment".into(),
        },
    };
    let once = runtime
        .desktop_task_write(comment.clone(), None, None, &session)
        .unwrap();
    let twice = runtime
        .desktop_task_write(comment, None, None, &session)
        .unwrap();
    assert!(twice.replayed);
    assert_eq!(once.snapshot.revision, twice.snapshot.revision);
    assert_eq!(twice.snapshot.comments_total, 1);
    let stale = DesktopTaskRequest {
        request_id: "edit".into(),
        operation: DesktopTaskOperation::Edit {
            id: first.task.id.clone(),
            expected_revision: first.revision,
            fields: DesktopTaskFields {
                title: Some("stale overwrite".into()),
                ..Default::default()
            },
        },
    };
    assert!(
        runtime
            .desktop_task_write(stale, None, None, &session)
            .is_err()
    );
    assert_eq!(
        runtime.get_task(&first.task.id).unwrap().title,
        "Desktop fixture"
    );
    let changed = DesktopTaskRequest {
        request_id: "comment".into(),
        operation: DesktopTaskOperation::Comment {
            id: first.task.id,
            expected_revision: twice.snapshot.revision,
            comment: "changed retry".into(),
        },
    };
    assert!(
        runtime
            .desktop_task_write(changed, None, None, &session)
            .is_err()
    );
}
fn verdict(decision: DesktopReviewDecision) -> DesktopReviewVerdict {
    DesktopReviewVerdict {
        decision,
        rationale: "checked fixture evidence".into(),
        criteria: vec![DesktopCriterionOutcome {
            criterion: "verified behavior".into(),
            met: decision == DesktopReviewDecision::Accept,
            evidence: vec!["execution_summary".into()],
        }],
        evidence: vec!["execution_summary".into()],
        expected_run_id: None,
        expected_head: None,
    }
}
#[test]
fn desktop_review_completion_requires_authority_and_commits_verdict_once() {
    if !enter_isolated_child(
        module_path!(),
        "desktop_review_completion_requires_authority_and_commits_verdict_once",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let runtime = runtime.with_actor(ActorIdentity::human("fixture"));
    let task = runtime
        .add_task(TaskAddParams {
            title: "Review fixture".into(),
            acceptance_criteria: vec!["verified behavior".into()],
            ..Default::default()
        })
        .unwrap();
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Review),
                execution_summary: Some("fixture completed".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let snapshot = runtime
        .desktop_task_snapshot(&task.id, &session(false))
        .unwrap();
    assert!(!snapshot.actions.complete.enabled);
    let request = DesktopTaskRequest {
        request_id: "accept".into(),
        operation: DesktopTaskOperation::Review {
            id: task.id.clone(),
            expected_revision: snapshot.revision,
            verdict: verdict(DesktopReviewDecision::Accept),
            complete: true,
        },
    };
    assert!(
        runtime
            .desktop_task_write(request.clone(), None, None, &session(false))
            .is_err()
    );
    let done = runtime
        .desktop_task_write(request.clone(), None, None, &session(true))
        .unwrap();
    assert_eq!(done.snapshot.task.status, TaskStatus::Done);
    assert_eq!(done.snapshot.comments_total, 1);
    assert!(
        runtime
            .desktop_task_write(request, None, None, &session(true))
            .unwrap()
            .replayed
    );
    let history = runtime.get_task_history(&task.id).unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|h| h.event == "desktop_mutation")
            .count(),
        1
    );
}
#[test]
fn desktop_changes_requested_stays_review_and_pagination_reads_older_comments() {
    if !enter_isolated_child(
        module_path!(),
        "desktop_changes_requested_stays_review_and_pagination_reads_older_comments",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let session = session(false);
    let task = runtime
        .add_task(TaskAddParams {
            title: "Change fixture".into(),
            acceptance_criteria: vec!["verified behavior".into()],
            ..Default::default()
        })
        .unwrap();
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Review),
                execution_summary: Some("fixture evidence".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let snapshot = runtime.desktop_task_snapshot(&task.id, &session).unwrap();
    let result = runtime
        .desktop_task_write(
            DesktopTaskRequest {
                request_id: "changes".into(),
                operation: DesktopTaskOperation::Review {
                    id: task.id.clone(),
                    expected_revision: snapshot.revision,
                    verdict: verdict(DesktopReviewDecision::ChangesRequested),
                    complete: false,
                },
            },
            None,
            None,
            &session,
        )
        .unwrap();
    assert_eq!(result.snapshot.task.status, TaskStatus::Review);
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                comment: Some("second".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let page = runtime
        .desktop_task_snapshot_page(&task.id, 1, 0, 0, 1, &session)
        .unwrap();
    assert_eq!(page.comments_total, 2);
    assert_eq!(page.comments.len(), 1);
    assert_eq!(page.comments[0].message, "second");
}
#[test]
fn desktop_concurrent_edit_has_exactly_one_winner() {
    if !enter_isolated_child(
        module_path!(),
        "desktop_concurrent_edit_has_exactly_one_winner",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let session = session(false);
    let snapshot = runtime
        .desktop_task_write(create("create"), None, None, &session)
        .unwrap()
        .snapshot;
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let jobs: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|title| {
                let snapshot = &snapshot;
                let runtime = &runtime;
                let session = &session;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    runtime.desktop_task_write(
                        DesktopTaskRequest {
                            request_id: title.into(),
                            operation: DesktopTaskOperation::Edit {
                                id: snapshot.task.id.clone(),
                                expected_revision: snapshot.revision.clone(),
                                fields: DesktopTaskFields {
                                    title: Some(title.into()),
                                    ..Default::default()
                                },
                            },
                        },
                        None,
                        None,
                        session,
                    )
                })
            })
            .collect();
        jobs.into_iter()
            .map(|job| job.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        runtime
            .get_task_history(&snapshot.task.id)
            .unwrap()
            .iter()
            .filter(|h| h.event == "desktop_mutation")
            .count(),
        1
    );
}
#[test]
fn desktop_failed_or_missing_run_evidence_cannot_complete() {
    if !enter_isolated_child(
        module_path!(),
        "desktop_failed_or_missing_run_evidence_cannot_complete",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let session = session(true);
    let task = runtime
        .add_task(TaskAddParams {
            title: "Failed fixture".into(),
            acceptance_criteria: vec!["verified behavior".into()],
            ..Default::default()
        })
        .unwrap();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("desktop-failed", 1, chrono::Utc::now(), None, None)
        .unwrap();
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, chrono::Utc::now(), std::process::id())
        .unwrap();
    runtime
        .stores()
        .jobs()
        .finalize_job_run(
            &run.run_id,
            orbit_types::workflow::JobRunState::Failed,
            chrono::Utc::now(),
            Some(1),
        )
        .unwrap();
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Review),
                job_run_id: Some(Some(run.run_id.clone())),
                ..Default::default()
            },
        )
        .unwrap();
    let snapshot = runtime.desktop_task_snapshot(&task.id, &session).unwrap();
    assert!(!snapshot.actions.complete.enabled);
    let mut v = verdict(DesktopReviewDecision::Accept);
    v.expected_run_id = Some(run.run_id.clone());
    v.evidence = vec![run.run_id.clone()];
    v.criteria[0].evidence = v.evidence.clone();
    assert!(
        runtime
            .desktop_task_write(
                DesktopTaskRequest {
                    request_id: "failed-review".into(),
                    operation: DesktopTaskOperation::Review {
                        id: task.id.clone(),
                        expected_revision: snapshot.revision.clone(),
                        verdict: v,
                        complete: true
                    }
                },
                None,
                None,
                &session
            )
            .is_err()
    );
    let mut missing = verdict(DesktopReviewDecision::Accept);
    missing.expected_run_id = Some(run.run_id);
    missing.evidence = vec!["absent-artifact.txt".into()];
    assert!(
        runtime
            .desktop_task_write(
                DesktopTaskRequest {
                    request_id: "missing-review".into(),
                    operation: DesktopTaskOperation::Review {
                        id: task.id.clone(),
                        expected_revision: snapshot.revision,
                        verdict: missing,
                        complete: false
                    }
                },
                None,
                None,
                &session
            )
            .is_err()
    );
    assert_eq!(
        runtime.get_task(&task.id).unwrap().status,
        TaskStatus::Review
    );
    assert_eq!(runtime.get_task_comments(&task.id).unwrap().len(), 0);
}
#[test]
fn desktop_live_run_and_truncated_content_disable_completion() {
    if !enter_isolated_child(
        module_path!(),
        "desktop_live_run_and_truncated_content_disable_completion",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let session = session(true);
    let task = runtime
        .add_task(TaskAddParams {
            title: "Live fixture".into(),
            acceptance_criteria: vec!["verified behavior".into()],
            ..Default::default()
        })
        .unwrap();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("desktop-live", 1, chrono::Utc::now(), None, None)
        .unwrap();
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, chrono::Utc::now(), std::process::id())
        .unwrap();
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Review),
                job_run_id: Some(Some(run.run_id.clone())),
                execution_summary: Some("pending owner".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let snapshot = runtime.desktop_task_snapshot(&task.id, &session).unwrap();
    assert!(!snapshot.actions.complete.enabled);
    let mut v = verdict(DesktopReviewDecision::Accept);
    v.expected_run_id = Some(run.run_id);
    assert!(
        runtime
            .desktop_task_write(
                DesktopTaskRequest {
                    request_id: "live-review".into(),
                    operation: DesktopTaskOperation::Review {
                        id: task.id.clone(),
                        expected_revision: snapshot.revision,
                        verdict: v,
                        complete: true
                    }
                },
                None,
                None,
                &session
            )
            .is_err()
    );
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                description: Some("é".repeat(20_000)),
                ..Default::default()
            },
        )
        .unwrap();
    let snapshot = runtime.desktop_task_snapshot(&task.id, &session).unwrap();
    assert!(snapshot.content_truncated);
    assert!(snapshot.task.description.len() <= 32768);
    assert!(!snapshot.actions.edit.enabled);
    assert!(!snapshot.actions.review.enabled);
    assert!(!snapshot.actions.complete.enabled);
    assert!(
        snapshot
            .truncated_fields
            .iter()
            .any(|f| f == "task.description")
    );
}
#[test]
fn desktop_redacts_before_receipt_and_preserves_retries() {
    if !enter_isolated_child(
        module_path!(),
        "desktop_redacts_before_receipt_and_preserves_retries",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let session = session(false);
    let snapshot = runtime
        .desktop_task_write(create("create"), None, None, &session)
        .unwrap()
        .snapshot;
    let secret = "abc123def456ghi789SECRETTOKEN";
    let request = DesktopTaskRequest {
        request_id: "redacted".into(),
        operation: DesktopTaskOperation::Comment {
            id: snapshot.task.id.clone(),
            expected_revision: snapshot.revision,
            comment: format!("Authorization: Bearer {secret}"),
        },
    };
    let once = runtime
        .desktop_task_write(request.clone(), None, None, &session)
        .unwrap();
    let twice = runtime
        .desktop_task_write(request, None, None, &session)
        .unwrap();
    assert!(twice.replayed);
    assert_eq!(once.snapshot.revision, twice.snapshot.revision);
    assert!(
        !runtime.get_task_comments(&snapshot.task.id).unwrap()[0]
            .message
            .contains(secret)
    );
}
#[test]
fn desktop_pending_run_without_owner_cannot_complete() {
    if !enter_isolated_child(
        module_path!(),
        "desktop_pending_run_without_owner_cannot_complete",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let session = session(true);
    let task = runtime
        .add_task(TaskAddParams {
            title: "Pending fixture".into(),
            acceptance_criteria: vec!["verified behavior".into()],
            ..Default::default()
        })
        .unwrap();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("desktop-pending", 1, chrono::Utc::now(), None, None)
        .unwrap();
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Review),
                job_run_id: Some(Some(run.run_id.clone())),
                execution_summary: Some("claimed evidence before pending execution".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let snapshot = runtime.desktop_task_snapshot(&task.id, &session).unwrap();
    assert!(!snapshot.actions.complete.enabled);
    let mut v = verdict(DesktopReviewDecision::Accept);
    v.expected_run_id = Some(run.run_id);
    assert!(
        runtime
            .desktop_task_write(
                DesktopTaskRequest {
                    request_id: "pending-review".into(),
                    operation: DesktopTaskOperation::Review {
                        id: task.id.clone(),
                        expected_revision: snapshot.revision,
                        verdict: v,
                        complete: true
                    }
                },
                None,
                None,
                &session
            )
            .is_err()
    );
    assert_eq!(
        runtime.get_task(&task.id).unwrap().status,
        TaskStatus::Review
    );
    assert_eq!(runtime.get_task_comments(&task.id).unwrap().len(), 0);
}
#[cfg(unix)]
#[test]
fn desktop_pr_head_is_observed_and_changed_head_refuses_verdict() {
    if !enter_isolated_child(
        module_path!(),
        "desktop_pr_head_is_observed_and_changed_head_refuses_verdict",
    ) {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let (root, runtime) = test_runtime();
    let session = session(true);
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let url = "https://github.com/fixture/repository/pull/1";
    let observed = "1111111111111111111111111111111111111111";
    let changed = "2222222222222222222222222222222222222222";
    let gh = bin.join("gh");
    let script = |head: &str| {
        format!("#!/bin/sh\nprintf '%s\\n' '[{{\"url\":\"{url}\",\"headRefOid\":\"{head}\"}}]'\n")
    };
    std::fs::write(&gh, script(observed)).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700)).unwrap();
    let prior_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(bin.clone()).chain(std::env::split_paths(&prior_path)),
    )
    .unwrap();
    // This fixture runs in its own process; its PATH and stub cannot affect other tests or the parent.
    unsafe {
        std::env::set_var("PATH", path);
    }
    let task = runtime
        .add_task(TaskAddParams {
            title: "PR fixture".into(),
            acceptance_criteria: vec!["verified behavior".into()],
            external_refs: vec![
                orbit_types::task::ExternalRef::try_new(
                    orbit_types::task::GITHUB_PR_EXTERNAL_REF_SYSTEM.into(),
                    "1".into(),
                    Some(url.into()),
                )
                .unwrap(),
            ],
            ..Default::default()
        })
        .unwrap();
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Review),
                execution_summary: Some("fixture evidence".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let snapshot = runtime.desktop_task_snapshot(&task.id, &session).unwrap();
    assert_eq!(snapshot.reviewed_head.as_deref(), Some(observed));
    std::fs::write(&gh, script(changed)).unwrap();
    let mut v = verdict(DesktopReviewDecision::Accept);
    v.expected_head = Some(observed.into());
    assert!(
        runtime
            .desktop_task_write(
                DesktopTaskRequest {
                    request_id: "head-review".into(),
                    operation: DesktopTaskOperation::Review {
                        id: task.id.clone(),
                        expected_revision: snapshot.revision,
                        verdict: v,
                        complete: true
                    }
                },
                None,
                None,
                &session
            )
            .is_err()
    );
    assert_eq!(
        runtime.get_task(&task.id).unwrap().status,
        TaskStatus::Review
    );
    assert_eq!(runtime.get_task_comments(&task.id).unwrap().len(), 0);
    std::fs::write(&gh, "#!/bin/sh\nexit 1\n").unwrap();
    let unavailable = runtime.desktop_task_snapshot(&task.id, &session).unwrap();
    assert!(unavailable.reviewed_head.is_none());
    assert!(unavailable.reviewed_head_reason.is_some());
    assert!(!unavailable.actions.complete.enabled);
}

#[test]
fn desktop_snapshot_bounds_legacy_labels_and_omits_oversized_identities() {
    if !enter_isolated_child(
        module_path!(),
        "desktop_snapshot_bounds_legacy_labels_and_omits_oversized_identities",
    ) {
        return;
    }
    use crate::application::task::TaskRecordUpdateParams;
    use orbit_types::task::{ExternalRef, TaskArtifact, TaskHistoryEntry};
    let (_root, runtime) = test_runtime();
    let session = session(false);
    let task = runtime
        .desktop_task_write(create("legacy-bounds"), None, None, &session)
        .unwrap()
        .snapshot
        .task;
    let credential = format!("ghp_{}", "a".repeat(36));
    let attribution = format!("{credential} {}", "界".repeat(500));
    let long_path = format!("a/{}/result.txt", vec!["x".repeat(200); 11].join("/"));
    let mut visible = TaskArtifact::from_text("z-result.txt", "small body");
    visible.media_type = "text/".to_string() + &"x".repeat(200);
    visible.created_by = Some(attribution.clone());
    runtime
        .stores()
        .task_records()
        .update(
            &task.id,
            TaskRecordUpdateParams {
                actor: "test".into(),
                crew: Some(Some("c".repeat(1000))),
                orchestrator: Some(Some("o".repeat(1000))),
                created_by: Some(Some(attribution.clone())),
                planned_by: Some(Some(attribution.clone())),
                implemented_by: Some(Some(attribution)),
                pr_status: Some(Some("p".repeat(1000))),
                job_run_id: Some(Some("jrun-".to_string() + &"r".repeat(1000))),
                external_refs: Some(vec![ExternalRef {
                    system: "test".into(),
                    id: "i".repeat(1500),
                    url: None,
                }]),
                append_history: vec![TaskHistoryEntry {
                    at: chrono::Utc::now(),
                    by: "test".into(),
                    event: "event".repeat(100),
                    note: None,
                    from_status: None,
                    to_status: None,
                }],
                upsert_artifacts: vec![
                    TaskArtifact::from_text(&long_path, "oversized logical address"),
                    visible,
                ],
                ..Default::default()
            },
        )
        .unwrap();
    let revision = runtime
        .stores()
        .tasks()
        .desktop_task_revision(&task.id)
        .unwrap();
    let snapshot = runtime.desktop_task_snapshot(&task.id, &session).unwrap();
    assert_eq!(snapshot.revision, revision);
    assert!(snapshot.content_truncated);
    assert!(!snapshot.actions.edit.enabled);
    assert!(!snapshot.actions.review.enabled);
    assert!(snapshot.task.crew.as_ref().unwrap().len() <= 128);
    assert!(snapshot.task.orchestrator.as_ref().unwrap().len() <= 128);
    assert!(snapshot.task.created_by.as_ref().unwrap().len() <= 512);
    assert!(
        !snapshot
            .task
            .created_by
            .as_ref()
            .unwrap()
            .contains(&credential)
    );
    assert!(snapshot.task.job_run_id.is_none());
    assert!(snapshot.task.external_refs.is_empty());
    assert_eq!(snapshot.artifacts_total, 2);
    assert_eq!(snapshot.artifacts.len(), 1);
    assert_eq!(snapshot.artifacts[0].path, "z-result.txt");
    assert!(snapshot.artifacts[0].media_type.len() <= 128);
    assert!(snapshot.artifacts[0].created_by.len() <= 512);
    assert!(!snapshot.artifacts[0].created_by.contains(&credential));
    assert!(
        snapshot
            .history
            .iter()
            .all(|event| event.event.len() <= 128)
    );
    assert!(
        snapshot
            .truncated_fields
            .iter()
            .any(|field| field == "artifacts[0].path")
    );
    assert!(
        snapshot
            .truncated_fields
            .iter()
            .any(|field| field == "task.job_run_id")
    );
    let persisted = runtime.get_task(&task.id).unwrap();
    assert_eq!(persisted.crew.unwrap().len(), 1000);
    assert!(persisted.job_run_id.unwrap().len() > 512);
    assert_eq!(
        runtime.get_task_artifact_manifest(&task.id).unwrap()[0].path,
        long_path
    );
}
#[test]
fn desktop_committed_write_with_missing_refresh_stays_accepted() {
    if !enter_isolated_child(
        module_path!(),
        "desktop_committed_write_with_missing_refresh_stays_accepted",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let session = session(false);
    let written = runtime
        .desktop_task_write(create("accepted-before-refresh"), None, None, &session)
        .unwrap();
    let id = written.snapshot.task.id;
    // Reproduce a competing deletion after receipt publication, before refreshing.
    runtime.stores().tasks().delete_task(&id).unwrap();
    let error = runtime
        .desktop_write_result(&id, false, &session)
        .unwrap_err();
    assert!(
        matches!(error, orbit_common::OrbitError::DesktopWriteAccepted { task_id, .. } if task_id == id)
    );
}
