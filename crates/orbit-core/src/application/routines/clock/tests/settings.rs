use std::fs;
use std::path::Path;

use orbit_common::OrbitError;
use tempfile::tempdir;

use super::super::install::{launchd_plist_path, systemd_timer_path, write_launchd_unit};
use super::super::manager::{ClockPlatform, ManagerCommandOutput};
use super::super::settings::{ClockSettings, load_clock_settings, set_clock_cadence_with};
use super::support::{MockRunner, systemd_activation_outputs};

const ORBIT_BIN: &str = "/opt/orbit/bin/orbit";

fn probe(success: bool, stderr: &str) -> Result<ManagerCommandOutput, OrbitError> {
    Ok(ManagerCommandOutput {
        success,
        exit_code: Some(if success { 0 } else { 1 }),
        stdout: String::new(),
        stderr: stderr.to_string(),
    })
}

fn set_cadence(
    root: &Path,
    home: &Path,
    platform: ClockPlatform,
    runner: &MockRunner,
) -> Result<super::super::install::ClockInstallReport, OrbitError> {
    set_clock_cadence_with(root, 300, ORBIT_BIN, platform, runner, home)
}

fn configured_cadence(root: &Path) -> u64 {
    load_clock_settings(root).unwrap().cadence_seconds
}

#[test]
fn cadence_change_keeps_a_paused_clock_paused() {
    for platform in [ClockPlatform::Systemd, ClockPlatform::Launchd] {
        let root = tempdir().unwrap();
        let home = tempdir().unwrap();
        let (probe_output, status_command) = match platform {
            ClockPlatform::Systemd => {
                let timer = systemd_timer_path(home.path());
                fs::create_dir_all(timer.parent().unwrap()).unwrap();
                fs::write(&timer, "OnActiveSec=60s\n").unwrap();
                (
                    probe(false, "disabled\n"),
                    "systemctl --user is-enabled orbit-sweep.timer",
                )
            }
            ClockPlatform::Launchd => {
                write_launchd_unit(
                    root.path(),
                    ORBIT_BIN,
                    ClockSettings::default(),
                    home.path(),
                )
                .unwrap();
                (
                    probe(false, "Could not find service \"com.orbit.sweep\"\n"),
                    "launchctl list com.orbit.sweep",
                )
            }
        };
        let runner = MockRunner::with_probes(vec![probe_output], Vec::new());

        let report = set_cadence(root.path(), home.path(), platform, &runner).unwrap();

        assert!(report.paused && !report.activated, "{platform:?}");
        assert_eq!(
            runner.commands(),
            vec![status_command],
            "{platform:?}: a paused clock must not be enabled, restarted, reloaded or loaded"
        );
        assert_eq!(configured_cadence(root.path()), 300, "{platform:?}");
        let unit = match platform {
            ClockPlatform::Systemd => fs::read_to_string(systemd_timer_path(home.path())),
            ClockPlatform::Launchd => fs::read_to_string(launchd_plist_path(home.path())),
        }
        .unwrap();
        assert!(
            unit.contains("300"),
            "{platform:?}: unit not rewritten: {unit}"
        );
    }
}

#[test]
fn cadence_change_on_a_running_clock_activates_or_restores_the_setting() {
    // Enabled: reload and activate.
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let runner = MockRunner::with_probes_and_outputs(
        vec![probe(true, "enabled\n")],
        systemd_activation_outputs("1000000", None),
        vec![Ok(true), Ok(true), Ok(true)],
    );
    let report = set_cadence(root.path(), home.path(), ClockPlatform::Systemd, &runner).unwrap();
    assert!(report.activated && !report.paused);
    let commands = runner.commands();
    assert!(
        commands.contains(&"systemctl --user enable orbit-sweep.timer".to_string())
            && commands.contains(&"systemctl --user restart orbit-sweep.timer".to_string()),
        "{commands:?}"
    );
    assert_eq!(configured_cadence(root.path()), 300);

    // Enabled but activation fails: the previous cadence comes back.
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let runner = MockRunner::with_probes(
        vec![probe(true, "enabled\n")],
        // daemon-reload succeeds and enable fails, for the change and its rollback.
        vec![Ok(true), Ok(false), Ok(true), Ok(false)],
    );
    let error = set_cadence(root.path(), home.path(), ClockPlatform::Systemd, &runner).unwrap_err();
    assert!(error.to_string().contains("not activated"), "{error}");
    assert_eq!(
        configured_cadence(root.path()),
        ClockSettings::default().cadence_seconds
    );

    // Never installed: not a paused clock, so the change installs and activates it.
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let runner = MockRunner::with_probes_and_outputs(
        vec![probe(
            false,
            "Failed to get unit file state: No such file or directory\n",
        )],
        systemd_activation_outputs("1000000", None),
        vec![Ok(true), Ok(true), Ok(true)],
    );
    let report = set_cadence(root.path(), home.path(), ClockPlatform::Systemd, &runner).unwrap();
    assert!(report.activated && !report.paused);
}
