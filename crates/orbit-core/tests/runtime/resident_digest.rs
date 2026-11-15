//! Resident digest reports the same stall state as the resident list, and
//! omits an ended mandate with its vetoes while keeping handed-off and active
//! ones.

use std::path::Path;

use chrono::{Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_types::task::{TaskArtifact, TaskEnvelopeV2, TaskStatus};
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

fn stalled_reason_for(rows: &Value, id: &str) -> Value {
    rows.get("mandates")
        .or_else(|| rows.get("residents"))
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["task_id"] == id)
        .unwrap()["stalled_reason"]
        .clone()
}

/// The workspace's resident cycle routine, present but disabled: no cycle
/// will ever fire until it is enabled. Mirrors the disabled-routine
/// fixture in resident_cycle::deadline_expiry for this test's roots
/// (`home/.orbit` global, `repo/.orbit` checkout).
fn install_disabled_cycle_routine(root: &Path, enabled: bool) {
    // The routine's target job must be a known job, or the loader retires
    // the routine instead of reporting it disabled.
    let jobs = root.join("home/.orbit/resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::write(
        jobs.join("resident_cycle_pipeline.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: resident_cycle_pipeline\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      default_input:\n        seconds: 0\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n",
    )
    .unwrap();
    let routines = root.join("repo/.orbit/routines");
    std::fs::create_dir_all(&routines).unwrap();
    std::fs::write(
        routines.join("resident_cycle.yaml"),
        format!(
            "schemaVersion: 1\nname: resident-cycle-fixture\nenabled: {}\ntrigger:\n  state:\n    kind: resident_due\n    owner_machine: fixture-machine\n    branch: main\n    debounce_minutes: 1\n    max_wait_minutes: 5\n    max_items: 50\n    batch_size: 1\n    retries: 2\n    deadline_minutes: 60\ntarget: job:resident_cycle_pipeline\n",
            if enabled { "true" } else { "false" }
        ),
    )
    .unwrap();
}

fn digest_reports_the_disabled_routine_stall_like_show_list_and_attention() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    install_disabled_cycle_routine(root.path(), false);
    drop(runtime);
    let runtime = runtime_at(root.path());
    // Fresh mandate, two-hour heartbeat, no checkpoints: not
    // heartbeat-stalled, but no cycle can ever fire while the routine is
    // disabled.
    let id = mandate(&runtime, 120);

    let shown = call(&runtime, "orbit.resident.show", json!({"task_id": id}));
    assert_eq!(shown["cycle_routine_disabled"], true, "{shown}");
    assert_eq!(shown["stalled"], true, "{shown}");

    let listed = call(&runtime, "orbit.resident.list", json!({}));
    let digested = call(&runtime, "orbit.resident.digest", json!({"since": "1d"}));
    assert!(stalled_for(&listed, &id), "{listed}");
    assert!(
        stalled_for(&digested, &id),
        "digest must agree with show/list: {digested}"
    );
    assert_eq!(
        stalled_reason_for(&digested, &id),
        shown["stalled_reason"].clone(),
        "digest reason must agree with show: {digested} vs {shown}"
    );
    assert!(
        stalled_reason_for(&digested, &id)
            .as_str()
            .is_some_and(|reason| reason.contains("disabled")),
        "the stall reason names the disabled routine: {digested}"
    );

    let attention = call(
        &runtime,
        "orbit.attention",
        json!({"kind": "resident_stalled"}),
    );
    assert!(
        attention["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item["refs"]["task_id"] == id || item["task_id"] == id }),
        "the mandate surfaces in attention: {attention}"
    );

    // Enabling the routine removes this cause: a fresh mandate with a
    // healthy heartbeat is stalled nowhere.
    install_disabled_cycle_routine(root.path(), true);
    drop(runtime);
    let runtime = runtime_at(root.path());
    let shown = call(&runtime, "orbit.resident.show", json!({"task_id": id}));
    assert_eq!(shown["cycle_routine_disabled"], false, "{shown}");
    assert_eq!(shown["stalled"], false, "{shown}");
    let digested = call(&runtime, "orbit.resident.digest", json!({"since": "1d"}));
    assert!(!stalled_for(&digested, &id), "{digested}");
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

/// One checkpoint carrying a single reversible decision, so the mandate
/// would be offered a veto in the digest. A checkpoint must name the live
/// cycle, so the cycle is published for the write and then released.
fn checkpoint_with_one_veto(runtime: &OrbitRuntime, mandate: &str, decision: &str) {
    let cycle = format!("rcy-{decision}");
    let run_id = format!("jrun-{decision}");
    let live = ResidentLiveCycle {
        cycle: cycle.clone(),
        run_id: run_id.clone(),
        expires_at: Utc::now() + Duration::hours(1),
    };
    runtime
        .set_resident_live_cycle(mandate, None, Some(&live))
        .unwrap();
    runtime
        .append_resident_checkpoint(
            mandate,
            json!({
                "kind": "resident.checkpoint", "schema": 1,
                "mandate": mandate, "cycle": cycle, "run_id": run_id,
                "crew": "sol", "recovery_of": null,
                "wake": {"reasons": ["heartbeat"], "digest": format!("sha256:{}", "0".repeat(64))},
                "authority": {"rights": [], "observe_only": true, "narrowed_by": null},
                "actions": [],
                "decisions": [{
                    "id": decision, "summary": format!("Take {decision}"), "class": "reversible",
                    "reversible": true, "p_wrong": "low", "rework_units": 1, "precedent": null
                }],
                "requests": [], "costs": [], "notes": "",
                "next_wake": {"reason": "heartbeat", "not_before": "2030-01-01T00:00:00Z"},
                "continuity": "off", "outcome": "idle", "probe": false
            }),
            Some("codex".into()),
            None,
        )
        .unwrap();
    runtime
        .set_resident_live_cycle(mandate, Some(&cycle), None)
        .unwrap();
}

fn digest_omits_an_ended_mandate_and_its_vetoes_but_keeps_handed_off_and_active_ones() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let stopped = mandate(&runtime, 120);
    let handed_off = mandate(&runtime, 120);
    let active = mandate(&runtime, 120);
    checkpoint_with_one_veto(&runtime, &stopped, "veto-stopped");
    checkpoint_with_one_veto(&runtime, &handed_off, "veto-handed-off");
    checkpoint_with_one_veto(&runtime, &active, "veto-active");

    // A stop ends the mandate but leaves its assignment in review.
    call(
        &runtime,
        "orbit.resident.control",
        json!({"task_id": stopped, "action": "stop", "reason": "Enough for now"}),
    );
    // A task starts with a plan, and proposed must pass in-progress before review.
    for id in [&active, &handed_off] {
        runtime
            .update_task_as_human(
                id,
                TaskUpdateParams {
                    plan: Some("Run cycles until stopped.".into()),
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                },
                "human:daniel".into(),
            )
            .unwrap();
    }
    runtime
        .update_task_as_human(
            &handed_off,
            TaskUpdateParams {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();
    assert_eq!(
        runtime.get_task(&stopped).unwrap().status,
        TaskStatus::Review
    );
    assert_eq!(
        runtime.show_resident_mandate(&stopped).unwrap()["state"],
        "ended"
    );
    assert_eq!(
        runtime.show_resident_mandate(&handed_off).unwrap()["state"],
        "handed_off"
    );

    let digested = call(&runtime, "orbit.resident.digest", json!({"since": "1d"}));
    let residents: Vec<&str> = digested["residents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["task_id"].as_str().unwrap())
        .collect();
    let vetoes: Vec<&str> = digested["decisions_awaiting_veto"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["mandate"].as_str().unwrap())
        .collect();
    for (id, expected) in [(&stopped, false), (&handed_off, true), (&active, true)] {
        assert_eq!(
            residents.contains(&id.as_str()),
            expected,
            "residents for {id}: {digested}"
        );
        assert_eq!(
            vetoes.contains(&id.as_str()),
            expected,
            "decisions awaiting veto for {id}: {digested}"
        );
    }
}

crate::scenario_table!(
    resident_digest,
    digest_and_list_agree_for_a_single_incomplete_checkpoint_and_long_silence,
    digest_omits_an_ended_mandate_and_its_vetoes_but_keeps_handed_off_and_active_ones,
    digest_reports_the_disabled_routine_stall_like_show_list_and_attention,
);
