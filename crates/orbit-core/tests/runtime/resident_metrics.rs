//! Resident metrics at the tool boundary: a human write that undoes a
//! resident action recorded in a checkpoint is counted once as a reversal
//! of that action within the reversal window, and an interactive baseline
//! reports the measures it cannot derive as `not_derivable`, never zero.
//! A baseline whose references live in another workspace reads them through
//! their registered owner instead of failing `task_not_found` [ORB-21642].
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::resident::{
    ResidentChannelCaller, ResidentControlAction, ResidentControlParams,
};
use orbit_core::application::task::{AgentDecisionRaise, TaskAddParams, TaskUpdateParams};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{
    DecisionAnswer, DecisionBinding, DecisionGate, DecisionRequest, DecisionRiskTier, TaskArtifact,
    TaskComment, TaskHistoryEntry, TaskRelation, TaskRelationType, TaskStatus,
};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::ResidentLiveCycle;
use serde_json::{Value, json};

const CONFIG: &str = "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n[crews.dk]\nkind = \"human\"\ndescription = \"Daniel's own work\"\n";

fn runtime_at(root: &Path) -> OrbitRuntime {
    let global = root.join("home/.orbit");
    let orbit = root.join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&orbit).unwrap();
    std::fs::write(orbit.join("config.toml"), CONFIG).unwrap();
    OrbitRuntime::from_roots(&global, &orbit).unwrap()
}

fn operator() -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: [McpCapability::Operator].into(),
        ..Default::default()
    }
}

fn call(runtime: &OrbitRuntime, tool: &str, input: Value) -> Result<Value, String> {
    runtime
        .execute_tool_command_dispatch_with_session_context(
            tool,
            input,
            None,
            None,
            ToolEntryPoint::Mcp,
            operator(),
        )
        .map(|outcome| outcome.value)
        .map_err(|error| error.to_string())
}

fn metrics(runtime: &OrbitRuntime, input: Value) -> Value {
    call(runtime, "orbit.resident.metrics", input).unwrap()
}

fn mandate_with_watched(runtime: &OrbitRuntime) -> (String, String) {
    let workspace = runtime.workspace_id().unwrap();
    let watched = runtime
        .add_task(TaskAddParams {
            title: "leaf slice".into(),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let id = runtime
        .add_task(TaskAddParams {
            title: "[orchestration]: scribe v2 - sol".into(),
            tags: vec!["resident".into()],
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let body = json!({
        "schema": "resident.mandate/v1",
        "goal": "Deliver the remaining scribe v2 slices",
        "exit_criteria": ["Every scribe-v2 task is done or rejected"],
        "workspaces": [workspace],
        "watch": {"tasks": [watched]},
        "crew": "sol",
        "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120}
    });
    runtime
        .update_task_as_human(
            &id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text(
                    "resident/mandate.json",
                    body.to_string(),
                )],
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();
    (id, watched)
}

fn human_write_undoes_checkpoint_action_once() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let (id, watched) = mandate_with_watched(&runtime);

    // The resident promoted the watched task; the checkpoint records it.
    runtime
        .update_task_as_human(
            &watched,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();
    let live = ResidentLiveCycle {
        cycle: "rcy-live".into(),
        run_id: "jrun-live".into(),
        expires_at: Utc::now() + Duration::hours(1),
    };
    runtime
        .set_resident_live_cycle(&id, None, Some(&live))
        .unwrap();
    let key = format!("resident:{id}:rcy-live:1");
    runtime
        .append_resident_checkpoint(
            &id,
            json!({"kind": "resident.checkpoint", "schema": 1, "mandate": id,
                "cycle": "rcy-live", "run_id": "jrun-live", "crew": "sol",
                "recovery_of": null,
                "wake": {"reasons": ["heartbeat"], "digest": format!("sha256:{}", "0".repeat(64))},
                "authority": {"rights": ["promote"], "observe_only": false, "narrowed_by": null},
                "actions": [{"key": key, "kind": "promote", "subject": watched, "result": "ok", "ref": null}],
                "decisions": [{"id": "d1", "summary": "Promote the leaf slice first",
                               "class": "routine", "reversible": true, "p_wrong": "low",
                               "rework_units": 1, "precedent": null}],
                "requests": [], "costs": [], "notes": "",
                "next_wake": {"reason": "heartbeat", "not_before": "2030-01-01T00:00:00Z"},
                "continuity": "off", "outcome": "progress", "probe": false}),
            Some("codex".into()),
            None,
        )
        .unwrap();
    runtime
        .set_resident_live_cycle(&id, Some("rcy-live"), None)
        .unwrap();

    let before = metrics(&runtime, json!({"mandate": id}));
    assert_eq!(before["mandates"][0]["reversals"]["total"], 2, "{before}");
    assert_eq!(
        before["mandates"][0]["reversals"]["reversed"], 0,
        "{before}"
    );
    assert_eq!(before["mandates"][0]["reversals"]["rate"], 0.0, "{before}");

    // Two human writes undo the same recorded action: a status write back
    // to `proposed`, and a relay naming the action key. Counted once.
    runtime
        .update_task_as_human(
            &watched,
            TaskUpdateParams {
                status: Some(TaskStatus::Proposed),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();
    let answer = json!({"kind": "resident.answer", "schema": 1, "mandate": id,
        "request_id": null, "reverses": key,
        "quote": "undo that promotion",
        "source": {"human": "daniel", "via": "cli", "session": null,
                   "said_at": Utc::now().to_rfc3339()},
        "relayed_by": null});
    runtime
        .update_task_as_human(
            &id,
            TaskUpdateParams {
                comment: Some(answer.to_string()),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();

    let after = metrics(&runtime, json!({"mandate": id}));
    let reversals = &after["mandates"][0]["reversals"];
    assert_eq!(reversals["total"], 2, "{after}");
    assert_eq!(
        reversals["reversed"], 1,
        "one reversal, counted once: {after}"
    );
    assert_eq!(reversals["rate"], 0.5, "{after}");
    assert_eq!(
        after["mandates"][0]["recovery"]["repeated_actions"], 0,
        "{after}"
    );
}

fn baseline_marks_underivable_measures_not_derivable() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let baseline = runtime
        .add_task(TaskAddParams {
            title: "[orchestration]: interactive scribe v2".into(),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    runtime
        .update_task_as_human(
            &baseline,
            TaskUpdateParams {
                comment: Some("steering: hold the schema slice until Monday".into()),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();

    let report = metrics(&runtime, json!({"baseline": baseline}));
    let baseline_report = &report["baseline"];
    assert_eq!(baseline_report["task_id"], baseline, "{report}");
    assert_eq!(baseline_report["reversals"], "not_derivable", "{report}");
    assert_eq!(
        baseline_report["reversal_rate"], "not_derivable",
        "{report}"
    );
    assert_eq!(baseline_report["recovery"], "not_derivable", "{report}");
    assert_eq!(
        baseline_report["recovery_cost"], "not_derivable",
        "{report}"
    );
    // Interventions are derivable from the interactive task's history.
    assert!(
        baseline_report["interventions"]["count"]
            .as_u64()
            .unwrap_or(0)
            >= 1,
        "{report}"
    );
    assert_eq!(baseline_report["escalations"]["count"], 0, "{report}");
}

fn closed_interactive_baseline_bounds_related_task_events_to_active_span() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let related = runtime
        .add_task(TaskAddParams {
            title: "interactive baseline related task".into(),
            ..Default::default()
        })
        .unwrap();
    let related_id = related.id.to_string();
    let baseline = runtime
        .add_task(TaskAddParams {
            title: "closed interactive baseline".into(),
            relations: vec![TaskRelation {
                relation_type: TaskRelationType::RelatedTo,
                target: related.id,
                regression: None,
            }],
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .unwrap();
    let baseline_id = baseline.id.to_string();

    // The related task records events on both sides of the baseline's active
    // span, plus matching events while the baseline is active.
    let before = baseline.created_at - Duration::seconds(1);
    append_comment(
        &runtime,
        &related_id,
        before,
        "human:daniel",
        "before baseline",
    );
    seed_agent_decision_request(
        &runtime,
        &related_id,
        before,
        before + Duration::days(3),
        "Before the baseline started?",
    );

    let inside = Utc::now();
    append_comment(
        &runtime,
        &related_id,
        inside,
        "human:daniel",
        "inside baseline span",
    );
    let inside_request = seed_agent_decision_request(
        &runtime,
        &related_id,
        inside,
        inside + Duration::days(3),
        "During the baseline?",
    );

    runtime
        .apply_task_automation_update(
            &baseline_id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .unwrap();
    let closed_at = runtime
        .get_task_history(&baseline_id)
        .unwrap()
        .into_iter()
        .rev()
        .find(|entry| entry.to_status == Some(TaskStatus::Review))
        .unwrap()
        .at;
    let after = Utc::now();
    assert!(
        after > closed_at,
        "post-close event must follow {closed_at}"
    );
    append_comment(
        &runtime,
        &related_id,
        after,
        "human:daniel",
        "after baseline review",
    );
    seed_agent_decision_request(
        &runtime,
        &related_id,
        after,
        after + Duration::days(3),
        "After the baseline closed?",
    );

    let report = call(
        &runtime,
        "orbit.resident.metrics",
        json!({"baseline": baseline_id, "since": "21d"}),
    )
    .unwrap();
    let baseline_report = &report["baseline"];
    assert_eq!(baseline_report["active_days"], 1.0, "{report}");
    assert_eq!(baseline_report["interventions"]["count"], 1, "{report}");
    assert_eq!(
        baseline_report["interventions"]["items"][0]["task_id"], related_id,
        "{report}"
    );
    assert_eq!(
        baseline_report["interventions"]["items"][0]["at"],
        json!(inside),
        "{report}"
    );
    assert_eq!(
        baseline_report["interventions"]["per_mandate_day"], 1.0,
        "{report}"
    );
    assert_eq!(baseline_report["escalations"]["count"], 1, "{report}");
    assert_eq!(
        baseline_report["escalations"]["requests"],
        json!([inside_request.id]),
        "{report}"
    );
    assert_eq!(
        baseline_report["escalations"]["per_mandate_day"], 1.0,
        "{report}"
    );
}

fn task_creation_is_not_an_intervention_but_later_status_changes_are() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let (id, watched) = mandate_with_watched(&runtime);

    let before = metrics(&runtime, json!({"mandate": id}));
    let interventions = &before["mandates"][0]["interventions"];
    assert_eq!(interventions["count"], 0, "{before}");
    assert_eq!(
        interventions["items"].as_array().unwrap().len(),
        0,
        "{before}"
    );

    runtime
        .update_task_as_human(
            &watched,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();

    let after = metrics(&runtime, json!({"mandate": id}));
    let items = after["mandates"][0]["interventions"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(items.len(), 1, "{after}");
    assert_eq!(items[0]["task_id"], watched, "{after}");
    assert_eq!(items[0]["kind"], "status_change", "{after}");
}

fn mandate_with_human_watched(runtime: &OrbitRuntime, leaf_title: &str) -> (String, String) {
    let workspace = runtime.workspace_id().unwrap();
    let watched = runtime
        .add_task(TaskAddParams {
            title: leaf_title.into(),
            crew: Some("dk".into()),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let id = runtime
        .add_task(TaskAddParams {
            title: format!("[orchestration]: scribe v2 - {leaf_title}"),
            tags: vec!["resident".into()],
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let body = json!({
        "schema": "resident.mandate/v1",
        "goal": "Deliver the remaining scribe v2 slices",
        "exit_criteria": ["Every scribe-v2 task is done or rejected"],
        "workspaces": [workspace],
        "watch": {"tasks": [watched]},
        "crew": "sol",
        "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120}
    });
    runtime
        .update_task_as_human(
            &id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text(
                    "resident/mandate.json",
                    body.to_string(),
                )],
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();
    (id, watched)
}

fn totals_divide_per_mandate_day_by_summed_active_days() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let (_id_a, watched_a) = mandate_with_human_watched(&runtime, "leaf alpha");
    let (_id_b, watched_b) = mandate_with_human_watched(&runtime, "leaf beta");

    // One human intervention per mandate: a steering comment on its watched task.
    for (watched, note) in [
        (watched_a.clone(), "steering: hold the alpha slice"),
        (watched_b.clone(), "steering: hold the beta slice"),
    ] {
        runtime
            .update_task_as_human(
                &watched,
                TaskUpdateParams {
                    comment: Some(note.into()),
                    ..Default::default()
                },
                "human:daniel".into(),
            )
            .unwrap();
    }

    // One resident escalation per mandate: an agent-raised decision on its
    // human-crew watched task, which the mandate's watch covers.
    for (index, watched) in [watched_a.clone(), watched_b.clone()].iter().enumerate() {
        runtime
            .raise_agent_decision_request(AgentDecisionRaise {
                task_id: watched.clone(),
                question: format!("Which slice ships next for totals {index}?"),
                options: vec![
                    orbit_types::task::AgentDecisionOptionInput {
                        label: "Keep the slice".into(),
                        consequence: "No change.".into(),
                        risk_tier: None,
                        recommended: false,
                    },
                    orbit_types::task::AgentDecisionOptionInput {
                        label: "Ship the slice".into(),
                        consequence: "The slice ships.".into(),
                        risk_tier: None,
                        recommended: true,
                    },
                ],
                topic: Some("totals/check".into()),
                filer: "codex".into(),
            })
            .unwrap();
    }

    let report = metrics(&runtime, json!({}));
    let mandates = report["mandates"].as_array().unwrap();
    assert_eq!(mandates.len(), 2, "{report}");
    for mandate in mandates {
        assert_eq!(mandate["escalations"]["count"], 1, "{report}");
        assert!(
            mandate["interventions"]["count"].as_u64().unwrap_or(0) >= 1,
            "each mandate records its steering comment: {report}"
        );
    }
    let summed_active_days: f64 = mandates
        .iter()
        .map(|mandate| mandate["active_days"].as_f64().unwrap())
        .sum();
    assert!(
        summed_active_days > 0.0,
        "mandates carry active days: {report}"
    );
    let window_days = report["window"]["days"].as_f64().unwrap();
    assert!(window_days > 0.0, "{report}");
    // The calendar window spans all mandates at once, so with two active
    // mandates it exceeds the summed mandate-days and separates the two
    // denominators.
    assert!(
        (window_days - summed_active_days).abs() > 1e-9,
        "window days ({window_days}) and summed mandate-days ({summed_active_days}) must differ to separate the rates: {report}"
    );
    let totals = &report["totals"];
    let total_interventions = totals["interventions"]["count"].as_u64().unwrap();
    let total_escalations = totals["escalations"]["count"].as_u64().unwrap();
    let summed_interventions: u64 = mandates
        .iter()
        .map(|mandate| mandate["interventions"]["count"].as_u64().unwrap())
        .sum();
    assert_eq!(total_interventions, summed_interventions, "{report}");
    assert_eq!(total_escalations, 2, "{report}");
    let expected_interventions_per_mandate_day = total_interventions as f64 / summed_active_days;
    let expected_escalations_per_mandate_day = total_escalations as f64 / summed_active_days;
    let expected_per_human_day = total_escalations as f64 / window_days;
    for (pointer, value, expected) in [
        (
            "totals interventions per_mandate_day",
            &totals["interventions"]["per_mandate_day"],
            expected_interventions_per_mandate_day,
        ),
        (
            "totals escalations per_mandate_day",
            &totals["escalations"]["per_mandate_day"],
            expected_escalations_per_mandate_day,
        ),
    ] {
        let actual = value.as_f64().unwrap();
        assert!(
            (actual - expected).abs() < 1e-9,
            "{pointer} is total count over summed mandate active days ({summed_active_days}): got {actual}, want {expected}: {report}"
        );
    }
    let actual_per_human_day = totals["escalations"]["per_human_day"].as_f64().unwrap();
    assert!(
        (actual_per_human_day - expected_per_human_day).abs() < 1e-9,
        "totals escalations per_human_day stays total count over window days ({window_days}): got {actual_per_human_day}, want {expected_per_human_day}: {report}"
    );
}

fn invalid_and_day_windows_at_tool_boundary() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    for window in ["999999999999d", "99999999d", "-5d"] {
        let error = runtime
            .execute_tool_command_dispatch_with_session_context(
                "orbit.resident.metrics",
                json!({"since": window}),
                None,
                None,
                ToolEntryPoint::Mcp,
                operator(),
            )
            .unwrap_err();
        assert!(
            matches!(&error, orbit_common::OrbitError::InvalidInput(message) if message.contains(window)),
            "expected invalid input naming {window}, got {error:?}"
        );
    }
    let report = metrics(&runtime, json!({"since": "7days"}));
    assert_eq!(report["window"]["days"], 7.0);
}

/// [ORB-21642] An orchestration baseline lives in one workspace while its
/// work lives in owning workspaces. A related task registered in another
/// workspace on this machine used to fail the whole report with
/// `task_not_found`; it is now read through its registered owner, and its
/// human writes and agent-raised requests count.
fn baseline_reads_references_in_another_workspace() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let foreign_orbit = root.path().join("foreign/.orbit");
    std::fs::create_dir_all(&foreign_orbit).unwrap();
    std::fs::write(foreign_orbit.join("config.toml"), CONFIG).unwrap();
    let foreign =
        OrbitRuntime::from_roots(&root.path().join("home/.orbit"), &foreign_orbit).unwrap();
    let foreign_id = foreign
        .add_task(TaskAddParams {
            title: "owning-workspace slice".into(),
            crew: Some("dk".into()),
            ..Default::default()
        })
        .unwrap()
        .id;
    assert!(
        runtime.get_task(&foreign_id.to_string()).is_err(),
        "the fixture task must live outside the baseline's workspace"
    );

    let baseline = runtime
        .add_task(TaskAddParams {
            title: "[orchestration]: interactive cross-workspace".into(),
            relations: vec![TaskRelation {
                relation_type: TaskRelationType::RelatedTo,
                target: foreign_id.clone(),
                regression: None,
            }],
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();

    // A human status write and an agent-raised decision request, both in
    // the owning workspace and after the baseline began.
    foreign
        .update_task_as_human(
            &foreign_id.to_string(),
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();
    foreign
        .raise_agent_decision_request(AgentDecisionRaise {
            task_id: foreign_id.to_string(),
            question: "Which slice ships next across workspaces?".into(),
            options: vec![
                orbit_types::task::AgentDecisionOptionInput {
                    label: "Keep the slice".into(),
                    consequence: "No change.".into(),
                    risk_tier: None,
                    recommended: false,
                },
                orbit_types::task::AgentDecisionOptionInput {
                    label: "Ship the slice".into(),
                    consequence: "The slice ships.".into(),
                    risk_tier: None,
                    recommended: true,
                },
            ],
            topic: Some("baseline/cross-workspace".into()),
            filer: "codex".into(),
        })
        .unwrap();

    let report = call(
        &runtime,
        "orbit.resident.metrics",
        json!({"baseline": baseline}),
    )
    .expect("a baseline with a cross-workspace reference reports instead of task_not_found");
    let baseline_report = &report["baseline"];
    assert_eq!(baseline_report["task_id"], baseline, "{report}");
    let foreign_refs = baseline_report["references"]["foreign"].as_array().unwrap();
    assert_eq!(foreign_refs.len(), 1, "{report}");
    assert_eq!(
        foreign_refs[0]["task_id"],
        foreign_id.to_string(),
        "{report}"
    );
    assert_eq!(
        baseline_report["references"]["unreachable"],
        json!([]),
        "{report}"
    );
    let items = baseline_report["interventions"]["items"]
        .as_array()
        .unwrap();
    let foreign_key = foreign_id.to_string();
    assert!(
        items
            .iter()
            .any(|item| item["task_id"] == foreign_key.as_str() && item["kind"] == "status_change"),
        "the owning workspace's human status write counts: {report}"
    );
    assert_eq!(
        baseline_report["escalations"]["count"], 1,
        "the owning workspace's agent-raised request counts: {report}"
    );
}

fn append_history(runtime: &OrbitRuntime, task_id: &str, entries: Vec<TaskHistoryEntry>) {
    runtime
        .apply_task_automation_update(
            task_id,
            TaskAutomationUpdate {
                append_history: entries,
                ..Default::default()
            },
        )
        .unwrap();
}

fn append_comment(
    runtime: &OrbitRuntime,
    task_id: &str,
    at: DateTime<Utc>,
    by: &str,
    message: &str,
) {
    runtime
        .apply_task_automation_update(
            task_id,
            TaskAutomationUpdate {
                append_comments: vec![TaskComment {
                    at,
                    by: by.into(),
                    message: message.into(),
                    operation_id: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
}

fn seed_cycle_grant(runtime: &OrbitRuntime, mandate_id: &str, at: DateTime<Utc>, cycle: &str) {
    let grant = json!({
        "cycle": cycle,
        "run_id": format!("jrun-{cycle}"),
        "rights": [],
        "workspaces": [runtime.workspace_id().unwrap()],
        "observe_only": true,
        "narrowed_by": null,
        "recovery_of": null,
        "digest": format!("sha256:{}", "0".repeat(64)),
        "reasons": ["first_cycle"],
    });
    append_history(
        runtime,
        mandate_id,
        vec![TaskHistoryEntry {
            at,
            by: "system".into(),
            event: "resident_cycle_granted".into(),
            note: Some(grant.to_string()),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: None,
        }],
    );
}

fn seed_stop(runtime: &OrbitRuntime, mandate_id: &str, at: DateTime<Utc>) {
    let control = json!({
        "action": "stop",
        "state": "ended",
        "reason": "completed",
        "by": "human:daniel",
    });
    append_history(
        runtime,
        mandate_id,
        vec![TaskHistoryEntry {
            at,
            by: "human:daniel".into(),
            event: "resident_control".into(),
            note: Some(control.to_string()),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: None,
        }],
    );
}

fn seed_agent_decision_request(
    runtime: &OrbitRuntime,
    task_id: &str,
    raised_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    question: &str,
) -> DecisionRequest {
    let task = runtime.get_task(task_id).unwrap();
    let mut request = DecisionRequest::new(
        task_id.to_string(),
        DecisionGate::AgentRequest,
        vec![
            DecisionAnswer {
                id: "opt-1".into(),
                label: "Keep".into(),
                consequence: "No change.".into(),
                risk_tier: DecisionRiskTier::Routine,
                option: None,
            },
            DecisionAnswer {
                id: "opt-2".into(),
                label: "Ship".into(),
                consequence: "Ships.".into(),
                risk_tier: DecisionRiskTier::Routine,
                option: None,
            },
        ],
        DecisionBinding::TaskRevision {
            revision: task.spec_digest(),
            status: task.status,
            candidate: None,
        },
        "agent_request".into(),
        raised_at,
    );
    request.recommended = Some("opt-2".into());
    request.summary = Some(question.into());
    request.expires_at = expires_at;
    let mut entry = request.history_entry("codex");
    entry.at = raised_at;
    append_history(runtime, task_id, vec![entry]);
    request
}

fn tag_without_mandate_artifact_is_excluded_from_mandates_and_totals() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let (real_id, watched) = mandate_with_watched(&runtime);
    runtime
        .update_task_as_human(
            &watched,
            TaskUpdateParams {
                comment: Some("steering: real mandate intervention".into()),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();

    // A bug task tagged `resident` without a `resident/mandate.json` artifact,
    // plus its own human comment that must not leak into totals.
    let tag_only = runtime
        .add_task(TaskAddParams {
            title: "resident bug task without mandate artifact".into(),
            tags: vec!["resident".into()],
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    runtime
        .update_task_as_human(
            &tag_only,
            TaskUpdateParams {
                comment: Some("unrelated bug note".into()),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();

    let report = metrics(&runtime, json!({}));
    let mandates = report["mandates"].as_array().unwrap();
    assert_eq!(mandates.len(), 1, "{report}");
    assert_eq!(mandates[0]["task_id"], real_id, "{report}");
    assert_eq!(report["totals"]["interventions"]["count"], 1, "{report}");
}

fn ended_mandate_bounds_active_days_by_its_end() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let now = Utc::now();

    // 2-day ended mandate inside a 21-day window: activated 10d ago, stopped 8d ago.
    let (two_day_id, _) = mandate_with_watched(&runtime);
    seed_cycle_grant(&runtime, &two_day_id, now - Duration::days(10), "rcy-2d");
    seed_stop(&runtime, &two_day_id, now - Duration::days(8));

    // 9-hour ended mandate inside a 21-day window: activated 10d ago, ended by
    // deadline 9h later.
    let workspace = runtime.workspace_id().unwrap();
    let short_id = runtime
        .add_task(TaskAddParams {
            title: "short 9h mandate".into(),
            tags: vec!["resident".into()],
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let deadline = now - Duration::days(10) + Duration::hours(9);
    let body = json!({
        "schema": "resident.mandate/v1",
        "goal": "Short mandate",
        "exit_criteria": ["Done"],
        "deadline": deadline,
        "workspaces": [workspace],
        "watch": {},
        "crew": "sol",
        "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120}
    });
    runtime
        .update_task_as_human(
            &short_id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text(
                    "resident/mandate.json",
                    body.to_string(),
                )],
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();
    seed_cycle_grant(&runtime, &short_id, now - Duration::days(10), "rcy-9h");

    // Assignment closed via Review 6d ago (activated 8d ago -> 2 active days).
    let (closed_id, _) = mandate_with_watched(&runtime);
    seed_cycle_grant(&runtime, &closed_id, now - Duration::days(8), "rcy-rev");
    runtime
        .apply_task_automation_update(
            &closed_id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                append_history: vec![TaskHistoryEntry {
                    at: now - Duration::days(6),
                    by: "system".into(),
                    event: "updated".into(),
                    note: None,
                    from_status: Some(TaskStatus::InProgress),
                    to_status: Some(TaskStatus::Review),
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();

    // Mandate that ended before the 21d window started (30d ago -> 25d ago):
    // must be omitted from the unfiltered 21d report.
    let (old_id, _) = mandate_with_watched(&runtime);
    seed_cycle_grant(&runtime, &old_id, now - Duration::days(30), "rcy-old");
    seed_stop(&runtime, &old_id, now - Duration::days(25));

    let report = metrics(&runtime, json!({"since": "21d"}));
    assert_eq!(report["window"]["days"], 21.0, "{report}");
    let mandates = report["mandates"].as_array().unwrap();
    assert_eq!(mandates.len(), 3, "{report}");
    let active_for = |id: &str| {
        mandates.iter().find(|m| m["task_id"] == id).unwrap()["active_days"]
            .as_f64()
            .unwrap()
    };
    assert_eq!(active_for(&two_day_id), 2.0, "{report}");
    assert_eq!(active_for(&short_id), 1.0, "{report}");
    assert_eq!(active_for(&closed_id), 2.0, "{report}");
}

fn interventions_and_escalations_outside_active_span_are_excluded() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let now = Utc::now();
    let (mandate_id, watched) = mandate_with_human_watched(&runtime, "span leaf");

    let activated = now - Duration::days(10);
    let ended = now - Duration::days(8);
    seed_cycle_grant(&runtime, &mandate_id, activated, "rcy-span");
    seed_stop(&runtime, &mandate_id, ended);

    // Pre-activation edge (before activation).
    let before = activated - Duration::hours(12);
    append_comment(
        &runtime,
        &watched,
        before,
        "human:daniel",
        "before activation",
    );
    seed_agent_decision_request(
        &runtime,
        &watched,
        before,
        before + Duration::days(3),
        "Before activation?",
    );

    // Inside active span.
    let inside = activated + Duration::hours(12);
    append_comment(&runtime, &watched, inside, "human:daniel", "inside span");
    let inside_req = seed_agent_decision_request(
        &runtime,
        &watched,
        inside,
        now + Duration::days(3),
        "Inside span?",
    );

    // Post-end edge (after mandate ended).
    let after = ended + Duration::hours(12);
    append_comment(&runtime, &watched, after, "human:daniel", "after end");
    seed_agent_decision_request(
        &runtime,
        &watched,
        after,
        after + Duration::days(3),
        "After end?",
    );

    // Also verify real-time stop edge via `control_resident`.
    let (live_id, live_watched) = mandate_with_human_watched(&runtime, "live stop");
    runtime
        .update_task_as_human(
            &live_watched,
            TaskUpdateParams {
                comment: Some("in live span".into()),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();
    runtime
        .control_resident(
            &ResidentChannelCaller::default(),
            ResidentControlParams {
                task_id: live_id.clone(),
                action: ResidentControlAction::Stop,
                reason: "done".into(),
                status_request: false,
                by_session: None,
            },
        )
        .unwrap();
    runtime
        .update_task_as_human(
            &live_watched,
            TaskUpdateParams {
                comment: Some("after control stop".into()),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();

    let report = metrics(&runtime, json!({"since": "21d"}));
    let mandates = report["mandates"].as_array().unwrap();
    let backdated = mandates
        .iter()
        .find(|m| m["task_id"] == mandate_id)
        .unwrap();
    assert_eq!(backdated["interventions"]["count"], 1, "{backdated}");
    assert_eq!(
        backdated["interventions"]["items"][0]["at"],
        json!(inside),
        "{backdated}"
    );
    assert_eq!(backdated["escalations"]["count"], 1, "{backdated}");
    assert_eq!(
        backdated["escalations"]["requests"],
        json!([inside_req.id]),
        "{backdated}"
    );

    let live = mandates.iter().find(|m| m["task_id"] == live_id).unwrap();
    assert_eq!(live["interventions"]["count"], 1, "{live}");
}

fn escalation_buckets_sum_to_count_and_per_human_day_uses_window_days() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let now = Utc::now();
    let workspace = runtime.workspace_id().unwrap();

    let mut leaves = Vec::new();
    for label in ["ans", "sup", "exp"] {
        leaves.push(
            runtime
                .add_task(TaskAddParams {
                    title: format!("escalation leaf {label}"),
                    crew: Some("dk".into()),
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string(),
        );
    }
    let mandate_id = runtime
        .add_task(TaskAddParams {
            title: "[orchestration]: escalation buckets".into(),
            tags: vec!["resident".into()],
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let body = json!({
        "schema": "resident.mandate/v1",
        "goal": "Check escalation buckets and divisors",
        "exit_criteria": ["All checked"],
        "workspaces": [workspace],
        "watch": {"tasks": leaves},
        "crew": "sol",
        "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120}
    });
    runtime
        .update_task_as_human(
            &mandate_id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text(
                    "resident/mandate.json",
                    body.to_string(),
                )],
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();
    // Mandate active for ~2 days (47h59m < 48h, so ceil() is deterministically 2.0) inside a 21-day window.
    seed_cycle_grant(
        &runtime,
        &mandate_id,
        now - Duration::days(2) + Duration::minutes(1),
        "rcy-bkt",
    );

    // 1. Answered (raised and answered through the real decision APIs).
    let ans_req = runtime
        .raise_agent_decision_request(AgentDecisionRaise {
            task_id: leaves[0].clone(),
            question: "Answered?".into(),
            options: vec![
                orbit_types::task::AgentDecisionOptionInput {
                    label: "Keep".into(),
                    consequence: "No change.".into(),
                    risk_tier: None,
                    recommended: false,
                },
                orbit_types::task::AgentDecisionOptionInput {
                    label: "Ship".into(),
                    consequence: "Ships.".into(),
                    risk_tier: None,
                    recommended: true,
                },
            ],
            topic: Some("buckets/answered".into()),
            filer: "codex".into(),
        })
        .unwrap()
        .request;
    runtime
        .answer_decision_request_as_human(
            &ans_req.id,
            &ans_req.answers[1].id,
            None,
            "human:daniel".into(),
            Some("cli".into()),
        )
        .unwrap();

    // 2. Superseded + Open (raising a second request on the same task supersedes the first).
    let _sup_req = seed_agent_decision_request(
        &runtime,
        &leaves[1],
        Utc::now(),
        now + Duration::days(3),
        "Superseded?",
    );
    let _open_req = seed_agent_decision_request(
        &runtime,
        &leaves[1],
        Utc::now(),
        now + Duration::days(3),
        "Open?",
    );

    // 3. Expired unanswered.
    let _exp_req = seed_agent_decision_request(
        &runtime,
        &leaves[2],
        Utc::now(),
        now - Duration::hours(1),
        "Expired?",
    );

    runtime
        .control_resident(
            &ResidentChannelCaller::default(),
            ResidentControlParams {
                task_id: mandate_id,
                action: ResidentControlAction::Stop,
                reason: "done".into(),
                status_request: false,
                by_session: None,
            },
        )
        .unwrap();

    let report = metrics(&runtime, json!({"since": "21d"}));
    let window_days = report["window"]["days"].as_f64().unwrap();
    assert_eq!(window_days, 21.0, "{report}");

    for (label, section) in [
        ("mandate", &report["mandates"][0]["escalations"]),
        ("totals", &report["totals"]["escalations"]),
    ] {
        let count = section["count"].as_u64().unwrap();
        let answered = section["answered"].as_u64().unwrap();
        let open = section["open"].as_u64().unwrap();
        let expired_unanswered = section["expired_unanswered"].as_u64().unwrap();
        let superseded = section["superseded"].as_u64().unwrap();
        assert_eq!(count, 4, "{label}: {report}");
        assert_eq!(answered, 1, "{label}: {report}");
        assert_eq!(open, 1, "{label}: {report}");
        assert_eq!(expired_unanswered, 1, "{label}: {report}");
        assert_eq!(superseded, 1, "{label}: {report}");
        assert_eq!(
            answered + open + expired_unanswered + superseded,
            count,
            "{label}: {report}"
        );

        let per_mandate_day = section["per_mandate_day"].as_f64().unwrap();
        let per_human_day = section["per_human_day"].as_f64().unwrap();
        assert!(
            (per_mandate_day - 4.0 / 2.0).abs() < 1e-9,
            "{label}: {report}"
        );
        assert!(
            (per_human_day - 4.0 / 21.0).abs() < 1e-9,
            "{label}: {report}"
        );
        assert!(
            (per_mandate_day - per_human_day).abs() > 1e-9,
            "{label}: per_human_day must use human calendar days distinct from mandate days: {report}"
        );
    }
}

crate::scenario_table!(
    resident_metrics,
    invalid_and_day_windows_at_tool_boundary,
    human_write_undoes_checkpoint_action_once,
    baseline_marks_underivable_measures_not_derivable,
    closed_interactive_baseline_bounds_related_task_events_to_active_span,
    task_creation_is_not_an_intervention_but_later_status_changes_are,
    totals_divide_per_mandate_day_by_summed_active_days,
    baseline_reads_references_in_another_workspace,
    tag_without_mandate_artifact_is_excluded_from_mandates_and_totals,
    ended_mandate_bounds_active_days_by_its_end,
    interventions_and_escalations_outside_active_span_are_excluded,
    escalation_buckets_sum_to_count_and_per_human_day_uses_window_days,
);
