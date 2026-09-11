use orbit_cmd::{WorkspaceDoctorResult, WorkspaceDoctorStatus};
use orbit_core::OrbitRuntime;

use super::super::doctor::{doctor_row_json, human_detail};
use super::super::{CommandOutput, Execute};

#[test]
fn doctor_warning_renders_structured_and_human_remediation() {
    let row = WorkspaceDoctorResult {
        check_name: "task-reservations".to_string(),
        status: WorkspaceDoctorStatus::Warning,
        message: "reservation-123 is stale".to_string(),
        remediation: Some("Run `orbit doctor --fix-stale-task-locks`.".to_string()),
    };

    let json = doctor_row_json(&row);
    assert_eq!(
        json["remediation"],
        "Run `orbit doctor --fix-stale-task-locks`."
    );
    let human = human_detail(&row);
    assert!(human.contains("reservation-123 is stale"), "{human}");
    assert!(
        human.contains("Action: Run `orbit doctor --fix-stale-task-locks`."),
        "{human}"
    );
}

#[test]
fn healthy_doctor_row_has_null_remediation_and_no_action_line() {
    let row = WorkspaceDoctorResult {
        check_name: "task-reservations".to_string(),
        status: WorkspaceDoctorStatus::Ok,
        message: "none stale".to_string(),
        remediation: None,
    };

    assert!(doctor_row_json(&row)["remediation"].is_null());
    assert_eq!(human_detail(&row), "none stale");
}

#[test]
fn failing_workspace_renders_diagnostics_and_exits_nonzero() {
    let runtime = OrbitRuntime::in_memory().expect("build in-memory runtime");
    std::fs::write(runtime.global_root().join("config.toml"), "[").expect("write invalid config");

    let output = super::super::doctor::DoctorCommand {
        json: false,
        fix_stale_locks: false,
        fix_stale_task_locks: false,
        remove_graph: false,
        fix_stale_artifacts: false,
        fix_retired_activity_backends: false,
        fix_orphan_task_stores: false,
    }
    .execute(&runtime)
    .expect("doctor should render a failing report");

    let CommandOutput::Payload(payload) = output else {
        panic!("doctor should return its report payload");
    };
    assert_eq!(payload.exit_code(), 1);
    let (_, view) = payload.into_view();
    let crate::output::payload::View::Blocks(blocks) = view else {
        panic!("doctor should render table blocks");
    };
    let crate::output::payload::Block::Table(table) = &blocks[0] else {
        panic!("doctor should render a table first");
    };
    let rendered = table.render_at(None, false, false).body;
    assert!(rendered.contains("config"), "{rendered}");
    assert!(
        rendered.contains("Action: Address the condition named in the diagnostic details"),
        "{rendered}"
    );
}

#[test]
fn warning_only_workspace_keeps_zero_exit_and_structured_rows() {
    let runtime = OrbitRuntime::in_memory().expect("build in-memory runtime");
    let lock_path = runtime.paths().state_dir.join("doctor-test.lock");
    std::fs::write(
        lock_path,
        r#"{"pid":0,"acquired_at":"2026-08-15T23:00:00Z","label":"test"}"#,
    )
    .expect("write stale lock metadata");

    let output = super::super::doctor::DoctorCommand {
        json: false,
        fix_stale_locks: false,
        fix_stale_task_locks: false,
        remove_graph: false,
        fix_stale_artifacts: false,
        fix_retired_activity_backends: false,
        fix_orphan_task_stores: false,
    }
    .execute(&runtime)
    .expect("doctor should render a warning report");

    let CommandOutput::Payload(payload) = output else {
        panic!("doctor should return its report payload");
    };
    assert_eq!(payload.exit_code(), 0);
    let (document, _) = payload.into_view();
    assert_eq!(
        document
            .as_array()
            .expect("doctor rows")
            .iter()
            .find(|row| row["check"] == "stale-locks")
            .expect("stale-locks row")["status"],
        "warning"
    );
}

#[test]
fn fix_stale_locks_records_repair_count_in_payload_doc() {
    let runtime = OrbitRuntime::in_memory().expect("build in-memory runtime");
    let lock_path = runtime.paths().state_dir.join("doctor-test.lock");
    std::fs::write(
        lock_path,
        r#"{"pid":0,"acquired_at":"2026-08-15T23:00:00Z","label":"test"}"#,
    )
    .expect("write stale lock metadata");

    let output = super::super::doctor::DoctorCommand {
        json: false,
        fix_stale_locks: true,
        fix_stale_task_locks: false,
        remove_graph: false,
        fix_stale_artifacts: false,
        fix_retired_activity_backends: false,
        fix_orphan_task_stores: false,
    }
    .execute(&runtime)
    .expect("doctor should run repairs and render report");

    let CommandOutput::Payload(payload) = output else {
        panic!("doctor should return its report payload");
    };
    assert_eq!(payload.exit_code(), 0);
    let (document, _) = payload.into_view();
    let rows = document.as_array().expect("doctor rows");
    let fix_row = rows
        .iter()
        .find(|row| row["check"] == "fix-stale-locks")
        .expect("fix-stale-locks row");
    assert_eq!(fix_row["status"], "ok");
    assert!(
        fix_row["message"]
            .as_str()
            .expect("message")
            .contains("Removed 1 stale lock file(s)."),
        "expected repair count in fix-stale-locks row message, got: {:?}",
        fix_row["message"]
    );
}
