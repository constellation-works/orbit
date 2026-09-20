use std::fs;

use chrono::Utc;
use orbit_types::workflow::{JobRun, JobRunState};
use tempfile::TempDir;

use super::super::legacy_state::{import_legacy_v2_state, import_marker_key};
use crate::Store;

#[test]
fn import_is_marker_gated() {
    let temp = TempDir::new().expect("tempdir");
    let orbit = temp.path().join(".orbit");
    fs::create_dir_all(orbit.join("state/audit/v2_loop")).expect("audit dir");
    let event = serde_json::json!({
        "schemaVersion": 1,
        "event_type": "tool.denied",
        "event_id": "evt-1",
        "ts": "2026-01-01T00:00:00Z",
        "run_id": "run-1",
        "agent_identity": "codex",
        "body_kind": "tool_denied",
        "tool_name": "fs.read",
        "reason": "denied"
    });
    fs::write(
        orbit.join("state/audit/v2_loop/run-1.jsonl"),
        format!("{event}\n"),
    )
    .expect("write event");

    let run = JobRun {
        executed_on: None,
        run_id: "run-1".to_string(),
        job_id: "job-a".to_string(),
        attempt: 1,
        state: JobRunState::Success,
        scheduled_at: Utc::now(),
        started_at: None,
        finished_at: None,
        duration_ms: None,
        created_at: Utc::now(),
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };
    let run_dir = orbit.join("state/job-runs/job-a/run-1");
    fs::create_dir_all(&run_dir).expect("run dir");
    fs::write(
        run_dir.join("jrun.yaml"),
        serde_yaml::to_string(&serde_json::json!({"schema_version": 1, "run": run}))
            .expect("serialize"),
    )
    .expect("write run");
    let store = Store::open_in_memory().expect("store");
    let first = import_legacy_v2_state(&store, &orbit, "ws_a").expect("first");
    assert!(!first.skipped);
    assert_eq!(first.audit_events_inserted, 1);
    assert_eq!(first.job_runs_inserted, 1);

    let second = import_legacy_v2_state(&store, &orbit, "ws_a").expect("second");
    assert!(second.skipped);
    assert_eq!(
        store
            .count_v2_audit_events(&crate::V2AuditEventFilter {
                workspace_id: "ws_a".to_string(),
                ..Default::default()
            })
            .expect("count"),
        1
    );
}

#[test]
fn import_sets_marker_even_when_audit_lines_are_skipped() {
    let temp = TempDir::new().expect("tempdir");
    let orbit = temp.path().join(".orbit");
    fs::create_dir_all(orbit.join("state/audit/v2_loop")).expect("audit dir");
    fs::write(orbit.join("state/audit/v2_loop/run-1.jsonl"), "not-json\n")
        .expect("write malformed event");

    let store = Store::open_in_memory().expect("store");
    let first = import_legacy_v2_state(&store, &orbit, "ws_a").expect("first");
    assert!(!first.skipped);
    assert_eq!(first.audit_events_inserted, 0);
    assert_eq!(first.audit_events_skipped, 1);
    assert!(first.skipped_records());
    assert!(
        store
            .schema_meta_value(&import_marker_key("ws_a"))
            .expect("marker read")
            .is_some()
    );

    let second = import_legacy_v2_state(&store, &orbit, "ws_a").expect("second");
    assert!(second.skipped);
}

#[test]
fn import_skips_malformed_job_run_and_step_files() {
    let temp = TempDir::new().expect("tempdir");
    let orbit = temp.path().join(".orbit");

    let valid_run = JobRun {
        executed_on: None,
        run_id: "run-good".to_string(),
        job_id: "job-a".to_string(),
        attempt: 1,
        state: JobRunState::Success,
        scheduled_at: Utc::now(),
        started_at: None,
        finished_at: None,
        duration_ms: None,
        created_at: Utc::now(),
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };
    let valid_dir = orbit.join("state/job-runs/job-a/run-good");
    fs::create_dir_all(valid_dir.join("steps")).expect("valid run dir");
    fs::write(
        valid_dir.join("jrun.yaml"),
        serde_yaml::to_string(&serde_json::json!({"schema_version": 1, "run": valid_run}))
            .expect("serialize run"),
    )
    .expect("write valid run");
    fs::write(valid_dir.join("steps/000.json"), "not-json\n").expect("write bad step");

    let malformed_dir = orbit.join("state/job-runs/job-a/run-bad");
    fs::create_dir_all(&malformed_dir).expect("malformed run dir");
    fs::write(malformed_dir.join("jrun.yaml"), "run: [").expect("write bad run");

    let store = Store::open_in_memory().expect("store");
    let report = import_legacy_v2_state(&store, &orbit, "ws_a").expect("import");

    assert_eq!(report.job_runs_inserted, 1);
    assert_eq!(report.job_runs_skipped, 1);
    assert_eq!(report.job_run_steps_inserted, 0);
    assert_eq!(report.job_run_steps_skipped, 1);
    assert!(report.skipped_records());
    let loaded = store
        .get_job_run_for_workspace("ws_a", "run-good")
        .expect("read valid run")
        .expect("valid run inserted");
    assert_eq!(loaded.job_id, "job-a");
}
