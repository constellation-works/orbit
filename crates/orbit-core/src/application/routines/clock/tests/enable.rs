use std::fs;

use tempfile::tempdir;

use orbit_common::OrbitError;

use super::super::converge::{clock_reload_pending_path, converge_clock_unit_with};
use super::super::enable::set_clock_enabled_with;
use super::super::install::{install_clock_with, render_systemd_service, render_systemd_timer};
use super::super::manager::ClockPlatform;
use super::super::settings::{ClockSettings, save_clock_settings};
use super::support::{
    INSTALLED_PROGRAM, MockRunner, SystemdManagerFake, manager_output, systemd_show_command,
    write_launchd_unit, write_stale_on_startup_timer,
};

#[test]
fn enable_migrates_stale_on_startup_sec_elapsed_timer() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    save_clock_settings(
        root.path(),
        ClockSettings {
            cadence_seconds: 300,
        },
    )
    .expect("persist cadence matching the stale unit");
    write_stale_on_startup_timer(home.path(), 300);
    let week = 7 * 24 * 60 * 60;
    let runner = SystemdManagerFake::late_elapsed(home.path(), week, 60);

    let status = set_clock_enabled_with(
        root.path(),
        true,
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect("enable migrates and re-arms a stale timer");

    assert!(status.enabled);
    assert!(status.schedulable);
    let next = runner
        .next_trigger()
        .expect("finite trigger after enable migration");
    assert!((week + 300..=week + 305).contains(&next));

    let timer = fs::read_to_string(home.path().join(".config/systemd/user/orbit-sweep.timer"))
        .expect("read migrated timer");
    assert!(timer.contains("OnActiveSec=300s"));
    assert!(timer.contains("OnUnitActiveSec=300s"));
    assert!(!timer.contains("OnStartupSec="));
    assert_eq!(
        runner.commands(),
        vec![
            "systemctl --user is-enabled orbit-sweep.timer".to_string(),
            "systemctl --user daemon-reload".to_string(),
            "systemctl --user enable orbit-sweep.timer".to_string(),
            "systemctl --user restart orbit-sweep.timer".to_string(),
            systemd_show_command().to_string(),
        ]
    );
}

#[test]
fn enable_rewrites_a_legacy_systemd_sweep_service_to_clock_tick() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    let unit_dir = home.path().join(".config/systemd/user");
    fs::create_dir_all(&unit_dir).expect("create systemd user unit dir");
    fs::write(
        unit_dir.join("orbit-sweep.service"),
        "[Service]\nType=oneshot\nExecStart=/opt/orbit/bin/orbit sweep\n",
    )
    .expect("write legacy service");
    fs::write(
        unit_dir.join("orbit-sweep.timer"),
        render_systemd_timer(ClockSettings::default()),
    )
    .expect("write current timer");
    let runner = SystemdManagerFake::new(home.path(), 10);

    let status = set_clock_enabled_with(
        root.path(),
        true,
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect("enable rewrites legacy service");

    assert!(status.schedulable);
    assert_eq!(
        fs::read_to_string(unit_dir.join("orbit-sweep.service")).expect("rewritten service"),
        render_systemd_service("/opt/orbit/bin/orbit")
    );
}

#[test]
fn enable_is_idempotent_for_current_on_active_sec_timer() {
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
    .expect("install current OnActiveSec timer");
    let before = fs::read_to_string(home.path().join(".config/systemd/user/orbit-sweep.timer"))
        .expect("read current timer");
    let commands_after_install = runner.commands().len();

    let status = set_clock_enabled_with(
        root.path(),
        true,
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect("enable current timer");

    assert!(status.enabled);
    assert!(status.schedulable);
    let after = fs::read_to_string(home.path().join(".config/systemd/user/orbit-sweep.timer"))
        .expect("read timer after enable");
    assert_eq!(before, after);
    assert_eq!(
        runner.commands()[commands_after_install..],
        vec![
            "systemctl --user is-enabled orbit-sweep.timer".to_string(),
            "systemctl --user daemon-reload".to_string(),
            "systemctl --user enable orbit-sweep.timer".to_string(),
            "systemctl --user restart orbit-sweep.timer".to_string(),
            systemd_show_command().to_string(),
        ]
    );
}

/// The operator pausing the clock after a failed repair is the final word: a
/// later `orbit clock repair` must not retry the reload and resume it.
#[test]
fn disable_forgets_a_reload_a_failed_repair_left_pending() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    let running = home.path().join("orbit");
    fs::write(&running, "binary").expect("running binary");
    write_launchd_unit(home.path(), INSTALLED_PROGRAM);
    let repair = MockRunner::new(vec![Ok(true), Ok(true), Ok(false)]);
    converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Launchd,
        &repair,
        home.path(),
    )
    .expect("repair rewrites the unit");
    assert!(clock_reload_pending_path(root.path()).exists());

    // Loaded per `launchctl list`, and the unload succeeds.
    let pause = MockRunner::new(vec![Ok(true), Ok(true)]);
    let status = set_clock_enabled_with(
        root.path(),
        false,
        ClockPlatform::Launchd,
        &pause,
        home.path(),
    )
    .expect("pause the clock");
    assert!(!status.enabled);
    assert!(!clock_reload_pending_path(root.path()).exists());

    let retry = MockRunner::new(Vec::new());
    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Launchd,
        &retry,
        home.path(),
    )
    .expect("repair leaves the paused clock alone");
    assert!(!convergence.needs_follow_up());
    assert!(retry.commands().is_empty());
}

#[test]
fn pause_and_enable_are_idempotent_for_launchd() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    let runner = MockRunner::with_probes(
        vec![Ok(true), Ok(false), Ok(true), Ok(true)],
        Vec::new(),
        vec![
            Ok(manager_output(true, "", "")),
            Ok(manager_output(
                false,
                "",
                "Could not find service com.orbit.sweep in domain for user",
            )),
        ],
    );
    assert!(
        !set_clock_enabled_with(
            root.path(),
            false,
            ClockPlatform::Launchd,
            &runner,
            home.path()
        )
        .expect("pause")
        .enabled
    );
    assert!(
        !set_clock_enabled_with(
            root.path(),
            false,
            ClockPlatform::Launchd,
            &runner,
            home.path()
        )
        .expect("repeat pause")
        .enabled
    );
    assert!(
        set_clock_enabled_with(
            root.path(),
            true,
            ClockPlatform::Launchd,
            &runner,
            home.path()
        )
        .expect("enable")
        .enabled
    );
    assert!(
        set_clock_enabled_with(
            root.path(),
            true,
            ClockPlatform::Launchd,
            &runner,
            home.path()
        )
        .expect("repeat enable")
        .enabled
    );
    assert_eq!(runner.commands().len(), 6);
}

#[test]
fn unavailable_or_unknown_systemd_state_refuses_pause_before_mutation() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    save_clock_settings(
        root.path(),
        ClockSettings {
            cadence_seconds: 300,
        },
    )
    .expect("write clock settings");
    let settings_before = fs::read_to_string(root.path().join("clock.toml"))
        .expect("read clock settings before pause");
    let unit_dir = home.path().join(".config/systemd/user");
    fs::create_dir_all(&unit_dir).expect("create systemd unit directory");
    let timer_path = unit_dir.join("orbit-sweep.timer");
    fs::write(&timer_path, "sentinel timer\n").expect("write sentinel timer");

    let unavailable = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![Err(OrbitError::Execution(
            "systemctl transport unavailable".to_string(),
        ))],
    );
    let error = set_clock_enabled_with(
        root.path(),
        false,
        ClockPlatform::Systemd,
        &unavailable,
        home.path(),
    )
    .expect_err("unavailable systemd manager must refuse pause");
    assert!(
        error
            .to_string()
            .contains("systemd clock manager is unavailable")
    );
    assert_eq!(
        unavailable.commands(),
        vec!["systemctl --user is-enabled orbit-sweep.timer"]
    );

    let unknown = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![Ok(manager_output(false, "", "Access denied"))],
    );
    let error = set_clock_enabled_with(
        root.path(),
        false,
        ClockPlatform::Systemd,
        &unknown,
        home.path(),
    )
    .expect_err("unknown systemd state must refuse pause");
    assert!(
        error
            .to_string()
            .contains("systemd clock manager is unavailable")
    );
    assert!(error.to_string().contains("Access denied"));
    assert_eq!(
        unknown.commands(),
        vec!["systemctl --user is-enabled orbit-sweep.timer"]
    );
    assert_eq!(
        fs::read_to_string(root.path().join("clock.toml"))
            .expect("read clock settings after pause"),
        settings_before
    );
    assert_eq!(
        fs::read_to_string(timer_path).expect("read timer after pause"),
        "sentinel timer\n"
    );
}

#[test]
fn unavailable_launchd_state_refuses_pause_before_mutation() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    save_clock_settings(
        root.path(),
        ClockSettings {
            cadence_seconds: 300,
        },
    )
    .expect("write clock settings");
    let settings_before = fs::read_to_string(root.path().join("clock.toml"))
        .expect("read clock settings before pause");
    let agents_dir = home.path().join("Library/LaunchAgents");
    fs::create_dir_all(&agents_dir).expect("create launchd agent directory");
    let plist_path = agents_dir.join("com.orbit.sweep.plist");
    fs::write(&plist_path, "sentinel plist\n").expect("write sentinel plist");
    let runner = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![
            Ok(manager_output(false, "", "Operation not permitted")),
            Ok(manager_output(false, "", "Operation not permitted")),
        ],
    );

    let error = set_clock_enabled_with(
        root.path(),
        false,
        ClockPlatform::Launchd,
        &runner,
        home.path(),
    )
    .expect_err("unavailable launchd manager must refuse pause");

    assert!(
        error
            .to_string()
            .contains("launchd clock manager is unavailable")
    );
    assert_eq!(
        runner.commands(),
        vec!["launchctl list com.orbit.sweep", "launchctl list"]
    );
    assert_eq!(
        fs::read_to_string(root.path().join("clock.toml"))
            .expect("read clock settings after pause"),
        settings_before
    );
    assert_eq!(
        fs::read_to_string(plist_path).expect("read plist after pause"),
        "sentinel plist\n"
    );
}

#[test]
fn systemd_bus_missing_socket_refuses_pause() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    let settings_before = fs::read_to_string(root.path().join("clock.toml")).ok();
    let runner = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![Ok(manager_output(
            false,
            "",
            "Failed to connect to bus: No such file or directory",
        ))],
    );

    let error = set_clock_enabled_with(
        root.path(),
        false,
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect_err("an unreachable user bus must refuse pause instead of claiming it succeeded");

    assert!(
        error
            .to_string()
            .contains("systemd clock manager is unavailable")
    );
    assert_eq!(
        runner.commands(),
        vec!["systemctl --user is-enabled orbit-sweep.timer"],
        "no manager mutation is issued when the timer state is unobservable"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("clock.toml")).ok(),
        settings_before,
        "clock settings are untouched by a refused pause"
    );
}

#[test]
fn recognized_inactive_manager_states_keep_pause_idempotent() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");

    for diagnostic in [
        "disabled",
        "Failed to get unit file state: No such file or directory",
        "Unit orbit-sweep.timer could not be found.",
    ] {
        let runner = MockRunner::with_probes(
            Vec::new(),
            Vec::new(),
            vec![Ok(manager_output(false, "", diagnostic))],
        );
        let status = set_clock_enabled_with(
            root.path(),
            false,
            ClockPlatform::Systemd,
            &runner,
            home.path(),
        )
        .expect("recognized inactive systemd state is idempotent");
        assert!(!status.enabled);
        assert_eq!(
            runner.commands(),
            vec!["systemctl --user is-enabled orbit-sweep.timer"]
        );
    }

    let launchd = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![Ok(manager_output(
            false,
            "",
            "Could not find service com.orbit.sweep in domain for user",
        ))],
    );
    let status = set_clock_enabled_with(
        root.path(),
        false,
        ClockPlatform::Launchd,
        &launchd,
        home.path(),
    )
    .expect("recognized not-loaded launchd state is idempotent");
    assert!(!status.enabled);
    assert_eq!(launchd.commands(), vec!["launchctl list com.orbit.sweep"]);
}

#[test]
fn manager_failures_include_exact_recovery_command() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    let runner = MockRunner::new(vec![Ok(false), Ok(true), Ok(false)]);
    let error = set_clock_enabled_with(
        root.path(),
        true,
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect_err("failed enable is surfaced");
    assert!(
        error
            .to_string()
            .contains("systemctl --user enable orbit-sweep.timer")
    );
    assert!(error.to_string().contains("recovery:"));
}
