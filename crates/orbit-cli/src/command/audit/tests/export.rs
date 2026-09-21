use super::super::export::export_csv;

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
