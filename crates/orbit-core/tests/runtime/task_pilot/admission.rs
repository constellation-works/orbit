//! Applied findings hold directly filed backlog tasks across the real drain,
//! ship-selection and readiness boundaries, with assessment-scoped decisions.

use orbit_core::application::task::{
    DecisionSubmitCaller, DecisionSubmitParams, TaskAddParams, TaskUpdateParams,
};
use orbit_core::{Task, TaskComplexity, TaskStatus};
use orbit_types::task::{PilotAdmissionVerdict, TaskDecision};
use serde_json::json;

use super::Workspace;

fn backlog_task(workspace: &Workspace) -> Task {
    task_at_status(workspace, TaskStatus::Backlog)
}

fn task_at_status(workspace: &Workspace, status: TaskStatus) -> Task {
    workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Directly filed backlog work".into(),
            description: "Repair the README fixture.".into(),
            acceptance_criteria: vec!["The fixture repair is observable.".into()],
            plan: "Inspect README.md.".into(),
            status: Some(status),
            complexity: TaskComplexity::Low,
            context_files: vec!["file:README.md".into()],
            ..Default::default()
        })
        .unwrap()
}

/// Drive the same default, non-promoting prepare/apply actions that routine
/// task-pilot uses, rather than seeding an admission-only representation.
fn assess(workspace: &Workspace, task: &Task, finding: Option<&str>) {
    assess_disposition(workspace, task, finding, "selectors");
}

fn assess_disposition(
    workspace: &Workspace,
    task: &Task,
    finding: Option<&str>,
    disposition: &str,
) {
    let prepared = workspace.prepare(&[&task.id]);
    let current = workspace.runtime.get_task(&task.id).unwrap();
    let mut assessment = json!({
        "task_id": task.id,
        "context_files_before": current.context_files,
        "context_files_after": ["file:README.md"],
        "disposition": "selectors", "recommended_crew": "fixture",
        "recommended_complexity": "low", "confidence": "high",
        "assessment_rationale": "README.md contains the affected material.",
        "validation_approach": "Exercise drain admission.",
        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
        "duplicate_of": null, "already_landed": null,
    });
    assessment["disposition"] = json!(disposition);
    if disposition != "selectors" {
        assessment["context_files_after"] = json!([]);
        assessment["evidence"] =
            json!("The deliverable requires an owner-side operation outside every managed lane.");
    }
    if let Some(field) = finding {
        assessment[field] = if field.ends_with("_warnings") {
            json!(["The operator should review the proposed approach."])
        } else {
            json!({
                "task_id": task.id,
                "evidence": "The same README repair is already covered by existing work.",
            })
        };
    }
    let applied = workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo, "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": [task.id], "tasks": [assessment]}],
        }),
    );
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(applied["applied_count"], 1, "{applied}");
    assert_eq!(applied["tasks"][0]["task_id"], task.id, "{applied}");
    assert_eq!(
        workspace.runtime.get_task(&task.id).unwrap().status,
        current.status
    );
}

fn host_operational_holds_human_approved_work_until_a_current_operator_decision() {
    for decision in ["approve-anyway", "clear", "evaluated"] {
        let workspace = Workspace::new();
        let task = task_at_status(&workspace, TaskStatus::Proposed);
        // Even a no-diff task needs an operator decision for this disposition.
        workspace.runtime.update_task_as_human(&task.id, TaskUpdateParams {
            tags: Some(vec!["no-diff-expected".into()]),
            comment: Some(format!("task-pilot-admission: {decision}\nA prior decision cannot release a future assessment.")),
            ..Default::default()
        }, "human:fixture".into()).unwrap();
        assess_disposition(&workspace, &task, None, "host_operational");
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
        assert_admission(&workspace, &task, Some("host_operational_handoff"));
        let history = workspace.runtime.get_task_history(&task.id).unwrap();
        assert!(
            history
                .iter()
                .any(|entry| entry.from_status == Some(TaskStatus::Proposed)
                    && entry.to_status == Some(TaskStatus::Backlog)
                    && entry.by == "human:fixture")
        );
        let records = history
            .iter()
            .filter(|entry| entry.event == "host_operational_held")
            .collect::<Vec<_>>();
        assert_eq!(
            records.len(),
            1,
            "repeated admission must not duplicate the handoff"
        );
        let record: serde_json::Value =
            serde_json::from_str(records[0].note.as_deref().unwrap()).unwrap();
        assert_eq!(record["disposition"], "host_operational");
        let evidence = record["hold"]["evidence"].as_str().unwrap();
        assert!(!evidence.is_empty());
        let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
        let audit: serde_json::Value =
            serde_json::from_str(comments.last().unwrap().receipt().unwrap().body).unwrap();
        assert_eq!(audit["host_operational_hold"], record["hold"]);
        assert_eq!(audit["assessment"]["evidence"], evidence);
        let refused = orbit_engine::RuntimeHost::admit_task_for_workflow(
            &workspace.runtime,
            &task.id,
            "worktree_setup",
        )
        .unwrap_err();
        assert!(
            refused.to_string().contains("host_operational")
                && refused.to_string().contains(evidence),
            "{refused}"
        );
        assert_eq!(
            workspace.runtime.get_task(&task.id).unwrap().status,
            TaskStatus::Backlog
        );

        let comment = format!(
            "task-pilot-admission: {decision}\nThe operator accepts the owner-side repair; see operator-evaluation.json."
        );
        workspace
            .runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    comment: Some(comment.clone()),
                    ..Default::default()
                },
                Some("codex".into()),
                Some("gpt-6.1-sol".into()),
            )
            .unwrap();
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    priority: Some(orbit_core::TaskPriority::High),
                    comment: Some("The operator is still reviewing this finding.".into()),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    comment: Some(format!("task-pilot-admission: {decision}")),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap_err();
        if decision == "evaluated" {
            workspace
                .runtime
                .update_task_as_human(
                    &task.id,
                    TaskUpdateParams {
                        comment: Some(comment.clone()),
                        ..Default::default()
                    },
                    "human:fixture".into(),
                )
                .unwrap_err();
        }
        assert_admission(&workspace, &task, Some("host_operational_handoff"));
        assert!(
            !workspace
                .runtime
                .get_task_history(&task.id)
                .unwrap()
                .iter()
                .any(|entry| entry.event == "host_operational_resolved")
        );
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    comment: Some(comment),
                    upsert_artifacts: if decision == "evaluated" {
                        vec![orbit_types::task::TaskArtifact::from_text(
                            "operator-evaluation.json",
                            "Owner-side repair verified.",
                        )]
                    } else {
                        vec![]
                    },
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
        assert_admission(&workspace, &task, None);
        assert!(
            workspace
                .runtime
                .get_task_history(&task.id)
                .unwrap()
                .iter()
                .any(|entry| entry.event == "host_operational_resolved"
                    && entry.by == "human:fixture")
        );
        // A new finding is not covered by the previous operator decision.
        assess_disposition(&workspace, &task, None, "host_operational");
        assert_admission(&workspace, &task, Some("host_operational_handoff"));
    }
}

fn host_operational_material_and_newer_dispositions_control_admission() {
    for release in ["selectors", "verified_no_diff", "rescope"] {
        let workspace = Workspace::new();
        let task =
            human_approved_validation_task(&workspace, "The owner-side repair is observable.");
        assess_disposition(&workspace, &task, None, "host_operational");
        assert_admission(&workspace, &task, Some("host_operational_handoff"));
        if release == "rescope" {
            workspace
                .runtime
                .update_task_as_human(
                    &task.id,
                    TaskUpdateParams {
                        acceptance_criteria: Some(vec![
                            "The repository fixture repair is observable.".into(),
                        ]),
                        ..Default::default()
                    },
                    "human:fixture".into(),
                )
                .unwrap();
        } else {
            assess_disposition(&workspace, &task, None, release);
        }
        assert_admission(&workspace, &task, None);
        assert_eq!(
            orbit_engine::RuntimeHost::admit_task_for_workflow(
                &workspace.runtime,
                &task.id,
                "worktree_setup"
            )
            .unwrap()
            .status,
            TaskStatus::InProgress
        );
    }
}

fn legacy_host_operational_receipt_is_backfilled_once_and_tracks_its_material() {
    let workspace = Workspace::new();
    let task = human_approved_validation_task(&workspace, "The owner-side repair is observable.");
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(
            &workspace.runtime.global_root(),
        ),
    )
    .unwrap();
    let backends = orbit_store::compose::workspace_coordinated_backends(
        registry,
        workspace.runtime.workspace_id().unwrap(),
        workspace.runtime.sqlite_store().unwrap(),
    )
    .unwrap()
    .task;
    let at = chrono::Utc::now();
    let audit = json!({"assessment": {
        "disposition": "host_operational", "evidence": "The operator must repair the owner-side definition.",
        "duplicate_of": null, "already_landed": null,
    }});
    backends
        .history
        .update_task_history(
            &task.id,
            orbit_store::contracts::TaskHistoryUpdateParams {
                actor: "task-pilot".into(),
                append_comments: vec![orbit_types::task::TaskComment {
                    at,
                    by: "task-pilot".into(),
                    message: format!("operation_id=legacy-host-pilot\n{audit}"),
                    operation_id: None,
                }],
                append_history: vec![orbit_types::task::TaskHistoryEntry {
                    at,
                    by: "task-pilot".into(),
                    event: "task_pilot_applied".into(),
                    note: Some("host_operational (operation_id=legacy-host-pilot)".into()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
    // Old receipts must not mistake metadata edits before the first admission
    // for a re-scope of the host-operational work.
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                priority: Some(orbit_core::TaskPriority::High),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    assert_admission(&workspace, &task, Some("host_operational_handoff"));
    assert_admission(&workspace, &task, Some("host_operational_handoff"));
    let records = workspace
        .runtime
        .get_task_history(&task.id)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.event == "host_operational_held")
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1);
    let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
    let (header, record) = comments.last().unwrap().message.split_once('\n').unwrap();
    assert_eq!(header, "host-operational-handoff");
    let record: serde_json::Value = serde_json::from_str(record).unwrap();
    assert_eq!(record["disposition"], "host_operational");
    assert_eq!(record["hold"]["evidence"], audit["assessment"]["evidence"]);
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                priority: Some(orbit_core::TaskPriority::High),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    assert_admission(&workspace, &task, Some("host_operational_handoff"));
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                acceptance_criteria: Some(vec![
                    "The repository fixture repair is observable.".into(),
                ]),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    assert_admission(&workspace, &task, None);
}

fn assert_admission(workspace: &Workspace, task: &Task, reason: Option<&str>) {
    for input in [json!({}), json!({"task_ids": [task.id]})] {
        let output = workspace.action("list_backlog_tasks", input);
        if let Some(reason) = reason {
            assert_eq!(output["task_ids"], json!([]), "{output}");
            let exclusion = output["excluded"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"] == task.id)
                .unwrap();
            assert_eq!(exclusion["reason"], reason, "{output}");
            assert!(!exclusion["detail"].as_str().unwrap().is_empty());
        } else {
            assert_eq!(output["task_ids"], json!([task.id]), "{output}");
        }
    }
    let wave = workspace.action(
        "classify_workspace_auto_tasks",
        json!({"max_active_leaf_runs": 2}),
    );
    assert_eq!(
        wave["loose_task_ids"],
        if reason.is_some() {
            json!([])
        } else {
            json!([task.id])
        },
        "{wave}"
    );
    let readiness = workspace
        .runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    let entry = readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task.id)
        .unwrap_or_else(|| panic!("task missing from readiness: {readiness}"));
    assert_eq!(entry["eligible"], reason.is_none(), "{readiness}");
    if let Some(reason) = reason {
        assert_eq!(entry["reason"], reason, "{readiness}");
    }
}

fn latest_pilot_finding_holds_backlog_until_a_clear_assessment() {
    for (field, reason) in [
        ("duplicate_of", "pilot_duplicate"),
        ("already_landed", "pilot_already_landed"),
    ] {
        let workspace = Workspace::new();
        let task = backlog_task(&workspace);
        assert_admission(&workspace, &task, None);
        assess(&workspace, &task, Some(field));
        assert_admission(&workspace, &task, Some(reason));
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    description: Some(
                        "Edited scope still needs a decision about the finding.".into(),
                    ),
                    comment: Some("The operator is reviewing the assessment.".into()),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
        assert_admission(&workspace, &task, Some(reason));
        assess(&workspace, &task, None);
        assert_admission(&workspace, &task, None);
    }
}

fn human_pilot_decisions_release_only_the_current_assessment() {
    for decision in ["approve-anyway", "clear"] {
        for (field, reason) in [
            ("duplicate_of", "pilot_duplicate"),
            ("already_landed", "pilot_already_landed"),
        ] {
            let workspace = Workspace::new();
            let task = backlog_task(&workspace);
            let comment = format!("task-pilot-admission: {decision}\nReviewed the finding.");
            // A decision made before an assessment cannot approve future findings.
            workspace
                .runtime
                .update_task_as_human(
                    &task.id,
                    TaskUpdateParams {
                        comment: Some(comment.clone()),
                        ..Default::default()
                    },
                    "human:fixture".into(),
                )
                .unwrap();
            assess(&workspace, &task, Some(field));
            // Canonical agent provenance cannot forge the human release path.
            workspace
                .runtime
                .update_task_with_identity(
                    &task.id,
                    TaskUpdateParams {
                        comment: Some(comment.clone()),
                        ..Default::default()
                    },
                    Some("codex".into()),
                    Some("gpt-6.1-sol".into()),
                )
                .unwrap();
            assert_admission(&workspace, &task, Some(reason));
            workspace
                .runtime
                .update_task_as_human(
                    &task.id,
                    TaskUpdateParams {
                        comment: Some(comment),
                        ..Default::default()
                    },
                    "human:fixture".into(),
                )
                .unwrap();
            assert_admission(&workspace, &task, None);
            assess(&workspace, &task, Some(field));
            assert_admission(&workspace, &task, Some(reason));
        }
    }
}

fn human_approved_validation_task(workspace: &Workspace, criterion: &str) -> Task {
    human_approved_validation_task_with_tools(workspace, criterion, vec![])
}

fn human_approved_validation_task_with_criteria(
    workspace: &Workspace,
    criteria: Vec<String>,
    required_tools: Vec<String>,
) -> Task {
    // Use an allow-mode fixture to exercise a repairable missing grant. The
    // real implementation activity currently uses a deny list; a pilot must
    // support either policy without treating every warning as a hold.
    let mut policy: serde_json::Value = serde_yaml::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/activities/agent_implement.yaml"),
        )
        .unwrap(),
    )
    .unwrap();
    policy["spec"]
        .as_object_mut()
        .unwrap()
        .remove("tool_disallow_list");
    policy["spec"]["tools"] = json!(["orbit.task.show"]);
    std::fs::write(
        workspace
            .runtime
            .global_root()
            .join("resources/activities/agent_implement.yaml"),
        serde_yaml::to_string(&policy).unwrap(),
    )
    .unwrap();
    let task = workspace
        .runtime
        .add_task(TaskAddParams {
            required_tools,
            title: "Human-approved validation work".into(),
            description: "Repair the README fixture.".into(),
            acceptance_criteria: criteria,
            plan: "Inspect README.md.".into(),
            status: Some(TaskStatus::Proposed),
            complexity: TaskComplexity::Low,
            context_files: vec!["file:README.md".into()],
            ..Default::default()
        })
        .unwrap();
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap()
}

fn human_approved_validation_task_with_tools(
    workspace: &Workspace,
    criterion: &str,
    required_tools: Vec<String>,
) -> Task {
    human_approved_validation_task_with_criteria(workspace, vec![criterion.into()], required_tools)
}

fn operator_validation_holds_human_approved_work_and_requires_an_evidenced_decision() {
    for decision in ["evaluated", "approve-anyway", "clear"] {
        let mut workspace = Workspace::new();
        workspace.runtime = workspace
            .runtime
            .with_actor(orbit_core::ActorIdentity::human("human:fixture"));
        let task = human_approved_validation_task_with_tools(
            &workspace,
            "Run the live evaluation with `orbit.pipeline.invoke`.",
            if decision == "clear" {
                vec!["orbit.pipeline.invoke".into()]
            } else {
                vec![]
            },
        );
        let comment = format!(
            "task-pilot-admission: {decision}\nOperator reviewed operator-evaluation.json and accepts this validation."
        );
        // A decision before the pilot cannot approve a future assessment.
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    comment: Some(comment.clone()),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
        assess(&workspace, &task, None);
        assert_admission(&workspace, &task, Some("operator_validation_handoff"));
        let history = workspace.runtime.get_task_history(&task.id).unwrap();
        assert!(
            history
                .iter()
                .any(|entry| entry.from_status == Some(TaskStatus::Proposed)
                    && entry.to_status == Some(TaskStatus::Backlog)
                    && entry.by == "human:fixture")
        );
        let hold = history
            .iter()
            .find(|entry| entry.event == "operator_validation_held")
            .unwrap();
        let hold: serde_json::Value = serde_json::from_str(hold.note.as_deref().unwrap()).unwrap();
        assert_eq!(hold["hold"]["requirements"][0]["criterion"], 1);
        assert_eq!(
            hold["hold"]["requirements"][0]["tool"],
            "orbit.pipeline.invoke"
        );
        let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
        let audit: serde_json::Value =
            serde_json::from_str(comments.last().unwrap().receipt().unwrap().body).unwrap();
        assert_eq!(
            audit["operator_validation_hold"]["requirements"],
            hold["hold"]["requirements"]
        );

        // Even a queued run bypassing selection meets the hold at admission.
        let refused = orbit_engine::RuntimeHost::admit_task_for_workflow(
            &workspace.runtime,
            &task.id,
            "worktree_setup",
        )
        .unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("criterion 1 requires `orbit.pipeline.invoke`")
        );
        assert_eq!(
            workspace.runtime.get_task(&task.id).unwrap().status,
            TaskStatus::Backlog
        );
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    priority: Some(orbit_core::TaskPriority::High),
                    comment: Some("Discussing the finding does not resolve it.".into()),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
        workspace
            .runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    comment: Some(comment.clone()),
                    ..Default::default()
                },
                Some("codex".into()),
                Some("gpt-6.1-sol".into()),
            )
            .unwrap();
        // The registered agent tool cannot mint an operator decision even
        // when a caller omits provenance on a human-owned runtime.
        let _ = workspace.runtime.run_tool(
            "orbit.task.update",
            json!({"id": task.id, "comment": comment}),
        );
        assert_admission(&workspace, &task, Some("operator_validation_handoff"));
        assert!(
            !workspace
                .runtime
                .get_task_history(&task.id)
                .unwrap()
                .iter()
                .any(|entry| entry.event == "operator_validation_resolved")
        );
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    comment: Some(format!("task-pilot-admission: {decision}")),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap_err();
        assert_admission(&workspace, &task, Some("operator_validation_handoff"));
        if decision == "evaluated" {
            let missing_artifact = workspace
                .runtime
                .update_task_as_human(
                    &task.id,
                    TaskUpdateParams {
                        comment: Some(comment.clone()),
                        ..Default::default()
                    },
                    "human:fixture".into(),
                )
                .unwrap_err();
            assert!(
                missing_artifact
                    .to_string()
                    .contains("attached evaluation artifact")
            );
            assert_admission(&workspace, &task, Some("operator_validation_handoff"));
        }
        let resolution = TaskUpdateParams {
            comment: Some(comment),
            upsert_artifacts: if decision == "evaluated" {
                vec![orbit_types::task::TaskArtifact::from_text(
                    "operator-evaluation.json",
                    json!({"criterion": 1, "tool": "orbit.pipeline.invoke", "outcome": "passed"})
                        .to_string(),
                )]
            } else {
                vec![]
            },
            ..Default::default()
        };
        if decision == "clear" {
            workspace
                .runtime
                .update_task_with_identity(&task.id, resolution, None, None)
                .unwrap();
        } else {
            workspace
                .runtime
                .update_task_as_human(&task.id, resolution, "human:fixture".into())
                .unwrap();
        }
        if decision == "evaluated" {
            assert!(
                workspace
                    .runtime
                    .get_task_artifact(&task.id, "operator-evaluation.json")
                    .unwrap()
                    .is_some()
            );
        }
        assert_admission(&workspace, &task, None);
        assert!(
            workspace
                .runtime
                .get_task_history(&task.id)
                .unwrap()
                .iter()
                .any(|entry| entry.event == "operator_validation_resolved"
                    && entry.by == "human:fixture")
        );
        assess(&workspace, &task, None);
        assert_admission(&workspace, &task, Some("operator_validation_handoff"));
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    acceptance_criteria: Some(vec![
                        "Observe the fixture repair through the runtime boundary.".into(),
                    ]),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
        assert_admission(&workspace, &task, None);
        let admitted = orbit_engine::RuntimeHost::admit_task_for_workflow(
            &workspace.runtime,
            &task.id,
            "worktree_setup",
        )
        .unwrap();
        assert_eq!(admitted.status, TaskStatus::InProgress);
    }
}

fn repairable_tool_findings_and_expected_denials_remain_advisory() {
    for (criterion, warns) in [
        ("Call `proc.spawn` over MCP to validate the fixture.", true),
        ("Read the fixture through `orbit.task.eligible`.", true),
        (
            "An agent calling `orbit.pipeline.invoke` receives the expected denial.",
            false,
        ),
        (
            "The transcript quotes \"orbit.pipeline.invoke\" as the rejected operation.",
            false,
        ),
        (
            "Show the captured example:\n```\norbit.pipeline.invoke\n```",
            false,
        ),
    ] {
        let workspace = Workspace::new();
        let task = human_approved_validation_task(&workspace, criterion);
        assess(&workspace, &task, None);
        let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
        let audit: serde_json::Value =
            serde_json::from_str(comments.last().unwrap().receipt().unwrap().body).unwrap();
        assert_eq!(
            !audit["assessment"]["validation_tool_warnings"]
                .as_array()
                .unwrap()
                .is_empty(),
            warns,
            "{audit}"
        );
        assert!(audit["operator_validation_hold"].is_null(), "{audit}");
        assert_admission(&workspace, &task, None);
    }
    let workspace = Workspace::new();
    let task = human_approved_validation_task(&workspace, "The repair is observable.");
    assess(&workspace, &task, Some("utility_warnings"));
    assert_admission(&workspace, &task, None);
}

fn changed_criteria_make_the_pilot_validation_assessment_stale() {
    let workspace = Workspace::new();
    let task =
        human_approved_validation_task(&workspace, "Evaluate through `orbit.pipeline.invoke`.");
    assess(&workspace, &task, None);
    assert_admission(&workspace, &task, Some("operator_validation_handoff"));
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                acceptance_criteria: Some(vec![
                    "Evaluate the revised scenario through `orbit.pipeline.invoke`.".into(),
                ]),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    assert_admission(&workspace, &task, None);
    assess(&workspace, &task, None);
    assert_admission(&workspace, &task, Some("operator_validation_handoff"));
}

fn legacy_operator_warning_is_backfilled_once_at_admission_and_stales_on_edit() {
    let workspace = Workspace::new();
    let task = human_approved_validation_task(&workspace, "Evaluate with `orbit.pipeline.invoke`.");
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(
            &workspace.runtime.global_root(),
        ),
    )
    .unwrap();
    let backends = orbit_store::compose::workspace_coordinated_backends(
        registry,
        workspace.runtime.workspace_id().unwrap(),
        workspace.runtime.sqlite_store().unwrap(),
    )
    .unwrap()
    .task;
    let at = chrono::Utc::now();
    let audit = json!({"assessment": {
        "validation_tool_warnings": ["acceptance criterion requires `orbit.pipeline.invoke`, a governed operation reserved for the operator capability; route validation to an operator"],
        "duplicate_of": null, "already_landed": null,
    }});
    backends
        .history
        .update_task_history(
            &task.id,
            orbit_store::contracts::TaskHistoryUpdateParams {
                actor: "task-pilot".into(),
                append_history: vec![orbit_types::task::TaskHistoryEntry {
                    at,
                    by: "task-pilot".into(),
                    event: "task_pilot_applied".into(),
                    note: Some("legacy assessment (operation_id=legacy-pilot)".into()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                append_comments: vec![orbit_types::task::TaskComment {
                    at,
                    by: "task-pilot".into(),
                    message: format!("operation_id=legacy-pilot\n{audit}"),
                    operation_id: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
    assert_admission(&workspace, &task, Some("operator_validation_handoff"));
    assert_admission(&workspace, &task, Some("operator_validation_handoff"));
    let history = workspace.runtime.get_task_history(&task.id).unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|entry| entry.event == "operator_validation_held")
            .count(),
        1
    );
    let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
    let record: serde_json::Value =
        serde_json::from_str(comments.last().unwrap().message.split_once('\n').unwrap().1).unwrap();
    assert_eq!(record["hold"]["requirements"][0]["criterion"], 1);
    assert_eq!(
        record["hold"]["requirements"][0]["tool"],
        "orbit.pipeline.invoke"
    );
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                acceptance_criteria: Some(vec!["The fixture repair is observable.".into()]),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    assert_admission(&workspace, &task, None);
}

fn operator_validation_hold_is_retained_when_warnings_exceed_cap() {
    let criteria = vec![
        "Validate with `github.run.list` over MCP.".into(),
        "Validate with `github.run.view` over MCP.".into(),
        "Validate with `github.run.logs` over MCP.".into(),
        "Validate with `orbit.pipeline.invoke`.".into(),
    ];

    // 1. Fresh pilot assessment retains the typed hold even when the warning cap drops the governed finding.
    let workspace = Workspace::new();
    let task = human_approved_validation_task_with_criteria(&workspace, criteria.clone(), vec![]);
    assess(&workspace, &task, None);
    let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
    let audit: serde_json::Value =
        serde_json::from_str(comments.last().unwrap().receipt().unwrap().body).unwrap();
    let warnings = audit["assessment"]["validation_tool_warnings"]
        .as_array()
        .unwrap();
    assert_eq!(warnings.len(), 8, "warnings must hit the cap: {warnings:?}");
    assert!(
        !warnings.iter().any(|w| {
            w.as_str().unwrap().starts_with(
                "acceptance criterion requires `orbit.pipeline.invoke`, a governed operation",
            )
        }),
        "governed operation warning must be dropped by the cap: {warnings:?}"
    );
    let hold_requirements = audit["operator_validation_hold"]["requirements"]
        .as_array()
        .unwrap();
    assert_eq!(hold_requirements.len(), 1);
    assert_eq!(hold_requirements[0]["criterion"], 4);
    assert_eq!(hold_requirements[0]["tool"], "orbit.pipeline.invoke");
    assert_admission(&workspace, &task, Some("operator_validation_handoff"));

    // 2. Legacy pilot assessment backfill also derives the hold directly, independent of the warning cap.
    let workspace2 = Workspace::new();
    let task2 = human_approved_validation_task_with_criteria(&workspace2, criteria, vec![]);
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(
            &workspace2.runtime.global_root(),
        ),
    )
    .unwrap();
    let backends = orbit_store::compose::workspace_coordinated_backends(
        registry,
        workspace2.runtime.workspace_id().unwrap(),
        workspace2.runtime.sqlite_store().unwrap(),
    )
    .unwrap()
    .task;
    let at = chrono::Utc::now();
    let legacy_audit = json!({"assessment": {
        "validation_tool_warnings": warnings,
        "duplicate_of": null, "already_landed": null,
    }});
    backends
        .history
        .update_task_history(
            &task2.id,
            orbit_store::contracts::TaskHistoryUpdateParams {
                actor: "task-pilot".into(),
                append_history: vec![orbit_types::task::TaskHistoryEntry {
                    at,
                    by: "task-pilot".into(),
                    event: "task_pilot_applied".into(),
                    note: Some("legacy assessment (operation_id=legacy-pilot)".into()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                append_comments: vec![orbit_types::task::TaskComment {
                    at,
                    by: "task-pilot".into(),
                    message: format!("operation_id=legacy-pilot\n{legacy_audit}"),
                    operation_id: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
    assert_admission(&workspace2, &task2, Some("operator_validation_handoff"));
    let history2 = workspace2.runtime.get_task_history(&task2.id).unwrap();
    assert_eq!(
        history2
            .iter()
            .filter(|entry| entry.event == "operator_validation_held")
            .count(),
        1
    );
    let comments2 = workspace2.runtime.get_task_comments(&task2.id).unwrap();
    let record2: serde_json::Value = serde_json::from_str(
        comments2
            .last()
            .unwrap()
            .message
            .split_once('\n')
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(record2["hold"]["requirements"][0]["criterion"], 4);
    assert_eq!(
        record2["hold"]["requirements"][0]["tool"],
        "orbit.pipeline.invoke"
    );
}

/// Append `message` as `by`'s comment at `at`, as a binary that predates
/// typed decisions recorded it.
fn append_comment(workspace: &Workspace, task: &Task, by: &str, at: &str, message: &str) {
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(
            &workspace.runtime.global_root(),
        ),
    )
    .unwrap();
    orbit_store::compose::workspace_coordinated_backends(
        registry,
        workspace.runtime.workspace_id().unwrap(),
        workspace.runtime.sqlite_store().unwrap(),
    )
    .unwrap()
    .task
    .history
    .update_task_history(
        &task.id,
        orbit_store::contracts::TaskHistoryUpdateParams {
            actor: by.into(),
            append_comments: vec![orbit_types::task::TaskComment {
                at: at.parse().unwrap(),
                by: by.into(),
                message: message.into(),
                operation_id: None,
            }],
            ..Default::default()
        },
    )
    .unwrap();
}

fn an_override_comment_decides_only_when_written_before_the_store_recorded_typed_decisions() {
    for (decision, verdict) in [
        ("approve-anyway", PilotAdmissionVerdict::ApproveAnyway),
        ("clear", PilotAdmissionVerdict::Clear),
    ] {
        for (field, reason) in [
            ("duplicate_of", "pilot_duplicate"),
            ("already_landed", "pilot_already_landed"),
        ] {
            // The first open by this binary records the store's boundary.
            let first_open = chrono::Utc::now();
            let workspace = Workspace::new();
            let task = backlog_task(&workspace);
            let comment = format!("task-pilot-admission: {decision}\nReviewed the finding.");
            // Overrides an older binary wrote: before typed decisions shipped,
            // on the day they shipped before this host upgraded, and on a host
            // that kept running an older binary until just before this store
            // was first opened by this one.
            for legacy_at in [
                "2026-10-01T00:00:00Z".to_string(),
                "2026-10-12T09:00:00Z".to_string(),
                (first_open - chrono::Duration::seconds(1)).to_rfc3339(),
            ] {
                assess(&workspace, &task, Some(field));
                // Comment text written since is open to any author; it decides
                // nothing, and opening the store again does not move the boundary.
                let now = chrono::Utc::now().to_rfc3339();
                append_comment(&workspace, &task, "human:fixture", &now, &comment);
                super::runtime_at(
                    &workspace.runtime.global_root(),
                    &workspace.repo.join(".orbit"),
                );
                assert_admission(&workspace, &task, Some(reason));
                append_comment(&workspace, &task, "human:fixture", &legacy_at, &comment);
                assert_admission(&workspace, &task, None);
            }

            // A decision made now is the typed one the trusted path records;
            // an override needs no explanation below its header.
            assess(&workspace, &task, Some(field));
            assert_admission(&workspace, &task, Some(reason));
            workspace
                .runtime
                .update_task_as_human(
                    &task.id,
                    TaskUpdateParams {
                        comment: Some(format!("task-pilot-admission: {decision}")),
                        ..Default::default()
                    },
                    "human:fixture".into(),
                )
                .unwrap();
            assert_admission(&workspace, &task, None);
            let decided = workspace
                .runtime
                .get_task_history(&task.id)
                .unwrap()
                .into_iter()
                .filter_map(|entry| match entry.decision {
                    Some(TaskDecision::PilotAdmission { verdict, .. }) => Some(verdict),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(decided, vec![verdict]);
        }
    }
}

fn an_incomplete_boundary_counts_from_its_modification_time_until_a_writable_open_repairs_it() {
    let comment = "task-pilot-admission: approve-anyway\nReviewed the finding.";
    // Empty, as a crash after the older binary created the file left it, and
    // cut short, as a failed write did.
    for content in ["", "2026-10-12T1"] {
        let workspace = Workspace::new();
        let global_root = workspace.runtime.global_root();
        let task = backlog_task(&workspace);
        let modified = crate::drain_approval::seed_incomplete_boundary(&global_root, content);
        // A reader counts from the incomplete file's modification time; then
        // a writable open records that instant, never a later one.
        for reopen in [false, true] {
            if reopen {
                super::runtime_at(&global_root, &workspace.repo.join(".orbit"));
                assert_eq!(
                    crate::drain_approval::recorded_boundary(&global_root),
                    modified,
                    "{content:?}"
                );
            }
            assess(&workspace, &task, Some("duplicate_of"));
            let now = chrono::Utc::now().to_rfc3339();
            append_comment(&workspace, &task, "human:fixture", &now, comment);
            assert_admission(&workspace, &task, Some("pilot_duplicate"));
            // Written on the rollout day before the upgrade.
            append_comment(
                &workspace,
                &task,
                "human:fixture",
                "2026-10-12T09:00:00Z",
                comment,
            );
            assert_admission(&workspace, &task, None);
        }
    }
}

fn decision_requests(
    workspace: &Workspace,
    task: &Task,
) -> Vec<orbit_types::task::DecisionRequest> {
    workspace
        .runtime
        .get_task_decision_requests(&task.id)
        .unwrap()
}

fn decision_channel_workspace() -> Workspace {
    Workspace::with_config(
        "[crews.fixture]\nmodel = \"fixture-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"fixture\"\n[decision_channel]\nenabled = true\n",
    )
}

fn an_enabled_pilot_hold_raises_one_open_decision_request() {
    let workspace = decision_channel_workspace();
    let task = backlog_task(&workspace);
    assess(&workspace, &task, Some("duplicate_of"));
    assert_admission(&workspace, &task, Some("pilot_duplicate"));
    // Repeated admission of the same hold raises nothing further.
    assert_admission(&workspace, &task, Some("pilot_duplicate"));
    let requests = decision_requests(&workspace, &task);
    assert_eq!(requests.len(), 1, "{requests:?}");
    let request = &requests[0];
    assert_eq!(request.state, orbit_types::task::DecisionRequestState::Open);
    assert_eq!(
        request.gate,
        orbit_types::task::DecisionGate::PilotAdmission
    );
    assert_eq!(
        request
            .answers
            .iter()
            .map(|answer| answer.id.as_str())
            .collect::<Vec<_>>(),
        vec!["approve_anyway", "clear", "evaluated"]
    );
    assert!(
        matches!(&request.binding, orbit_types::task::DecisionBinding::PilotOperation { operation_id } if !operation_id.is_empty()),
        "{request:?}"
    );
}

fn answering_a_pilot_request_records_the_verdict_and_releases_the_hold() {
    let workspace = decision_channel_workspace();
    let task = backlog_task(&workspace);
    assess(&workspace, &task, Some("duplicate_of"));
    assert_admission(&workspace, &task, Some("pilot_duplicate"));
    let request = decision_requests(&workspace, &task).pop().unwrap();
    let answered = workspace
        .runtime
        .answer_decision_request_as_human(
            &request.id,
            "approve_anyway",
            Some("Not a duplicate: the deliverables differ.".into()),
            "human:fixture".into(),
            Some("dashboard".into()),
        )
        .unwrap();
    assert_eq!(
        answered.state,
        orbit_types::task::DecisionRequestState::Answered
    );
    assert_eq!(answered.resolution.unwrap().answer_id, "approve_anyway");
    // The verdict comment the human path recorded is what admission reads:
    // the hold is released without a new assessment.
    assert_admission(&workspace, &task, None);
}

fn a_disabled_pilot_hold_writes_no_decision_request() {
    let workspace = Workspace::new();
    let task = backlog_task(&workspace);
    assess(&workspace, &task, Some("duplicate_of"));
    assert_admission(&workspace, &task, Some("pilot_duplicate"));
    assert!(decision_requests(&workspace, &task).is_empty());
}

fn a_changed_pilot_hold_supersedes_its_request_without_a_new_request() {
    for change in ["operator_clear", "clear_assessment", "replacement_hold"] {
        let workspace = decision_channel_workspace();
        let task = backlog_task(&workspace);
        assess(&workspace, &task, Some("duplicate_of"));
        assert_admission(&workspace, &task, Some("pilot_duplicate"));
        match change {
            "operator_clear" => {
                workspace
                    .runtime
                    .update_task_as_human(
                        &task.id,
                        TaskUpdateParams {
                            comment: Some(
                                "task-pilot-admission: clear\nThe finding no longer applies."
                                    .into(),
                            ),
                            ..Default::default()
                        },
                        "human:fixture".into(),
                    )
                    .unwrap();
            }
            "clear_assessment" => assess(&workspace, &task, None),
            _ => assess(&workspace, &task, Some("duplicate_of")),
        }
        // Reading must invalidate the old binding before admission raises anything else.
        let requests = decision_requests(&workspace, &task);
        assert_eq!(
            requests.len(),
            1,
            "{change}: no replacement request was raised"
        );
        assert_eq!(
            requests[0].state,
            orbit_types::task::DecisionRequestState::Superseded,
            "{change}"
        );
    }
}

fn a_second_pilot_hold_supersedes_the_first() {
    let workspace = decision_channel_workspace();
    let task = backlog_task(&workspace);
    assess(&workspace, &task, Some("duplicate_of"));
    assert_admission(&workspace, &task, Some("pilot_duplicate"));
    assess(&workspace, &task, Some("duplicate_of"));
    assert_admission(&workspace, &task, Some("pilot_duplicate"));
    let requests = decision_requests(&workspace, &task);
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(
        requests[0].state,
        orbit_types::task::DecisionRequestState::Superseded
    );
    assert_eq!(
        requests[1].state,
        orbit_types::task::DecisionRequestState::Open
    );
    assert_ne!(requests[0].binding.key(), requests[1].binding.key());
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.state == orbit_types::task::DecisionRequestState::Open)
            .count(),
        1,
        "at most one request per task is open"
    );
}

fn submit_pilot_answer(
    workspace: &Workspace,
    request_id: &str,
    token: &str,
    caller: DecisionSubmitCaller,
) -> Result<orbit_types::task::DecisionRequest, orbit_core::OrbitError> {
    workspace
        .runtime
        .submit_decision_via_channel(DecisionSubmitParams {
            channel: "email".into(),
            request_id: request_id.into(),
            answer_id: "approve_anyway".into(),
            note: Some("Not a duplicate: the deliverables differ.".into()),
            token: token.into(),
            sender: None,
            auth_results: None,
            caller,
        })
}

fn a_registered_channel_submission_releases_the_pilot_hold_as_the_human() {
    let workspace = decision_channel_workspace();
    let task = backlog_task(&workspace);
    assess(&workspace, &task, Some("duplicate_of"));
    assert_admission(&workspace, &task, Some("pilot_duplicate"));
    let request = decision_requests(&workspace, &task).pop().unwrap();
    workspace
        .runtime
        .register_decision_channel("email", "human:fixture", "decision-mail", vec![])
        .unwrap();
    let token = workspace
        .runtime
        .mint_decision_token("email", &request.id)
        .unwrap();
    let answered = submit_pilot_answer(
        &workspace,
        &request.id,
        &token,
        DecisionSubmitCaller::PluginCallback {
            plugin: "decision-mail".into(),
        },
    )
    .unwrap();
    assert_eq!(
        answered.state,
        orbit_types::task::DecisionRequestState::Answered
    );
    let resolution = answered.resolution.unwrap();
    assert_eq!(resolution.answered_by, "human:fixture");
    assert!(
        resolution
            .provenance
            .unwrap()
            .starts_with("decision-channel email (request "),
        "channel provenance is on the resolution"
    );
    // The verdict the human funnel recorded is what admission reads: the
    // hold is released without a new assessment.
    assert_admission(&workspace, &task, None);
}

fn agent_and_unregistered_submissions_leave_the_pilot_hold_held() {
    let workspace = decision_channel_workspace();
    let task = backlog_task(&workspace);
    assess(&workspace, &task, Some("duplicate_of"));
    assert_admission(&workspace, &task, Some("pilot_duplicate"));
    let request = decision_requests(&workspace, &task).pop().unwrap();
    workspace
        .runtime
        .register_decision_channel("email", "human:fixture", "decision-mail", vec![])
        .unwrap();
    let token = workspace
        .runtime
        .mint_decision_token("email", &request.id)
        .unwrap();
    // The §3.3 guard: a well-formed answer with a valid token, from an
    // agent envelope and from a plugin the channel is not bound to, is
    // refused both times and the hold stays held.
    let agent = submit_pilot_answer(&workspace, &request.id, &token, DecisionSubmitCaller::Agent)
        .expect_err("an agent-envelope submission is refused");
    assert!(agent.to_string().contains("caller_not_allowed"), "{agent}");
    let unregistered = submit_pilot_answer(
        &workspace,
        &request.id,
        &token,
        DecisionSubmitCaller::PluginCallback {
            plugin: "other-plugin".into(),
        },
    )
    .expect_err("an unregistered plugin submission is refused");
    assert!(
        unregistered.to_string().contains("channel_not_registered"),
        "{unregistered}"
    );
    assert_admission(&workspace, &task, Some("pilot_duplicate"));
    assert_eq!(
        decision_requests(&workspace, &task)[0].state,
        orbit_types::task::DecisionRequestState::Open,
        "the request is still open for the human"
    );
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    decision_channel_pilot_hold_scenarios,
    an_enabled_pilot_hold_raises_one_open_decision_request,
    answering_a_pilot_request_records_the_verdict_and_releases_the_hold,
    a_disabled_pilot_hold_writes_no_decision_request,
    a_second_pilot_hold_supersedes_the_first,
    a_changed_pilot_hold_supersedes_its_request_without_a_new_request,
    a_registered_channel_submission_releases_the_pilot_hold_as_the_human,
    agent_and_unregistered_submissions_leave_the_pilot_hold_held
);

crate::scenario_table!(
    host_operational_holds_human_approved_work_until_a_current_operator_decision_scenarios,
    host_operational_holds_human_approved_work_until_a_current_operator_decision,
    host_operational_material_and_newer_dispositions_control_admission,
    legacy_host_operational_receipt_is_backfilled_once_and_tracks_its_material,
    latest_pilot_finding_holds_backlog_until_a_clear_assessment,
    human_pilot_decisions_release_only_the_current_assessment
);

crate::scenario_table!(
    operator_validation_holds_human_approved_work_and_requires_an_evidenced_decision_scenarios,
    operator_validation_holds_human_approved_work_and_requires_an_evidenced_decision,
    repairable_tool_findings_and_expected_denials_remain_advisory,
    changed_criteria_make_the_pilot_validation_assessment_stale,
    legacy_operator_warning_is_backfilled_once_at_admission_and_stales_on_edit,
    operator_validation_hold_is_retained_when_warnings_exceed_cap
);

crate::scenario_table!(
    an_override_comment_decides_only_when_written_before_the_store_recorded_typed_decisions_scenarios,
    an_override_comment_decides_only_when_written_before_the_store_recorded_typed_decisions,
    an_incomplete_boundary_counts_from_its_modification_time_until_a_writable_open_repairs_it
);
