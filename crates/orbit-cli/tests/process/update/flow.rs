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

fn oversized_mirror_inputs_fail_before_executable_replacement() {
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

fn successful_read_only_inspection_cannot_authorize_a_downgrade() {
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

fn a_replacement_without_admission_protocol_is_refused_before_installation() {
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

fn an_update_refreshes_an_installed_bundled_bubblewrap_with_the_new_release() {
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

fn recovery_hint_targets_requested_version_when_newer_release_is_published() {
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

fn recovery_hint_preserves_allow_downgrade_when_retrying_older_release() {
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

#[test]
fn update_flow_contracts_0() {
    if crate::isolated_test::run_in_child("update::flow::update_flow_contracts_0") {
        return;
    }
    for (name, check) in [
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        (
            "a_hanging_candidate_probe_times_out_without_holding_an_authority",
            a_hanging_candidate_probe_times_out_without_holding_an_authority as fn(),
        ),
        (
            "oversized_mirror_inputs_fail_before_executable_replacement",
            oversized_mirror_inputs_fail_before_executable_replacement as fn(),
        ),
        (
            "successful_read_only_inspection_cannot_authorize_a_downgrade",
            successful_read_only_inspection_cannot_authorize_a_downgrade as fn(),
        ),
        (
            "a_replacement_without_admission_protocol_is_refused_before_installation",
            a_replacement_without_admission_protocol_is_refused_before_installation as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
    }
}

#[test]
fn update_flow_contracts_1() {
    if crate::isolated_test::run_in_child("update::flow::update_flow_contracts_1") {
        return;
    }
    for (name, check) in [
        (
            "an_update_refreshes_an_installed_bundled_bubblewrap_with_the_new_release",
            an_update_refreshes_an_installed_bundled_bubblewrap_with_the_new_release as fn(),
        ),
        (
            "recovery_hint_targets_requested_version_when_newer_release_is_published",
            recovery_hint_targets_requested_version_when_newer_release_is_published as fn(),
        ),
        (
            "recovery_hint_preserves_allow_downgrade_when_retrying_older_release",
            recovery_hint_preserves_allow_downgrade_when_retrying_older_release as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
    }
}

/// The version probe times out only after its real child starts. Exercise the
/// public updater directly so the clock seam avoids a minute of wall-clock wait
/// on macOS; generation admission must remain free during and after the probe.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn a_hanging_candidate_probe_times_out_without_holding_an_authority() {
    use orbit_common::fs::generation::{GenerationGuard, executable_generation};
    use orbit_common::test_env::with_supervision_clock;
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    let fixture = Fixture::new("0.18.0");
    let environment = fixture.environment();
    let authority = &environment.admission_roots[0];
    let probing = authority.with_file_name("probe-started");
    let candidate = format!(
        "#!/bin/sh\nif [ \"$1\" = --version ]; then : > '{}'; exec sleep 600; fi\nexit 0\n",
        probing.display()
    );
    fixture.publish_script("0.19.0", candidate.as_bytes());
    let digest = executable_generation(&fixture.executable).expect("installed digest");
    drop(GenerationGuard::acquire(authority, &digest).expect("initialize generation"));
    let generation = authority.join(".generation.lock");
    let before_generation = std::fs::read(&generation).expect("generation record");
    let before = std::fs::read(&fixture.executable).expect("installed bytes");
    let observed = std::rc::Rc::new(Cell::new(false));
    let clock_observed = observed.clone();
    let clock_roots = environment.admission_roots.clone();
    let origin = Instant::now();
    let error = with_supervision_clock(
        move || {
            if probing.exists() {
                if !clock_observed.replace(true) {
                    drop(
                        orbit_cmd::update::acquire_admissions(&clock_roots)
                            .expect("a live candidate probe must not hold generation admission"),
                    );
                }
                origin + Duration::from_secs(3_601)
            } else {
                origin
            }
        },
        || run_update(&environment, &request()).expect_err("candidate probe must time out"),
    );
    assert!(
        observed.get(),
        "the intended candidate actually started its probe"
    );
    let message = error.to_string();
    assert!(message.contains("did not finish within"), "{message}");
    assert!(message.contains("--version"), "{message}");
    assert!(message.contains("nothing was replaced"), "{message}");
    assert_eq!(std::fs::read(&fixture.executable).unwrap(), before);
    assert_eq!(std::fs::read(&generation).unwrap(), before_generation);
    assert!(
        !fixture.staging_file_remains(),
        "timed-out staging must be cleaned"
    );
    assert!(!super::integrity::backup_path(&fixture.executable).exists());
    drop(
        orbit_cmd::update::acquire_admissions(&environment.admission_roots)
            .expect("the timed-out update must release every authority"),
    );
}
