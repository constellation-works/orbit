use std::path::Path;
use std::sync::{Arc, Barrier};

use crate::update::channel::{
    CANONICAL_HOMEBREW_FORMULA, InstallChannel, LEGACY_HOMEBREW_FORMULA, homebrew_remediation,
};
use crate::update::tests::fixture::{
    FakeBinary, Fixture, PausingLatestSource, request, tar_gz, tar_gz_named,
};
use crate::update::{
    EXIT_NEEDS_RECOVERY, EXIT_UPDATE_AVAILABLE, UpdateEnvironment, UpdateOutcome, run_update,
};

#[test]
fn updating_to_latest_replaces_the_binary_then_migrates_before_syncing_assets() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);

    let report = run_update(&fixture.environment(), &request()).expect("update succeeds");

    assert_eq!(report.outcome, UpdateOutcome::Updated);
    assert_eq!(report.exit_code(), 0);
    assert!(report.replaced);
    assert_eq!(report.current_version, "0.18.0");
    assert_eq!(report.target_version, "0.19.0");
    assert_eq!(report.signing_key_id.as_deref(), Some("orbit-test-key-1"));
    assert!(report.archive_sha256.is_some());
    assert_eq!(fixture.installed_reports(), "orbit 0.19.0");
    // The replacement executable is the one that converges state, and layout
    // migration runs before managed-asset reconciliation.
    assert_eq!(
        fixture.invocations(),
        vec![
            fixture.invocation("0.19.0", "migrate --confirm"),
            fixture.invocation("0.19.0", "workspace sync"),
        ]
    );
    assert_eq!(report.workspace_root, Some(fixture.workspace_root()));
    assert!(report.steps.iter().all(|step| !step.failed()));
}

#[test]
fn an_explicit_version_is_installed_instead_of_the_newest_one() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    fixture.publish("0.20.0", FakeBinary::Healthy);

    let mut requested = request();
    requested.target_version = Some("v0.19.0".to_string());
    let report = run_update(&fixture.environment(), &requested).expect("update succeeds");

    assert_eq!(report.target_version, "0.19.0");
    assert_eq!(fixture.installed_reports(), "orbit 0.19.0");
}

#[test]
fn check_reports_the_available_version_without_downloading_or_replacing() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);

    let mut requested = request();
    requested.check = true;
    let report = run_update(&fixture.environment(), &requested).expect("check succeeds");

    assert_eq!(report.outcome, UpdateOutcome::UpdateAvailable);
    assert_eq!(report.exit_code(), EXIT_UPDATE_AVAILABLE);
    assert!(!report.replaced);
    assert!(report.archive_sha256.is_none());
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
    assert!(fixture.invocations().is_empty());
    assert_eq!(fixture.install_dir_entries(), vec!["orbit".to_string()]);
}

#[test]
fn check_answers_availability_even_where_a_package_manager_owns_the_install() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    let mut environment = fixture.environment();
    environment.install_channel = InstallChannel::Homebrew {
        remediation: Some(
            "Homebrew owns this installation; run \
             `brew update && brew upgrade constellation-works/tap/orbit`"
                .to_string(),
        ),
    };

    let mut requested = request();
    requested.check = true;
    let report = run_update(&environment, &requested).expect("check succeeds");

    assert_eq!(report.outcome, UpdateOutcome::UpdateAvailable);
    assert!(!report.updatable);
    assert!(
        report
            .remediation
            .as_deref()
            .is_some_and(|text| text.contains("brew upgrade constellation-works/tap/orbit")),
        "{:?}",
        report.remediation
    );
}

#[test]
fn a_legacy_tap_install_is_refused_with_a_migration_that_preserves_unrelated_taps() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    let mut environment = fixture.environment();
    environment.install_channel = InstallChannel::Homebrew {
        remediation: Some(homebrew_remediation(Ok(vec![
            LEGACY_HOMEBREW_FORMULA.to_string(),
        ]))),
    };

    let error =
        run_update(&environment, &request()).expect_err("a legacy Homebrew install is refused");

    let message = error.to_string();
    assert!(
        message.contains(&format!("brew uninstall {LEGACY_HOMEBREW_FORMULA}")),
        "{message}"
    );
    assert!(
        message.contains(&format!("brew install {CANONICAL_HOMEBREW_FORMULA}")),
        "{message}"
    );
    // Nothing was downloaded or replaced: the channel guard runs before staging.
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
    assert_eq!(fixture.install_dir_entries(), vec!["orbit".to_string()]);
}

#[test]
fn a_canonical_tap_install_is_refused_with_an_ordinary_qualified_upgrade() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    let mut environment = fixture.environment();
    environment.install_channel = InstallChannel::Homebrew {
        remediation: Some(homebrew_remediation(Ok(vec![
            CANONICAL_HOMEBREW_FORMULA.to_string(),
        ]))),
    };

    let error =
        run_update(&environment, &request()).expect_err("a canonical Homebrew install is refused");

    let message = error.to_string();
    assert!(
        message.contains(&format!(
            "brew update && brew upgrade {CANONICAL_HOMEBREW_FORMULA}"
        )),
        "{message}"
    );
    assert!(!message.contains("uninstall"), "{message}");
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
    assert_eq!(fixture.install_dir_entries(), vec!["orbit".to_string()]);
}

#[test]
fn a_package_manager_install_is_refused_before_anything_is_downloaded() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    let mut environment = fixture.environment();
    environment.install_channel = InstallChannel::Npm;

    let error = run_update(&environment, &request()).expect_err("npm install is not updatable");

    assert!(
        error
            .to_string()
            .contains("npm install -g @orbit-tools/cli@0.19.0"),
        "{error}"
    );
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
    assert_eq!(fixture.install_dir_entries(), vec!["orbit".to_string()]);
}

#[test]
fn rerunning_at_the_installed_version_reconverges_without_replacing_anything() {
    let fixture = Fixture::new("0.19.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);

    let report = run_update(&fixture.environment(), &request()).expect("resume succeeds");

    assert_eq!(report.outcome, UpdateOutcome::AlreadyCurrent);
    assert_eq!(report.exit_code(), 0);
    assert!(!report.replaced);
    assert!(report.backup_path.is_none());
    // Convergence still runs: re-running `orbit update` is the documented way
    // to finish a run whose migration or sync failed.
    assert_eq!(
        fixture.invocations(),
        vec![
            fixture.invocation("0.19.0", "migrate --confirm"),
            fixture.invocation("0.19.0", "workspace sync"),
        ]
    );
}

#[test]
fn a_downgrade_is_refused_until_it_is_asked_for_explicitly() {
    let fixture = Fixture::new("0.19.0");
    fixture.publish("0.18.0", FakeBinary::Healthy);

    let error = run_update(&fixture.environment(), &request()).expect_err("downgrade is refused");

    assert!(error.to_string().contains("--allow-downgrade"), "{error}");
    assert!(error.to_string().contains("0.18.0"), "{error}");
    assert_eq!(fixture.installed_reports(), "orbit 0.19.0");
}

#[test]
fn an_explicit_prerelease_downgrade_is_refused_until_it_is_asked_for() {
    let fixture = Fixture::new("0.19.0-rc.10");
    fixture.publish("0.19.0-rc.2", FakeBinary::Healthy);

    let mut requested = request();
    requested.target_version = Some("0.19.0-rc.2".to_string());
    let error = run_update(&fixture.environment(), &requested).expect_err("downgrade is refused");

    assert!(error.to_string().contains("--allow-downgrade"), "{error}");
    assert!(error.to_string().contains("0.19.0-rc.2"), "{error}");
    assert_eq!(fixture.installed_reports(), "orbit 0.19.0-rc.10");
    assert!(fixture.invocations().is_empty());
}

#[test]
fn an_incompatible_downgrade_is_caught_before_the_binary_is_replaced() {
    let fixture = Fixture::new("0.19.0");
    // The older release cannot open state the newer one already migrated, so
    // its `migrate --dry-run` pre-flight fails.
    fixture.publish("0.18.0", FakeBinary::MigrationFails);

    let mut requested = request();
    requested.allow_downgrade = true;
    let cwd_b = fixture.workspace.join("checkout-b");
    let root_a = fixture.workspace.join("root-a");
    std::fs::create_dir_all(&cwd_b).expect("create alternate cwd");
    let environment = fixture.environment_for_workspace(cwd_b.clone(), root_a.clone());
    let error = run_update(&environment, &requested).expect_err("incompatible downgrade");

    assert!(
        error.to_string().contains("cannot open this workspace"),
        "{error}"
    );
    assert!(
        error.to_string().contains("nothing was replaced"),
        "{error}"
    );
    assert_eq!(fixture.installed_reports(), "orbit 0.19.0");
    assert_eq!(
        fixture.invocations(),
        vec![format!(
            "0.18.0: --root {} migrate --dry-run",
            root_a.display()
        )]
    );
    assert!(
        fixture
            .invocations()
            .iter()
            .all(|invocation| !invocation.contains(&cwd_b.display().to_string())),
        "downgrade probe leaked the caller cwd into root selection: {:?}",
        fixture.invocations()
    );
    assert_eq!(
        fixture.install_dir_entries(),
        vec!["orbit".to_string(), ".orbit-update.lock".to_string()]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
}

#[test]
fn an_incompatible_prerelease_downgrade_runs_the_compatibility_preflight() {
    let fixture = Fixture::new("0.19.0-rc.10");
    fixture.publish("0.19.0-rc.2", FakeBinary::MigrationFails);

    let mut requested = request();
    requested.target_version = Some("0.19.0-rc.2".to_string());
    requested.allow_downgrade = true;
    let error = run_update(&fixture.environment(), &requested).expect_err("incompatible downgrade");

    assert!(
        error.to_string().contains("cannot open this workspace"),
        "{error}"
    );
    assert!(
        error.to_string().contains("nothing was replaced"),
        "{error}"
    );
    assert_eq!(fixture.installed_reports(), "orbit 0.19.0-rc.10");
    assert_eq!(
        fixture.invocations(),
        vec![fixture.invocation("0.19.0-rc.2", "migrate --dry-run")]
    );
}

#[test]
fn a_compatible_downgrade_proceeds_when_it_is_asked_for() {
    let fixture = Fixture::new("0.19.0");
    fixture.publish("0.18.0", FakeBinary::Healthy);

    let mut requested = request();
    requested.allow_downgrade = true;
    let report = run_update(&fixture.environment(), &requested).expect("downgrade succeeds");

    assert_eq!(report.outcome, UpdateOutcome::Updated);
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
}

#[test]
fn a_tampered_archive_fails_the_checksum_and_never_reaches_the_install_path() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    fixture.tamper_with_archive("0.19.0");

    let error = run_update(&fixture.environment(), &request()).expect_err("checksum mismatch");

    assert!(
        error.to_string().contains("checksum verification failed"),
        "{error}"
    );
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
    assert!(!staging_file_remains(&fixture));
}

#[test]
fn a_manifest_signed_over_different_bytes_is_rejected_before_download() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish_archive("0.19.0", &tar_gz(b"#!/bin/sh\nexit 0\n"), false);

    let error = run_update(&fixture.environment(), &request()).expect_err("bad signature");

    assert!(
        error
            .to_string()
            .contains("no trusted release signing key matched"),
        "{error}"
    );
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
}

#[test]
fn an_archive_carrying_more_than_the_orbit_binary_is_rejected() {
    let fixture = Fixture::new("0.18.0");
    let archive = tar_gz_named(&[
        ("orbit", b"#!/bin/sh\nexit 0\n".as_slice()),
        ("../evil", b"payload".as_slice()),
    ]);
    fixture.publish_archive("0.19.0", &archive, true);

    let error = run_update(&fixture.environment(), &request()).expect_err("extra member");

    assert!(error.to_string().contains("must contain only"), "{error}");
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
    assert!(!staging_file_remains(&fixture));
}

#[test]
fn an_archive_whose_member_is_not_named_orbit_is_rejected() {
    let fixture = Fixture::new("0.18.0");
    let archive = tar_gz_named(&[("../../etc/profile", b"payload".as_slice())]);
    fixture.publish_archive("0.19.0", &archive, true);

    let error = run_update(&fixture.environment(), &request()).expect_err("unexpected member");

    assert!(error.to_string().contains("unexpected member"), "{error}");
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
}

#[test]
fn a_release_that_reports_the_wrong_version_is_rolled_back() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::VersionMismatch);

    let error = run_update(&fixture.environment(), &request()).expect_err("version mismatch");

    assert!(
        error.to_string().contains("reports itself as 0.0.1"),
        "{error}"
    );
    assert!(
        error.to_string().contains("changed no workspace state"),
        "{error}"
    );
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
    assert!(fixture.invocations().is_empty());
}

#[test]
fn a_failed_migration_is_reported_as_needing_recovery_not_as_success() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::MigrationFails);

    let report = run_update(&fixture.environment(), &request()).expect("run completes");

    assert_eq!(report.outcome, UpdateOutcome::NeedsRecovery);
    assert_eq!(report.exit_code(), EXIT_NEEDS_RECOVERY);
    assert!(report.replaced);
    assert_eq!(fixture.installed_reports(), "orbit 0.19.0");
    // Managed-asset sync does not run into a workspace whose layout migration
    // did not finish.
    assert_eq!(report.steps.len(), 1);
    assert!(report.steps[0].failed());
    assert!(
        report.steps[0]
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("pending layout v9")),
        "{:?}",
        report.steps[0].detail
    );
    let recovery = report.recovery.expect("recovery guidance");
    assert!(recovery.contains("Re-run `orbit --root"), "{recovery}");
    assert!(recovery.contains("migrate --confirm"), "{recovery}");
    assert!(
        recovery.contains(&fixture.workspace_root().display().to_string()),
        "{recovery}"
    );
    assert!(
        recovery.contains(&fixture.backup_path().display().to_string()),
        "{recovery}"
    );
    assert!(fixture.backup_path().exists());
}

#[test]
fn a_failed_managed_asset_sync_also_reports_needing_recovery() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::SyncFails);

    let report = run_update(&fixture.environment(), &request()).expect("run completes");

    assert_eq!(report.outcome, UpdateOutcome::NeedsRecovery);
    assert_eq!(report.steps.len(), 2);
    assert!(!report.steps[0].failed());
    assert!(report.steps[1].failed());
    assert!(
        report
            .recovery
            .as_deref()
            .is_some_and(|text| text.contains("workspace sync")),
        "{:?}",
        report.recovery
    );
}

#[test]
fn outside_a_workspace_the_convergence_steps_are_skipped_with_a_reason() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);

    let report =
        run_update(&fixture.environment_without_workspace(), &request()).expect("update succeeds");

    assert_eq!(report.outcome, UpdateOutcome::Updated);
    assert_eq!(report.steps.len(), 2);
    assert!(report.steps.iter().all(|step| !step.failed()));
    assert!(
        report.steps.iter().all(|step| step
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("not an initialized Orbit workspace"))),
        "{:?}",
        report.steps
    );
    assert!(fixture.invocations().is_empty());
}

#[test]
fn a_second_concurrent_update_is_refused_rather_than_queued() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    let install_dir = fixture.executable.parent().expect("bin dir");
    let held = crate::update::lock::UpdateLock::acquire(install_dir).expect("first lock");

    let error = run_update(&fixture.environment(), &request()).expect_err("second update");

    assert!(
        error
            .to_string()
            .contains("another orbit update is already running"),
        "{error}"
    );
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
    drop(held);
    run_update(&fixture.environment(), &request()).expect("update after the lock is released");
}

#[test]
fn a_stale_writer_refuses_to_replace_a_newer_install() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    fixture.publish("0.20.0", FakeBinary::Healthy);

    let error = with_stale_discovery(
        &fixture,
        "0.19.0",
        |environment| run_update(environment, &request()).expect_err("stale writer must refuse"),
        || {
            let mut newer = request();
            newer.target_version = Some("0.20.0".to_string());
            let report =
                run_update(&fixture.environment(), &newer).expect("interloper installs 0.20");
            assert_eq!(report.outcome, UpdateOutcome::Updated);
            assert_eq!(fixture.installed_reports(), "orbit 0.20.0");
        },
    );

    assert!(error.to_string().contains("--allow-downgrade"), "{error}");
    assert!(error.to_string().contains("0.20"), "{error}");
    assert!(error.to_string().contains("0.19"), "{error}");
    assert_eq!(fixture.installed_reports(), "orbit 0.20.0");
    assert!(
        !fixture
            .invocations()
            .iter()
            .any(|line| line.starts_with("0.19.0:")),
        "stale 0.19 writer must not run against the 0.20 install: {:?}",
        fixture.invocations()
    );
}

#[test]
fn a_stale_writer_reconverges_when_the_lock_already_holds_the_target() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);

    let report = with_stale_discovery(
        &fixture,
        "0.19.0",
        |environment| run_update(environment, &request()).expect("stale writer reconverges"),
        || {
            let mut newer = request();
            newer.target_version = Some("0.19.0".to_string());
            run_update(&fixture.environment(), &newer).expect("interloper installs 0.19");
        },
    );

    assert_eq!(report.outcome, UpdateOutcome::AlreadyCurrent);
    assert!(!report.replaced);
    assert_eq!(report.current_version, "0.19.0");
    assert_eq!(fixture.installed_reports(), "orbit 0.19.0");
    let migrate = fixture
        .invocations()
        .iter()
        .filter(|line| *line == &fixture.invocation("0.19.0", "migrate --confirm"))
        .count();
    assert_eq!(migrate, 2, "{:?}", fixture.invocations());
}

#[test]
fn a_stale_writer_upgrades_from_the_locked_installed_version() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    fixture.publish("0.20.0", FakeBinary::Healthy);

    let report = with_stale_discovery(
        &fixture,
        "0.20.0",
        |environment| run_update(environment, &request()).expect("stale writer upgrades"),
        || {
            let mut middle = request();
            middle.target_version = Some("0.19.0".to_string());
            run_update(&fixture.environment(), &middle).expect("interloper installs 0.19");
        },
    );

    assert_eq!(report.outcome, UpdateOutcome::Updated);
    assert!(report.replaced);
    assert_eq!(report.current_version, "0.19.0");
    assert_eq!(report.target_version, "0.20.0");
    assert_eq!(fixture.installed_reports(), "orbit 0.20.0");
}

#[test]
fn a_stale_permitted_downgrade_still_preflights_the_workspace() {
    let fixture = Fixture::new("0.19.0");
    fixture.publish("0.18.0", FakeBinary::MigrationFails);
    fixture.publish("0.20.0", FakeBinary::Healthy);

    let mut stale_request = request();
    stale_request.allow_downgrade = true;

    let error = with_stale_discovery(
        &fixture,
        "0.18.0",
        |environment| run_update(environment, &stale_request).expect_err("incompatible downgrade"),
        || {
            let mut newer = request();
            newer.target_version = Some("0.20.0".to_string());
            run_update(&fixture.environment(), &newer).expect("interloper installs 0.20");
        },
    );

    assert!(
        error.to_string().contains("cannot open this workspace"),
        "{error}"
    );
    assert!(
        error.to_string().contains("nothing was replaced"),
        "{error}"
    );
    assert!(error.to_string().contains("0.20"), "{error}");
    assert_eq!(fixture.installed_reports(), "orbit 0.20.0");
    assert!(
        fixture
            .invocations()
            .iter()
            .any(|line| line == &fixture.invocation("0.18.0", "migrate --dry-run")),
        "{:?}",
        fixture.invocations()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn replacement_uses_the_live_path_when_current_exe_is_a_deleted_inode() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    let mut environment = fixture.environment();
    environment.executable = environment.executable.with_file_name("orbit (deleted)");

    let report = run_update(&environment, &request()).expect("resolves live path");

    assert_eq!(report.outcome, UpdateOutcome::Updated);
    assert_eq!(report.executable, fixture.executable);
    assert_eq!(fixture.installed_reports(), "orbit 0.19.0");
}

/// Run `stale` parked in `latest_version` while `interloper` installs under
/// the same lock, then resume. `stale` must call `latest_version`.
fn with_stale_discovery<R, S, I>(
    fixture: &Fixture,
    frozen_latest: &str,
    stale: S,
    interloper: I,
) -> R
where
    R: Send,
    S: FnOnce(&UpdateEnvironment) -> R + Send,
    I: FnOnce(),
{
    let paused = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let mut environment = fixture.environment();
    let inner = environment.source;
    environment.source = Box::new(PausingLatestSource::wrap(
        inner,
        frozen_latest,
        Arc::clone(&paused),
        Arc::clone(&resume),
    ));
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| stale(&environment));
        paused.wait();
        let interloper_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(interloper));
        resume.wait();
        let stale_result = handle.join().expect("stale writer thread");
        if let Err(payload) = interloper_result {
            std::panic::resume_unwind(payload);
        }
        stale_result
    })
}

#[test]
fn an_unpublished_version_fails_without_touching_the_installation() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);

    let mut requested = request();
    requested.target_version = Some("0.99.0".to_string());
    let error = run_update(&fixture.environment(), &requested).expect_err("missing release");

    assert!(error.to_string().contains("release mirror"), "{error}");
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
}

fn staging_file_remains(fixture: &Fixture) -> bool {
    fixture
        .install_dir_entries()
        .iter()
        .any(|name| name.starts_with(".orbit-update-staged"))
}

/// Guards the assumption the flow relies on: the fixture's install directory
/// starts with nothing but the executable.
#[test]
fn a_fresh_fixture_install_directory_holds_only_the_executable() {
    let fixture = Fixture::new("0.18.0");

    assert_eq!(fixture.install_dir_entries(), vec!["orbit".to_string()]);
    assert!(Path::new(&fixture.executable).exists());
}
