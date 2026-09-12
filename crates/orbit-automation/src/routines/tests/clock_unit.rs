use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use super::super::clock::ClockPlatform;
use super::super::clock_unit::{
    ClockUnitVerdict, RunningBinary, inspect_clock_unit_at, probe_program_version,
};

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
            .contains("orbit clock enable")
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
            .contains("orbit clock enable")
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
            .contains("orbit clock enable")
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
