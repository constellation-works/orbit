//! Applied findings hold directly filed backlog tasks across the real drain,
//! ship-selection and readiness boundaries, with assessment-scoped decisions.

use orbit_core::application::task::{
    DecisionSubmitCaller, DecisionSubmitParams, TaskAddParams, TaskUpdateParams,
};
use orbit_core::{Task, TaskComplexity, TaskPriority, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::TaskHistoryUpdateParams;
use orbit_types::task::{
    DecisionGate, DecisionRequest, DecisionRequestState, PilotAdmissionVerdict, TaskArtifact,
    TaskComment, TaskDecision, TaskHistoryEntry,
};
use serde_json::json;

use super::Workspace;

fn backlog_task(workspace: &Workspace) -> Task {
    task_at_status(workspace, TaskStatus::Backlog)
}

fn hu(workspace: &Workspace, task: &Task, params: TaskUpdateParams) {
    hu_by(workspace, task, params, "human:fixture");
}
fn hu_by(workspace: &Workspace, task: &Task, params: TaskUpdateParams, by: &str) {
    workspace
        .runtime
        .update_task_as_human(&task.id, params, by.into())
        .unwrap();
}
fn hu_err(workspace: &Workspace, task: &Task, params: TaskUpdateParams) -> orbit_core::OrbitError {
    workspace
        .runtime
        .update_task_as_human(&task.id, params, "human:fixture".into())
        .unwrap_err()
}
fn human_comment(workspace: &Workspace, task: &Task, comment: String) {
    hu(
        workspace,
        task,
        TaskUpdateParams {
            comment: Some(comment),
            ..Default::default()
        },
    );
}
fn human_comment_err(
    workspace: &Workspace,
    task: &Task,
    comment: String,
) -> orbit_core::OrbitError {
    hu_err(
        workspace,
        task,
        TaskUpdateParams {
            comment: Some(comment),
            ..Default::default()
        },
    )
}
fn agent_update(workspace: &Workspace, task: &Task, params: TaskUpdateParams) {
    workspace
        .runtime
        .update_task_with_identity(
            &task.id,
            params,
            Some("codex".into()),
            Some("gpt-6.1-sol".into()),
        )
        .unwrap();
}
fn agent_comment(workspace: &Workspace, task: &Task, comment: String) {
    agent_update(
        workspace,
        task,
        TaskUpdateParams {
            comment: Some(comment),
            ..Default::default()
        },
    );
}
fn bump_priority(workspace: &Workspace, task: &Task) {
    hu(
        workspace,
        task,
        TaskUpdateParams {
            priority: Some(TaskPriority::High),
            ..Default::default()
        },
    );
}
fn hist(workspace: &Workspace, task: &Task) -> Vec<TaskHistoryEntry> {
    workspace.runtime.get_task_history(&task.id).unwrap()
}
fn has_event(workspace: &Workspace, task: &Task, event: &str) -> bool {
    hist(workspace, task)
        .iter()
        .any(|entry| entry.event == event)
}
fn event_count(workspace: &Workspace, task: &Task, event: &str) -> usize {
    hist(workspace, task)
        .iter()
        .filter(|entry| entry.event == event)
        .count()
}
fn assert_event(workspace: &Workspace, task: &Task, event: &str) {
    assert!(has_event(workspace, task, event), "{event}");
}
fn assert_no_event(workspace: &Workspace, task: &Task, event: &str) {
    assert!(!has_event(workspace, task, event), "{event}");
}
fn task_comments(workspace: &Workspace, task: &Task) -> Vec<TaskComment> {
    workspace.runtime.get_task_comments(&task.id).unwrap()
}
fn last_audit(workspace: &Workspace, task: &Task) -> serde_json::Value {
    super::pilot_audit(
        &workspace.runtime,
        &task.id,
        task_comments(workspace, task).last().unwrap(),
    )
}

fn got(workspace: &Workspace, task: &Task) -> Task {
    workspace.runtime.get_task(&task.id).unwrap()
}
fn verdicts(workspace: &Workspace, task: &Task) -> Vec<PilotAdmissionVerdict> {
    hist(workspace, task)
        .into_iter()
        .filter_map(|entry| match entry.decision {
            Some(TaskDecision::PilotAdmission { verdict, .. }) => Some(verdict),
            _ => None,
        })
        .collect()
}
fn assert_workflow_admitted(workspace: &Workspace, task: &Task) {
    assert_eq!(
        RuntimeHost::admit_task_for_workflow(&workspace.runtime, &task.id, "worktree_setup")
            .unwrap()
            .status,
        TaskStatus::InProgress
    );
}
fn workflow_refusal(workspace: &Workspace, task: &Task) -> String {
    RuntimeHost::admit_task_for_workflow(&workspace.runtime, &task.id, "worktree_setup")
        .unwrap_err()
        .to_string()
}
fn update_task_history(workspace: &Workspace, task: &Task, params: TaskHistoryUpdateParams) {
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
    .update_task_history(&task.id, params)
    .unwrap();
}
fn seed_legacy(workspace: &Workspace, task: &Task, audit: &serde_json::Value, operation: &str) {
    let note = if operation == "legacy-host-pilot" {
        "host_operational (operation_id=legacy-host-pilot)".to_string()
    } else {
        format!("legacy assessment (operation_id={operation})")
    };
    let at = chrono::Utc::now();
    update_task_history(
        workspace,
        task,
        TaskHistoryUpdateParams {
            actor: "task-pilot".into(),
            append_comments: vec![TaskComment {
                at,
                by: "task-pilot".into(),
                message: format!("operation_id={operation}\n{audit}"),
                operation_id: None,
            }],
            append_history: vec![TaskHistoryEntry {
                at,
                by: "task-pilot".into(),
                event: "task_pilot_applied".into(),
                note: Some(note),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: None,
            }],
            ..Default::default()
        },
    );
}
fn answer_req(
    workspace: &Workspace,
    request_id: &str,
    answer: &str,
    note: Option<String>,
) -> DecisionRequest {
    workspace
        .runtime
        .answer_decision_request_as_human(
            request_id,
            answer,
            note,
            "human:fixture".into(),
            Some("dashboard".into()),
        )
        .unwrap()
}
fn assert_answered(request: &DecisionRequest, answer: &str) {
    assert_eq!(request.state, DecisionRequestState::Answered);
    assert_eq!(request.resolution.as_ref().unwrap().answer_id, answer);
}
fn to_backlog(workspace: &Workspace, task: &Task) {
    hu(
        workspace,
        task,
        TaskUpdateParams {
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        },
    );
}
fn priority_comment(workspace: &Workspace, task: &Task, comment: &'static str) {
    hu(
        workspace,
        task,
        TaskUpdateParams {
            priority: Some(TaskPriority::High),
            comment: Some(comment.into()),
            ..Default::default()
        },
    );
}
fn event_note(workspace: &Workspace, task: &Task, event: &str) -> serde_json::Value {
    let history = hist(workspace, task);
    let entry = history.iter().find(|entry| entry.event == event).unwrap();
    serde_json::from_str(entry.note.as_deref().unwrap()).unwrap()
}
fn assert_proposed_to_backlog(workspace: &Workspace, task: &Task) {
    assert!(
        hist(workspace, task)
            .iter()
            .any(|entry| entry.from_status == Some(TaskStatus::Proposed)
                && entry.to_status == Some(TaskStatus::Backlog)
                && entry.by == "human:fixture")
    );
}
fn assert_event_by(workspace: &Workspace, task: &Task, event: &str) {
    assert!(
        hist(workspace, task)
            .iter()
            .any(|entry| entry.event == event && entry.by == "human:fixture"),
        "{event}"
    );
}
fn assert_open(request: &DecisionRequest) {
    assert_eq!(request.state, DecisionRequestState::Open);
}
fn attach_passed_eval(workspace: &Workspace, task: &Task) {
    hu(
        workspace,
        task,
        TaskUpdateParams {
            upsert_artifacts: vec![TaskArtifact::from_text(
                "operator-evaluation.json",
                json!({"criterion": 1, "tool": "orbit.pipeline.invoke", "outcome": "passed"})
                    .to_string(),
            )],
            ..Default::default()
        },
    );
}
fn answer_req_refused(
    workspace: &Workspace,
    request_id: &str,
    answer: &str,
    note: Option<String>,
    msg: &'static str,
) -> String {
    workspace
        .runtime
        .answer_decision_request_as_human(
            request_id,
            answer,
            note,
            "human:fixture".into(),
            Some("dashboard".into()),
        )
        .expect_err(msg)
        .to_string()
}
fn set_deps(workspace: &Workspace, task: &Task, deps: Vec<String>) {
    hu(
        workspace,
        task,
        TaskUpdateParams {
            dependencies: Some(deps),
            ..Default::default()
        },
    );
}
fn tool_update(workspace: &Workspace, value: serde_json::Value) {
    workspace
        .runtime
        .run_tool("orbit.task.update", value)
        .unwrap();
}
fn add_fixture_task(
    workspace: &Workspace,
    title: &str,
    description: &str,
    criterion: &str,
    status: TaskStatus,
    complexity: TaskComplexity,
    context: &str,
) -> Task {
    workspace
        .runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: description.into(),
            acceptance_criteria: vec![criterion.into()],
            status: Some(status),
            complexity,
            context_files: vec![context.into()],
            ..Default::default()
        })
        .unwrap()
}
fn answer_ids(request: &DecisionRequest) -> Vec<&str> {
    request
        .answers
        .iter()
        .map(|answer| answer.id.as_str())
        .collect()
}
fn channel_held(field: &str, reason: &str) -> (Workspace, Task, DecisionRequest) {
    let workspace = decision_channel_workspace();
    let task = backlog_task(&workspace);
    assess(&workspace, &task, Some(field));
    assert_admission(&workspace, &task, Some(reason));
    let request = decision_requests(&workspace, &task).pop().unwrap();
    (workspace, task, request)
}
fn assert_snapshot_unchanged(
    workspace: &Workspace,
    task: &Task,
    comments: &[TaskComment],
    history: &[TaskHistoryEntry],
) {
    assert_eq!(
        task_comments(workspace, task),
        comments,
        "a refused answer writes no comment"
    );
    assert_eq!(
        hist(workspace, task),
        history,
        "a refused answer writes no resolution or answered marker"
    );
}
fn assert_req0(requirements: &serde_json::Value, criterion: u64) {
    assert_eq!(requirements[0]["criterion"], criterion);
    assert_eq!(requirements[0]["tool"], "orbit.pipeline.invoke");
}
fn last_comment_record(workspace: &Workspace, task: &Task) -> serde_json::Value {
    let comments = task_comments(workspace, task);
    serde_json::from_str(comments.last().unwrap().message.split_once('\n').unwrap().1).unwrap()
}
fn channel_token(workspace: &Workspace, request_id: &str) -> String {
    workspace
        .runtime
        .register_decision_channel("email", "human:fixture", "decision-mail", vec![])
        .unwrap();
    workspace
        .runtime
        .mint_decision_token("email", request_id)
        .unwrap()
}
fn fresh_backlog() -> (Workspace, Task) {
    let workspace = Workspace::new();
    let task = backlog_task(&workspace);
    (workspace, task)
}
fn fresh_validation(criterion: &str) -> (Workspace, Task) {
    let workspace = Workspace::new();
    let task = human_approved_validation_task(&workspace, criterion);
    (workspace, task)
}
fn set_criteria(workspace: &Workspace, task: &Task, criteria: Vec<String>) {
    hu(
        workspace,
        task,
        TaskUpdateParams {
            acceptance_criteria: Some(criteria),
            ..Default::default()
        },
    );
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
    assess_with(workspace, task, finding, disposition, json!({}));
}

/// [`assess_disposition`] with `extra` assessment fields merged in.
fn assess_with(
    workspace: &Workspace,
    task: &Task,
    finding: Option<&str>,
    disposition: &str,
    extra: serde_json::Value,
) {
    let prepared = workspace.prepare(&[&task.id]);
    let current = got(workspace, task);
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
    for (field, value) in extra.as_object().unwrap() {
        assessment[field] = value.clone();
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
    assert_eq!(got(workspace, task).status, current.status);
}

fn host_operational_holds_human_approved_work_until_a_current_operator_decision() {
    for decision in ["approve-anyway", "clear", "evaluated"] {
        let workspace = Workspace::new();
        let task = task_at_status(&workspace, TaskStatus::Proposed);
        // Even a no-diff task needs an operator decision for this disposition.
        hu(
            &workspace,
            &task,
            TaskUpdateParams {
                tags: Some(vec!["no-diff-expected".into()]),
                comment: Some(format!(
                    "task-pilot-admission: {decision}\nA prior decision cannot release a future assessment."
                )),
                ..Default::default()
            },
        );
        assess_disposition(&workspace, &task, None, "host_operational");
        to_backlog(&workspace, &task);
        assert_admission(&workspace, &task, Some("host_operational_handoff"));
        assert_proposed_to_backlog(&workspace, &task);
        assert_eq!(
            event_count(&workspace, &task, "host_operational_held"),
            1,
            "repeated admission must not duplicate the handoff"
        );
        let record = event_note(&workspace, &task, "host_operational_held");
        assert_eq!(record["disposition"], "host_operational");
        let evidence = record["hold"]["evidence"].as_str().unwrap();
        assert!(!evidence.is_empty());
        let audit = last_audit(&workspace, &task);
        assert_eq!(audit["host_operational_hold"], record["hold"]);
        assert_eq!(audit["assessment"]["evidence"], evidence);
        let refused = workflow_refusal(&workspace, &task);
        assert!(
            refused.contains("host_operational") && refused.contains(evidence),
            "{refused}"
        );
        assert_eq!(got(&workspace, &task).status, TaskStatus::Backlog);

        let comment = format!(
            "task-pilot-admission: {decision}\nThe operator accepts the owner-side repair; see operator-evaluation.json."
        );
        agent_comment(&workspace, &task, comment.clone());
        priority_comment(
            &workspace,
            &task,
            "The operator is still reviewing this finding.",
        );
        human_comment_err(
            &workspace,
            &task,
            format!("task-pilot-admission: {decision}"),
        );
        if decision == "evaluated" {
            human_comment_err(&workspace, &task, comment.clone());
        }
        assert_admission(&workspace, &task, Some("host_operational_handoff"));
        assert_no_event(&workspace, &task, "host_operational_resolved");
        hu(
            &workspace,
            &task,
            TaskUpdateParams {
                comment: Some(comment),
                upsert_artifacts: if decision == "evaluated" {
                    vec![TaskArtifact::from_text(
                        "operator-evaluation.json",
                        "Owner-side repair verified.",
                    )]
                } else {
                    vec![]
                },
                ..Default::default()
            },
        );
        assert_admission(&workspace, &task, None);
        assert_event_by(&workspace, &task, "host_operational_resolved");
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
            set_criteria(
                &workspace,
                &task,
                vec!["The repository fixture repair is observable.".into()],
            );
            assert_admission(&workspace, &task, Some("pilot_assessment_stale"));
            assess_disposition(&workspace, &task, None, "selectors");
        } else {
            assess_disposition(&workspace, &task, None, release);
        }
        assert_admission(&workspace, &task, None);
        assert_workflow_admitted(&workspace, &task);
    }
}

fn legacy_host_operational_receipt_is_backfilled_once_and_tracks_its_material() {
    let (workspace, task) = fresh_validation("The owner-side repair is observable.");
    let audit = json!({"assessment": {
        "disposition": "host_operational", "evidence": "The operator must repair the owner-side definition.",
        "duplicate_of": null, "already_landed": null,
    }});
    seed_legacy(&workspace, &task, &audit, "legacy-host-pilot");
    // Old receipts must not mistake metadata edits before the first admission
    // for a re-scope of the host-operational work.
    bump_priority(&workspace, &task);
    assert_admission(&workspace, &task, Some("host_operational_handoff"));
    assert_admission(&workspace, &task, Some("host_operational_handoff"));
    assert_eq!(event_count(&workspace, &task, "host_operational_held"), 1);
    let comments = task_comments(&workspace, &task);
    let (header, record) = comments.last().unwrap().message.split_once('\n').unwrap();
    assert_eq!(header, "host-operational-handoff");
    let record: serde_json::Value = serde_json::from_str(record).unwrap();
    assert_eq!(record["disposition"], "host_operational");
    assert_eq!(record["hold"]["evidence"], audit["assessment"]["evidence"]);
    bump_priority(&workspace, &task);
    assert_admission(&workspace, &task, Some("host_operational_handoff"));
    set_criteria(
        &workspace,
        &task,
        vec!["The repository fixture repair is observable.".into()],
    );
    assert_admission(&workspace, &task, Some("pilot_assessment_stale"));
    assess(&workspace, &task, None);
    assert_admission(&workspace, &task, None);
}

/// A `host_operational_held` entry an older binary wrote carries its hold
/// only in its JSON note. Admission still recovers that hold, and the handoff
/// it records is not posted a second time.
fn legacy_note_only_host_operational_handoff_is_recovered_and_not_recorded_again() {
    let criterion = "The owner-side repair is observable.";
    let audit = json!({"assessment": {
        "disposition": "host_operational", "evidence": "The operator must repair the owner-side definition.",
        "duplicate_of": null, "already_landed": null,
    }});
    // A current binary records the handoff with a typed decision; the same
    // task material in another store yields the same hold material.
    let (typed, typed_task) = fresh_validation(criterion);
    seed_legacy(&typed, &typed_task, &audit, "legacy-host-pilot");
    assert_admission(&typed, &typed_task, Some("host_operational_handoff"));
    let handoff = hist(&typed, &typed_task)
        .into_iter()
        .find(|entry| entry.event == "host_operational_held")
        .unwrap();
    let Some(TaskDecision::PilotOperatorHandoff { operation_id, hold }) = &handoff.decision else {
        panic!("handoff must carry its typed decision: {handoff:?}");
    };
    assert_eq!(operation_id, "legacy-host-pilot");
    let note: serde_json::Value = serde_json::from_str(handoff.note.as_deref().unwrap()).unwrap();
    assert_eq!(
        note,
        json!({"operation_id": "legacy-host-pilot", "disposition": "host_operational", "hold": hold})
    );

    let (workspace, task) = fresh_validation(criterion);
    seed_legacy(&workspace, &task, &audit, "legacy-host-pilot");
    let recorded_evidence = "The recorded hold names the owner-side definition repair.";
    let legacy_note = json!({
        "operation_id": "legacy-host-pilot", "disposition": "host_operational",
        "hold": {"material": hold["material"], "evidence": recorded_evidence},
    })
    .to_string();
    append_history(
        &workspace,
        &task,
        TaskHistoryEntry {
            at: chrono::Utc::now(),
            by: "system".into(),
            event: "host_operational_held".into(),
            note: Some(legacy_note.clone()),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: None,
        },
    );
    assert_admission(&workspace, &task, Some("host_operational_handoff"));
    assert_admission(&workspace, &task, Some("host_operational_handoff"));
    let refused = workflow_refusal(&workspace, &task);
    assert!(refused.contains(recorded_evidence), "{refused}");
    let handoffs = hist(&workspace, &task)
        .into_iter()
        .filter(|entry| entry.event == "host_operational_held")
        .collect::<Vec<_>>();
    assert_eq!(
        handoffs.len(),
        1,
        "the legacy handoff must not be re-posted"
    );
    assert_eq!(handoffs[0].note.as_deref(), Some(legacy_note.as_str()));
    assert!(
        !task_comments(&workspace, &task)
            .iter()
            .any(|comment| comment.message.starts_with("host-operational-handoff\n")),
        "no second handoff comment"
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

fn latest_pilot_finding_holds_backlog_until_a_clear_assessment() {
    for (field, reason) in [
        ("duplicate_of", "pilot_duplicate"),
        ("already_landed", "pilot_already_landed"),
    ] {
        let (workspace, task) = fresh_backlog();
        assert_admission(&workspace, &task, None);
        assess(&workspace, &task, Some(field));
        assert_admission(&workspace, &task, Some(reason));
        hu(
            &workspace,
            &task,
            TaskUpdateParams {
                description: Some("Edited scope still needs a decision about the finding.".into()),
                comment: Some("The operator is reviewing the assessment.".into()),
                ..Default::default()
            },
        );
        assert_admission(&workspace, &task, Some("pilot_assessment_stale"));
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
            let (workspace, task) = fresh_backlog();
            let comment = format!("task-pilot-admission: {decision}\nReviewed the finding.");
            // A decision made before an assessment cannot approve future findings.
            human_comment(&workspace, &task, comment.clone());
            assess(&workspace, &task, Some(field));
            // Canonical agent provenance cannot forge the human release path.
            agent_comment(&workspace, &task, comment.clone());
            assert_admission(&workspace, &task, Some(reason));
            human_comment(&workspace, &task, comment);
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
    to_backlog(workspace, &task);
    task
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
        human_comment(&workspace, &task, comment.clone());
        assess(&workspace, &task, None);
        assert_admission(&workspace, &task, Some("operator_validation_handoff"));
        assert_proposed_to_backlog(&workspace, &task);
        let hold = event_note(&workspace, &task, "operator_validation_held");
        assert_req0(&hold["hold"]["requirements"], 1);
        let audit = last_audit(&workspace, &task);
        assert_eq!(
            audit["operator_validation_hold"]["requirements"],
            hold["hold"]["requirements"]
        );

        // Even a queued run bypassing selection meets the hold at admission.
        let refused = workflow_refusal(&workspace, &task);
        assert!(refused.contains("criterion 1 requires `orbit.pipeline.invoke`"));
        assert_eq!(got(&workspace, &task).status, TaskStatus::Backlog);
        priority_comment(
            &workspace,
            &task,
            "Discussing the finding does not resolve it.",
        );
        agent_comment(&workspace, &task, comment.clone());
        // The registered agent tool cannot mint an operator decision even
        // when a caller omits provenance on a human-owned runtime.
        let _ = workspace.runtime.run_tool(
            "orbit.task.update",
            json!({"id": task.id, "comment": comment}),
        );
        assert_admission(&workspace, &task, Some("operator_validation_handoff"));
        assert_no_event(&workspace, &task, "operator_validation_resolved");
        human_comment_err(
            &workspace,
            &task,
            format!("task-pilot-admission: {decision}"),
        );
        assert_admission(&workspace, &task, Some("operator_validation_handoff"));
        if decision == "evaluated" {
            let missing_artifact = human_comment_err(&workspace, &task, comment.clone());
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
                vec![TaskArtifact::from_text(
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
            hu(&workspace, &task, resolution);
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
        assert_event_by(&workspace, &task, "operator_validation_resolved");
        assess(&workspace, &task, None);
        assert_admission(&workspace, &task, Some("operator_validation_handoff"));
        set_criteria(
            &workspace,
            &task,
            vec!["Observe the fixture repair through the runtime boundary.".into()],
        );
        assert_admission(&workspace, &task, Some("pilot_assessment_stale"));
        assess(&workspace, &task, None);
        assert_admission(&workspace, &task, None);
        assert_workflow_admitted(&workspace, &task);
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
        let audit = last_audit(&workspace, &task);
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
    let (workspace, task) = fresh_validation("The repair is observable.");
    assess(&workspace, &task, Some("utility_warnings"));
    assert_admission(&workspace, &task, None);
}

/// [ORB-23900] A criterion that needs an owner sign-off recorded is flagged as
/// operator-only, and a criterion that only mentions a decision in docs is not.
fn owner_sign_off_criterion_is_flagged_as_operator_only() {
    for (criterion, flagged) in [
        (
            "The owner signs off the retry policy before the task merges.",
            true,
        ),
        (
            "Record the operator sign-off as a decision before promotion.",
            true,
        ),
        ("The design doc records the retry decision.", false),
        (
            "The docs describe how the operator decision flow works.",
            false,
        ),
    ] {
        let (workspace, task) = fresh_validation(criterion);
        assess(&workspace, &task, None);
        let audit = last_audit(&workspace, &task);
        let sign_off_findings = audit["assessment"]["validation_tool_warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|warning| {
                warning
                    .as_str()
                    .unwrap()
                    .contains("requires an owner sign-off to be recorded")
            })
            .count();
        assert_eq!(
            sign_off_findings,
            usize::from(flagged),
            "{criterion}: {audit}"
        );
        assert!(audit["operator_validation_hold"].is_null(), "{audit}");
        assert_admission(&workspace, &task, None);
    }
}

const RESERVED_FINDING: &str = "a governed operation reserved for the";

fn reserved_findings(workspace: &Workspace, task: &Task) -> usize {
    last_audit(workspace, task)["assessment"]["validation_tool_warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|warning| warning.as_str().unwrap().contains(RESERVED_FINDING))
        .count()
}

/// [ORB-21291] A criterion that names an operator-only tool only as what the
/// change edits does not hold once the pilot classifies it as the subject.
fn operator_tool_named_as_the_change_subject_does_not_hold() {
    let (workspace, task) = fresh_validation(
        "The description `orbit.pipeline.invoke` advertises names its operator handoff, shown by the tool-list golden.",
    );
    let prepared = workspace.prepare(&[&task.id]);
    assert_eq!(
        prepared["partitions"][0]["operator_tool_candidates"][&task.id],
        json!([{"criterion": 1, "tool": "orbit.pipeline.invoke"}]),
        "{prepared}"
    );
    assess_with(
        &workspace,
        &task,
        None,
        "selectors",
        json!({"operator_tool_classifications": [{
            "criterion": 1, "tool": "orbit.pipeline.invoke", "classification": "subject",
            "reason": "The criterion checks the advertised description text, not a run.",
        }]}),
    );
    let audit = last_audit(&workspace, &task);
    assert!(audit["operator_validation_hold"].is_null(), "{audit}");
    assert_eq!(
        audit["assessment"]["operator_tool_classifications"][0]["classification"],
        "subject"
    );
    assert_eq!(reserved_findings(&workspace, &task), 0, "{audit}");
    assert_no_event(&workspace, &task, "operator_validation_held");
    assert_admission(&workspace, &task, None);
    assert_workflow_admitted(&workspace, &task);
}

/// [ORB-21291] An `invoke` classification keeps the operator handoff.
fn operator_tool_the_criterion_must_invoke_still_holds() {
    let (workspace, task) =
        fresh_validation("Run the live evaluation with `orbit.pipeline.invoke`.");
    assess_with(
        &workspace,
        &task,
        None,
        "selectors",
        json!({"operator_tool_classifications": [{
            "criterion": 1, "tool": "orbit.pipeline.invoke", "classification": "invoke",
            "reason": "Validation runs the live evaluation.",
        }]}),
    );
    let audit = last_audit(&workspace, &task);
    assert_req0(&audit["operator_validation_hold"]["requirements"], 1);
    assert_eq!(
        audit["operator_validation_hold"]["requirements"][0]["tool"],
        "orbit.pipeline.invoke"
    );
    assert_eq!(reserved_findings(&workspace, &task), 1, "{audit}");
    assert_admission(&workspace, &task, Some("operator_validation_handoff"));
    assert!(
        workflow_refusal(&workspace, &task)
            .contains("criterion 1 requires `orbit.pipeline.invoke`")
    );
}

/// [ORB-21291] A detected mention without a usable classification fails safe.
fn unclassified_or_malformed_operator_tool_classification_still_holds() {
    let subject = |criterion: serde_json::Value, tool: &str, classification: &str, reason: &str| {
        json!({"operator_tool_classifications": [{
            "criterion": criterion, "tool": tool,
            "classification": classification, "reason": reason,
        }]})
    };
    for extra in [
        json!({}),
        json!({"operator_tool_classifications": "subject"}),
        json!({"operator_tool_classifications": ["subject"]}),
        subject(json!(1), "orbit.pipeline.invoke", "subject", " "),
        subject(json!(1), "orbit.pipeline.invoke", "maybe", "Unsure."),
        subject(json!("1"), "orbit.pipeline.invoke", "subject", "Text only."),
        subject(
            json!(2),
            "orbit.pipeline.invoke",
            "subject",
            "Wrong criterion.",
        ),
        subject(json!(1), "orbit.task.requeue", "subject", "Wrong tool."),
        json!({"operator_tool_classifications": [
            {"criterion": 1, "tool": "orbit.pipeline.invoke", "classification": "subject", "reason": "Text."},
            {"criterion": 1, "tool": "orbit.pipeline.invoke", "classification": "invoke", "reason": "Run."},
        ]}),
    ] {
        let (workspace, task) =
            fresh_validation("The description `orbit.pipeline.invoke` advertises is reworded.");
        assess_with(&workspace, &task, None, "selectors", extra.clone());
        let audit = last_audit(&workspace, &task);
        assert_req0(&audit["operator_validation_hold"]["requirements"], 1);
        assert_eq!(reserved_findings(&workspace, &task), 1, "{extra}: {audit}");
        assert_admission(&workspace, &task, Some("operator_validation_handoff"));
    }
}

/// A re-check must not carry a prior `subject` classification forward when
/// the criterion now requires invoking the tool: the stale entry would
/// otherwise drop the operator hold for the edited criterion.
fn recheck_after_subject_to_invoke_criteria_edit_still_holds() {
    let (workspace, task) = fresh_validation(
        "The description `orbit.pipeline.invoke` advertises names its operator handoff, shown by the tool-list golden.",
    );
    assess_with(
        &workspace,
        &task,
        Some("utility_warnings"),
        "selectors",
        json!({"operator_tool_classifications": [{
            "criterion": 1, "tool": "orbit.pipeline.invoke", "classification": "subject",
            "reason": "The criterion checks the advertised description text, not a run.",
        }]}),
    );
    let audit = last_audit(&workspace, &task);
    assert!(audit["operator_validation_hold"].is_null(), "{audit}");

    set_criteria(
        &workspace,
        &task,
        vec!["Run `orbit.pipeline.invoke` to perform the live evaluation.".into()],
    );
    let prepared = workspace.prepare(&[&task.id]);
    assert_eq!(prepared["tasks"][0]["pilot_mode"], "recheck", "{prepared}");
    assert_eq!(
        prepared["partitions"][0]["operator_tool_candidates"][&task.id],
        json!([{"criterion": 1, "tool": "orbit.pipeline.invoke"}]),
        "{prepared}"
    );
    // The re-check pilot judges only the prior holding finding and returns
    // no classifications; apply must not reuse the stale `subject` entry.
    let applied = workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo, "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": [task.id], "tasks": [{
                "task_id": task.id,
                "recheck_findings": [{
                    "id": "utility_warnings[0]", "status": "resolved",
                    "evidence": "The fixture utility check now passes.",
                }],
            }]}],
        }),
    );
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(applied["applied_count"], 1, "{applied}");
    let audit = last_audit(&workspace, &task);
    assert_eq!(audit["assessment"]["mode"], "recheck", "{audit}");
    assert!(
        !audit["assessment"]["recheck_kept_fields"]
            .as_array()
            .unwrap()
            .contains(&json!("operator_tool_classifications")),
        "{audit}"
    );
    assert_req0(&audit["operator_validation_hold"]["requirements"], 1);
    assert_eq!(reserved_findings(&workspace, &task), 1, "{audit}");
    assert_admission(&workspace, &task, Some("operator_validation_handoff"));
}

fn changed_criteria_make_the_pilot_validation_assessment_stale() {
    let (workspace, task) = fresh_validation("Evaluate through `orbit.pipeline.invoke`.");
    assess(&workspace, &task, None);
    assert_admission(&workspace, &task, Some("operator_validation_handoff"));
    set_criteria(
        &workspace,
        &task,
        vec!["Evaluate the revised scenario through `orbit.pipeline.invoke`.".into()],
    );
    assert_admission(&workspace, &task, Some("pilot_assessment_stale"));
    assess(&workspace, &task, None);
    assert_admission(&workspace, &task, Some("operator_validation_handoff"));
}

fn legacy_operator_warning_is_backfilled_once_at_admission_and_stales_on_edit() {
    let (workspace, task) = fresh_validation("Evaluate with `orbit.pipeline.invoke`.");
    let audit = json!({"assessment": {
        "validation_tool_warnings": ["acceptance criterion requires `orbit.pipeline.invoke`, a governed operation reserved for the operator capability; route validation to an operator"],
        "duplicate_of": null, "already_landed": null,
    }});
    seed_legacy(&workspace, &task, &audit, "legacy-pilot");
    assert_admission(&workspace, &task, Some("operator_validation_handoff"));
    assert_admission(&workspace, &task, Some("operator_validation_handoff"));
    assert_eq!(
        event_count(&workspace, &task, "operator_validation_held"),
        1
    );
    let record = last_comment_record(&workspace, &task);
    assert_req0(&record["hold"]["requirements"], 1);
    let mut session = orbit_types::tool::ToolSessionContext::default();
    session
        .effective_capabilities
        .insert(orbit_types::tool::McpCapability::Operator);
    let revision = workspace
        .runtime
        .desktop_task_snapshot(&task.id, &session)
        .unwrap()
        .revision;
    workspace
        .runtime
        .desktop_task_write(
            orbit_types::desktop::DesktopTaskRequest {
                request_id: "legacy-requirements-rescope".into(),
                operation: orbit_types::desktop::DesktopTaskOperation::Edit {
                    id: task.id.clone(),
                    expected_revision: revision,
                    fields: orbit_types::desktop::DesktopTaskFields {
                        acceptance_criteria: Some(vec!["The fixture repair is observable.".into()]),
                        ..Default::default()
                    },
                },
            },
            None,
            None,
            &session,
        )
        .unwrap();
    assert_admission(&workspace, &task, Some("pilot_assessment_stale"));
    assess(&workspace, &task, None);
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
    let audit = last_audit(&workspace, &task);
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
    assert_eq!(
        audit["operator_validation_hold"]["requirements"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_req0(&audit["operator_validation_hold"]["requirements"], 4);
    assert_admission(&workspace, &task, Some("operator_validation_handoff"));

    // 2. Legacy pilot assessment backfill also derives the hold directly, independent of the warning cap.
    let workspace2 = Workspace::new();
    let task2 = human_approved_validation_task_with_criteria(&workspace2, criteria, vec![]);
    let legacy_audit = json!({"assessment": {
        "validation_tool_warnings": warnings,
        "duplicate_of": null, "already_landed": null,
    }});
    seed_legacy(&workspace2, &task2, &legacy_audit, "legacy-pilot");
    assert_admission(&workspace2, &task2, Some("operator_validation_handoff"));
    assert_eq!(
        event_count(&workspace2, &task2, "operator_validation_held"),
        1
    );
    let record2 = last_comment_record(&workspace2, &task2);
    assert_req0(&record2["hold"]["requirements"], 4);
}

/// Append `message` as `by`'s comment at `at`, as a binary that predates
/// typed decisions recorded it.
fn append_comment(workspace: &Workspace, task: &Task, by: &str, at: &str, message: &str) {
    update_task_history(
        workspace,
        task,
        TaskHistoryUpdateParams {
            actor: by.into(),
            append_comments: vec![TaskComment {
                at: at.parse().unwrap(),
                by: by.into(),
                message: message.into(),
                operation_id: None,
            }],
            ..Default::default()
        },
    );
}

pub(super) fn append_history(workspace: &Workspace, task: &Task, entry: TaskHistoryEntry) {
    let actor = entry.by.clone();
    update_task_history(
        workspace,
        task,
        TaskHistoryUpdateParams {
            actor,
            append_history: vec![entry],
            ..Default::default()
        },
    );
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
            let (workspace, task) = fresh_backlog();
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
            human_comment(
                &workspace,
                &task,
                format!("task-pilot-admission: {decision}"),
            );
            assert_admission(&workspace, &task, None);
            assert_eq!(verdicts(&workspace, &task), vec![verdict]);
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

fn decision_requests(workspace: &Workspace, task: &Task) -> Vec<DecisionRequest> {
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
    assert_open(request);
    assert_eq!(
        request.risk_tier,
        orbit_types::task::DecisionRiskTier::Authority
    );
    assert_eq!(
        request.category.as_deref(),
        Some(
            format!(
                "{}/pilot_admission",
                workspace.runtime.workspace_id().unwrap()
            )
            .as_str()
        )
    );
    assert_eq!(request.gate, DecisionGate::PilotAdmission);
    assert_eq!(answer_ids(request), vec!["approve_anyway", "clear"]);
    assert!(
        matches!(&request.binding, orbit_types::task::DecisionBinding::PilotOperation { operation_id } if !operation_id.is_empty()),
        "{request:?}"
    );
}

fn an_evaluated_answer_on_a_finding_hold_is_refused_and_a_valid_answer_releases_it() {
    for (field, reason) in [
        ("duplicate_of", "pilot_duplicate"),
        ("already_landed", "pilot_already_landed"),
    ] {
        let (workspace, task, request) = channel_held(field, reason);
        assert_eq!(
            answer_ids(&request),
            vec!["approve_anyway", "clear"],
            "{field}: evaluated does not resolve this hold, so it is not offered"
        );
        let comments_before = task_comments(&workspace, &task);
        let history_before = hist(&workspace, &task);
        let message = answer_req_refused(
            &workspace,
            &request.id,
            "evaluated",
            None,
            "evaluated is not an answer for a finding hold",
        );
        assert!(
            message.contains("answer_not_allowed") && message.contains("nothing was applied"),
            "{field}: {message}"
        );
        assert_snapshot_unchanged(&workspace, &task, &comments_before, &history_before);
        let open = decision_requests(&workspace, &task);
        assert_eq!(open.len(), 1, "{field}");
        assert_eq!(open[0].id, request.id);
        assert_open(&open[0]);
        assert!(open[0].resolution.is_none(), "{field}");
        assert_admission(&workspace, &task, Some(reason));

        let answered = answer_req(&workspace, &request.id, "clear", None);
        assert_answered(&answered, "clear");
        assert_admission(&workspace, &task, None);
        assert_eq!(
            verdicts(&workspace, &task),
            vec![PilotAdmissionVerdict::Clear],
            "{field}"
        );
    }
}

/// A request raised before the offer was hold-aware still lists `evaluated`.
/// Answering it must refuse before the comment and the answered marker, and
/// the same request must still accept a verdict that releases the hold.
fn an_evaluated_answer_still_listed_on_an_older_request_is_refused() {
    let (workspace, task, current) = channel_held("duplicate_of", "pilot_duplicate");
    let older_offer = DecisionRequest::new(
        task.id.clone(),
        DecisionGate::PilotAdmission,
        PilotAdmissionVerdict::decision_answers(),
        current.binding.clone(),
        "task_pilot".into(),
        current.raised_at + chrono::Duration::seconds(1),
    );
    append_history(&workspace, &task, older_offer.history_entry("task-pilot"));
    let requests = decision_requests(&workspace, &task);
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(requests[0].state, DecisionRequestState::Superseded);
    let request_id = requests[1].id.clone();
    assert_open(&requests[1]);
    assert!(
        requests[1]
            .answers
            .iter()
            .any(|answer| answer.id == "evaluated"),
        "the pre-fix offer still lists evaluated: {requests:?}"
    );
    let comments_before = task_comments(&workspace, &task);
    let history_before = hist(&workspace, &task);
    let refused = workspace
        .runtime
        .answer_decision_request_as_human(
            &request_id,
            "evaluated",
            None,
            "human:fixture".into(),
            Some("dashboard".into()),
        )
        .expect_err("evaluated does not resolve a duplicate hold");
    let message = refused.to_string();
    assert!(
        message.contains("answer_not_applicable") && message.contains("nothing was applied"),
        "{message}"
    );
    assert_eq!(
        task_comments(&workspace, &task),
        comments_before,
        "a refused answer writes no comment"
    );
    assert_eq!(
        hist(&workspace, &task),
        history_before,
        "a refused answer writes no resolution or answered marker"
    );
    let still = decision_requests(&workspace, &task);
    assert_open(&still[1]);
    assert!(still[1].resolution.is_none());
    assert_eq!(still[1].id, request_id);
    assert_admission(&workspace, &task, Some("pilot_duplicate"));

    let answered = answer_req(
        &workspace,
        &request_id,
        "approve_anyway",
        Some("Not a duplicate: the deliverables differ.".into()),
    );
    assert_answered(&answered, "approve_anyway");
    assert_admission(&workspace, &task, None);
}

fn a_disabled_pilot_hold_writes_no_decision_request() {
    let (workspace, task) = fresh_backlog();
    assess(&workspace, &task, Some("duplicate_of"));
    assert_admission(&workspace, &task, Some("pilot_duplicate"));
    assert!(decision_requests(&workspace, &task).is_empty());
}

fn a_changed_pilot_hold_supersedes_its_request_without_a_new_request() {
    for change in [
        "operator_clear",
        "clear_assessment",
        "replacement_hold",
        "second_hold",
    ] {
        let (workspace, task, _first) = channel_held("duplicate_of", "pilot_duplicate");
        match change {
            "operator_clear" => {
                human_comment(
                    &workspace,
                    &task,
                    "task-pilot-admission: clear\nThe finding no longer applies.".into(),
                );
            }
            "clear_assessment" => assess(&workspace, &task, None),
            _ => assess(&workspace, &task, Some("duplicate_of")),
        }
        if change == "second_hold" {
            assert_admission(&workspace, &task, Some("pilot_duplicate"));
            let requests = decision_requests(&workspace, &task);
            assert_eq!(requests.len(), 2, "{requests:?}");
            assert_eq!(requests[0].state, DecisionRequestState::Superseded);
            assert_open(&requests[1]);
            assert_ne!(requests[0].binding.key(), requests[1].binding.key());
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.state == DecisionRequestState::Open)
                    .count(),
                1,
                "at most one request per task is open"
            );
            continue;
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
            DecisionRequestState::Superseded,
            "{change}"
        );
    }
}

fn resolving_one_typed_hold_raises_a_fresh_request_for_the_next_hold() {
    let workspace = decision_channel_workspace();
    let task = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Layered pilot holds work".into(),
            description: "Repair the README fixture.".into(),
            acceptance_criteria: vec![
                "Run the live evaluation with `orbit.pipeline.invoke`.".into(),
            ],
            plan: "Inspect README.md.".into(),
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Low,
            context_files: vec!["file:README.md".into()],
            ..Default::default()
        })
        .unwrap();
    assess_disposition(&workspace, &task, None, "host_operational");
    let audit = last_audit(&workspace, &task);
    assert!(
        !audit["host_operational_hold"].is_null(),
        "one assessment persists the host-operational hold: {audit}"
    );
    assert_eq!(
        audit["operator_validation_hold"]["requirements"][0]["criterion"], 1,
        "one assessment persists the operator-validation hold: {audit}"
    );
    assert_eq!(
        audit["operator_validation_hold"]["requirements"][0]["tool"], "orbit.pipeline.invoke",
        "{audit}"
    );
    assert_admission(&workspace, &task, Some("host_operational_handoff"));
    let first = decision_requests(&workspace, &task).pop().unwrap();
    assert_open(&first);

    let answered_first = answer_req(
        &workspace,
        &first.id,
        "clear",
        Some("The operator accepts the owner-side operation.".into()),
    );
    assert_answered(&answered_first, "clear");
    assert_event(&workspace, &task, "host_operational_resolved");
    assert_no_event(&workspace, &task, "operator_validation_resolved");

    // Repeating admission exposes the remaining hold and raises a fresh
    // request for it, without a new assessment or a raw decision comment.
    assert_admission(&workspace, &task, Some("operator_validation_handoff"));
    let requests = decision_requests(&workspace, &task);
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(requests[0].id, first.id);
    assert_answered(&requests[0], "clear");
    let second = requests[1].clone();
    assert_ne!(second.id, first.id, "the next hold gets a fresh request");
    assert_open(&second);
    assert_eq!(
        second.binding, first.binding,
        "both requests bind to the same pilot operation"
    );
    assert!(
        second.answers.iter().any(|answer| answer.id == "evaluated"),
        "the operator-validation request offers evaluated: {second:?}"
    );

    // Replay of the answered request is refused and changes nothing.
    let replay = answer_req_refused(
        &workspace,
        &first.id,
        "clear",
        Some("A stale replay must not target the next hold.".into()),
        "an answered request refuses a replay",
    );
    assert!(
        replay.contains("request_superseded") && replay.contains("nothing was applied"),
        "{replay}"
    );
    let requests = decision_requests(&workspace, &task);
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(requests[1].id, second.id);
    assert_open(&requests[1]);
    assert_no_event(&workspace, &task, "operator_validation_resolved");
    assert_admission(&workspace, &task, Some("operator_validation_handoff"));

    attach_passed_eval(&workspace, &task);
    let answered_second = answer_req(
        &workspace,
        &second.id,
        "evaluated",
        Some("Operator reviewed operator-evaluation.json and accepts this validation.".into()),
    );
    assert_answered(&answered_second, "evaluated");
    for event in ["host_operational_resolved", "operator_validation_resolved"] {
        assert_eq!(
            event_count(&workspace, &task, event),
            1,
            "each typed hold resolves exactly once: {event}"
        );
    }
    assert_eq!(
        verdicts(&workspace, &task),
        vec![
            PilotAdmissionVerdict::Clear,
            PilotAdmissionVerdict::Evaluated
        ],
        "successive answers record their own verdicts"
    );
    assert_admission(&workspace, &task, None);
    let requests = decision_requests(&workspace, &task);
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(
        requests
            .iter()
            .all(|request| request.state == DecisionRequestState::Answered),
        "no request stays open after release: {requests:?}"
    );
}

fn submit_pilot_answer(
    workspace: &Workspace,
    request_id: &str,
    token: &str,
    caller: DecisionSubmitCaller,
) -> Result<DecisionRequest, orbit_core::OrbitError> {
    workspace
        .runtime
        .submit_decision_via_channel(DecisionSubmitParams {
            channel: "email".into(),
            request_id: request_id.into(),
            answer_id: "approve_anyway".into(),
            note: Some("Not a duplicate: the deliverables differ.".into()),
            token: token.into(),
            sender: Some("operator@example.com".into()),
            auth_results: Some(
                "mx.example.com; dkim=pass header.d=example.com; spf=pass smtp.mailfrom=example.com; dmarc=pass"
                    .into(),
            ),
            raw_message: None,
            caller,
        })
}

fn a_registered_channel_submission_releases_the_pilot_hold_as_the_human() {
    let (workspace, task, request) = channel_held("duplicate_of", "pilot_duplicate");
    let token = channel_token(&workspace, &request.id);
    let answered = submit_pilot_answer(
        &workspace,
        &request.id,
        &token,
        DecisionSubmitCaller::PluginCallback {
            plugin: "decision-mail".into(),
        },
    )
    .unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
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
    let (workspace, task, request) = channel_held("duplicate_of", "pilot_duplicate");
    let token = channel_token(&workspace, &request.id);
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
        DecisionRequestState::Open,
        "the request is still open for the human"
    );
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    decision_channel_pilot_hold_scenarios,
    an_enabled_pilot_hold_raises_one_open_decision_request,
    an_evaluated_answer_on_a_finding_hold_is_refused_and_a_valid_answer_releases_it,
    an_evaluated_answer_still_listed_on_an_older_request_is_refused,
    a_disabled_pilot_hold_writes_no_decision_request,
    a_changed_pilot_hold_supersedes_its_request_without_a_new_request,
    a_registered_channel_submission_releases_the_pilot_hold_as_the_human,
    agent_and_unregistered_submissions_leave_the_pilot_hold_held,
    resolving_one_typed_hold_raises_a_fresh_request_for_the_next_hold
);

/// The stale-hold detail the drain records for `task`.
fn stale_detail(workspace: &Workspace, task: &Task) -> String {
    let output = workspace.action("list_backlog_tasks", json!({"task_ids": [task.id]}));
    output["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == task.id)
        .and_then(|entry| entry["detail"].as_str())
        .unwrap_or_else(|| panic!("no exclusion for {}: {output}", task.id))
        .to_string()
}

fn set_tags(workspace: &Workspace, task: &Task, tags: &[&str]) {
    hu_by(
        workspace,
        task,
        TaskUpdateParams {
            tags: Some(tags.iter().map(|tag| (*tag).to_string()).collect()),
            ..Default::default()
        },
        "human:operator",
    );
}

/// A backlog task whose description an operator rewrote after its pilot.
fn stale_backlog_task(workspace: &Workspace) -> Task {
    let task = backlog_task(workspace);
    assess(workspace, &task, None);
    hu(
        workspace,
        &task,
        TaskUpdateParams {
            description: Some("Repair the README fixture, as settled by the operator.".into()),
            ..Default::default()
        },
    );
    assert_admission(workspace, &task, Some("pilot_assessment_stale"));
    task
}

/// [ORB-21238] An operator's `no-pilot` skip admits a stale task through the
/// drain, wave classification, readiness and ship admission; removing it
/// restores the hold, and the hold names both remedies.
fn no_pilot_admits_a_stale_assessment_until_the_tag_is_removed() {
    let workspace = Workspace::new();
    let task = stale_backlog_task(&workspace);
    let detail = stale_detail(&workspace, &task);
    assert!(
        detail.contains(&format!("orbit task repilot {}", task.id)),
        "{detail}"
    );
    assert!(
        detail.contains(&format!("orbit task update {} --tag no-pilot", task.id)),
        "{detail}"
    );
    set_tags(&workspace, &task, &["no-pilot"]);
    assert_admission(&workspace, &task, None);
    set_tags(&workspace, &task, &[]);
    assert_admission(&workspace, &task, Some("pilot_assessment_stale"));
    set_tags(&workspace, &task, &["no-pilot"]);
    assert_admission(&workspace, &task, None);
    assert_workflow_admitted(&workspace, &task);
}

/// [ORB-21238] `no-pilot` bypasses stale assessments but cannot release a
/// current owner-side operational hold without an evidenced operator decision.
fn no_pilot_keeps_a_host_operational_hold_until_an_operator_decision() {
    let workspace = Workspace::new();
    let task = task_at_status(&workspace, TaskStatus::Proposed);
    assess_disposition(&workspace, &task, None, "host_operational");
    to_backlog(&workspace, &task);
    assert_admission(&workspace, &task, Some("host_operational_handoff"));

    set_tags(&workspace, &task, &["no-pilot"]);
    assert_admission(&workspace, &task, Some("host_operational_handoff"));
    assert_eq!(event_count(&workspace, &task, "host_operational_held"), 1);

    hu(
        &workspace,
        &task,
        TaskUpdateParams {
            comment: Some(
                "task-pilot-admission: approve-anyway\nThe operator reviewed the owner-side evidence and accepts admission."
                    .into(),
            ),
            ..Default::default()
        },
    );
    assert_admission(&workspace, &task, None);
    assert_event_by(&workspace, &task, "host_operational_resolved");
}

/// [ORB-21238] Only an operator adds `no-pilot` to an assessed task. The
/// agent's write is refused whole, and the operator's records who skipped
/// and the requirements it dispatches without assessment.
fn only_an_operator_skips_an_assessed_pilot_and_the_skip_is_audited() {
    let workspace = Workspace::new();
    let task = stale_backlog_task(&workspace);
    let refused = workspace
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({"id": task.id, "model": "codex", "tags": ["no-pilot"], "priority": "high"}),
        )
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("only an operator can add `no-pilot`"),
        "{refused}"
    );
    let stored = got(&workspace, &task);
    assert!(stored.tags.is_empty(), "the refused write must not apply");
    assert_eq!(stored.priority, task.priority);
    assert_admission(&workspace, &task, Some("pilot_assessment_stale"));

    set_tags(&workspace, &task, &["no-pilot"]);
    let history = hist(&workspace, &task);
    let skips = history
        .iter()
        .filter(|entry| entry.event == "pilot_skipped")
        .collect::<Vec<_>>();
    assert_eq!(skips.len(), 1, "{history:?}");
    assert_eq!(skips[0].by, "human:operator");
    let note: serde_json::Value = serde_json::from_str(skips[0].note.as_deref().unwrap()).unwrap();
    let comments = task_comments(&workspace, &task);
    let (operation_id, audit) = comments
        .iter()
        .rev()
        .find_map(|comment| {
            let receipt = comment.receipt()?;
            Some((
                receipt.operation_id.to_string(),
                super::pilot_audit(&workspace.runtime, &task.id, comment),
            ))
        })
        .unwrap();
    assert_eq!(note["pilot_operation_id"], operation_id, "{note}");
    assert_eq!(
        note["assessed_requirements"], audit["assessed_requirements"],
        "{note}"
    );
    let digest = note["requirements_digest"].as_str().unwrap();
    assert_eq!(digest.len(), 64, "{note}");
    assert_ne!(
        json!(digest),
        audit["assessed_requirements"],
        "the skip covers the edited requirements"
    );

    // Keeping the tag is no new skip, and a never-piloted task stays open to
    // every writer, as filing with the tag is.
    tool_update(
        &workspace,
        json!({"id": task.id, "model": "codex", "tags": ["no-pilot", "docs"]}),
    );
    let fresh = backlog_task(&workspace);
    tool_update(
        &workspace,
        json!({"id": fresh.id, "model": "codex", "tags": ["no-pilot"]}),
    );
    for id in [&task.id, &fresh.id] {
        let skips = workspace
            .runtime
            .get_task_history(id)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.event == "pilot_skipped")
            .count();
        assert_eq!(skips, usize::from(*id == task.id));
    }
}

/// [ORB-21238] `no-pilot` lifts only pilot state: dependencies, context
/// locks and readiness gaps still keep the task waiting.
fn no_pilot_keeps_every_other_admission_rule() {
    let workspace = Workspace::new();
    let task = stale_backlog_task(&workspace);
    set_tags(&workspace, &task, &["no-pilot"]);
    let readiness_reason = |task: &Task| {
        let readiness = workspace
            .runtime
            .workspace_auto_readiness(&[], None, 50, &[])
            .unwrap();
        let entry = readiness["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["task_id"] == task.id)
            .cloned()
            .unwrap_or_else(|| panic!("task missing from readiness: {readiness}"));
        assert_eq!(entry["eligible"], false, "{entry}");
        entry["reason"].as_str().unwrap_or_default().to_string()
    };
    let unready = |task: &Task| {
        let output = workspace.action("list_backlog_tasks", json!({}));
        assert!(
            !output["task_ids"]
                .as_array()
                .unwrap()
                .contains(&json!(task.id)),
            "{output}"
        );
    };

    let blocker = add_fixture_task(
        &workspace,
        "Prerequisite",
        "Land first.",
        "The prerequisite is observable.",
        TaskStatus::Backlog,
        TaskComplexity::Low,
        "file:docs/other.md",
    );
    set_deps(&workspace, &task, vec![blocker.id.to_string()]);
    unready(&task);
    assert_eq!(readiness_reason(&task), "unmet_dependency");
    set_deps(&workspace, &task, Vec::new());
    let admitted = workspace.action("list_backlog_tasks", json!({}));
    assert!(
        admitted["task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(task.id)),
        "{admitted}"
    );

    add_fixture_task(
        &workspace,
        "Holds the README lock",
        "Running work on the same file.",
        "The holder is observable.",
        TaskStatus::InProgress,
        TaskComplexity::Low,
        "file:README.md",
    );
    unready(&task);
    assert_eq!(readiness_reason(&task), "context_lock_conflict");

    let unassessed = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Unassessed skip".into(),
            description: "Filed without a size.".into(),
            acceptance_criteria: vec!["The repair is observable.".into()],
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Unassessed,
            tags: vec!["no-pilot".into()],
            context_files: vec!["file:docs/unassessed.md".into()],
            ..Default::default()
        })
        .unwrap();
    unready(&unassessed);
    readiness_reason(&unassessed);
}

crate::scenario_table!(
    host_operational_holds_human_approved_work_until_a_current_operator_decision_scenarios,
    host_operational_holds_human_approved_work_until_a_current_operator_decision,
    host_operational_material_and_newer_dispositions_control_admission,
    legacy_host_operational_receipt_is_backfilled_once_and_tracks_its_material,
    legacy_note_only_host_operational_handoff_is_recovered_and_not_recorded_again,
    latest_pilot_finding_holds_backlog_until_a_clear_assessment,
    human_pilot_decisions_release_only_the_current_assessment
);

crate::scenario_table!(
    operator_validation_holds_human_approved_work_and_requires_an_evidenced_decision_scenarios,
    operator_validation_holds_human_approved_work_and_requires_an_evidenced_decision,
    repairable_tool_findings_and_expected_denials_remain_advisory,
    owner_sign_off_criterion_is_flagged_as_operator_only,
    changed_criteria_make_the_pilot_validation_assessment_stale,
    legacy_operator_warning_is_backfilled_once_at_admission_and_stales_on_edit,
    operator_validation_hold_is_retained_when_warnings_exceed_cap,
    operator_tool_named_as_the_change_subject_does_not_hold,
    operator_tool_the_criterion_must_invoke_still_holds,
    unclassified_or_malformed_operator_tool_classification_still_holds,
    recheck_after_subject_to_invoke_criteria_edit_still_holds
);

crate::scenario_table!(
    an_override_comment_decides_only_when_written_before_the_store_recorded_typed_decisions_scenarios,
    an_override_comment_decides_only_when_written_before_the_store_recorded_typed_decisions,
    an_incomplete_boundary_counts_from_its_modification_time_until_a_writable_open_repairs_it
);

crate::scenario_table!(
    no_pilot_skip_scenarios,
    no_pilot_admits_a_stale_assessment_until_the_tag_is_removed,
    no_pilot_keeps_a_host_operational_hold_until_an_operator_decision,
    only_an_operator_skips_an_assessed_pilot_and_the_skip_is_audited,
    no_pilot_keeps_every_other_admission_rule
);
