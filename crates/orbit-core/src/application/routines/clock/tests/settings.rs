use std::fs;

use tempfile::tempdir;

use super::super::enable::set_clock_enabled_with;
use super::super::install::install_clock_with;
use super::super::manager::ClockPlatform;
use super::super::settings::{
    ClockSettings, load_clock_settings, save_clock_settings, set_clock_cadence_with,
};
use super::support::{MockRunner, SystemdManagerFake};

#[test]
fn clock_cadence_rejects_subminute_and_out_of_range_values() {
    assert!(
        ClockSettings {
            cadence_seconds: 30
        }
        .validate()
        .is_err()
    );
    assert!(
        ClockSettings {
            cadence_seconds: 90
        }
        .validate()
        .is_err()
    );
    assert!(
        ClockSettings {
            cadence_seconds: 3_660
        }
        .validate()
        .is_err()
    );
}

#[test]
fn missing_clock_settings_use_the_default() {
    let root = tempdir().expect("create global root");

    assert_eq!(
        load_clock_settings(root.path()).expect("load default clock settings"),
        ClockSettings::default()
    );
}

#[cfg(unix)]
#[test]
fn clock_settings_reject_symlink_escape() {
    use std::os::unix::fs::symlink;

    let root = tempdir().expect("create global root");
    let outside = tempdir().expect("create outside root");
    let outside_settings = outside.path().join("clock.toml");
    save_clock_settings(outside.path(), ClockSettings::default())
        .expect("write outside clock settings");
    symlink(&outside_settings, root.path().join("clock.toml")).expect("create settings symlink");

    let error = load_clock_settings(root.path()).expect_err("reject escaped settings path");

    assert!(
        error
            .to_string()
            .contains("clock configuration must be a regular clock.toml directly under")
    );
}

#[test]
fn systemd_cadence_change_and_reenable_establish_new_deadlines() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    let runner = SystemdManagerFake::new(home.path(), 100);
    install_clock_with(
        root.path(),
        "/opt/orbit/bin/orbit",
        ClockSettings::default(),
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect("initial install");

    runner.set_now(1_000);
    set_clock_cadence_with(
        root.path(),
        300,
        "/opt/orbit/bin/orbit",
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect("cadence change re-arms timer");
    assert!((1_300..=1_305).contains(&runner.next_trigger().expect("cadence deadline")));

    let paused = set_clock_enabled_with(
        root.path(),
        false,
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect("pause systemd timer");
    assert!(!paused.enabled);
    assert!(runner.next_trigger().is_none());

    runner.set_now(2_000);
    let enabled = set_clock_enabled_with(
        root.path(),
        true,
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect("re-enable and verify systemd timer");
    assert!(enabled.schedulable);
    assert!((2_300..=2_305).contains(&runner.next_trigger().expect("re-enable deadline")));
}

#[test]
fn cadence_reload_failure_restores_config_and_reactivates_previous_systemd_unit() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    let runner = MockRunner::with_outputs(
        vec![Ok(true), Ok(true), Ok(false), Ok(true), Ok(true), Ok(true)],
        vec![Ok(Some(
            "LoadState=loaded\nActiveState=active\nNextElapseUSecMonotonic=60s".to_string(),
        ))],
    );
    let error = set_clock_cadence_with(
        root.path(),
        300,
        "/opt/orbit/bin/orbit",
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect_err("failed activation rolls back");
    assert!(
        error
            .to_string()
            .contains("restored the previous configured cadence")
    );
    assert_eq!(
        load_clock_settings(root.path())
            .expect("load restored config")
            .cadence_seconds,
        60
    );
    let timer = fs::read_to_string(home.path().join(".config/systemd/user/orbit-sweep.timer"))
        .expect("read restored timer");
    assert!(timer.contains("OnUnitActiveSec=60s"));
    assert_eq!(
        runner.commands(),
        vec![
            "systemctl --user daemon-reload",
            "systemctl --user enable orbit-sweep.timer",
            "systemctl --user restart orbit-sweep.timer",
            "systemctl --user daemon-reload",
            "systemctl --user enable orbit-sweep.timer",
            "systemctl --user restart orbit-sweep.timer",
            "systemctl --user show orbit-sweep.timer --property=LoadState --property=ActiveState --property=NextElapseUSecRealtime --property=NextElapseUSecMonotonic --property=LastTriggerUSec",
        ]
    );
}

#[test]
fn launchd_activation_failure_restores_the_previous_rendered_unit() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    let runner = MockRunner::new(vec![Ok(true), Ok(false), Ok(true), Ok(true)]);
    set_clock_cadence_with(
        root.path(),
        300,
        "/opt/orbit/bin/orbit",
        ClockPlatform::Launchd,
        &runner,
        home.path(),
    )
    .expect_err("failed load rolls back");
    let plist = fs::read_to_string(
        home.path()
            .join("Library/LaunchAgents/com.orbit.sweep.plist"),
    )
    .expect("read restored plist");
    assert!(plist.contains("<integer>60</integer>"));
    assert_eq!(
        runner.commands(),
        vec![
            format!(
                "launchctl unload {}/Library/LaunchAgents/com.orbit.sweep.plist",
                home.path().display()
            ),
            format!(
                "launchctl load {}/Library/LaunchAgents/com.orbit.sweep.plist",
                home.path().display()
            ),
            format!(
                "launchctl unload {}/Library/LaunchAgents/com.orbit.sweep.plist",
                home.path().display()
            ),
            format!(
                "launchctl load {}/Library/LaunchAgents/com.orbit.sweep.plist",
                home.path().display()
            ),
        ]
    );
}
