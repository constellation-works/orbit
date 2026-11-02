//! Resident metrics at the tool boundary: a human write that undoes a
//! resident action recorded in a checkpoint is counted once as a reversal
//! of that action within the reversal window, and an interactive baseline
//! reports the measures it cannot derive as `not_derivable`, never zero.
use std::path::Path;

use chrono::{Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::task::{AgentDecisionRaise, TaskAddParams, TaskUpdateParams};
use orbit_types::task::{TaskArtifact, TaskStatus};
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

crate::scenario_table!(
    resident_metrics,
    invalid_and_day_windows_at_tool_boundary,
    human_write_undoes_checkpoint_action_once,
    baseline_marks_underivable_measures_not_derivable,
    totals_divide_per_mandate_day_by_summed_active_days,
);
