//! Resident digest reports the same stall state as the resident list.

use std::path::Path;

use chrono::{Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_types::task::{TaskArtifact, TaskEnvelopeV2};
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

fn mandate(runtime: &OrbitRuntime, heartbeat_minutes: u32) -> String {
    let watched = runtime
        .add_task(TaskAddParams {
            title: "watched task".into(),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let id = runtime
        .add_task(TaskAddParams {
            title: "resident mandate".into(),
            tags: vec!["resident".into()],
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let body = json!({
        "schema": "resident.mandate/v1",
        "goal": "Keep the watched task moving",
        "exit_criteria": ["The watched task is complete"],
        "workspaces": [runtime.workspace_id().unwrap()],
        "watch": {"tasks": [watched]},
        "crew": "sol",
        "cadence": {"floor_minutes": 5, "heartbeat_minutes": heartbeat_minutes}
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
    id
}

fn operator() -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: [McpCapability::Operator].into(),
        ..Default::default()
    }
}

fn call(runtime: &OrbitRuntime, tool: &str, input: Value) -> Value {
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
        .unwrap()
}

fn set_task_creation_time(
    runtime: &OrbitRuntime,
    global: &Path,
    id: &str,
    at: chrono::DateTime<Utc>,
) {
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(global),
    )
    .unwrap();
    let path = registry
        .canonical_task_bundle_path(&runtime.workspace_id().unwrap(), id)
        .unwrap()
        .join(orbit_types::task::TASK_ENVELOPE_FILE_NAME);
    let mut envelope: TaskEnvelopeV2 =
        serde_yaml::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    envelope.created_at = at;
    let replacement = path.with_extension("test-aging");
    std::fs::write(&replacement, serde_yaml::to_string(&envelope).unwrap()).unwrap();
    std::fs::rename(replacement, path).unwrap();
}

fn stalled_for(rows: &Value, id: &str) -> bool {
    rows.get("mandates")
        .or_else(|| rows.get("residents"))
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["task_id"] == id)
        .unwrap()["stalled"]
        .as_bool()
        .unwrap()
}

fn digest_and_list_agree_for_a_single_incomplete_checkpoint_and_long_silence() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let runtime = runtime_at(root.path());
    let incomplete = mandate(&runtime, 120);
    let live = ResidentLiveCycle {
        cycle: "rcy-incomplete".into(),
        run_id: "jrun-incomplete".into(),
        expires_at: Utc::now() + Duration::hours(1),
    };
    runtime
        .set_resident_live_cycle(&incomplete, None, Some(&live))
        .unwrap();
    runtime
        .append_resident_checkpoint(
            &incomplete,
            json!({
                "kind": "resident.checkpoint", "schema": 1,
                "mandate": incomplete, "cycle": "rcy-incomplete",
                "run_id": "jrun-incomplete", "crew": "sol", "recovery_of": null,
                "wake": {"reasons": ["heartbeat"], "digest": format!("sha256:{}", "0".repeat(64))},
                "authority": {"rights": [], "observe_only": true, "narrowed_by": null},
                "actions": [], "decisions": [], "requests": [], "costs": [], "notes": "",
                "next_wake": {"reason": "heartbeat", "not_before": "2030-01-01T00:00:00Z"},
                "continuity": "off", "outcome": "incomplete", "probe": false,
                "error": "the cycle did not finish"
            }),
            Some("codex".into()),
            None,
        )
        .unwrap();
    runtime
        .set_resident_live_cycle(&incomplete, Some("rcy-incomplete"), None)
        .unwrap();

    let silent = mandate(&runtime, 5);
    set_task_creation_time(
        &runtime,
        &global,
        &silent,
        Utc::now() - Duration::minutes(11),
    );
    drop(runtime);

    let runtime = runtime_at(root.path());
    let listed = call(&runtime, "orbit.resident.list", json!({}));
    let digested = call(&runtime, "orbit.resident.digest", json!({"since": "1d"}));

    for (id, expected) in [(&incomplete, false), (&silent, true)] {
        let list_stalled = stalled_for(&listed, id);
        let digest_stalled = stalled_for(&digested, id);
        assert_eq!(list_stalled, expected, "list state for {id}: {listed}");
        assert_eq!(digest_stalled, list_stalled, "digest: {digested}");
    }
}

crate::scenario_table!(
    resident_digest,
    digest_and_list_agree_for_a_single_incomplete_checkpoint_and_long_silence,
);
