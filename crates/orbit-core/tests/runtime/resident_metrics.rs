//! Resident metrics at the tool boundary: a human write that undoes a
//! resident action recorded in a checkpoint is counted once as a reversal
//! of that action within the reversal window, and an interactive baseline
//! reports the measures it cannot derive as `not_derivable`, never zero.
use std::path::Path;

use chrono::{Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_types::task::{TaskArtifact, TaskStatus};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::ResidentLiveCycle;
use serde_json::{Value, json};

const CONFIG: &str =
    "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n";

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

crate::scenario_table!(
    resident_metrics,
    human_write_undoes_checkpoint_action_once,
    baseline_marks_underivable_measures_not_derivable,
);
