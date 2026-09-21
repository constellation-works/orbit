use chrono::{Duration, Utc};
use orbit_core::{AuditEventStatus, OrbitRuntime, V2AuditEventInsertParams};
use orbit_types::workflow::activity_job::{V2_DENIAL_EVENT_TYPES, V2AuditEventKind};
use serde_json::json;

use super::super::denials::{
    SQLITE_FS_BOUNDARY_PROFILE, collect_denial_rows, denials_payload, v2_denial_event_types,
};

#[test]
fn v2_denial_filter_matches_audit_event_denial_variants() {
    let denial_variants = [
        V2AuditEventKind::FsCallDenied {
            profile: "restricted".to_string(),
            op: "read".to_string(),
            path: "./secret.txt".to_string(),
            allowed: false,
            matched_rule: "deny".to_string(),
        },
        V2AuditEventKind::ToolDenied {
            tool_name: "orbit.task.delete".to_string(),
            reason: "not allowed".to_string(),
        },
        V2AuditEventKind::StepDenied {
            step_id: "implement".to_string(),
            reason: "tool denied".to_string(),
        },
    ];
    let expected = denial_variants
        .iter()
        .map(V2AuditEventKind::event_type)
        .collect::<Vec<_>>();

    assert_eq!(expected, V2_DENIAL_EVENT_TYPES);
    assert_eq!(v2_denial_event_types(), expected.as_slice());
}

#[test]
fn denials_payload_combines_v2_and_sqlite_denials() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let now = Utc::now();
    seed_v2_denial(
        &runtime,
        "evt-fs-denied",
        "fs.call.denied",
        json!({
            "schemaVersion": 1,
            "event_type": "fs.call.denied",
            "event_id": "evt-fs-denied",
            "ts": now.to_rfc3339(),
            "run_id": "run-v2-denials",
            "agent_identity": "codex / gpt-5",
            "body_kind": "fs_call_denied",
            "profile": "restricted",
            "path": "./secret.txt"
        }),
        now,
    );
    seed_v2_denial(
        &runtime,
        "evt-tool-denied",
        "tool.denied",
        json!({
            "schemaVersion": 1,
            "event_type": "tool.denied",
            "event_id": "evt-tool-denied",
            "ts": now.to_rfc3339(),
            "run_id": "run-v2-denials",
            "agent_identity": "codex / gpt-5",
            "body_kind": "tool_denied",
            "tool_name": "orbit.task.delete"
        }),
        now,
    );
    seed_v2_denial(
        &runtime,
        "evt-step-denied",
        "step.denied",
        json!({
            "schemaVersion": 1,
            "event_type": "step.denied",
            "event_id": "evt-step-denied",
            "ts": now.to_rfc3339(),
            "run_id": "run-v2-denials",
            "agent_identity": "codex / gpt-5",
            "body_kind": "step_denied",
            "step_id": "implement",
            "reason": "tool denied"
        }),
        now,
    );
    runtime
        .record_audit_event(&orbit_core::AuditEventInsertParams {
            execution_id: "exec-sqlite-fs".to_string(),
            command: "tool".to_string(),
            subcommand: Some("fs.write".to_string()),
            tool_name: Some("fs.write".to_string()),
            target_type: Some("tool".to_string()),
            target_id: Some("fs.write".to_string()),
            role: "codex".to_string(),
            status: AuditEventStatus::Denied,
            exit_code: 1,
            duration_ms: 5,
            working_directory: "/workspace".to_string(),
            arguments_json: None,
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: Some("path is outside workspace: /usr/bin/false".to_string()),
            host: None,
            pid: 123,
            session_id: None,
            workspace_id: None,
            caller_machine_id: None,
            caller_machine_name: None,
            process_machine_id: None,
            process_machine_name: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: None,
            job_run_id: None,
            activity_id: None,
            step_index: None,
        })
        .expect("record sqlite denial");

    let since = now - Duration::minutes(5);
    let rows = collect_denial_rows(&runtime, Some(since), None, None).expect("collect denials");
    let payload = denials_payload(&rows, None, Some(since));
    assert_eq!(payload["total"], 4);
    assert!(payload["by_target"].to_string().contains("/usr/bin/false"));
    assert!(payload["by_target"].to_string().contains("./secret.txt"));
    assert!(
        payload["by_target"]
            .to_string()
            .contains("orbit.task.delete")
    );
    assert!(payload["by_target"].to_string().contains("implement"));

    let fs_payload = denials_payload(&rows, Some("fs"), Some(since));
    assert_eq!(fs_payload["total"], 2);
    assert!(
        fs_payload["by_profile"]
            .to_string()
            .contains(SQLITE_FS_BOUNDARY_PROFILE)
    );
    assert!(fs_payload["by_profile"].to_string().contains("restricted"));

    let tool_payload = denials_payload(&rows, Some("tool"), Some(since));
    assert_eq!(tool_payload["total"], 2);

    let sqlite_only = collect_denial_rows(
        &runtime,
        Some(since),
        Some(SQLITE_FS_BOUNDARY_PROFILE),
        Some("codex"),
    )
    .expect("collect filtered sqlite denials");
    assert_eq!(sqlite_only.len(), 1);
    assert_eq!(sqlite_only[0].target(), "/usr/bin/false");
}

fn seed_v2_denial(
    runtime: &OrbitRuntime,
    event_id: &str,
    event_type: &str,
    payload: serde_json::Value,
    ts: chrono::DateTime<Utc>,
) {
    runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: runtime.workspace_id().expect("workspace id"),
            event_id: event_id.to_string(),
            source: "v2_envelope".to_string(),
            schema_version: 1,
            event_type: event_type.to_string(),
            ts,
            run_id: "run-v2-denials".to_string(),
            agent_identity: "codex / gpt-5".to_string(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: payload.to_string(),
        })
        .expect("insert v2 denial");
}

#[test]
fn denials_payload_distinguishes_job_runs_from_audit_executions() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let since = Utc::now() - Duration::minutes(5);
    runtime
        .record_audit_event(&orbit_core::AuditEventInsertParams {
            execution_id: "exec-linked-to-run".to_string(),
            command: "tool".to_string(),
            subcommand: Some("orbit.task.update".to_string()),
            tool_name: Some("orbit.task.update".to_string()),
            target_type: Some("task".to_string()),
            target_id: Some("ORB-00001".to_string()),
            role: "codex".to_string(),
            status: AuditEventStatus::Denied,
            exit_code: 1,
            duration_ms: 7,
            working_directory: "/workspace".to_string(),
            arguments_json: None,
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: Some("denied by policy".to_string()),
            host: None,
            pid: 123,
            session_id: None,
            workspace_id: None,
            caller_machine_id: None,
            caller_machine_name: None,
            process_machine_id: None,
            process_machine_name: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: Some("ORB-00001".to_string()),
            job_run_id: Some("jrun-real-policy".to_string()),
            activity_id: Some("agent_implement".to_string()),
            step_index: Some(0),
        })
        .expect("record linked denial");
    runtime
        .record_audit_event(&orbit_core::AuditEventInsertParams {
            execution_id: "audit-task-locks-reserve-denied-test".to_string(),
            command: "task.locks.reserve.denied".to_string(),
            subcommand: None,
            tool_name: Some("orbit.task.locks.reserve".to_string()),
            target_type: Some("task_reservation".to_string()),
            target_id: None,
            role: "admin".to_string(),
            status: AuditEventStatus::Denied,
            exit_code: 1,
            duration_ms: 0,
            working_directory: "/workspace".to_string(),
            arguments_json: Some(
                json!({
                    "actor": "codex / gpt-5.5",
                    "task_ids": ["ORB-00001"],
                    "files": ["file:crates/orbit-cli/src/lib.rs"],
                    "conflicts": [{
                        "file": "file:crates/orbit-cli/src/lib.rs",
                        "held_by": "task",
                        "held_by_id": "ORB-00002"
                    }]
                })
                .to_string(),
            ),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: None,
            host: None,
            pid: 123,
            session_id: None,
            workspace_id: None,
            caller_machine_id: None,
            caller_machine_name: None,
            process_machine_id: None,
            process_machine_name: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: None,
            job_run_id: None,
            activity_id: None,
            step_index: None,
        })
        .expect("record task-lock denial");

    let rows = collect_denial_rows(&runtime, Some(since), None, None).expect("collect denials");
    let payload = denials_payload(&rows, None, Some(since));

    let by_run = payload["by_run"].as_array().expect("by_run array");
    assert_eq!(by_run.len(), 1);
    assert_eq!(by_run[0]["run_id"], "jrun-real-policy");
    assert!(
        !payload["by_run"]
            .to_string()
            .contains("audit-task-locks-reserve-denied-test"),
        "audit execution IDs must not be rendered as JobRun IDs"
    );
}
