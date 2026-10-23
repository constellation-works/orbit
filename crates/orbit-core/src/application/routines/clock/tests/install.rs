use std::fs;

use tempfile::tempdir;

use super::super::install::{install_clock_with, validated_sweep_log_path};
use super::super::manager::ClockPlatform;
use super::super::settings::ClockSettings;
use super::support::{MockRunner, systemd_activation_outputs};

#[test]
fn installed_systemd_unit_has_recovery_settings_that_pass_inspection() {
    use super::super::inspect::{ClockUnitVerdict, RunningBinary, inspect_clock_unit_at};
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let program = root.path().join("orbit");
    fs::write(&program, "fixture").unwrap();
    let runner = MockRunner::new(vec![Ok(false)]);
    let report = install_clock_with(
        root.path(),
        program.to_str().unwrap(),
        ClockSettings::default(),
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .unwrap();
    assert!(!report.activated, "fixture manager unavailable");
    let inspected = inspect_clock_unit_at(
        home.path(),
        ClockPlatform::Systemd,
        &RunningBinary {
            path: program,
            version: "1.0.0".into(),
        },
        |_| Ok("1.0.0".into()),
    );
    assert_eq!(
        inspected.verdict,
        ClockUnitVerdict::Matching,
        "installed recovery settings must be finite and clean up descendants"
    );
}

#[test]
fn systemd_install_accepts_healthy_in_flight_and_finite_timers() {
    let service = "ActiveState=activating\nExecMainStartTimestampMonotonic=0\nResult=success\nTimeoutStartUSec=10min\nKillMode=mixed\n";

    // `systemd_enable_accepts_healthy_in_flight_and_finite_timers` keeps the
    // full matrix. Install and enable both call `systemd_schedulability_with_health`;
    // this row only proves install still reaches that decision.
    let cases = [("healthy finite timer", "1000000", Some(service))];

    for (case, next_elapse, observed_service) in cases {
        let root = tempdir().unwrap();
        let home = tempdir().unwrap();
        let program = root.path().join("orbit");
        fs::write(&program, "fixture").unwrap();
        let runner = MockRunner::with_outputs(
            systemd_activation_outputs(next_elapse, observed_service),
            vec![Ok(true), Ok(true), Ok(true)],
        );

        let report = install_clock_with(
            root.path(),
            program.to_str().unwrap(),
            ClockSettings::default(),
            ClockPlatform::Systemd,
            &runner,
            home.path(),
        )
        .unwrap_or_else(|error| panic!("{case}: {error}"));

        assert!(report.activated, "{case}");
        assert!(report.manual_steps.is_empty(), "{case}");
        assert_eq!(
            runner.commands().len(),
            5,
            "{case}: no retry or extra probe"
        );
    }
}

#[test]
fn systemd_install_rejects_missing_inactive_failed_or_unhealthy_sweeps_without_next_trigger() {
    // The reject matrix lives on the enable twin, which calls the same
    // `systemd_schedulability_with_health` helper. This row proves install
    // still surfaces that refusal.
    let cases = [(
        "inactive service",
        Some("ActiveState=inactive\nResult=success\nTimeoutStartUSec=10min\nKillMode=mixed\n"),
    )];

    for (case, service) in cases {
        let root = tempdir().unwrap();
        let home = tempdir().unwrap();
        let program = root.path().join("orbit");
        fs::write(&program, "fixture").unwrap();
        let runner = MockRunner::with_outputs(
            systemd_activation_outputs("infinity", service),
            vec![Ok(true), Ok(true), Ok(true)],
        );

        let error = install_clock_with(
            root.path(),
            program.to_str().unwrap(),
            ClockSettings::default(),
            ClockPlatform::Systemd,
            &runner,
            home.path(),
        )
        .expect_err(case);

        assert!(
            error
                .to_string()
                .contains("systemd timer restart completed, but the manager did not report it active with a finite future trigger"),
            "{case}: {error}"
        );
        assert_eq!(
            runner.commands().len(),
            5,
            "{case}: no retry or extra probe"
        );
    }
}

#[cfg(unix)]
#[test]
fn sweep_log_path_rejects_symlinked_directory() {
    use std::os::unix::fs::symlink;

    let root = tempdir().expect("create global root");
    let outside = tempdir().expect("create outside root");
    symlink(outside.path(), root.path().join("logs")).expect("create logs symlink");

    let error = validated_sweep_log_path(root.path()).expect_err("reject escaped log directory");

    assert!(
        error
            .to_string()
            .contains("sweep log directory must be a regular directory directly under")
    );
}

#[cfg(unix)]
#[test]
fn sweep_log_path_rejects_symlinked_file() {
    use std::os::unix::fs::symlink;

    let root = tempdir().expect("create global root");
    let outside = tempdir().expect("create outside root");
    fs::create_dir(root.path().join("logs")).expect("create log directory");
    let outside_log = outside.path().join("sweep.log");
    fs::write(&outside_log, "redirected").expect("write outside log");
    symlink(&outside_log, root.path().join("logs/sweep.log")).expect("create log symlink");

    let error = validated_sweep_log_path(root.path()).expect_err("reject escaped log file");

    assert!(
        error
            .to_string()
            .contains("sweep log must be a regular file directly under")
    );
}

#[cfg(unix)]
#[test]
fn launchd_install_rejects_dangling_log_symlink_before_activation() {
    use std::os::unix::fs::symlink;

    let root = tempdir().expect("create global root");
    let outside = tempdir().expect("create outside root");
    let home = tempdir().expect("create home");
    fs::create_dir(root.path().join("logs")).expect("create log directory");
    let outside_log = outside.path().join("nonexistent.log");
    symlink(&outside_log, root.path().join("logs/sweep.log")).expect("create dangling log symlink");
    let runner = MockRunner::new(vec![Ok(true), Ok(true)]);

    let error = install_clock_with(
        root.path(),
        "/opt/orbit/bin/orbit",
        ClockSettings::default(),
        ClockPlatform::Launchd,
        &runner,
        home.path(),
    )
    .expect_err("reject dangling log symlink");

    assert!(
        error
            .to_string()
            .contains("sweep log must be a regular file directly under")
    );
    assert!(runner.commands().is_empty(), "manager was not activated");
    assert!(!outside_log.exists(), "no outside log was created");
    assert!(
        !home
            .path()
            .join("Library/LaunchAgents/com.orbit.sweep.plist")
            .exists(),
        "unit was not written"
    );
}
