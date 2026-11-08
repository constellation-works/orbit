//! Pilot-owned OS routing and crew redraws are certified by the atomic write,
//! including when approval resumes from that write's durable receipt.

use orbit_types::task::HostOs;

use super::*;

fn replay_does_not_certify_operator_edits_as_pilot_output() {
    for (edit, expected_replay) in [
        (
            TaskUpdateParams {
                tags: Some(vec!["os:macos".into(), "operator-tag".into()]),
                ..Default::default()
            },
            json!({
                "outcome": "superseded", "reason": "tags_changed", "decision": null,
                "classification": null, "decision_reason": null, "approved": null,
            }),
        ),
        (
            TaskUpdateParams {
                crew: Some(Some("grok".into())),
                ..Default::default()
            },
            json!({
                "outcome": "already_applied", "reason": null, "decision": "withhold",
                "classification": "changed_since_pilot", "decision_reason": "crew_changed",
                "approved": false,
            }),
        ),
        (
            TaskUpdateParams {
                crew: Some(Some("fixture".into())),
                ..Default::default()
            },
            json!({
                "outcome": "already_applied", "reason": null, "decision": "withhold",
                "classification": "changed_since_pilot", "decision_reason": "crew_changed",
                "approved": false,
            }),
        ),
        (
            TaskUpdateParams {
                description: Some("A different task meaning.".into()),
                ..Default::default()
            },
            json!({
                "outcome": "superseded", "reason": "material_changed", "decision": null,
                "classification": null, "decision_reason": null, "approved": null,
            }),
        ),
    ] {
        let workspace = Workspace::new();
        let task = workspace.task(
            "native validation",
            &[],
            &["file:README.md"],
            TaskComplexity::Low,
        );
        let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
        let pilot = workspace.pilot_child(&drain, &json!([task.id]));
        let prepared = workspace
            .action(
                "prepare_task_pilot",
                json!({
                    "task_ids": [task.id], "workspace_path": workspace.repo, "base_branch": "main",
                }),
            )
            .unwrap();
        let assessment = assessment(&task);
        let mut input = json!({
            "run_id": pilot, "workspace_path": workspace.repo, "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": [task.id], "tasks": [assessment]}],
        });
        let written = workspace
            .action("apply_task_pilot_results", input.clone())
            .unwrap();
        assert_eq!(written["applied_count"], 1, "{written}");
        workspace
            .runtime
            .update_task_as_human(&task.id, edit, "human:fixture".into())
            .unwrap();
        input["promotion_authorized"] = json!(true);
        input["drain_promotion"] = json!({"run_id": drain});
        let replay = workspace.action("apply_task_pilot_results", input).unwrap();
        // Pin both the replay outcome and whether drain approval was reached
        // so each operator edit retains its distinct replay semantics.
        let decision = &replay["drain_approval"][0];
        assert_eq!(
            json!({
                "outcome": replay["task_outcomes"][0]["outcome"],
                "reason": replay["task_outcomes"][0]["reason"],
                "decision": decision["decision"],
                "classification": decision["classification"],
                "decision_reason": decision["evidence"]["reason"],
                "approved": decision["approved"],
            }),
            expected_replay,
            "{replay}"
        );
        assert_eq!(workspace.status(&task), TaskStatus::Proposed);
        assert!(
            workspace
                .runtime
                .get_task_history(&task.id)
                .unwrap()
                .iter()
                .all(|entry| entry.event != "proposal_approved")
        );
        if expected_replay["classification"] == "changed_since_pilot" {
            // The drain-held stale task is queued for a fresh pilot on the
            // next pass instead of holding for an operator repilot.
            workspace.finish(&pilot, JobRunState::Success);
            let selection = workspace.select(&drain);
            assert!(
                selection["task_ids"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(task.id)),
                "{selection}"
            );
        }
    }
}

fn pilot_routing_and_crew_redraw_are_approved_once_and_preserve_host_holds() {
    for (redraw, resume_write) in [(false, false), (true, false), (true, true)] {
        let mut workspace = Workspace::with_config(
            "medium_complexity_crews = [\"sol\"]\n\
             [workflow.task_pilot_freshness]\n\
             material_fields = [\"title\", \"description\", \"criteria\", \"plan\", \"selectors\", \"tags\", \"crew\", \"complexity\", \"relations\", \"dependencies\"]\n",
        );
        workspace.runtime = workspace.runtime.clone().with_host_os(Some(HostOs::Macos));
        let task = workspace.task(
            "native validation",
            &[],
            &["file:README.md"],
            TaskComplexity::Low,
        );
        let task = workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    acceptance_criteria: Some(vec![
                        "A real macOS sandbox-exec launch succeeds.".into(),
                    ]),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
        let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
        let pilot = workspace.pilot_child(&drain, &json!([task.id]));
        let prepared = workspace
            .action(
                "prepare_task_pilot",
                json!({
                    "task_ids": [task.id], "workspace_path": workspace.repo, "base_branch": "main",
                }),
            )
            .unwrap();
        let mut assessment = assessment(&task);
        assessment["required_os"] = json!([{
            "criterion": 1, "os": "macos", "evidence": "The criterion needs a native sandbox-exec launch.",
        }]);
        if redraw {
            assessment["recommended_complexity"] = json!("medium");
        }
        let mut input = json!({
            "run_id": pilot, "workspace_path": workspace.repo, "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": [task.id], "tasks": [assessment]}],
        });
        if resume_write {
            let written = workspace
                .action("apply_task_pilot_results", input.clone())
                .unwrap();
            assert_eq!(written["applied_count"], 1, "{written}");
            assert_eq!(workspace.status(&task), TaskStatus::Proposed);
        }
        input["promotion_authorized"] = json!(true);
        input["drain_promotion"] = json!({"run_id": drain});
        let applied = workspace
            .action("apply_task_pilot_results", input.clone())
            .unwrap();
        let decision = &applied["drain_approval"][0];
        assert_eq!(decision["decision"], "promote", "{applied}");
        assert_eq!(decision["approved"], true, "{applied}");
        let routed = workspace.runtime.get_task(&task.id).unwrap();
        assert_eq!(routed.status, TaskStatus::Backlog);
        assert_eq!(routed.tags, ["os:macos"]);
        assert_eq!(
            routed.crew.as_deref(),
            Some(if redraw { "sol" } else { "fixture" })
        );
        assert_eq!(
            routed
                .crew_source
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some(if redraw { "pool:medium" } else { "default" })
        );
        let replay = workspace.action("apply_task_pilot_results", input).unwrap();
        assert_eq!(
            replay["drain_approval"][0]["classification"], "not_proposed",
            "{replay}"
        );
        let history = workspace.runtime.get_task_history(&task.id).unwrap();
        assert_eq!(
            history
                .iter()
                .filter(|entry| entry.event == "proposal_approved")
                .count(),
            1,
            "{history:?}"
        );
        assert_eq!(
            history
                .iter()
                .filter(|entry| entry.event == "task_pilot_applied")
                .count(),
            1,
            "{history:?}"
        );
        assert_eq!(
            history
                .iter()
                .filter(|entry| entry.event == "crew_redrawn")
                .count(),
            usize::from(redraw)
        );
        workspace.finish(&pilot, JobRunState::Success);

        let exclusion = |workspace: &Workspace| {
            let selected = workspace
                .action("list_backlog_tasks", json!({"task_ids": [task.id]}))
                .unwrap();
            selected["excluded"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"] == task.id)
                .map(|entry| entry["reason"].as_str().unwrap().to_string())
        };
        assert_eq!(exclusion(&workspace), None);
        workspace.runtime = workspace.runtime.clone().with_host_os(Some(HostOs::Linux));
        assert_eq!(exclusion(&workspace).as_deref(), Some("host_os_mismatch"));
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    tags: Some(vec![]),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
        assert_eq!(exclusion(&workspace).as_deref(), Some("native_os_required"));
    }
}

fn a_cross_workspace_dependency_applies_and_approves() {
    let workspace = Workspace::new();
    let foreign = workspace.foreign_task("foreign prerequisite", TaskStatus::Done);
    // The foreign task is registered, not local to this workspace.
    assert!(workspace.runtime.get_task(&foreign.id).is_err());
    assert_eq!(
        workspace
            .runtime
            .get_dependency_task(&foreign.id)
            .unwrap()
            .map(|task| task.status),
        Some(TaskStatus::Done)
    );
    let task = workspace.task(
        "cross workspace dependent",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
    );
    workspace.edit(
        &task,
        TaskUpdateParams {
            dependencies: Some(vec![foreign.id.clone()]),
            ..Default::default()
        },
    );
    let sibling = workspace.task(
        "local sibling",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
    );
    let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    let pilot = workspace.pilot_child(&drain, &json!([task.id, sibling.id]));
    // The sibling names the same foreign task only in pilot prose: its
    // completion, read through the registry, must not hold promotion.
    let mut sibling_assessment = assessment(&sibling);
    sibling_assessment["blocked_by"] = json!([format!("Await {}", foreign.id)]);
    let applied = workspace
        .pilot(&drain, &pilot, vec![assessment(&task), sibling_assessment])
        .unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(applied["applied_count"], 2, "{applied}");
    assert_eq!(workspace.status(&task), TaskStatus::Backlog);
    assert_eq!(workspace.status(&sibling), TaskStatus::Backlog);
}

fn a_missing_dependency_is_partial_and_held_not_retried() {
    let workspace = Workspace::new();
    let dependency = workspace.task(
        "deleted dependency",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
    );
    let bad = workspace.task(
        "missing dependency",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
    );
    workspace.edit(
        &bad,
        TaskUpdateParams {
            dependencies: Some(vec![dependency.id.clone()]),
            ..Default::default()
        },
    );
    let sibling = workspace.task(
        "applied sibling",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
    );
    let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    let pilot = workspace.pilot_child(&drain, &json!([bad.id, sibling.id]));
    let assessments = vec![assessment(&bad), assessment(&sibling)];
    let task_ids = assessments
        .iter()
        .map(|assessment| assessment["task_id"].clone())
        .collect::<Vec<_>>();
    let prepared = workspace
        .action(
            "prepare_task_pilot",
            json!({"task_ids": task_ids, "workspace_path": workspace.repo, "base_branch": "main"}),
        )
        .unwrap();
    // The dependency exists nowhere by the time approval runs.
    workspace.runtime.delete_task(&dependency.id).unwrap();
    let applied = workspace
        .action(
            "apply_task_pilot_results",
            json!({
                "run_id": pilot, "workspace_path": workspace.repo, "prepared": prepared,
                "results": [{"partition_index": 0, "task_ids": task_ids, "tasks": assessments}],
                "promotion_authorized": true, "drain_promotion": {"run_id": drain},
            }),
        )
        .unwrap();
    workspace.finish(&pilot, JobRunState::Success);
    // The sibling's applied result stands; the bad task is reported alone.
    assert_eq!(applied["applied_count"], 1, "{applied}");
    assert_eq!(applied["unresolved_count"], 1, "{applied}");
    assert_eq!(applied["status"], "failed", "{applied}");
    assert_eq!(workspace.status(&sibling), TaskStatus::Backlog);
    assert_eq!(workspace.status(&bad), TaskStatus::Proposed);
    let bad_outcome = applied["task_outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|outcome| outcome["task_id"] == bad.id)
        .unwrap()
        .clone();
    assert_eq!(bad_outcome["outcome"], "apply_failed", "{bad_outcome}");
    // The guard does not fail the sibling's applied result.
    let guard = workspace
        .action(
            "pipeline_success_guard",
            json!({"context": "task-pilot partition apply", "result": applied}),
        )
        .unwrap();
    assert_eq!(guard["succeeded"], true, "{guard}");
    // Recording the finished pilot holds the bad task for the operator
    // instead of selecting it again on the next drain pass.
    workspace.record(&drain);
    let selection = workspace.select(&drain);
    assert_eq!(
        held_reason(&selection, &bad),
        Some("pilot_unresolved"),
        "{selection}"
    );
    assert!(
        !selection["task_ids"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id == &bad.id),
        "{selection}"
    );
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    replay_does_not_certify_operator_edits_as_pilot_output_scenarios,
    replay_does_not_certify_operator_edits_as_pilot_output,
    pilot_routing_and_crew_redraw_are_approved_once_and_preserve_host_holds,
    a_cross_workspace_dependency_applies_and_approves,
    a_missing_dependency_is_partial_and_held_not_retried
);
