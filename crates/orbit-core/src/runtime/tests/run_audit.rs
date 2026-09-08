//! Sibling tests for `run_audit.rs` (migrated per ORB-00246 / docs/design-patterns/test_layout.md).

use crate::{OrbitRuntime, V2AuditEventInsertParams};
use chrono::{DateTime, Utc};
use orbit_common::process::identity::ProcessLiveness;
use orbit_common::storage::blob_store::BlobStore;

use serde_json::json;

fn seed_v2_audit_events(
    runtime: &OrbitRuntime,
    run_id: &str,
    events: impl IntoIterator<Item = serde_json::Value>,
) {
    let workspace_id = runtime.workspace_id().expect("workspace id");
    for (index, mut event) in events.into_iter().enumerate() {
        let object = event.as_object_mut().expect("event object");
        object
            .entry("schemaVersion".to_string())
            .or_insert_with(|| json!(1));
        object
            .entry("event_type".to_string())
            .or_insert_with(|| json!("test.event"));
        object
            .entry("run_id".to_string())
            .or_insert_with(|| json!(run_id));
        object
            .entry("agent_identity".to_string())
            .or_insert_with(|| json!("codex"));
        object.entry("ts".to_string()).or_insert_with(|| {
            json!(format!(
                "2026-04-26T07:{:02}:{:02}Z",
                (index / 60) % 60,
                index % 60
            ))
        });
        let ts = event["ts"]
            .as_str()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&chrono::Utc))
            .expect("event ts");
        runtime
            .insert_v2_audit_event(&V2AuditEventInsertParams {
                workspace_id: workspace_id.clone(),
                event_id: event["event_id"].as_str().expect("event id").to_string(),
                source: "v2_envelope".to_string(),
                schema_version: event["schemaVersion"]
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or(1),
                event_type: event["event_type"]
                    .as_str()
                    .expect("event type")
                    .to_string(),
                ts,
                run_id: event["run_id"].as_str().expect("run id").to_string(),
                agent_identity: event["agent_identity"]
                    .as_str()
                    .expect("agent identity")
                    .to_string(),
                parent_event_id: event
                    .get("parent_event_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                workspace_path: None,
                payload_json: event.to_string(),
            })
            .expect("insert v2 audit event");
    }
}

fn insert_v2_audit_payload(
    runtime: &OrbitRuntime,
    run_id: &str,
    event_id: &str,
    stored_at: DateTime<Utc>,
    payload_json: String,
) {
    runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: runtime.workspace_id().expect("workspace id"),
            event_id: event_id.to_string(),
            source: "v2_envelope".to_string(),
            schema_version: 1,
            event_type: "test.event".to_string(),
            ts: stored_at,
            run_id: run_id.to_string(),
            agent_identity: "codex".to_string(),
            parent_event_id: None,
            workspace_path: None,
            payload_json,
        })
        .expect("insert v2 audit payload");
}

#[test]
fn latest_audit_timestamp_uses_valid_envelope_payloads_not_row_order() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-timestamp-projection";
    let earliest = "2026-04-26T07:01:00Z";
    let latest = "2026-04-26T07:20:00Z";

    // The store orders these rows by its `ts`, but the envelope timestamps are
    // deliberately reversed. The timestamp projection must retain the old
    // full-projection behavior and select the latest payload value instead.
    insert_v2_audit_payload(
        &runtime,
        run_id,
        "evt-row-newer",
        DateTime::parse_from_rfc3339("2026-04-26T07:30:00Z")
            .expect("parse stored timestamp")
            .with_timezone(&Utc),
        json!({"event_id": "evt-row-newer", "ts": earliest}).to_string(),
    );
    insert_v2_audit_payload(
        &runtime,
        run_id,
        "evt-payload-newer",
        DateTime::parse_from_rfc3339("2026-04-26T07:02:00Z")
            .expect("parse stored timestamp")
            .with_timezone(&Utc),
        json!({"event_id": "evt-payload-newer", "ts": latest}).to_string(),
    );
    insert_v2_audit_payload(
        &runtime,
        run_id,
        "evt-malformed",
        Utc::now(),
        "not json".to_string(),
    );
    insert_v2_audit_payload(
        &runtime,
        run_id,
        "evt-missing-ts",
        Utc::now(),
        json!({"event_id": "evt-missing-ts"}).to_string(),
    );
    insert_v2_audit_payload(
        &runtime,
        run_id,
        "evt-missing-event-id",
        Utc::now(),
        json!({"ts": "2026-04-26T08:00:00Z"}).to_string(),
    );
    insert_v2_audit_payload(
        &runtime,
        run_id,
        "evt-invalid-ts",
        Utc::now(),
        json!({"event_id": "evt-invalid-ts", "ts": "not-a-timestamp"}).to_string(),
    );

    let timestamp = runtime
        .latest_run_audit_timestamp(run_id)
        .expect("read timestamp projection");
    assert_eq!(
        timestamp,
        Some(
            DateTime::parse_from_rfc3339(latest)
                .expect("parse latest")
                .with_timezone(&Utc)
        )
    );
    assert_eq!(
        runtime
            .latest_run_audit_timestamp("jrun-empty-timestamp-projection")
            .expect("read empty timestamp projection"),
        None
    );
}

#[test]
fn collect_run_cli_invocations_derives_step_ids_from_parent_chain() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let audit_root = runtime.data_root().join("state").join("audit");
    let blob_store = BlobStore::new(audit_root.join("blobs"));

    let stdout_one = blob_store.write(b"one stdout\n").expect("write stdout one");
    let stderr_one = blob_store.write(b"one stderr\n").expect("write stderr one");
    let stdout_two = blob_store.write(b"two stdout\n").expect("write stdout two");

    let run_id = "jrun-test";
    let events = [
        json!({
            "schemaVersion": 1,
            "event_type": "run.started",
            "event_id": "evt-run-started",
            "ts": "2026-04-26T07:00:00Z",
            "run_id": run_id,
            "agent_identity": "codex",
            "body_kind": "run_started",
            "job_name": "test-job"
        }),
        json!({
            "schemaVersion": 1,
            "event_type": "step.started",
            "event_id": "evt-step-one",
            "ts": "2026-04-26T07:00:01Z",
            "run_id": run_id,
            "agent_identity": "codex",
            "parent_event_id": "evt-run-started",
            "body_kind": "step_started",
            "step_id": "implement_one"
        }),
        json!({
            "schemaVersion": 1,
            "event_type": "activity.started",
            "event_id": "evt-activity-one",
            "ts": "2026-04-26T07:00:02Z",
            "run_id": run_id,
            "agent_identity": "codex",
            "parent_event_id": "evt-step-one",
            "body_kind": "activity_started",
            "activity_name": "worker",
            "activity_type": "agent_loop"
        }),
        json!({
            "schemaVersion": 1,
            "event_type": "cli.invocation.finished",
            "event_id": "evt-cli-one",
            "ts": "2026-04-26T07:00:03Z",
            "run_id": run_id,
            "agent_identity": "codex",
            "parent_event_id": "evt-activity-one",
            "body_kind": "cli_invocation_finished",
            "provider": "codex",
            "exit_code": 0,
            "duration_ms": 10,
            "stdout_blob_ref": stdout_one,
            "stderr_blob_ref": stderr_one,
            "harness_version": null,
            "timed_out": false
        }),
        json!({
            "schemaVersion": 1,
            "event_type": "step.started",
            "event_id": "evt-step-two",
            "ts": "2026-04-26T07:00:04Z",
            "run_id": run_id,
            "agent_identity": "codex",
            "parent_event_id": "evt-run-started",
            "body_kind": "step_started",
            "step_id": "review"
        }),
        json!({
            "schemaVersion": 1,
            "event_type": "activity.started",
            "event_id": "evt-activity-two",
            "ts": "2026-04-26T07:00:05Z",
            "run_id": run_id,
            "agent_identity": "codex",
            "parent_event_id": "evt-step-two",
            "body_kind": "activity_started",
            "activity_name": "reviewer",
            "activity_type": "agent_loop"
        }),
        json!({
            "schemaVersion": 1,
            "event_type": "cli.invocation.finished",
            "event_id": "evt-cli-two",
            "ts": "2026-04-26T07:00:06Z",
            "run_id": run_id,
            "agent_identity": "codex",
            "parent_event_id": "evt-activity-two",
            "body_kind": "cli_invocation_finished",
            "provider": "claude",
            "exit_code": 0,
            "duration_ms": 20,
            "stdout_blob_ref": stdout_two,
            "stderr_blob_ref": null,
            "harness_version": null,
            "timed_out": false
        }),
    ];
    seed_v2_audit_events(&runtime, run_id, events);

    let records = runtime
        .collect_run_cli_invocations(run_id)
        .expect("collect records");
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].run_id, run_id);
    assert_eq!(records[0].event_id, "evt-cli-one");
    assert_eq!(records[0].step_id.as_deref(), Some("implement_one"));
    assert_eq!(records[0].step_index, Some(0));
    assert_eq!(records[0].provider.as_deref(), Some("codex"));
    assert_eq!(records[0].stdout, "one stdout\n");
    assert_eq!(records[0].stderr, "one stderr\n");
    assert_eq!(records[0].exit_code, Some(0));
    assert!(!records[0].timed_out);
    assert_eq!(records[0].duration_ms, Some(10));
    assert_eq!(records[1].step_id.as_deref(), Some("review"));
    assert_eq!(records[1].step_index, Some(1));
    assert_eq!(records[1].provider.as_deref(), Some("claude"));
    assert_eq!(records[1].stdout, "two stdout\n");
    assert_eq!(records[1].stderr, "");
}

#[test]
fn missing_run_audit_file_returns_no_cli_invocations() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let records = runtime
        .collect_run_cli_invocations("jrun-missing")
        .expect("collect records");
    assert!(records.is_empty());
}

#[test]
fn collect_run_audit_steps_reads_step_finished_error_message_and_tolerates_absence() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-step-errors";
    let events = [
        json!({
            "event_id": "evt-step-one",
            "ts": "2026-04-26T07:00:01Z",
            "run_id": run_id,
            "body_kind": "step_started",
            "step_id": "plan"
        }),
        json!({
            "event_id": "evt-step-one-finished",
            "ts": "2026-04-26T07:00:02Z",
            "run_id": run_id,
            "body_kind": "step_finished",
            "step_id": "plan",
            "outcome": "error",
            "error_message": "planning failed"
        }),
        json!({
            "event_id": "evt-step-two",
            "ts": "2026-04-26T07:00:03Z",
            "run_id": run_id,
            "body_kind": "step_started",
            "step_id": "review"
        }),
        json!({
            "event_id": "evt-step-two-finished",
            "ts": "2026-04-26T07:00:04Z",
            "run_id": run_id,
            "body_kind": "step_finished",
            "step_id": "review",
            "outcome": "success"
        }),
    ];
    seed_v2_audit_events(&runtime, run_id, events);

    let steps = runtime
        .collect_run_audit_steps(run_id)
        .expect("collect steps");

    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].step_id, "plan");
    assert_eq!(steps[0].outcome.as_deref(), Some("error"));
    assert_eq!(steps[0].error_message.as_deref(), Some("planning failed"));
    assert_eq!(steps[1].step_id, "review");
    assert_eq!(steps[1].outcome.as_deref(), Some("success"));
    assert_eq!(steps[1].error_message, None);
}

#[test]
fn recovery_attempt_projection_distinguishes_outcomes_and_redacts_diagnostics() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-recovery-outcomes";
    let secret = "sk-proj-abcdefghijklmnopqrstuvwxyz0123456789";
    seed_v2_audit_events(
        &runtime,
        run_id,
        [
            json!({
                "event_id": "evt-run",
                "body_kind": "run_started"
            }),
            json!({
                "event_id": "evt-preparation",
                "body_kind": "step_recovery_attempted",
                "step_id": "sync_base",
                "recovery_activity": "step_failure_recovery",
                "recovery_succeeded": false,
                "failure_phase": "preparation",
                "error_message": format!("fixture preparation rejected {secret}")
            }),
            json!({
                "event_id": "evt-dispatch",
                "body_kind": "step_recovery_attempted",
                "step_id": "sync_base",
                "recovery_activity": "step_failure_recovery",
                "recovery_succeeded": false,
                "failure_phase": "dispatch",
                "error_message": "launcher refused recovery"
            }),
            json!({
                "event_id": "evt-activity",
                "body_kind": "step_recovery_attempted",
                "step_id": "sync_base",
                "recovery_activity": "step_failure_recovery",
                "recovery_succeeded": false,
                "failure_phase": "activity",
                "error_message": "recovery activity returned failure"
            }),
            json!({
                "event_id": "evt-denied",
                "body_kind": "step_recovery_attempted",
                "step_id": "sync_base",
                "recovery_activity": "step_failure_recovery",
                "recovery_succeeded": false,
                "failure_phase": "authorization",
                "error_message": "recovery admission denied"
            }),
            json!({
                "event_id": "evt-success",
                "body_kind": "step_recovery_attempted",
                "step_id": "sync_base",
                "recovery_activity": "step_failure_recovery",
                "recovery_succeeded": true
            }),
        ],
    );

    let attempts = runtime
        .collect_run_recovery_attempts(run_id)
        .expect("collect recovery attempts");

    assert_eq!(attempts.state, "recorded");
    assert_eq!(attempts.limit, 8);
    assert!(!attempts.truncated);
    assert_eq!(attempts.attempts.len(), 5);
    assert_eq!(attempts.attempts[0].run_id, run_id);
    assert_eq!(attempts.attempts[0].event_id, "evt-preparation");
    assert_eq!(attempts.attempts[0].failed_step_id, "sync_base");
    assert_eq!(
        attempts.attempts[0].failure_phase.as_deref(),
        Some("preparation")
    );
    assert!(
        !attempts.attempts[0]
            .diagnostic
            .as_deref()
            .unwrap_or_default()
            .contains(secret)
    );
    assert_eq!(
        attempts.attempts[1].failure_phase.as_deref(),
        Some("dispatch")
    );
    assert_eq!(
        attempts.attempts[2].failure_phase.as_deref(),
        Some("activity")
    );
    assert_eq!(
        attempts.attempts[3].failure_phase.as_deref(),
        Some("authorization")
    );
    assert_eq!(attempts.attempts[4].outcome, "succeeded");
    assert_eq!(attempts.attempts[4].failure_phase, None);
    assert_eq!(attempts.attempts[4].diagnostic, None);
}

#[test]
fn recovery_attempt_projection_bounds_history_and_marks_legacy_absence() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-recovery-bounded";
    let events = (0..9)
        .map(|index| {
            json!({
                "event_id": format!("evt-recovery-{index}"),
                "body_kind": "step_recovery_attempted",
                "step_id": "sync_base",
                "recovery_activity": "step_failure_recovery",
                "recovery_succeeded": false,
                "failure_phase": "dispatch",
                "error_message": "x".repeat(1100)
            })
        })
        .collect::<Vec<_>>();
    seed_v2_audit_events(&runtime, run_id, events);

    let bounded = runtime
        .collect_run_recovery_attempts(run_id)
        .expect("collect bounded attempts");
    assert_eq!(bounded.attempts.len(), 8);
    assert!(bounded.truncated);
    assert_eq!(bounded.attempts[0].event_id, "evt-recovery-1");
    assert!(bounded.attempts[0].diagnostic_truncated);
    assert_eq!(
        bounded.attempts[0]
            .diagnostic
            .as_deref()
            .unwrap_or_default()
            .chars()
            .count(),
        1025
    );

    let legacy = runtime
        .collect_run_recovery_attempts("jrun-legacy")
        .expect("collect legacy absence");
    assert_eq!(legacy.state, "unavailable");
    assert!(legacy.attempts.is_empty());

    seed_v2_audit_events(
        &runtime,
        "jrun-no-recovery",
        [json!({"event_id": "evt-run", "body_kind": "run_started"})],
    );
    let not_attempted = runtime
        .collect_run_recovery_attempts("jrun-no-recovery")
        .expect("collect no recovery attempt");
    assert_eq!(not_attempted.state, "not_attempted");
    assert!(not_attempted.attempts.is_empty());
}

/// [ORB-11625] A page loads recovery evidence in two bounded queries and
/// keeps per-run attribution when histories are uneven.
#[test]
fn recovery_attempts_for_runs_are_partitioned_and_counted_once() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let busy = "jrun-busy";
    let quiet = "jrun-quiet";
    let empty = "jrun-empty";
    seed_v2_audit_events(
        &runtime,
        busy,
        (0..20).map(|index| {
            json!({
                "event_id": format!("evt-busy-{index}"),
                "body_kind": "step_recovery_attempted",
                "step_id": "sync_base",
                "recovery_activity": "step_failure_recovery",
                "recovery_succeeded": false,
            })
        }),
    );
    seed_v2_audit_events(
        &runtime,
        quiet,
        [
            json!({"event_id": "evt-quiet-start", "body_kind": "step_started", "step_id": "sync_base"}),
            json!({
                "event_id": "evt-quiet-recovery",
                "body_kind": "step_recovery_attempted",
                "step_id": "sync_base",
                "recovery_activity": "step_failure_recovery",
                "recovery_succeeded": true,
            }),
        ],
    );

    let page = runtime
        .collect_run_recovery_attempts_for_runs(&[
            busy.to_string(),
            quiet.to_string(),
            empty.to_string(),
        ])
        .expect("batch collect");

    assert_eq!(page.event_queries, 1);
    assert_eq!(page.presence_queries, 1);
    assert_eq!(page.per_run_fetch_limit, 9);
    assert!(page.by_run_id[busy].truncated);
    assert_eq!(page.by_run_id[busy].attempts.len(), 8);
    assert_eq!(page.by_run_id[busy].attempts[0].event_id, "evt-busy-12");
    assert!(
        page.by_run_id[busy]
            .attempts
            .iter()
            .all(|attempt| attempt.run_id == busy)
    );
    assert_eq!(page.by_run_id[quiet].state, "recorded");
    assert!(!page.by_run_id[quiet].truncated);
    assert_eq!(
        page.by_run_id[quiet].attempts[0].event_id,
        "evt-quiet-recovery"
    );
    assert_eq!(page.by_run_id[empty].state, "unavailable");
    assert!(page.by_run_id[empty].attempts.is_empty());
}

#[test]
fn malformed_jsonl_and_missing_blobs_are_tolerated() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-tolerant";
    seed_v2_audit_events(
        &runtime,
        run_id,
        [
            json!({
                "event_id": "evt-step",
                "ts": "2026-04-26T07:00:01Z",
                "run_id": run_id,
                "body_kind": "step_started",
                "step_id": "implement"
            }),
            json!({
                "event_id": "evt-cli",
                "ts": "2026-04-26T07:00:02Z",
                "run_id": run_id,
                "parent_event_id": "evt-step",
                "body_kind": "cli_invocation_finished",
                "provider": "codex",
                "exit_code": 1,
                "duration_ms": 42,
                "stdout_blob_ref": "aa/missing",
                "stderr_blob_ref": "error:writer-failed",
                "timed_out": true
            }),
        ],
    );

    let events = runtime
        .collect_run_audit_events(run_id)
        .expect("collect events");
    assert_eq!(events.len(), 2);

    let records = runtime
        .collect_run_cli_invocations(run_id)
        .expect("collect records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].step_index, Some(0));
    assert_eq!(records[0].stdout, "");
    assert_eq!(records[0].stderr, "");
    assert_eq!(records[0].exit_code, Some(1));
    assert!(records[0].timed_out);
}

#[test]
fn provider_process_completion_uses_the_parallel_invocation_parent() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    // Regression fixture from jrun-20260905-1721. The process events share
    // `pilot`, but their direct parents identify separate invocations.
    let run_id = "jrun-20260905-1721";
    seed_v2_audit_events(
        &runtime,
        run_id,
        [
            json!({
                "event_id": "00000001",
                "run_id": run_id,
                "body_kind": "run_started"
            }),
            json!({
                "event_id": "0000000a",
                "run_id": run_id,
                "parent_event_id": "00000001",
                "body_kind": "step_started",
                "step_id": "pilot"
            }),
            json!({
                "event_id": "0000000b",
                "run_id": run_id,
                "parent_event_id": "0000000a",
                "body_kind": "activity_started"
            }),
            json!({
                "event_id": "0000000d",
                "run_id": run_id,
                "parent_event_id": "0000000a",
                "body_kind": "activity_started"
            }),
            json!({
                "event_id": "00000011",
                "run_id": run_id,
                "parent_event_id": "0000000d",
                "body_kind": "cli_invocation_process",
                "pid": 288858,
                "pid_start_time": "ps-lstart-utc-v1:exited"
            }),
            json!({
                "event_id": "00000013",
                "run_id": run_id,
                "parent_event_id": "0000000b",
                "body_kind": "cli_invocation_process",
                "pid": 289173,
                "pid_start_time": "ps-lstart-utc-v1:live"
            }),
            json!({
                "event_id": "00000014",
                "run_id": run_id,
                "parent_event_id": "0000000d",
                "body_kind": "cli_invocation_finished",
                "exit_code": 0,
                "duration_ms": 30_000
            }),
        ],
    );

    let probed = std::sync::Mutex::new(Vec::new());
    let records = runtime
        .collect_run_provider_processes_with(run_id, |pid, token| {
            probed
                .lock()
                .expect("probe lock")
                .push((pid, token.map(str::to_string)));
            ProcessLiveness::Alive
        })
        .expect("collect provider processes");

    assert!(records[0].finished);
    assert_eq!(records[0].pid, 288858);
    assert_eq!(records[0].exit_code, Some(0));
    assert!(!records[1].finished);
    assert_eq!(records[1].pid, 289173);
    assert_eq!(records[1].liveness, ProcessLiveness::Alive);
    assert_eq!(
        probed.into_inner().expect("probed pids"),
        vec![(289173, Some("ps-lstart-utc-v1:live".to_string()))]
    );
}

#[test]
fn parallel_provider_completions_can_arrive_in_reverse_order() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-parallel-reverse";
    seed_v2_audit_events(
        &runtime,
        run_id,
        [
            json!({ "event_id": "run", "run_id": run_id, "body_kind": "run_started" }),
            json!({ "event_id": "step", "run_id": run_id, "parent_event_id": "run", "body_kind": "step_started", "step_id": "pilot" }),
            json!({ "event_id": "first", "run_id": run_id, "parent_event_id": "step", "body_kind": "activity_started" }),
            json!({ "event_id": "second", "run_id": run_id, "parent_event_id": "step", "body_kind": "activity_started" }),
            json!({ "event_id": "pid-first", "run_id": run_id, "parent_event_id": "first", "body_kind": "cli_invocation_process", "pid": 101 }),
            json!({ "event_id": "pid-second", "run_id": run_id, "parent_event_id": "second", "body_kind": "cli_invocation_process", "pid": 202 }),
            json!({ "event_id": "finished-second", "run_id": run_id, "parent_event_id": "second", "body_kind": "cli_invocation_finished", "exit_code": 22 }),
            json!({ "event_id": "finished-first", "run_id": run_id, "parent_event_id": "first", "body_kind": "cli_invocation_finished", "exit_code": 11 }),
        ],
    );

    let records = runtime
        .collect_run_provider_processes_with(run_id, |_, _| panic!("no process remains open"))
        .expect("collect provider processes");

    assert!(records.iter().all(|record| record.finished));
    assert_eq!(records[0].pid, 101);
    assert_eq!(records[0].exit_code, Some(11));
    assert_eq!(records[1].pid, 202);
    assert_eq!(records[1].exit_code, Some(22));
}

#[test]
fn ancestry_free_completions_close_only_an_unambiguous_historical_process() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-historical-ancestry-free";
    seed_v2_audit_events(
        &runtime,
        run_id,
        [
            json!({ "event_id": "pid-one", "run_id": run_id, "body_kind": "cli_invocation_process", "step_id": "pilot", "pid": 101 }),
            json!({ "event_id": "finished", "run_id": run_id, "body_kind": "cli_invocation_finished", "step_id": "pilot", "exit_code": 0 }),
        ],
    );

    let records = runtime
        .collect_run_provider_processes_with(run_id, |_, _| {
            panic!("completed process is not probed")
        })
        .expect("collect provider processes");
    assert!(records[0].finished);

    let ambiguous_run_id = "jrun-historical-ambiguous";
    seed_v2_audit_events(
        &runtime,
        ambiguous_run_id,
        [
            json!({ "event_id": "pid-one", "run_id": ambiguous_run_id, "body_kind": "cli_invocation_process", "step_id": "pilot", "pid": 101 }),
            json!({ "event_id": "pid-two", "run_id": ambiguous_run_id, "body_kind": "cli_invocation_process", "step_id": "pilot", "pid": 202 }),
            json!({ "event_id": "finished", "run_id": ambiguous_run_id, "body_kind": "cli_invocation_finished", "step_id": "pilot", "exit_code": 0 }),
        ],
    );
    let records = runtime
        .collect_run_provider_processes_with(ambiguous_run_id, |_, _| ProcessLiveness::Alive)
        .expect("collect provider processes");
    assert!(records.iter().all(|record| !record.finished));
    assert!(
        records
            .iter()
            .all(|record| record.liveness == ProcessLiveness::Alive)
    );
}

#[test]
fn provider_processes_pair_each_spawn_with_the_exit_that_closes_it() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-provider-processes";
    seed_v2_audit_events(
        &runtime,
        run_id,
        [
            json!({
                "event_id": "evt-run",
                "ts": "2026-07-27T02:41:00Z",
                "run_id": run_id,
                "body_kind": "run_started",
                "job_name": "task_pr_pipeline"
            }),
            json!({
                "event_id": "evt-step-implement",
                "ts": "2026-07-27T02:41:01Z",
                "run_id": run_id,
                "parent_event_id": "evt-run",
                "body_kind": "step_started",
                "step_id": "agent_implement"
            }),
            // First attempt: spawned, then exited nonzero.
            json!({
                "event_id": "evt-pid-one",
                "ts": "2026-07-27T02:41:02Z",
                "run_id": run_id,
                "parent_event_id": "evt-step-implement",
                "body_kind": "cli_invocation_process",
                "provider": "codex",
                "pid": 1111,
                "pid_start_time": "ps-lstart-utc-v1:one"
            }),
            json!({
                "event_id": "evt-cli-one",
                "ts": "2026-07-27T02:41:03Z",
                "run_id": run_id,
                "parent_event_id": "evt-step-implement",
                "body_kind": "cli_invocation_finished",
                "provider": "codex",
                "exit_code": 1,
                "duration_ms": 900,
                "timed_out": false
            }),
            // Retry within the same step: still open.
            json!({
                "event_id": "evt-pid-two",
                "ts": "2026-07-27T02:41:04Z",
                "run_id": run_id,
                "parent_event_id": "evt-step-implement",
                "body_kind": "cli_invocation_process",
                "provider": "codex",
                "pid": 2222,
                "pid_start_time": "ps-lstart-utc-v1:two"
            }),
        ],
    );

    let probed = std::sync::Mutex::new(Vec::new());
    let records = runtime
        .collect_run_provider_processes_with(run_id, |pid, token| {
            probed
                .lock()
                .expect("probe lock")
                .push((pid, token.map(str::to_string)));
            ProcessLiveness::Alive
        })
        .expect("collect provider processes");

    assert_eq!(records.len(), 2);

    assert_eq!(records[0].event_id, "evt-pid-one");
    assert_eq!(records[0].run_id, run_id);
    assert_eq!(records[0].pid, 1111);
    assert_eq!(records[0].step_id.as_deref(), Some("agent_implement"));
    assert_eq!(records[0].step_index, Some(0));
    assert_eq!(records[0].provider.as_deref(), Some("codex"));
    assert!(records[0].finished);
    assert_eq!(records[0].exit_code, Some(1));
    assert_eq!(records[0].duration_ms, Some(900));
    assert!(!records[0].timed_out);
    assert_eq!(records[0].liveness, ProcessLiveness::Exited);

    assert_eq!(records[1].event_id, "evt-pid-two");
    assert_eq!(records[1].pid, 2222);
    assert!(!records[1].finished);
    assert_eq!(records[1].exit_code, None);
    assert_eq!(records[1].liveness, ProcessLiveness::Alive);

    // Only the still-open child is worth probing; a reaped one has an exit code.
    assert_eq!(
        probed.into_inner().expect("probed pids"),
        vec![(2222, Some("ps-lstart-utc-v1:two".to_string()))]
    );
}

#[test]
fn an_open_provider_process_reports_the_probed_liveness_verdict() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-provider-dead";
    seed_v2_audit_events(
        &runtime,
        run_id,
        [
            json!({
                "event_id": "evt-step",
                "ts": "2026-07-27T02:41:01Z",
                "run_id": run_id,
                "body_kind": "step_started",
                "step_id": "agent_implement"
            }),
            json!({
                "event_id": "evt-pid",
                "ts": "2026-07-27T02:41:02Z",
                "run_id": run_id,
                "parent_event_id": "evt-step",
                "body_kind": "cli_invocation_process",
                "provider": "codex",
                "pid": 3333
            }),
        ],
    );

    let records = runtime
        .collect_run_provider_processes_with(run_id, |_, _| ProcessLiveness::Exited)
        .expect("collect provider processes");

    assert_eq!(records.len(), 1);
    assert!(!records[0].finished);
    assert_eq!(records[0].pid_start_time, None);
    // The distinction the surface exists for: an open invocation whose child is
    // gone is a lost implementation agent, not a slow one.
    assert_eq!(records[0].liveness, ProcessLiveness::Exited);
}

#[test]
fn a_run_without_process_events_reports_no_provider_processes() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let records = runtime
        .collect_run_provider_processes("jrun-missing")
        .expect("collect provider processes");
    assert!(records.is_empty());
}
