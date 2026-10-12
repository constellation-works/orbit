//! `orbit_cmd::update::run_update` against a fake installation and mirror:
//! input bounds, candidate admission, downgrade authority, the bundled
//! Bubblewrap refresh and the recovery command a failed convergence prints.

use orbit_cmd::update::channel::release_archive_name;
use orbit_cmd::update::source::MIRROR_LATEST_FILE;
use orbit_cmd::update::{UpdateOutcome, UpdateReport, UpdateRequest, run_update};
use orbit_common::security::release::{
    RELEASE_CHECKSUMS_FILENAME, RELEASE_CHECKSUMS_SIGNATURE_FILENAME,
};

use super::release_fixture::{FakeBinary, Fixture, TEST_TARGET, request};

/// Larger than every release input limit; sparse, so it costs no disk.
const OVERSIZED_INPUT_BYTES: u64 = 1 << 30;

#[test]
fn oversized_mirror_inputs_fail_before_executable_replacement() {
    if crate::isolated_test::run_in_child(
        "update::flow::oversized_mirror_inputs_fail_before_executable_replacement",
    ) {
        return;
    }

    for asset_class in [
        MIRROR_LATEST_FILE,
        RELEASE_CHECKSUMS_FILENAME,
        RELEASE_CHECKSUMS_SIGNATURE_FILENAME,
        "archive",
    ] {
        let fixture = Fixture::new("0.18.0");
        fixture.publish("0.19.0", FakeBinary::Healthy);
        let asset = if asset_class == "archive" {
            release_archive_name(TEST_TARGET)
        } else {
            asset_class.to_string()
        };
        let input = fixture.mirror_input("0.19.0", &asset);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&input)
            .expect("published input")
            .set_len(OVERSIZED_INPUT_BYTES)
            .expect("oversized sparse input");
        let original = std::fs::read(&fixture.executable).expect("installed executable");
        let error =
            run_update(&fixture.environment(), &request()).expect_err("oversized mirror input");
        assert!(
            error.to_string().contains("-byte release input limit"),
            "{asset}: {error}"
        );
        assert_eq!(
            std::fs::read(&fixture.executable).expect("installed executable"),
            original
        );
        assert!(!fixture.staging_file_remains());
    }
}

#[test]
fn successful_read_only_inspection_cannot_authorize_a_downgrade() {
    if crate::isolated_test::run_in_child(
        "update::flow::successful_read_only_inspection_cannot_authorize_a_downgrade",
    ) {
        return;
    }

    let fixture = Fixture::new("0.19.0");
    fixture.publish("0.18.0", FakeBinary::ReadOnlyStore);
    let mut requested = request();
    requested.allow_downgrade = true;
    let before = std::fs::read(&fixture.executable).expect("old executable");
    run_update(&fixture.environment(), &requested).expect_err("read-only candidate refused");
    assert_eq!(
        std::fs::read(&fixture.executable).expect("old executable"),
        before
    );
}

#[test]
fn a_replacement_without_admission_protocol_is_refused_before_installation() {
    if crate::isolated_test::run_in_child(
        "update::flow::a_replacement_without_admission_protocol_is_refused_before_installation",
    ) {
        return;
    }

    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::NoAdmissionContract);
    let before = std::fs::read(&fixture.executable).expect("installed executable");
    let error = run_update(&fixture.environment(), &request()).expect_err("unprotected candidate");
    assert!(
        error
            .to_string()
            .contains("does not support executable generation admission")
    );
    assert_eq!(
        std::fs::read(&fixture.executable).expect("installed executable"),
        before
    );
    assert!(fixture.invocations().is_empty());
}

#[test]
fn an_update_refreshes_an_installed_bundled_bubblewrap_with_the_new_release() {
    if crate::isolated_test::run_in_child(
        "update::flow::an_update_refreshes_an_installed_bundled_bubblewrap_with_the_new_release",
    ) {
        return;
    }

    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    let mut environment = fixture.environment();
    environment.bundled_bwrap_installed = true;

    let report = run_update(&environment, &request()).expect("update");

    assert_eq!(report.exit_code(), 0, "{report:?}");
    let refresh = "0.19.0: init --host-prerequisites-only --non-interactive";
    assert_eq!(
        fixture.invocations().last().map(String::as_str),
        Some(refresh),
        "the replacement executable must refresh the bundled Bubblewrap, without --root"
    );

    // A host without one installed never has its Bubblewrap touched.
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    run_update(&fixture.environment(), &request()).expect("update");
    assert!(
        !fixture
            .invocations()
            .iter()
            .any(|line| line.contains("init")),
        "{:?}",
        fixture.invocations()
    );
}

fn parse_retry_request_from_recovery(report: &UpdateReport) -> UpdateRequest {
    let recovery = report.recovery.as_deref().expect("recovery text");
    let marker = "Re-run `";
    let start = recovery.find(marker).expect("Re-run marker") + marker.len();
    let end = recovery[start..].find('`').expect("closing backtick") + start;
    let command = &recovery[start..end];
    let mut request = UpdateRequest::default();
    let words: Vec<&str> = command.split_whitespace().collect();
    let mut i = 0;
    while i < words.len() {
        if words[i] == "--version" && i + 1 < words.len() {
            request.target_version = Some(words[i + 1].to_string());
            i += 2;
        } else if words[i] == "--allow-downgrade" {
            request.allow_downgrade = true;
            i += 1;
        } else {
            i += 1;
        }
    }
    request
}

#[test]
fn recovery_hint_targets_requested_version_when_newer_release_is_published() {
    if crate::isolated_test::run_in_child(
        "update::flow::recovery_hint_targets_requested_version_when_newer_release_is_published",
    ) {
        return;
    }

    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::FailsMigrationConfirm);
    fixture.publish("0.20.0", FakeBinary::Healthy);

    let mut requested = request();
    requested.target_version = Some("0.19.0".to_string());
    let report = run_update(&fixture.environment(), &requested).expect("update");

    assert_eq!(report.outcome, UpdateOutcome::NeedsRecovery);
    assert_eq!(report.target_version, "0.19.0");
    assert!(report.replaced);

    let retry_request = parse_retry_request_from_recovery(&report);
    assert_eq!(retry_request.target_version.as_deref(), Some("0.19.0"));
    assert!(!retry_request.allow_downgrade);

    fixture.repair_migration();
    let retry_report = run_update(&fixture.environment(), &retry_request).expect("retry update");

    assert_eq!(retry_report.outcome, UpdateOutcome::AlreadyCurrent);
    assert_eq!(retry_report.target_version, "0.19.0");
    assert_eq!(fixture.installed_reports(), "orbit 0.19.0");
}

#[test]
fn recovery_hint_preserves_allow_downgrade_when_retrying_older_release() {
    if crate::isolated_test::run_in_child(
        "update::flow::recovery_hint_preserves_allow_downgrade_when_retrying_older_release",
    ) {
        return;
    }

    let fixture = Fixture::new("0.19.0");
    fixture.publish("0.18.0", FakeBinary::FailsMigrationConfirm);
    fixture.publish("0.20.0", FakeBinary::Healthy);

    let mut requested = request();
    requested.target_version = Some("0.18.0".to_string());
    requested.allow_downgrade = true;
    let report = run_update(&fixture.environment(), &requested).expect("downgrade update");

    assert_eq!(report.outcome, UpdateOutcome::NeedsRecovery);
    assert_eq!(report.target_version, "0.18.0");
    assert_eq!(report.current_version, "0.19.0");
    assert!(report.replaced);

    let retry_request = parse_retry_request_from_recovery(&report);
    assert_eq!(retry_request.target_version.as_deref(), Some("0.18.0"));
    assert!(retry_request.allow_downgrade);

    fixture.repair_migration();
    let retry_report = run_update(&fixture.environment(), &retry_request).expect("retry downgrade");

    assert_eq!(retry_report.outcome, UpdateOutcome::AlreadyCurrent);
    assert_eq!(retry_report.target_version, "0.18.0");
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
}
