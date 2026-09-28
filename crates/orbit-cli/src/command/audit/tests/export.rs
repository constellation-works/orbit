use std::io::Write;

use chrono::Utc;
use orbit_core::{AuditEvent, AuditEventStatus, OrbitError, OrbitRuntime};
use serde_json::Value;

use super::super::export::{
    AuditExportArgs, ExportFormat, export_csv, export_json, write_json_export,
};
use crate::command::Execute;

#[test]
fn csv_header_appends_trusted_mcp_provenance_columns() {
    let file = tempfile::NamedTempFile::new().expect("temporary CSV");
    let path = file.path().to_str().expect("UTF-8 temp path");
    export_csv(path, &[]).expect("export empty CSV");

    let csv = std::fs::read_to_string(path).expect("read exported CSV");
    let header = csv.lines().next().expect("CSV header");
    assert!(header.contains("host,pid,session_id,workspace_id,caller_machine_id"));
    assert!(header.contains("process_machine_name,transport,effective_capabilities"));
    assert!(header.contains("origin_session_id,mcp_call_id,trace_id,caller_ip,lease_id"));
    assert!(header.ends_with("task_id,job_run_id,activity_id,step_index"));
}

fn sample_event(id: i64) -> AuditEvent {
    AuditEvent {
        id,
        execution_id: format!("exec-{id}"),
        timestamp: Utc::now(),
        command: "tool".to_string(),
        subcommand: Some("orbit.task.show".to_string()),
        tool_name: Some("orbit.task.show".to_string()),
        target_type: Some("task".to_string()),
        target_id: Some("ORB-13494".to_string()),
        role: "agent".to_string(),
        status: AuditEventStatus::Success,
        exit_code: 0,
        duration_ms: 25,
        working_directory: "/workspace".to_string(),
        arguments_json: Some(r#"{"id":"ORB-13494"}"#.to_string()),
        stdout_truncated: None,
        stderr_truncated: None,
        error_message: None,
        host: Some("orbit.local".to_string()),
        pid: 4321,
        session_id: Some("session-abc".to_string()),
        workspace_id: Some("ws-orbit".to_string()),
        caller_machine_id: None,
        caller_machine_name: None,
        process_machine_id: None,
        process_machine_name: None,
        transport: None,
        trace_id: None,
        caller_ip: None,
        effective_capabilities: Default::default(),
        origin_session_id: None,
        mcp_call_id: None,
        lease_id: None,
        task_id: Some("ORB-13494".to_string()),
        job_run_id: Some("jrun-123".to_string()),
        activity_id: Some("implement".to_string()),
        step_index: Some(1),
        self_reported_actor: None,
        plugin: None,
        plugin_secrets: Vec::new(),
        plugin_secret_updates: Default::default(),
        brokered: false,
        peer_pid: None,
    }
}

#[test]
fn export_json_empty_produces_complete_parseable_json() {
    let file = tempfile::NamedTempFile::new().expect("temporary JSON");
    let path = file.path().to_str().expect("UTF-8 temp path");
    export_json(path, &[]).expect("export empty JSON");

    let raw = std::fs::read_to_string(path).expect("read exported JSON");
    let parsed: Value = serde_json::from_str(&raw).expect("parse JSON array");
    let array = parsed.as_array().expect("root must be a JSON array");
    assert!(array.is_empty(), "expected empty JSON array for no events");
}

#[test]
fn export_json_nonempty_produces_complete_parseable_json() {
    let file = tempfile::NamedTempFile::new().expect("temporary JSON");
    let path = file.path().to_str().expect("UTF-8 temp path");
    let events = vec![sample_event(1), sample_event(2)];
    export_json(path, &events).expect("export nonempty JSON");

    let raw = std::fs::read_to_string(path).expect("read exported JSON");
    let parsed: Value = serde_json::from_str(&raw).expect("parse JSON array");
    let array = parsed.as_array().expect("root must be a JSON array");
    assert_eq!(array.len(), 2);
    assert_eq!(array[0]["id"], 1);
    assert_eq!(array[0]["command"], "tool");
    assert_eq!(array[0]["subcommand"], "orbit.task.show");
    assert_eq!(array[1]["id"], 2);
}

struct FlushFailingWriter;

impl Write for FlushFailingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("simulated flush failure"))
    }
}

#[test]
fn write_json_export_propagates_buffered_flush_error() {
    let writer = FlushFailingWriter;
    let buffered = std::io::BufWriter::new(writer);
    let err = write_json_export(buffered, &[], "simulated_sink")
        .expect_err("flush failure must propagate");
    assert!(
        matches!(err, OrbitError::Io(_)),
        "expected OrbitError::Io, got: {err:?}"
    );
    let err_str = err.to_string();
    assert!(
        err_str.contains("flush simulated_sink: simulated flush failure"),
        "expected flush error context, got: {err_str}"
    );
}

#[test]
fn export_json_fails_when_destination_flush_fails() {
    let dev_full = std::path::Path::new("/dev/full");
    if !dev_full.exists() {
        return;
    }

    let err = export_json("/dev/full", &[]).expect_err("flush to /dev/full must fail");
    assert!(
        matches!(err, OrbitError::Io(_)),
        "expected OrbitError::Io, got: {err:?}"
    );
    let err_str = err.to_string();
    assert!(
        err_str.contains("flush /dev/full"),
        "expected flush failure on /dev/full, got: {err_str}"
    );
}

#[test]
fn audit_export_command_fails_nonzero_on_buffered_flush_error() {
    let dev_full = std::path::Path::new("/dev/full");
    if !dev_full.exists() {
        return;
    }

    let runtime = OrbitRuntime::in_memory().expect("in-memory runtime");
    let args = AuditExportArgs {
        format: ExportFormat::Json,
        output: "/dev/full".to_string(),
        since: None,
        tool: None,
    };
    let err = args
        .execute(&runtime)
        .expect_err("execute must fail when destination flush fails");
    assert!(
        matches!(err, OrbitError::Io(_)),
        "expected OrbitError::Io, got: {err:?}"
    );
    let err_str = err.to_string();
    assert!(
        err_str.contains("flush /dev/full"),
        "expected flush failure on /dev/full, got: {err_str}"
    );
}
