//! Follow-mode readers poll a run's CLI invocations without re-reading
//! captures they already emitted or captures of other steps.

use std::collections::HashSet;

use chrono::Utc;
use orbit_common::storage::blob_store::BlobStore;
use orbit_core::OrbitRuntime;
use orbit_store::contracts::V2AuditEventInsertParams;
use orbit_store::maintenance::task_registry::{WorkspaceConfig, write_workspace_config};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::dispatch_admission::isolated;

const RUN: &str = "jrun-follow";

fn insert(runtime: &OrbitRuntime, event_id: &str, body: Value) {
    let mut payload = body;
    payload["event_id"] = json!(event_id);
    payload["run_id"] = json!(RUN);
    payload["ts"] = json!(Utc::now().to_rfc3339());
    runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: runtime.workspace_id().unwrap(),
            event_id: event_id.into(),
            source: "v2_envelope".into(),
            schema_version: 1,
            event_type: payload["body_kind"].as_str().unwrap().into(),
            ts: Utc::now(),
            run_id: RUN.into(),
            agent_identity: "codex".into(),
            parent_event_id: payload["parent_event_id"].as_str().map(str::to_string),
            workspace_path: None,
            payload_json: payload.to_string(),
        })
        .unwrap();
}

fn invocation(runtime: &OrbitRuntime, event_id: &str, step: &str, stdout: &str) {
    let blobs = BlobStore::new(runtime.data_root().join("state/audit/blobs"));
    let blob = blobs.write(stdout.as_bytes()).unwrap();
    insert(
        runtime,
        event_id,
        json!({
            "body_kind": "cli_invocation_finished",
            "parent_event_id": format!("start-{step}"),
            "provider": "codex",
            "stdout_blob_ref": blob,
        }),
    );
}

#[test]
fn new_invocations_skip_seen_events_and_other_steps() {
    if !isolated("run_cli_invocations::new_invocations_skip_seen_events_and_other_steps") {
        return;
    }
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    write_workspace_config(
        &root,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws_follow".into(),
        },
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&root, &root).unwrap();
    for step in ["implement", "review"] {
        insert(
            &runtime,
            &format!("start-{step}"),
            json!({"body_kind": "step_started", "step_id": step}),
        );
    }
    invocation(&runtime, "inv-1", "implement", "first\n");
    invocation(&runtime, "inv-other", "review", "elsewhere\n");

    let mut seen = HashSet::new();
    let first = runtime
        .collect_new_run_cli_invocations(RUN, Some("implement"), &seen)
        .unwrap();
    assert_eq!(
        first
            .iter()
            .map(|record| (record.event_id.as_str(), record.stdout.as_str()))
            .collect::<Vec<_>>(),
        [("inv-1", "first\n")],
        "only the selected step's invocation is loaded, with its full capture"
    );
    seen.extend(first.into_iter().map(|record| record.event_id));

    assert!(
        runtime
            .collect_new_run_cli_invocations(RUN, Some("implement"), &seen)
            .unwrap()
            .is_empty(),
        "an emitted invocation is not loaded again"
    );

    invocation(&runtime, "inv-2", "implement", "second\n");
    let next = runtime
        .collect_new_run_cli_invocations(RUN, Some("implement"), &seen)
        .unwrap();
    assert_eq!(
        next.iter()
            .map(|record| record.event_id.as_str())
            .collect::<Vec<_>>(),
        ["inv-2"]
    );

    let unfiltered = runtime
        .collect_new_run_cli_invocations(RUN, None, &seen)
        .unwrap();
    assert_eq!(
        unfiltered
            .iter()
            .map(|record| record.event_id.as_str())
            .collect::<Vec<_>>(),
        ["inv-other", "inv-2"],
        "without a step filter every unseen invocation is returned in event order"
    );
}
