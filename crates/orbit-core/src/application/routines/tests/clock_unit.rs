use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use super::super::clock::ClockPlatform;
use super::super::clock_unit::{
    ClockUnitConvergence, ClockUnitDrift, ClockUnitVerdict, RunningBinary,
    clock_unit_drift_warning_at, converge_clock_unit_with, inspect_clock_unit_at,
    probe_program_version,
};
use super::clock::MockRunner;

fn running(path: &str, version: &str) -> RunningBinary {
    RunningBinary {
        path: PathBuf::from(path),
        version: version.to_string(),
    }
}

fn write_launchd_unit(home: &Path, program: &str) -> PathBuf {
    let agents = home.join("Library/LaunchAgents");
    fs::create_dir_all(&agents).expect("launchd agents dir");
    let path = agents.join("com.orbit.sweep.plist");
    fs::write(
        &path,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.orbit.sweep</string>
    <key>ProgramArguments</key>
    <array>
        <string>{program}</string>
        <string>clock</string>
        <string>tick</string>
    </array>
</dict>
</plist>
"#
        ),
    )
    .expect("write launchd plist");
    path
}

fn write_systemd_unit(home: &Path, program: &str) -> PathBuf {
    let unit_dir = home.join(".config/systemd/user");
    fs::create_dir_all(&unit_dir).expect("systemd user dir");
    let path = unit_dir.join("orbit-sweep.service");
    fs::write(
        &path,
        format!("[Service]\nType=oneshot\nExecStart={program} clock tick\n"),
    )
    .expect("write systemd service");
    path
}

fn inspect_with_version(
    home: &Path,
    platform: ClockPlatform,
    running: &RunningBinary,
    version: &str,
) -> super::super::clock_unit::ClockUnitInspection {
    inspect_clock_unit_at(home, platform, running, |_| Ok(version.to_string()))
}

#[test]
fn no_unit_installed_is_skipped() {
    let home = tempdir().expect("home");
    let inspection = inspect_clock_unit_at(
        home.path(),
        ClockPlatform::Systemd,
        &running("/opt/orbit/bin/orbit", "0.21.0"),
        |_| panic!("probe must not run when no unit is installed"),
    );
    assert_eq!(inspection.verdict, ClockUnitVerdict::NoUnitInstalled);
    assert!(inspection.status_line_suffix().is_empty());
    assert!(inspection.doctor_message().contains("no sweep clock unit"));
    assert!(inspection.doctor_remediation().is_none());
}

#[test]
fn matching_path_and_version_is_ok() {
    let home = tempdir().expect("home");
    let program = home.path().join("orbit");
    fs::write(&program, "binary").expect("touch running binary");
    let unit = write_systemd_unit(home.path(), &program.to_string_lossy());
    let running = RunningBinary {
        path: program.clone(),
        version: "0.21.0".to_string(),
    };

    let inspection = inspect_with_version(
        home.path(),
        ClockPlatform::Systemd,
        &running,
        "orbit 0.21.0",
    );

    assert_eq!(inspection.verdict, ClockUnitVerdict::Matching);
    assert_eq!(inspection.unit_path.as_deref(), Some(unit.as_path()));
    assert_eq!(inspection.program_path.as_deref(), Some(program.as_path()));
    assert_eq!(inspection.program_version.as_deref(), Some("0.21.0"));
    assert!(
        inspection
            .status_line_suffix()
            .contains(&format!(" | program: {} 0.21.0", program.display())),
        "{}",
        inspection.status_line_suffix()
    );
    assert!(inspection.doctor_message().contains("runs this binary"));
}

#[test]
fn path_only_mismatch_is_a_warning() {
    let home = tempdir().expect("home");
    let unit_program = home.path().join("homebrew/orbit");
    let running_program = home.path().join("cargo/orbit");
    fs::create_dir_all(unit_program.parent().expect("parent")).expect("unit parent");
    fs::create_dir_all(running_program.parent().expect("parent")).expect("running parent");
    fs::write(&unit_program, "homebrew").expect("unit binary");
    fs::write(&running_program, "cargo").expect("running binary");
    write_launchd_unit(home.path(), &unit_program.to_string_lossy());

    let running = RunningBinary {
        path: running_program.clone(),
        version: "0.21.0".to_string(),
    };
    let inspection = inspect_with_version(home.path(), ClockPlatform::Launchd, &running, "0.21.0");

    assert_eq!(inspection.verdict, ClockUnitVerdict::PathMismatch);
    let message = inspection.doctor_message();
    assert!(
        message.contains(&unit_program.display().to_string()),
        "{message}"
    );
    assert!(
        message.contains(&running_program.display().to_string()),
        "{message}"
    );
    assert!(message.contains("Two installs"), "{message}");
    assert!(
        inspection
            .doctor_remediation()
            .expect("path mismatch remediation")
            .contains("orbit clock repair")
    );
}

#[test]
fn version_mismatch_is_a_failure_naming_both_paths_and_versions() {
    let home = tempdir().expect("home");
    let unit_program = home.path().join("homebrew/orbit");
    let running_program = home.path().join("cargo/orbit");
    fs::create_dir_all(unit_program.parent().expect("parent")).expect("unit parent");
    fs::create_dir_all(running_program.parent().expect("parent")).expect("running parent");
    fs::write(&unit_program, "homebrew").expect("unit binary");
    fs::write(&running_program, "cargo").expect("running binary");
    let unit = write_launchd_unit(home.path(), &unit_program.to_string_lossy());

    let running = RunningBinary {
        path: running_program.clone(),
        version: "0.21.0".to_string(),
    };
    let inspection = inspect_with_version(
        home.path(),
        ClockPlatform::Launchd,
        &running,
        "orbit 0.20.0",
    );

    assert_eq!(inspection.verdict, ClockUnitVerdict::VersionMismatch);
    let message = inspection.doctor_message();
    assert!(message.contains(&unit.display().to_string()), "{message}");
    assert!(
        message.contains(&unit_program.display().to_string()),
        "{message}"
    );
    assert!(
        message.contains(&running_program.display().to_string()),
        "{message}"
    );
    assert!(message.contains("0.20.0"), "{message}");
    assert!(message.contains("0.21.0"), "{message}");
    let suffix = inspection.status_line_suffix();
    assert!(suffix.contains("mismatch: running 0.21.0"), "{suffix}");
    assert!(
        inspection
            .doctor_remediation()
            .expect("version mismatch remediation")
            .contains("orbit clock repair")
    );
}

#[test]
fn missing_program_is_unrunnable_without_panic() {
    let home = tempdir().expect("home");
    let missing = home.path().join("gone/orbit");
    write_systemd_unit(home.path(), &missing.to_string_lossy());

    let inspection = inspect_clock_unit_at(
        home.path(),
        ClockPlatform::Systemd,
        &running("/opt/orbit/bin/orbit", "0.21.0"),
        probe_program_version,
    );

    match &inspection.verdict {
        ClockUnitVerdict::Unrunnable { reason } => {
            assert!(reason.contains("does not exist"), "{reason}");
        }
        other => panic!("expected unrunnable, got {other:?}"),
    }
    assert!(
        inspection
            .doctor_message()
            .contains("could not report a version")
    );
    assert!(inspection.doctor_remediation().is_some());
}

#[test]
fn unexecutable_program_is_unrunnable_without_panic() {
    let home = tempdir().expect("home");
    let program = home.path().join("not-executable");
    fs::write(&program, "not a binary").expect("write junk");
    let mut permissions = fs::metadata(&program).expect("metadata").permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(&program, permissions).expect("chmod");
    write_systemd_unit(home.path(), &program.to_string_lossy());

    let inspection = inspect_clock_unit_at(
        home.path(),
        ClockPlatform::Systemd,
        &running("/opt/orbit/bin/orbit", "0.21.0"),
        probe_program_version,
    );

    match inspection.verdict {
        ClockUnitVerdict::Unrunnable { reason } => {
            assert!(
                reason.contains("could not start")
                    || reason.contains("exited")
                    || reason.contains("Permission"),
                "{reason}"
            );
        }
        other => panic!("expected unrunnable, got {other:?}"),
    }
}

#[test]
fn parses_quoted_systemd_exec_start() {
    let home = tempdir().expect("home");
    let unit_dir = home.path().join(".config/systemd/user");
    fs::create_dir_all(&unit_dir).expect("systemd user dir");
    fs::write(
        unit_dir.join("orbit-sweep.service"),
        "[Service]\nExecStart=\"/opt/orbit with spaces/orbit\" clock tick\n",
    )
    .expect("write quoted service");

    let inspection = inspect_clock_unit_at(
        home.path(),
        ClockPlatform::Systemd,
        &running("/opt/orbit/bin/orbit", "0.21.0"),
        |_| Ok("0.21.0".to_string()),
    );
    assert_eq!(
        inspection.program_path.as_deref(),
        Some(Path::new("/opt/orbit with spaces/orbit"))
    );
}

#[test]
fn legacy_systemd_sweep_invocation_is_reported_as_stale() {
    let home = tempdir().expect("home");
    let program = home.path().join("orbit");
    fs::write(&program, "binary").expect("touch running binary");
    let unit_dir = home.path().join(".config/systemd/user");
    fs::create_dir_all(&unit_dir).expect("systemd user dir");
    fs::write(
        unit_dir.join("orbit-sweep.service"),
        format!("[Service]\nExecStart={} sweep\n", program.display()),
    )
    .expect("legacy service");
    let running = RunningBinary {
        path: program,
        version: "0.21.0".to_string(),
    };

    let inspection = inspect_with_version(
        home.path(),
        ClockPlatform::Systemd,
        &running,
        "orbit 0.21.0",
    );

    assert_eq!(inspection.verdict, ClockUnitVerdict::InvocationMismatch);
    assert!(inspection.status_line_suffix().contains("stale"));
    assert!(inspection.doctor_message().contains("orbit sweep"));
    assert!(
        inspection
            .doctor_remediation()
            .expect("remediation")
            .contains("orbit clock repair")
    );
}

#[test]
fn live_version_script_is_normalized() {
    let home = tempdir().expect("home");
    let program = home.path().join("fake-orbit");
    fs::write(&program, "#!/bin/sh\necho 'orbit 0.20.0'\n").expect("write script");
    let mut permissions = fs::metadata(&program).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&program, permissions).expect("chmod");
    write_systemd_unit(home.path(), &program.to_string_lossy());

    let inspection = inspect_clock_unit_at(
        home.path(),
        ClockPlatform::Systemd,
        &running("/opt/orbit/bin/orbit", "0.21.0"),
        probe_program_version,
    );
    assert_eq!(inspection.verdict, ClockUnitVerdict::VersionMismatch);
    assert_eq!(inspection.program_version.as_deref(), Some("0.20.0"));
}

/// A stale unit whose program was deleted — the launchd penalty-box failure —
/// is rewritten to the running binary and re-registered.
#[test]
fn launchd_converge_rewrites_a_unit_whose_program_no_longer_exists() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let running = home.path().join("orbit/bin/orbit");
    fs::create_dir_all(running.parent().expect("parent")).expect("running parent");
    fs::write(&running, "binary").expect("running binary");
    let removed = home.path().join("homebrew/bin/orbit");
    let unit = write_launchd_unit(home.path(), &removed.to_string_lossy());
    // Registered with launchd, failing every wake-up.
    let runner = MockRunner::new(vec![Ok(true), Ok(true), Ok(true)]);

    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Launchd,
        &runner,
        home.path(),
    )
    .expect("converge rewrites the stale unit");

    let ClockUnitConvergence::Rewritten(rewrite) = convergence else {
        panic!("expected a rewrite, got {convergence:?}");
    };
    assert_eq!(
        rewrite.drift,
        ClockUnitDrift::ProgramMissing {
            previous: removed.clone()
        }
    );
    assert_eq!(rewrite.unit_path, unit);
    assert!(rewrite.reactivated);
    assert!(rewrite.manual_steps.is_empty());
    let plist = fs::read_to_string(&unit).expect("rewritten plist");
    assert!(
        plist.contains(&running.display().to_string()),
        "rewritten unit must name the running binary: {plist}"
    );
    assert!(!plist.contains(&removed.display().to_string()), "{plist}");
    assert!(plist.contains("<string>clock</string>"), "{plist}");
    assert_eq!(
        runner.commands(),
        vec![
            "launchctl list com.orbit.sweep".to_string(),
            format!("launchctl unload {}", unit.display()),
            format!("launchctl load {}", unit.display()),
        ]
    );
}

/// systemd drift is the same failure with a different manager: the service
/// keeps naming a binary the installer moved away from.
#[test]
fn systemd_converge_rewrites_a_moved_program_and_rearms_the_timer() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let running = home.path().join("orbit/bin/orbit");
    let moved_from = home.path().join("cargo/bin/orbit");
    for path in [&running, &moved_from] {
        fs::create_dir_all(path.parent().expect("parent")).expect("program parent");
        fs::write(path, "binary").expect("program");
    }
    let unit = write_systemd_unit(home.path(), &moved_from.to_string_lossy());
    let runner = MockRunner::new(vec![Ok(true), Ok(true), Ok(true)]);

    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect("converge rewrites the stale unit");

    let ClockUnitConvergence::Rewritten(rewrite) = convergence else {
        panic!("expected a rewrite, got {convergence:?}");
    };
    assert_eq!(
        rewrite.drift,
        ClockUnitDrift::ProgramMoved {
            previous: moved_from.clone()
        }
    );
    assert!(rewrite.reactivated);
    assert_eq!(
        rewrite.files_written,
        vec![unit.clone(), unit.with_file_name("orbit-sweep.timer")]
    );
    let service = fs::read_to_string(&unit).expect("rewritten service");
    assert!(
        service.contains(&format!("ExecStart={} clock tick", running.display())),
        "{service}"
    );
    assert!(
        unit.with_file_name("orbit-sweep.timer").exists(),
        "the timer is written alongside the service"
    );
    assert_eq!(
        runner.commands(),
        vec![
            "systemctl --user is-enabled orbit-sweep.timer",
            "systemctl --user daemon-reload",
            "systemctl --user restart orbit-sweep.timer",
        ]
    );
}

/// Repair must not resume a clock the operator paused: the file is corrected,
/// the manager is left alone.
#[test]
fn converge_rewrites_a_paused_unit_without_starting_it() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let running = home.path().join("orbit");
    fs::write(&running, "binary").expect("running binary");
    let unit = write_launchd_unit(home.path(), "/opt/homebrew/bin/orbit");
    let runner = MockRunner::new(vec![Ok(false)]);

    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Launchd,
        &runner,
        home.path(),
    )
    .expect("converge rewrites the paused unit");

    let ClockUnitConvergence::Rewritten(rewrite) = convergence else {
        panic!("expected a rewrite, got {convergence:?}");
    };
    assert!(!rewrite.reactivated);
    assert!(
        rewrite.manual_steps.is_empty(),
        "a paused clock needs no follow-up: {:?}",
        rewrite.manual_steps
    );
    assert!(
        fs::read_to_string(&unit)
            .expect("rewritten plist")
            .contains(&running.display().to_string())
    );
    assert_eq!(runner.commands(), vec!["launchctl list com.orbit.sweep"]);
}

/// A rewritten unit the manager refuses to reload is unfinished work, not a
/// clean repair.
#[test]
fn converge_reports_manual_steps_when_the_manager_will_not_reload() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let running = home.path().join("orbit");
    fs::write(&running, "binary").expect("running binary");
    let unit = write_launchd_unit(home.path(), "/opt/homebrew/bin/orbit");
    let runner = MockRunner::new(vec![Ok(true), Ok(true), Ok(false)]);

    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Launchd,
        &runner,
        home.path(),
    )
    .expect("converge rewrites the stale unit");

    assert!(convergence.needs_follow_up());
    assert_eq!(
        convergence.manual_steps(),
        [format!("launchctl load {}", unit.display())]
    );
    assert!(convergence.summary().contains("NOT reloaded"));
}

/// A unit that runs this binary through the compatibility alias is converged
/// to `orbit clock tick` without the operator reaching for another command.
#[test]
fn converge_rewrites_a_legacy_sweep_invocation() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let running = home.path().join("orbit");
    fs::write(&running, "binary").expect("running binary");
    let unit_dir = home.path().join(".config/systemd/user");
    fs::create_dir_all(&unit_dir).expect("systemd user dir");
    let unit = unit_dir.join("orbit-sweep.service");
    fs::write(
        &unit,
        format!(
            "[Service]\nType=oneshot\nExecStart={} sweep\n",
            running.display()
        ),
    )
    .expect("write legacy service");
    let runner = MockRunner::new(vec![Ok(true), Ok(true), Ok(true)]);

    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect("converge rewrites the legacy invocation");

    let ClockUnitConvergence::Rewritten(rewrite) = convergence else {
        panic!("expected a rewrite, got {convergence:?}");
    };
    assert_eq!(rewrite.drift, ClockUnitDrift::InvocationStale);
    assert!(
        fs::read_to_string(&unit)
            .expect("rewritten service")
            .contains("clock tick")
    );
}

#[test]
fn converge_leaves_a_current_unit_and_its_manager_alone() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let running = home.path().join("orbit");
    fs::write(&running, "binary").expect("running binary");
    let unit = write_launchd_unit(home.path(), &running.to_string_lossy());
    let before = fs::read_to_string(&unit).expect("installed plist");
    let runner = MockRunner::new(Vec::new());

    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Launchd,
        &runner,
        home.path(),
    )
    .expect("converge inspects the current unit");

    assert_eq!(
        convergence,
        ClockUnitConvergence::AlreadyCurrent {
            unit_path: unit.clone(),
            program: running.clone(),
        }
    );
    assert_eq!(fs::read_to_string(&unit).expect("plist"), before);
    assert!(runner.commands().is_empty());
    assert!(!convergence.needs_follow_up());
}

#[test]
fn converge_without_an_installed_unit_changes_nothing() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let runner = MockRunner::new(Vec::new());

    let convergence = converge_clock_unit_with(
        root.path(),
        Path::new("/opt/orbit/bin/orbit"),
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect("converge is a no-op without a unit");

    assert_eq!(convergence, ClockUnitConvergence::NoUnitInstalled);
    assert!(runner.commands().is_empty());
    assert!(!home.path().join(".config/systemd/user").exists());
}

/// `orbit sweep` run by hand from another install is the one moment the drift
/// is observable, so it says so on stderr.
#[test]
fn a_pass_from_another_binary_warns_and_names_the_repair() {
    let home = tempdir().expect("home");
    let running = home.path().join("cargo/orbit");
    let unit_program = home.path().join("homebrew/orbit");
    for path in [&running, &unit_program] {
        fs::create_dir_all(path.parent().expect("parent")).expect("program parent");
        fs::write(path, "binary").expect("program");
    }
    let unit = write_launchd_unit(home.path(), &unit_program.to_string_lossy());

    let warning = clock_unit_drift_warning_at(home.path(), ClockPlatform::Launchd, &running)
        .expect("a pass from another binary warns");

    assert!(warning.contains(&unit.display().to_string()), "{warning}");
    assert!(
        warning.contains(&unit_program.display().to_string()),
        "{warning}"
    );
    assert!(
        warning.contains(&running.display().to_string()),
        "{warning}"
    );
    assert!(warning.contains("orbit clock repair"), "{warning}");
    assert_eq!(warning.lines().count(), 1, "{warning}");
}

#[test]
fn a_pass_from_the_binary_the_unit_names_is_silent() {
    let home = tempdir().expect("home");
    let running = home.path().join("orbit");
    fs::write(&running, "binary").expect("running binary");
    write_launchd_unit(home.path(), &running.to_string_lossy());

    assert_eq!(
        clock_unit_drift_warning_at(home.path(), ClockPlatform::Launchd, &running),
        None
    );
}
