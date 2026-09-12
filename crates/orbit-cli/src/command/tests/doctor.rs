use clap::CommandFactory;
use orbit_cmd::{OrphanTaskStoreRemoval, WorkspaceDoctorResult, WorkspaceDoctorStatus};
use orbit_core::OrbitRuntime;

use std::path::PathBuf;

use orbit_core::application::routines::{ClockUnitInspection, ClockUnitVerdict};

use super::super::doctor::{
    clock_unit_row_from_inspection, doctor_row_json, human_detail,
    orphan_task_store_removal_message,
};
use super::super::{Cli, CommandOutput, Execute};

fn clock_unit_inspection(
    verdict: ClockUnitVerdict,
    program_version: Option<&str>,
) -> ClockUnitInspection {
    ClockUnitInspection {
        unit_path: Some(PathBuf::from(
            "/Users/daniel/Library/LaunchAgents/com.orbit.sweep.plist",
        )),
        program_path: Some(PathBuf::from("/opt/homebrew/bin/orbit")),
        program_version: program_version.map(ToString::to_string),
        running_path: PathBuf::from("/Users/daniel/.cargo/bin/orbit"),
        running_version: "0.21.0".to_string(),
        verdict,
    }
}

#[test]
fn clock_unit_version_mismatch_is_a_named_failure() {
    let row = clock_unit_row_from_inspection(&clock_unit_inspection(
        ClockUnitVerdict::VersionMismatch,
        Some("0.20.0"),
    ));
    assert_eq!(row.check_name, "clock-unit");
    assert_eq!(row.status, WorkspaceDoctorStatus::Error);
    assert!(
        row.message.contains("/opt/homebrew/bin/orbit"),
        "{}",
        row.message
    );
    assert!(
        row.message.contains("/Users/daniel/.cargo/bin/orbit"),
        "{}",
        row.message
    );
    assert!(row.message.contains("0.20.0"), "{}", row.message);
    assert!(row.message.contains("0.21.0"), "{}", row.message);
    assert!(
        row.remediation
            .as_deref()
            .expect("remediation")
            .contains("orbit clock enable")
    );
}

#[test]
fn clock_unit_path_only_mismatch_is_a_warning() {
    let row = clock_unit_row_from_inspection(&clock_unit_inspection(
        ClockUnitVerdict::PathMismatch,
        Some("0.21.0"),
    ));
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning);
    assert!(row.message.contains("Two installs"), "{}", row.message);
}

#[test]
fn clock_unit_matching_is_ok() {
    let row = clock_unit_row_from_inspection(&clock_unit_inspection(
        ClockUnitVerdict::Matching,
        Some("0.21.0"),
    ));
    assert_eq!(row.status, WorkspaceDoctorStatus::Ok);
    assert!(row.remediation.is_none());
}

#[test]
fn clock_unit_absent_is_skipped() {
    let row = clock_unit_row_from_inspection(&ClockUnitInspection {
        unit_path: None,
        program_path: None,
        program_version: None,
        running_path: PathBuf::from("/Users/daniel/.cargo/bin/orbit"),
        running_version: "0.21.0".to_string(),
        verdict: ClockUnitVerdict::NoUnitInstalled,
    });
    assert_eq!(row.status, WorkspaceDoctorStatus::Skipped);
}

#[test]
fn clock_unit_unrunnable_is_a_warning() {
    let row = clock_unit_row_from_inspection(&clock_unit_inspection(
        ClockUnitVerdict::Unrunnable {
            reason: "program does not exist: /opt/homebrew/bin/orbit".to_string(),
        },
        None,
    ));
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning);
    assert!(
        row.message.contains("could not report a version"),
        "{}",
        row.message
    );
}

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
        confirm: false,
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
        confirm: false,
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
        confirm: false,
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

/// [ORB-12144] A repair that deletes a populated stale partition must not be
/// reported the same way as an empty one: the message names both counts and
/// the destroyed task bundles, and never calls the populated partition empty.
#[test]
fn orphan_task_store_removal_message_reports_populated_partitions_and_bundles_separately() {
    let removed = OrphanTaskStoreRemoval {
        empty_partitions: 1,
        populated_partitions: 2,
        task_bundles: 5,
    };

    let message = orphan_task_store_removal_message(&removed);

    assert_eq!(
        message,
        "Removed 1 empty orphaned task-store partition(s) and 2 populated partition(s) \
         (5 task bundle(s))."
    );
    assert!(
        !message.contains("2 empty"),
        "the populated partitions must not be reported as empty: {message}"
    );
}

/// A repair that removes nothing still reports both counts explicitly rather
/// than a bare "removed 0 empty partitions" that hides whether bundles were
/// ever at risk.
#[test]
fn orphan_task_store_removal_message_reports_zero_of_both_kinds() {
    let message = orphan_task_store_removal_message(&OrphanTaskStoreRemoval::default());

    assert_eq!(
        message,
        "Removed 0 empty orphaned task-store partition(s) and 0 populated partition(s) \
         (0 task bundle(s))."
    );
}

/// [ORB-12171] The help text for `--fix-orphan-task-stores` and `--confirm`
/// must accurately document that populated partitions whose checkout binding
/// is confirmed absent are deleted along with their task bundles, rather than
/// promising that partitions with task bundles are never deleted.
#[test]
fn fix_orphan_task_stores_help_documents_bundle_deletion_on_confirmed_absent_checkouts() {
    let mut command = Cli::command();
    let doctor = command
        .find_subcommand_mut("doctor")
        .expect("doctor subcommand");
    let help = doctor.render_long_help().to_string();

    assert!(
        help.contains("--fix-orphan-task-stores"),
        "expected --fix-orphan-task-stores in doctor help: {help}"
    );
    assert!(
        help.contains("Delete empty unclaimed task-store partitions, and populated partitions whose bound checkout is confirmed absent (including their task bundles)."),
        "expected accurate fix-orphan-task-stores description in help: {help}"
    );
    assert!(
        help.contains("Unowned or unreachable populated partitions are never touched."),
        "expected guard documentation in help: {help}"
    );
    assert!(
        !help.contains("Partitions that still hold task bundles are never deleted"),
        "help must not promise populated partitions are never deleted: {help}"
    );
    assert!(
        help.contains("Required by --fix-orphan-task-stores, which deletes partition directories and their task bundles"),
        "expected --confirm help to mention task bundle deletion: {help}"
    );
}

#[test]
fn fix_orphan_task_stores_without_confirm_fails_with_bundle_loss_message() {
    let runtime = OrbitRuntime::in_memory().expect("build in-memory runtime");
    let err = super::super::doctor::DoctorCommand {
        json: false,
        fix_stale_locks: false,
        fix_stale_task_locks: false,
        remove_graph: false,
        fix_stale_artifacts: false,
        fix_retired_activity_backends: false,
        fix_orphan_task_stores: true,
        confirm: false,
    }
    .execute(&runtime)
    .expect_err("fix_orphan_task_stores must require confirm");

    assert!(
        err.to_string().contains(
            "--fix-orphan-task-stores deletes task-store partition directories and their task bundles. Pass --confirm to proceed."
        ),
        "expected error message to mention bundle loss, got: {err}"
    );
}
