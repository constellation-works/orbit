use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tempfile::tempdir;

use orbit_common::OrbitError;

use super::super::clock::{
    ClockCommandRunner, ClockPlatform, ClockSettings, ManagerCommand, clock_status_with,
    install_clock_with, load_clock_settings, render_systemd_service, render_systemd_timer,
    save_clock_settings, set_clock_cadence_with, set_clock_enabled_with, validated_sweep_log_path,
};

struct MockRunner {
    results: Mutex<Vec<Result<bool, OrbitError>>>,
    outputs: Mutex<Vec<Result<Option<String>, OrbitError>>>,
    commands: Mutex<Vec<String>>,
}

impl MockRunner {
    fn new(results: Vec<Result<bool, OrbitError>>) -> Self {
        Self {
            results: Mutex::new(results.into_iter().rev().collect()),
            outputs: Mutex::new(Vec::new()),
            commands: Mutex::new(Vec::new()),
        }
    }

    fn with_outputs(
        results: Vec<Result<bool, OrbitError>>,
        outputs: Vec<Result<Option<String>, OrbitError>>,
    ) -> Self {
        Self {
            results: Mutex::new(results.into_iter().rev().collect()),
            outputs: Mutex::new(outputs.into_iter().rev().collect()),
            commands: Mutex::new(Vec::new()),
        }
    }

    fn commands(&self) -> Vec<String> {
        self.commands.lock().expect("test command log lock").clone()
    }
}

impl ClockCommandRunner for MockRunner {
    fn run(&self, command: &ManagerCommand) -> Result<bool, OrbitError> {
        self.commands
            .lock()
            .expect("test command log lock")
            .push(command.display());
        self.results
            .lock()
            .expect("test result queue lock")
            .pop()
            .expect("test configured a result for every manager command")
    }

    fn stdout(&self, command: &ManagerCommand) -> Result<Option<String>, OrbitError> {
        self.commands
            .lock()
            .expect("test command log lock")
            .push(command.display());
        self.outputs
            .lock()
            .expect("test output queue lock")
            .pop()
            .expect("test configured output for every manager query")
    }
}

#[derive(Debug)]
struct FakeSystemdState {
    now_seconds: u64,
    enabled: bool,
    active: bool,
    last_service_activation: Option<u64>,
    next_trigger: Option<u64>,
}

/// A temporal systemd fake: startup-relative deadlines are based on the
/// already-running manager, timer-relative deadlines are based on every
/// restart, and service-relative deadlines are recomputed after a sweep.
struct SystemdManagerFake {
    home: PathBuf,
    state: Mutex<FakeSystemdState>,
    commands: Mutex<Vec<String>>,
}

impl SystemdManagerFake {
    fn new(home: &Path, now_seconds: u64) -> Self {
        Self {
            home: home.to_path_buf(),
            state: Mutex::new(FakeSystemdState {
                now_seconds,
                enabled: false,
                active: false,
                last_service_activation: None,
                next_trigger: None,
            }),
            commands: Mutex::new(Vec::new()),
        }
    }

    fn late_elapsed(home: &Path, now_seconds: u64, last_service_activation: u64) -> Self {
        Self {
            home: home.to_path_buf(),
            state: Mutex::new(FakeSystemdState {
                now_seconds,
                enabled: true,
                active: true,
                last_service_activation: Some(last_service_activation),
                next_trigger: None,
            }),
            commands: Mutex::new(Vec::new()),
        }
    }

    fn next_trigger(&self) -> Option<u64> {
        self.state
            .lock()
            .expect("fake systemd state lock")
            .next_trigger
    }

    fn commands(&self) -> Vec<String> {
        self.commands.lock().expect("fake command log lock").clone()
    }

    fn set_now(&self, now_seconds: u64) {
        self.state
            .lock()
            .expect("fake systemd state lock")
            .now_seconds = now_seconds;
    }

    fn elapse_and_complete_service(&self) {
        let cadence = self
            .timer_value("OnUnitActiveSec")
            .expect("recurring timer directive");
        let mut state = self.state.lock().expect("fake systemd state lock");
        let triggered_at = state.next_trigger.expect("timer has a next trigger");
        state.now_seconds = triggered_at;
        state.last_service_activation = Some(triggered_at);
        state.next_trigger = Some(triggered_at + cadence);
    }

    fn timer_value(&self, directive: &str) -> Option<u64> {
        let timer = fs::read_to_string(self.home.join(".config/systemd/user/orbit-sweep.timer"))
            .expect("fake manager reads installed timer");
        timer.lines().find_map(|line| {
            line.strip_prefix(&format!("{directive}="))
                .and_then(|value| value.strip_suffix('s'))
                .and_then(|value| value.parse().ok())
        })
    }

    fn activate_timer(&self) {
        let on_active = self.timer_value("OnActiveSec");
        let on_startup = self.timer_value("OnStartupSec");
        let on_unit_active = self.timer_value("OnUnitActiveSec");
        let mut state = self.state.lock().expect("fake systemd state lock");
        let now = state.now_seconds;
        let activation_deadline = on_active.map(|cadence| now + cadence);
        // This models the late `active (elapsed)` regression: startup and
        // previous-service deadlines that elapsed before restart do not
        // establish a new future trigger.
        let startup_deadline = on_startup.filter(|deadline| *deadline > now);
        let recurring_deadline = on_unit_active
            .zip(state.last_service_activation)
            .map(|(cadence, activated)| activated + cadence)
            .filter(|deadline| *deadline > now);
        state.active = true;
        state.next_trigger = [activation_deadline, startup_deadline, recurring_deadline]
            .into_iter()
            .flatten()
            .min();
    }
}

impl ClockCommandRunner for SystemdManagerFake {
    fn run(&self, command: &ManagerCommand) -> Result<bool, OrbitError> {
        let display = command.display();
        self.commands
            .lock()
            .expect("fake command log lock")
            .push(display.clone());
        match display.as_str() {
            "systemctl --user daemon-reload" => Ok(true),
            "systemctl --user is-enabled orbit-sweep.timer" => {
                Ok(self.state.lock().expect("fake systemd state lock").enabled)
            }
            "systemctl --user enable orbit-sweep.timer" => {
                self.state.lock().expect("fake systemd state lock").enabled = true;
                Ok(true)
            }
            "systemctl --user restart orbit-sweep.timer" => {
                self.activate_timer();
                Ok(true)
            }
            "systemctl --user disable --now orbit-sweep.timer" => {
                let mut state = self.state.lock().expect("fake systemd state lock");
                state.enabled = false;
                state.active = false;
                state.next_trigger = None;
                Ok(true)
            }
            unexpected => panic!("unexpected fake systemd command: {unexpected}"),
        }
    }

    fn stdout(&self, command: &ManagerCommand) -> Result<Option<String>, OrbitError> {
        self.commands
            .lock()
            .expect("fake command log lock")
            .push(command.display());
        let state = self.state.lock().expect("fake systemd state lock");
        let next = state
            .next_trigger
            .map_or_else(|| "infinity".to_string(), |value| format!("{value}s"));
        let last = state
            .last_service_activation
            .map_or_else(String::new, |value| format!("{value}s"));
        Ok(Some(format!(
            "LoadState=loaded\nActiveState={}\nNextElapseUSecRealtime=\nNextElapseUSecMonotonic={next}\nLastTriggerUSec={last}",
            if state.active { "active" } else { "inactive" }
        )))
    }
}

fn service_path(rendered: &str) -> &str {
    rendered
        .lines()
        .find_map(|line| line.strip_prefix("Environment=PATH="))
        .expect("systemd service declares PATH")
}

fn finds_launcher(path: &str, launcher: &str) -> bool {
    path.split(':')
        .map(Path::new)
        .any(|directory| directory.join(launcher).is_file())
}

fn write_stale_on_startup_timer(home: &Path, cadence_seconds: u64) {
    let unit_dir = home.join(".config/systemd/user");
    fs::create_dir_all(&unit_dir).expect("create systemd user unit dir");
    fs::write(
        unit_dir.join("orbit-sweep.timer"),
        format!(
            "# stale pre-OnActiveSec timer\n[Unit]\nDescription=Run orbit sweep every {cadence_seconds} seconds\n\n[Timer]\nOnStartupSec={cadence_seconds}s\nOnUnitActiveSec={cadence_seconds}s\nAccuracySec=5s\n\n[Install]\nWantedBy=timers.target\n"
        ),
    )
    .expect("write stale OnStartupSec timer");
}

fn systemd_show_command() -> &'static str {
    "systemctl --user show orbit-sweep.timer --property=LoadState --property=ActiveState --property=NextElapseUSecRealtime --property=NextElapseUSecMonotonic --property=LastTriggerUSec"
}

#[test]
fn rendered_systemd_service_discovers_local_provider_launchers() {
    let home = tempdir().expect("create temporary home");
    let provider_bin = home.path().join(".local/bin");
    fs::create_dir_all(&provider_bin).expect("create provider directory");
    fs::write(provider_bin.join("codex"), "provider launcher").expect("write provider launcher");

    let manager_path = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
    assert!(
        !finds_launcher(manager_path, "codex"),
        "the minimal user-manager PATH does not find the provider launcher"
    );

    let rendered = render_systemd_service("/opt/orbit/bin/orbit");
    let rendered_path = service_path(&rendered);
    let effective_path = rendered_path.replace("%h", &home.path().to_string_lossy());

    assert!(
        finds_launcher(&effective_path, "codex"),
        "the service PATH finds a provider launcher from the user's .local/bin"
    );
    assert!(rendered_path.starts_with("%h/.local/bin:%h/.orbit/bin:%h/.cargo/bin:"));
    for directory in [
        "/usr/local/sbin",
        "/usr/local/bin",
        "/usr/sbin",
        "/usr/bin",
        "/sbin",
        "/bin",
    ] {
        assert!(rendered_path.split(':').any(|entry| entry == directory));
    }
    assert!(!rendered_path.contains('~'));
    assert!(!rendered_path.contains("/home/"));
    assert!(!rendered_path.contains("/nix/store/"));
    assert!(rendered.contains("Type=oneshot"));
    assert!(rendered.contains("KillMode=process"));
    assert!(rendered.contains("ExecStart=/opt/orbit/bin/orbit clock tick"));
}

#[test]
fn systemd_timer_renders_default_and_configured_cadence() {
    let default_timer = render_systemd_timer(ClockSettings::default());
    assert!(default_timer.contains("OnActiveSec=60s"));
    assert!(default_timer.contains("OnUnitActiveSec=60s"));
    assert!(!default_timer.contains("OnStartupSec="));
    assert!(!default_timer.contains("Persistent=true"));

    let configured_timer = render_systemd_timer(ClockSettings {
        cadence_seconds: 300,
    });
    assert!(configured_timer.contains("OnActiveSec=300s"));
    assert!(configured_timer.contains("OnUnitActiveSec=300s"));
}

#[test]
fn systemd_install_arms_and_recurs_at_default_and_configured_cadence() {
    for cadence_seconds in [60, 300] {
        let root = tempdir().expect("create global root");
        let home = tempdir().expect("create home");
        let runner = SystemdManagerFake::new(home.path(), 10);

        let report = install_clock_with(
            root.path(),
            "/opt/orbit/bin/orbit",
            ClockSettings { cadence_seconds },
            ClockPlatform::Systemd,
            &runner,
            home.path(),
        )
        .expect("install systemd clock");

        assert!(report.activated);
        assert_eq!(runner.next_trigger(), Some(10 + cadence_seconds));
        runner.elapse_and_complete_service();
        assert_eq!(
            runner.next_trigger(),
            Some(10 + cadence_seconds * 2),
            "service activation establishes the recurring deadline"
        );
    }
}

#[test]
fn late_reinstall_rearms_an_active_elapsed_timer_from_timer_activation() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    let week = 7 * 24 * 60 * 60;
    let runner = SystemdManagerFake::late_elapsed(home.path(), week, 60);

    let report = install_clock_with(
        root.path(),
        "/opt/orbit/bin/orbit",
        ClockSettings {
            cadence_seconds: 300,
        },
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect("late reinstall re-arms timer");

    assert!(report.activated);
    let next = runner
        .next_trigger()
        .expect("finite trigger after reinstall");
    assert!((week + 300..=week + 305).contains(&next));
}

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

#[test]
fn status_is_deterministic_for_each_manager_and_reports_configured_cadence() {
    let root = tempdir().expect("create global root");
    let launchd = MockRunner::new(vec![Ok(true)]);
    let status = clock_status_with(root.path(), ClockPlatform::Launchd, &launchd)
        .expect("read launchd status");
    assert!(status.enabled);
    assert!(status.schedulable);
    assert_eq!(status.configured_cadence_seconds, 60);
    assert_eq!(status.effective_cadence_seconds, Some(60));
    assert_eq!(status.platform, "launchd");

    let systemd = MockRunner::with_outputs(
        vec![Ok(true)],
        vec![Ok(Some(
            "LoadState=loaded\nActiveState=active\nNextElapseUSecRealtime=Sun 2026-08-16 04:30:00 UTC\nNextElapseUSecMonotonic=5min\nLastTriggerUSec=Sun 2026-08-16 04:29:00 UTC".to_string(),
        ))],
    );
    let status = clock_status_with(root.path(), ClockPlatform::Systemd, &systemd)
        .expect("read systemd status");
    assert!(status.enabled);
    assert!(status.schedulable);
    assert_eq!(status.effective_cadence_seconds, Some(60));
    assert!(status.health_issue.is_none());
    assert!(status.loaded);
    assert_eq!(status.running, Some(true));
    assert_eq!(
        status.last_tick_at.as_deref(),
        Some("Sun 2026-08-16 04:29:00 UTC")
    );
    assert_eq!(
        status.next_tick_at.as_deref(),
        Some("Sun 2026-08-16 04:30:00 UTC")
    );
    assert_eq!(status.platform, "systemd");
}

#[test]
fn enabled_systemd_timer_without_a_future_trigger_is_unhealthy() {
    let root = tempdir().expect("create global root");
    let runner = MockRunner::with_outputs(
        vec![Ok(true)],
        vec![Ok(Some(
            "LoadState=loaded\nActiveState=active\nNextElapseUSecRealtime=\nNextElapseUSecMonotonic=0"
                .to_string(),
        ))],
    );

    let status = clock_status_with(root.path(), ClockPlatform::Systemd, &runner)
        .expect("read elapsed timer status");

    assert!(status.enabled);
    assert!(!status.schedulable);
    assert_eq!(status.effective_cadence_seconds, None);
    assert!(status.health_issue.as_deref().is_some_and(|issue| {
        issue.contains("finite future trigger") && issue.contains("orbit clock enable")
    }));
    assert_eq!(
        runner.commands(),
        vec![
            "systemctl --user is-enabled orbit-sweep.timer",
            "systemctl --user show orbit-sweep.timer --property=LoadState --property=ActiveState --property=NextElapseUSecRealtime --property=NextElapseUSecMonotonic --property=LastTriggerUSec",
        ]
    );
}

#[test]
fn systemd_monotonic_duration_is_not_a_wall_clock_next_tick() {
    let root = tempdir().expect("create global root");
    let runner = MockRunner::with_outputs(
        vec![Ok(true)],
        vec![Ok(Some(
            "LoadState=loaded\nActiveState=active\nNextElapseUSecRealtime=n/a\nNextElapseUSecMonotonic=4h 14min\nLastTriggerUSec=Sun 2026-09-07 21:00:00 UTC"
                .to_string(),
        ))],
    );

    let status = clock_status_with(root.path(), ClockPlatform::Systemd, &runner)
        .expect("read monotonic-only timer status");

    assert!(status.enabled);
    assert!(status.schedulable);
    assert_eq!(status.effective_cadence_seconds, Some(60));
    assert_eq!(
        status.last_tick_at.as_deref(),
        Some("Sun 2026-09-07 21:00:00 UTC")
    );
    assert_eq!(
        status.next_tick_at, None,
        "NextElapseUSecMonotonic is a duration from boot, not the next wall-clock tick"
    );
}

#[test]
fn disabled_systemd_timer_reports_loaded_state_without_becoming_schedulable() {
    let root = tempdir().expect("create global root");
    let runner = MockRunner::with_outputs(
        vec![Ok(false)],
        vec![Ok(Some(
            "LoadState=loaded\nActiveState=inactive\nNextElapseUSecRealtime=Sun 2026-08-16 04:30:00 UTC"
                .to_string(),
        ))],
    );

    let status = clock_status_with(root.path(), ClockPlatform::Systemd, &runner)
        .expect("read disabled timer status");

    assert!(!status.enabled);
    assert!(status.loaded);
    assert_eq!(status.running, Some(false));
    assert!(!status.schedulable);
    assert_eq!(status.effective_cadence_seconds, None);
    assert!(status.health_issue.is_none());
}

#[test]
fn pause_and_enable_are_idempotent_for_launchd() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    let runner = MockRunner::new(vec![
        Ok(true),
        Ok(true),
        Ok(false),
        Ok(false),
        Ok(true),
        Ok(true),
    ]);
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
fn systemd_install_rejects_successful_commands_without_a_finite_trigger() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    let runner = MockRunner::with_outputs(
        vec![Ok(true), Ok(true), Ok(true)],
        vec![Ok(Some(
            "LoadState=loaded\nActiveState=active\nNextElapseUSecMonotonic=infinity".to_string(),
        ))],
    );

    let error = install_clock_with(
        root.path(),
        "/opt/orbit/bin/orbit",
        ClockSettings::default(),
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect_err("unschedulable activation is not reported as active");

    assert!(error.to_string().contains("finite future trigger"));
    assert!(error.to_string().contains("systemctl --user status"));
    assert!(error.to_string().contains("journalctl --user"));
    assert!(
        !error.to_string().contains("orbit clock enable"),
        "a completed enable/install that is still unschedulable must not tell the operator to repeat enable"
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
