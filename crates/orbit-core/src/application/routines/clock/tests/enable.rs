//! Fault-injected launchd health and activation: the public entry point fixes
//! its platform to the host, so it cannot exercise these states on Linux.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;

use orbit_common::OrbitError;
use tempfile::TempDir;

use super::super::enable::set_clock_enabled_with;
use super::super::install::{launchd_plist_path, write_launchd_unit};
use super::super::manager::{ClockPlatform, ManagerCommandOutput};
use super::super::program::discover_clock_unit_program;
use super::super::settings::ClockSettings;
use super::support::MockRunner;

struct Fixture {
    root: TempDir,
    home: TempDir,
}

impl Fixture {
    fn new(unrunnable: bool) -> Self {
        let fixture = Self {
            root: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
        };
        let program = fixture.home.path().join("installed-orbit");
        if !unrunnable {
            fs::write(&program, "#!/bin/sh\nprintf 'orbit 0.0.1\\n'\n").unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        }
        write_launchd_unit(
            fixture.root.path(),
            &program.to_string_lossy(),
            ClockSettings::default(),
            fixture.home.path(),
        )
        .unwrap();
        fixture
    }

    fn enable(&self, enabled: bool, runner: &MockRunner) -> Result<bool, OrbitError> {
        set_clock_enabled_with(
            self.root.path(),
            enabled,
            ClockPlatform::Launchd,
            runner,
            self.home.path(),
        )
        .map(|status| status.enabled)
    }

    fn plist(&self) -> String {
        fs::read_to_string(launchd_plist_path(self.home.path())).unwrap()
    }

    fn control_command(&self, action: &str) -> String {
        format!(
            "launchctl {action} {}",
            launchd_plist_path(self.home.path()).display()
        )
    }
}

fn dump(success: bool, stdout: &str) -> Result<ManagerCommandOutput, OrbitError> {
    Ok(ManagerCommandOutput {
        success,
        exit_code: Some(if success { 0 } else { 1 }),
        stdout: stdout.to_string(),
        stderr: String::new(),
    })
}

#[test]
fn loaded_unhealthy_launchd_is_rewritten_and_reloaded_or_reports_load_failure() {
    for (unrunnable, probe) in [
        (true, None),
        (false, Some(dump(false, ""))),
        (
            false,
            Some(Err(OrbitError::Execution("print transport failed".into()))),
        ),
        (false, Some(dump(true, "last exit code = 78: EX_CONFIG\n"))),
        (
            false,
            Some(dump(true, "properties = penalty box | inferred program\n")),
        ),
    ] {
        for loaded in [true, false] {
            let fixture = Fixture::new(unrunnable);
            let probes = match &probe {
                None => Vec::new(),
                Some(Ok(output)) => vec![Ok(output.clone())],
                Some(Err(error)) => vec![Err(OrbitError::Execution(error.to_string()))],
            };
            let runner = MockRunner::with_probes(probes, vec![Ok(true), Ok(true), Ok(loaded)]);
            let status = fixture.enable(true, &runner);
            if loaded {
                assert!(status.unwrap());
            } else {
                assert!(
                    status.is_err(),
                    "a failed reload must not report enable success"
                );
            }
            let (_, program, _) =
                discover_clock_unit_program(fixture.home.path(), ClockPlatform::Launchd)
                    .unwrap()
                    .unwrap();
            assert_eq!(program, std::env::current_exe().unwrap());
            let commands = runner.commands();
            assert_eq!(commands.first().unwrap(), "launchctl list com.orbit.sweep");
            assert_eq!(
                &commands[commands.len() - 2..],
                [
                    fixture.control_command("unload"),
                    fixture.control_command("load")
                ],
                "an unhealthy loaded agent must pick up the rewritten program"
            );
        }
    }
}

#[test]
fn loaded_healthy_launchd_keeps_its_plist_and_is_not_reloaded() {
    let fixture = Fixture::new(false);
    let before = fixture.plist();
    let runner = MockRunner::with_probes(
        vec![dump(
            true,
            "last exit code = 0\nproperties = inferred program\n",
        )],
        vec![Ok(true)],
    );

    assert!(fixture.enable(true, &runner).unwrap());
    assert_eq!(
        fixture.plist(),
        before,
        "a runnable different install stays intact"
    );
    let commands = runner.commands();
    assert_eq!(
        commands.len(),
        2,
        "health inspection must not reload a healthy agent"
    );
    assert_eq!(commands[0], "launchctl list com.orbit.sweep");
    assert!(commands[1].starts_with("launchctl print gui/"));
}

#[test]
fn launchd_pause_unloads_loaded_agents_and_leaves_paused_agents_alone() {
    for loaded in [true, false] {
        let fixture = Fixture::new(true);
        let before = fixture.plist();
        let probe = if loaded {
            dump(true, "")
        } else {
            Ok(ManagerCommandOutput {
                success: false,
                exit_code: Some(113),
                stdout: String::new(),
                stderr: "Could not find service \"com.orbit.sweep\" in domain".into(),
            })
        };
        let runner = MockRunner::with_probes(vec![probe], vec![Ok(true)]);

        assert!(!fixture.enable(false, &runner).unwrap());
        assert_eq!(
            fixture.plist(),
            before,
            "pause must not repair an unrunnable unit"
        );
        let mut expected = vec!["launchctl list com.orbit.sweep".to_string()];
        if loaded {
            expected.push(fixture.control_command("unload"));
        }
        assert_eq!(runner.commands(), expected);
    }
}
