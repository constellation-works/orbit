//! Applied findings hold directly filed backlog tasks across the real drain,
//! ship-selection and readiness boundaries, with assessment-scoped decisions.

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{Task, TaskComplexity, TaskStatus};
use serde_json::json;

use super::Workspace;
use crate::dispatch_admission::isolated;

fn backlog_task(workspace: &Workspace) -> Task {
    workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Directly filed backlog work".into(),
            description: "Repair the README fixture.".into(),
            acceptance_criteria: vec!["The fixture repair is observable.".into()],
            plan: "Inspect README.md.".into(),
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Low,
            context_files: vec!["file:README.md".into()],
            ..Default::default()
        })
        .unwrap()
}

/// Drive the same default, non-promoting prepare/apply actions that routine
/// task-pilot uses, rather than seeding an admission-only representation.
fn assess(workspace: &Workspace, task: &Task, finding: Option<&str>) {
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
        TaskStatus::Backlog
    );
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

#[test]
fn latest_pilot_finding_holds_backlog_until_a_clear_assessment() {
    if !isolated(
        "task_pilot::admission::latest_pilot_finding_holds_backlog_until_a_clear_assessment",
    ) {
        return;
    }
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

#[test]
fn human_pilot_decisions_release_only_the_current_assessment() {
    if !isolated("task_pilot::admission::human_pilot_decisions_release_only_the_current_assessment")
    {
        return;
    }
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

fn human_approved_validation_task_with_tools(
    workspace: &Workspace,
    criterion: &str,
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
            acceptance_criteria: vec![criterion.into()],
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

#[test]
fn operator_validation_holds_human_approved_work_and_requires_an_evidenced_decision() {
    if !isolated(
        "task_pilot::admission::operator_validation_holds_human_approved_work_and_requires_an_evidenced_decision",
    ) {
        return;
    }
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
            serde_json::from_str(comments.last().unwrap().message.split_once('\n').unwrap().1)
                .unwrap();
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

#[test]
fn repairable_tool_findings_and_expected_denials_remain_advisory() {
    if !isolated(
        "task_pilot::admission::repairable_tool_findings_and_expected_denials_remain_advisory",
    ) {
        return;
    }
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
            serde_json::from_str(comments.last().unwrap().message.split_once('\n').unwrap().1)
                .unwrap();
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

#[test]
fn changed_criteria_make_the_pilot_validation_assessment_stale() {
    if !isolated(
        "task_pilot::admission::changed_criteria_make_the_pilot_validation_assessment_stale",
    ) {
        return;
    }
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

#[test]
fn legacy_operator_warning_is_backfilled_once_at_admission_and_stales_on_edit() {
    if !isolated(
        "task_pilot::admission::legacy_operator_warning_is_backfilled_once_at_admission_and_stales_on_edit",
    ) {
        return;
    }
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
                }],
                append_comments: vec![orbit_types::task::TaskComment {
                    at,
                    by: "task-pilot".into(),
                    message: format!("operation_id=legacy-pilot\n{audit}"),
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
