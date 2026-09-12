//! OS clock integration [ORB-10021, ORB-12237]: the OS owns the wake-up,
//! Orbit owns everything else. `orbit routine init --install-clock` renders
//! the platform unit from the templates in `assets/clock/` and installs it
//! as a per-user unit (launchd agent on macOS, systemd user timer on Linux).
//! There is no resident Orbit daemon.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use serde::{Deserialize, Serialize};

const LAUNCHD_PLIST_TEMPLATE: &str = include_str!("../../../assets/clock/com.orbit.sweep.plist");
const SYSTEMD_SERVICE_TEMPLATE: &str = include_str!("../../../assets/clock/orbit-sweep.service");
const SYSTEMD_TIMER_TEMPLATE: &str = include_str!("../../../assets/clock/orbit-sweep.timer");
const CLOCK_SETTINGS_FILE: &str = "clock.toml";

/// launchd agent label (macOS).
pub const LAUNCHD_LABEL: &str = "com.orbit.sweep";
/// systemd unit base name (Linux).
pub const SYSTEMD_UNIT: &str = "orbit-sweep";
pub const DEFAULT_CLOCK_CADENCE_SECONDS: u64 = 60;
const MIN_CLOCK_CADENCE_SECONDS: u64 = 60;
const MAX_CLOCK_CADENCE_SECONDS: u64 = 3_600;

/// Host-local settings for the OS clock. This deliberately lives beside the
/// host database rather than in a workspace config: every registered workspace
/// shares one clock.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub struct ClockSettings {
    pub cadence_seconds: u64,
}

impl Default for ClockSettings {
    fn default() -> Self {
        Self {
            cadence_seconds: DEFAULT_CLOCK_CADENCE_SECONDS,
        }
    }
}

impl ClockSettings {
    pub fn validate(self) -> Result<Self, OrbitError> {
        if !(MIN_CLOCK_CADENCE_SECONDS..=MAX_CLOCK_CADENCE_SECONDS).contains(&self.cadence_seconds)
            || !self.cadence_seconds.is_multiple_of(60)
        {
            return Err(OrbitError::InvalidInput(format!(
                "clock cadence_seconds must be a whole minute from {MIN_CLOCK_CADENCE_SECONDS} to {MAX_CLOCK_CADENCE_SECONDS} (got {})",
                self.cadence_seconds
            )));
        }
        Ok(self)
    }
}

pub fn clock_settings_path(global_root: &Path) -> PathBuf {
    global_root.join(CLOCK_SETTINGS_FILE)
}

pub fn load_clock_settings(global_root: &Path) -> Result<ClockSettings, OrbitError> {
    let path = validated_clock_settings_path(global_root)?;
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ClockSettings::default());
        }
        Err(error) => {
            return Err(OrbitError::Io(format!("read {}: {error}", path.display())));
        }
    };
    toml::from_str::<ClockSettings>(&raw)
        .map_err(|error| {
            OrbitError::InvalidInput(format!(
                "invalid clock configuration {}: {error}",
                path.display()
            ))
        })?
        .validate()
}

pub fn save_clock_settings(global_root: &Path, settings: ClockSettings) -> Result<(), OrbitError> {
    let settings = settings.validate()?;
    let rendered = toml::to_string(&settings).map_err(|error| {
        OrbitError::Execution(format!("serialize clock configuration: {error}"))
    })?;
    let path = validated_clock_settings_path(global_root)?;
    atomic_write_text(&path, &rendered)
        .map_err(|error| OrbitError::Io(format!("write clock configuration: {error}")))
}

/// Resolve the clock settings file beneath the canonical global root.
///
/// The root may be selected through an explicit CLI override, but the clock
/// settings path itself is fixed. Canonicalizing both components and requiring
/// the exact expected file prevents a symlink or traversal from redirecting a
/// settings read to another host file.
fn validated_clock_settings_path(global_root: &Path) -> Result<PathBuf, OrbitError> {
    let canonical_root = fs::canonicalize(global_root).map_err(|error| {
        OrbitError::Io(format!(
            "resolve clock configuration root {}: {error}",
            global_root.display()
        ))
    })?;
    let expected_path = canonical_root.join(CLOCK_SETTINGS_FILE);

    let canonical_path = match fs::canonicalize(&expected_path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(expected_path);
        }
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "resolve clock configuration {}: {error}",
                expected_path.display()
            )));
        }
    };
    if canonical_path != expected_path || !canonical_path.is_file() {
        return Err(OrbitError::InvalidInput(format!(
            "clock configuration must be a regular {CLOCK_SETTINGS_FILE} directly under {}",
            canonical_root.display()
        )));
    }

    Ok(canonical_path)
}

/// Change cadence transactionally from the operator's perspective: the
/// persisted setting is restored if the reloaded native unit cannot activate.
pub fn set_clock_cadence(
    global_root: &Path,
    cadence_seconds: u64,
) -> Result<ClockInstallReport, OrbitError> {
    let orbit_bin = std::env::current_exe()
        .map_err(|error| OrbitError::Io(format!("resolve current orbit executable: {error}")))?
        .to_string_lossy()
        .to_string();
    set_clock_cadence_with(
        global_root,
        cadence_seconds,
        &orbit_bin,
        ClockPlatform::current(),
        &NativeClockCommandRunner,
        &home_dir()?,
    )
}

pub(super) fn set_clock_cadence_with(
    global_root: &Path,
    cadence_seconds: u64,
    orbit_bin: &str,
    platform: ClockPlatform,
    runner: &dyn ClockCommandRunner,
    home: &Path,
) -> Result<ClockInstallReport, OrbitError> {
    let previous = load_clock_settings(global_root)?;
    save_clock_settings(global_root, ClockSettings { cadence_seconds })?;
    match install_clock_with(
        global_root,
        orbit_bin,
        ClockSettings { cadence_seconds },
        platform,
        runner,
        home,
    ) {
        Ok(report) if report.activated => Ok(report),
        Ok(report) => {
            save_clock_settings(global_root, previous)?;
            let rollback =
                install_clock_with(global_root, orbit_bin, previous, platform, runner, home)?;
            Err(OrbitError::Execution(format!(
                "clock update was not activated; restored the previous configured cadence and {} the previous unit; recovery: {}",
                if rollback.activated {
                    "reactivated"
                } else {
                    "could not reactivate"
                },
                report.manual_steps.join("; "),
            )))
        }
        Err(error) => {
            save_clock_settings(global_root, previous)?;
            match install_clock_with(global_root, orbit_bin, previous, platform, runner, home) {
                Ok(rollback) => Err(OrbitError::Execution(format!(
                    "clock update failed: {error}; restored the previous configured cadence and {} the previous unit{}",
                    if rollback.activated {
                        "reactivated"
                    } else {
                        "could not reactivate"
                    },
                    if rollback.manual_steps.is_empty() {
                        String::new()
                    } else {
                        format!("; recovery: {}", rollback.manual_steps.join("; "))
                    }
                ))),
                Err(rollback_error) => Err(OrbitError::Execution(format!(
                    "clock update failed: {error}; restored the previous configured cadence but could not restore its native unit: {rollback_error}"
                ))),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockStatus {
    pub configured_cadence_seconds: u64,
    pub effective_cadence_seconds: Option<u64>,
    pub enabled: bool,
    /// Whether the native manager reports the unit as loaded.
    pub loaded: bool,
    /// Whether the native timer is currently active/waiting, when exposed.
    pub running: Option<bool>,
    /// Whether an enabled native clock has a future trigger. A paused clock is
    /// intentionally not schedulable and is not unhealthy.
    pub schedulable: bool,
    /// Actionable detail when an enabled clock cannot be shown to have a
    /// future trigger.
    pub health_issue: Option<String>,
    /// Most recent native timer trigger, in the manager's display format.
    pub last_tick_at: Option<String>,
    /// Next native timer trigger, in the manager's display format.
    pub next_tick_at: Option<String>,
    pub platform: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClockPlatform {
    Launchd,
    Systemd,
}

impl ClockPlatform {
    pub(crate) fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Launchd
        } else {
            Self::Systemd
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Launchd => "launchd",
            Self::Systemd => "systemd",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagerCommand {
    program: &'static str,
    args: Vec<String>,
}

impl ManagerCommand {
    pub(super) fn display(&self) -> String {
        std::iter::once(self.program.to_string())
            .chain(self.args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub(super) trait ClockCommandRunner {
    fn run(&self, command: &ManagerCommand) -> Result<bool, OrbitError>;
    fn stdout(&self, command: &ManagerCommand) -> Result<Option<String>, OrbitError>;

    fn probe(&self, command: &ManagerCommand) -> Result<ManagerCommandOutput, OrbitError> {
        let success = self.run(command)?;
        Ok(ManagerCommandOutput {
            success,
            exit_code: Some(if success { 0 } else { 1 }),
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagerCommandOutput {
    pub(super) success: bool,
    pub(super) exit_code: Option<i32>,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

struct NativeClockCommandRunner;

impl ClockCommandRunner for NativeClockCommandRunner {
    fn run(&self, command: &ManagerCommand) -> Result<bool, OrbitError> {
        Command::new(command.program)
            .args(&command.args)
            .output()
            .map(|output| output.status.success())
            .map_err(|error| OrbitError::Execution(format!("run {}: {error}", command.display())))
    }

    fn stdout(&self, command: &ManagerCommand) -> Result<Option<String>, OrbitError> {
        let output = self.probe(command)?;
        if output.success {
            Ok(Some(output.stdout))
        } else {
            Err(manager_probe_failure(command, &output))
        }
    }

    fn probe(&self, command: &ManagerCommand) -> Result<ManagerCommandOutput, OrbitError> {
        Command::new(command.program)
            .args(&command.args)
            .output()
            .map(|output| ManagerCommandOutput {
                success: output.status.success(),
                exit_code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
            .map_err(|error| OrbitError::Execution(format!("run {}: {error}", command.display())))
    }
}

/// Path launchd redirects `orbit clock tick` stdout/stderr to on macOS, and the
/// file `run_sweep` rotates so it stays bounded on an always-on host. Single
/// source of truth shared by the installer and the sweep
/// pass so the writer and the rotator never disagree. (Linux logs to the
/// journal, which rotates on its own, so only macOS needs this file.)
pub fn sweep_log_path(global_root: &Path) -> PathBuf {
    global_root.join("logs").join("sweep.log")
}

/// Resolve the launchd log beneath the canonical global root.
///
/// The root can come from an explicit runtime override, but the launchd log
/// location is fixed. Requiring the existing `logs` directory and log file to
/// resolve to their expected locations prevents a symlink from redirecting
/// launchd output outside the selected Orbit root.
pub(super) fn validated_sweep_log_path(global_root: &Path) -> Result<PathBuf, OrbitError> {
    let canonical_root = fs::canonicalize(global_root).map_err(|error| {
        OrbitError::Io(format!(
            "resolve sweep log root {}: {error}",
            global_root.display()
        ))
    })?;
    let expected_parent = canonical_root.join("logs");
    let expected_path = expected_parent.join("sweep.log");

    let canonical_parent = match fs::canonicalize(&expected_parent) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(expected_path);
        }
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "resolve sweep log directory {}: {error}",
                expected_parent.display()
            )));
        }
    };
    if canonical_parent != expected_parent || !canonical_parent.is_dir() {
        return Err(OrbitError::InvalidInput(format!(
            "sweep log directory must be a regular directory directly under {}",
            canonical_root.display()
        )));
    }

    let canonical_path = match fs::canonicalize(&expected_path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(expected_path);
        }
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "resolve sweep log {}: {error}",
                expected_path.display()
            )));
        }
    };
    if canonical_path != expected_path || !canonical_path.is_file() {
        return Err(OrbitError::InvalidInput(format!(
            "sweep log must be a regular file directly under {}",
            expected_parent.display()
        )));
    }

    Ok(canonical_path)
}

/// What an installation attempt did: files written, plus either a successful
/// activation or the commands the user must run themselves.
#[derive(Debug)]
pub struct ClockInstallReport {
    /// Unit files written.
    pub files_written: Vec<PathBuf>,
    /// True when the unit was activated successfully. On systemd this also
    /// means the manager reported it active with a finite future trigger.
    pub activated: bool,
    /// Follow-up commands to run manually when activation failed or was
    /// unavailable (e.g. no user systemd session).
    pub manual_steps: Vec<String>,
}

/// Render and install the platform clock unit for the current user, then try
/// to activate it. File writes are mandatory (errors propagate); activation
/// is best-effort with explicit manual steps on failure, because unit
/// managers behave differently across sessions (SSH, headless, containers).
pub fn install_clock(global_root: &Path) -> Result<ClockInstallReport, OrbitError> {
    let orbit_bin = std::env::current_exe()
        .map_err(|error| OrbitError::Io(format!("resolve current orbit executable: {error}")))?;
    let orbit_bin = orbit_bin.to_string_lossy().to_string();

    let settings = load_clock_settings(global_root)?;
    install_clock_with(
        global_root,
        &orbit_bin,
        settings,
        ClockPlatform::current(),
        &NativeClockCommandRunner,
        &home_dir()?,
    )
}

pub(super) fn install_clock_with(
    global_root: &Path,
    orbit_bin: &str,
    settings: ClockSettings,
    platform: ClockPlatform,
    runner: &dyn ClockCommandRunner,
    home: &Path,
) -> Result<ClockInstallReport, OrbitError> {
    match platform {
        ClockPlatform::Launchd => install_launchd(global_root, orbit_bin, settings, runner, home),
        ClockPlatform::Systemd => install_systemd(orbit_bin, settings, runner, home),
    }
}

fn install_launchd(
    global_root: &Path,
    orbit_bin: &str,
    settings: ClockSettings,
    runner: &dyn ClockCommandRunner,
    home: &Path,
) -> Result<ClockInstallReport, OrbitError> {
    let log_path = validated_sweep_log_path(global_root)?;
    let log_parent = log_path.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "sweep log path has no parent directory: {}",
            log_path.display()
        ))
    })?;
    fs::create_dir_all(log_parent).map_err(|error| OrbitError::Io(error.to_string()))?;
    let plist = LAUNCHD_PLIST_TEMPLATE
        .replace("{{ORBIT_BIN}}", orbit_bin)
        .replace("{{CADENCE_SECONDS}}", &settings.cadence_seconds.to_string())
        .replace("{{LOG_PATH}}", &log_path.to_string_lossy());

    let agents_dir = home.join("Library/LaunchAgents");
    fs::create_dir_all(&agents_dir).map_err(|error| OrbitError::Io(error.to_string()))?;
    let plist_path = launchd_plist_path(home);
    fs::write(&plist_path, plist).map_err(|error| {
        OrbitError::Io(format!(
            "failed to write '{}': {error}",
            plist_path.display()
        ))
    })?;

    // `launchctl load` is deprecated but still the most portable activation;
    // a stale agent is unloaded first so re-installs pick up the new binary
    // path.
    let unload = ManagerCommand {
        program: "launchctl",
        args: vec!["unload".into(), plist_path.display().to_string()],
    };
    let _ = runner.run(&unload);
    let load = ManagerCommand {
        program: "launchctl",
        args: vec!["load".into(), plist_path.display().to_string()],
    };
    let activated = runner.run(&load).unwrap_or(false);

    let manual_steps = if activated {
        Vec::new()
    } else {
        vec![format!("launchctl load {}", plist_path.display())]
    };
    Ok(ClockInstallReport {
        files_written: vec![plist_path],
        activated,
        manual_steps,
    })
}

fn install_systemd(
    orbit_bin: &str,
    settings: ClockSettings,
    runner: &dyn ClockCommandRunner,
    home: &Path,
) -> Result<ClockInstallReport, OrbitError> {
    let unit_dir = systemd_user_unit_dir(home);
    fs::create_dir_all(&unit_dir).map_err(|error| OrbitError::Io(error.to_string()))?;

    let service_path = systemd_service_path(home);
    let timer_path = systemd_timer_path(home);
    atomic_write_text(&service_path, &render_systemd_service(orbit_bin)).map_err(|error| {
        OrbitError::Io(format!(
            "failed to write '{}': {error}",
            service_path.display()
        ))
    })?;
    let timer = render_systemd_timer(settings);
    atomic_write_text(&timer_path, &timer).map_err(|error| {
        OrbitError::Io(format!(
            "failed to write '{}': {error}",
            timer_path.display()
        ))
    })?;

    let reload = systemd_daemon_reload_command();
    let enable = systemd_enable_command();
    let restart = systemd_restart_command();
    let reloaded = runner.run(&reload).unwrap_or(false);
    let enabled = reloaded && runner.run(&enable).unwrap_or(false);
    // `enable --now` is a no-op for an already-active timer, including the
    // broken `active (elapsed)` state this installer must repair. An explicit
    // restart re-arms the rendered timer on both fresh installs and upgrades.
    let restarted = enabled && runner.run(&restart).unwrap_or(false);
    let activated = if restarted {
        let details = query_systemd_clock_details(runner)?;
        if !details.is_schedulable() {
            return Err(systemd_unschedulable_error("restart completed"));
        }
        true
    } else {
        false
    };

    let manual_steps = if activated {
        Vec::new()
    } else {
        vec![
            "systemctl --user daemon-reload".to_string(),
            format!("systemctl --user enable {SYSTEMD_UNIT}.timer"),
            format!("systemctl --user restart {SYSTEMD_UNIT}.timer"),
        ]
    };
    Ok(ClockInstallReport {
        files_written: vec![service_path, timer_path],
        activated,
        manual_steps,
    })
}

/// Render the systemd service independently of the user manager environment.
pub(super) fn render_systemd_service(orbit_bin: &str) -> String {
    SYSTEMD_SERVICE_TEMPLATE.replace("{{ORBIT_BIN}}", orbit_bin)
}

pub(super) fn render_systemd_timer(settings: ClockSettings) -> String {
    SYSTEMD_TIMER_TEMPLATE.replace("{{CADENCE_SECONDS}}", &settings.cadence_seconds.to_string())
}

fn systemd_user_unit_dir(home: &Path) -> PathBuf {
    home.join(".config/systemd/user")
}

/// Per-user launchd agent path written by [`install_clock`].
pub(crate) fn launchd_plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

/// Per-user systemd service path written by [`install_clock`].
pub(crate) fn systemd_service_path(home: &Path) -> PathBuf {
    systemd_user_unit_dir(home).join(format!("{SYSTEMD_UNIT}.service"))
}

fn systemd_timer_path(home: &Path) -> PathBuf {
    systemd_user_unit_dir(home).join(format!("{SYSTEMD_UNIT}.timer"))
}

fn systemd_daemon_reload_command() -> ManagerCommand {
    ManagerCommand {
        program: "systemctl",
        args: vec!["--user".into(), "daemon-reload".into()],
    }
}

fn systemd_enable_command() -> ManagerCommand {
    ManagerCommand {
        program: "systemctl",
        args: vec![
            "--user".into(),
            "enable".into(),
            format!("{SYSTEMD_UNIT}.timer"),
        ],
    }
}

fn systemd_restart_command() -> ManagerCommand {
    ManagerCommand {
        program: "systemctl",
        args: vec![
            "--user".into(),
            "restart".into(),
            format!("{SYSTEMD_UNIT}.timer"),
        ],
    }
}

/// Rewrite an already-installed timer when it differs from the embedded
/// template. A missing unit is left to `systemctl enable`.
fn migrate_stale_systemd_timer(home: &Path, settings: ClockSettings) -> Result<(), OrbitError> {
    let timer_path = systemd_timer_path(home);
    if !timer_path.exists() {
        return Ok(());
    }
    let installed = fs::read_to_string(&timer_path).map_err(|error| {
        OrbitError::Io(format!(
            "failed to read '{}': {error}",
            timer_path.display()
        ))
    })?;
    let expected = render_systemd_timer(settings);
    if installed == expected {
        return Ok(());
    }
    atomic_write_text(&timer_path, &expected).map_err(|error| {
        OrbitError::Io(format!(
            "failed to write '{}': {error}",
            timer_path.display()
        ))
    })?;
    Ok(())
}

/// Rewrite an already-installed service when it differs from the embedded
/// template. In particular, this upgrades the compatibility invocation
/// `orbit sweep` to the canonical `orbit clock tick` while preserving the
/// installed Orbit program path.
fn migrate_stale_systemd_service(home: &Path) -> Result<(), OrbitError> {
    let service_path = systemd_service_path(home);
    if !service_path.exists() {
        return Ok(());
    }
    let installed = fs::read_to_string(&service_path).map_err(|error| {
        OrbitError::Io(format!(
            "failed to read '{}': {error}",
            service_path.display()
        ))
    })?;
    let orbit_bin = installed
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("ExecStart="))
        .and_then(|command| {
            let command = command.trim();
            command.strip_prefix('"').map_or_else(
                || command.split_whitespace().next(),
                |quoted| quoted.split('"').next(),
            )
        })
        .filter(|program| !program.is_empty())
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "installed clock service '{}' has no ExecStart program",
                service_path.display()
            ))
        })?;
    let expected = render_systemd_service(orbit_bin);
    if installed == expected {
        return Ok(());
    }
    atomic_write_text(&service_path, &expected).map_err(|error| {
        OrbitError::Io(format!(
            "failed to write '{}': {error}",
            service_path.display()
        ))
    })?;
    Ok(())
}

fn manager_status_command(platform: ClockPlatform) -> ManagerCommand {
    match platform {
        ClockPlatform::Launchd => ManagerCommand {
            program: "launchctl",
            args: vec!["list".into(), LAUNCHD_LABEL.into()],
        },
        ClockPlatform::Systemd => ManagerCommand {
            program: "systemctl",
            args: vec![
                "--user".into(),
                "is-enabled".into(),
                format!("{SYSTEMD_UNIT}.timer"),
            ],
        },
    }
}

fn launchd_manager_probe_command() -> ManagerCommand {
    ManagerCommand {
        program: "launchctl",
        args: vec!["list".into()],
    }
}

fn systemd_next_trigger_command() -> ManagerCommand {
    ManagerCommand {
        program: "systemctl",
        args: vec![
            "--user".into(),
            "show".into(),
            format!("{SYSTEMD_UNIT}.timer"),
            "--property=LoadState".into(),
            "--property=ActiveState".into(),
            "--property=NextElapseUSecRealtime".into(),
            "--property=NextElapseUSecMonotonic".into(),
            "--property=LastTriggerUSec".into(),
        ],
    }
}

fn manager_set_enabled_command(
    platform: ClockPlatform,
    enabled: bool,
    home: &Path,
) -> ManagerCommand {
    match (platform, enabled) {
        (ClockPlatform::Launchd, true) => ManagerCommand {
            program: "launchctl",
            args: vec![
                "load".into(),
                launchd_plist_path(home).display().to_string(),
            ],
        },
        (ClockPlatform::Launchd, false) => ManagerCommand {
            program: "launchctl",
            args: vec![
                "unload".into(),
                launchd_plist_path(home).display().to_string(),
            ],
        },
        (ClockPlatform::Systemd, true) => systemd_enable_command(),
        (ClockPlatform::Systemd, false) => ManagerCommand {
            program: "systemctl",
            args: vec![
                "--user".into(),
                "disable".into(),
                "--now".into(),
                format!("{SYSTEMD_UNIT}.timer"),
            ],
        },
    }
}

/// Enable or pause the native per-user clock. This does not touch the routine
/// store, so manual `orbit clock tick` and per-routine pause state remain available.
pub fn set_clock_enabled(global_root: &Path, enabled: bool) -> Result<ClockStatus, OrbitError> {
    set_clock_enabled_with(
        global_root,
        enabled,
        ClockPlatform::current(),
        &NativeClockCommandRunner,
        &home_dir()?,
    )
}

pub(super) fn set_clock_enabled_with(
    global_root: &Path,
    enabled: bool,
    platform: ClockPlatform,
    runner: &dyn ClockCommandRunner,
    home: &Path,
) -> Result<ClockStatus, OrbitError> {
    let settings = load_clock_settings(global_root)?;
    let current = if enabled {
        // Preserve the enable/rearm path: systemd always repairs and verifies
        // the unit below, while launchd keeps its existing idempotent load.
        runner
            .run(&manager_status_command(platform))
            .unwrap_or(false)
    } else {
        observe_clock_enabled_for_pause(platform, runner)?
    };
    if platform == ClockPlatform::Systemd && enabled {
        migrate_stale_systemd_service(home)?;
        migrate_stale_systemd_timer(home, settings)?;
        let reload = systemd_daemon_reload_command();
        if !runner.run(&reload)? {
            return Err(manager_command_error(&reload));
        }
        let enable = systemd_enable_command();
        if !runner.run(&enable)? {
            return Err(manager_command_error(&enable));
        }
        // Starting an already-active elapsed timer is a no-op. Restarting
        // after daemon-reload makes `enable` repair a stale installed unit
        // as well as resume a paused clock.
        let restart = systemd_restart_command();
        if !runner.run(&restart)? {
            return Err(manager_command_error(&restart));
        }
        let details = query_systemd_clock_details(runner)?;
        if !details.is_schedulable() {
            return Err(systemd_unschedulable_error("enable completed"));
        }
        return Ok(clock_status_from(
            settings,
            true,
            true,
            platform,
            None,
            Some(details),
        ));
    }
    if current == enabled {
        return Ok(clock_status_from(
            settings, enabled, enabled, platform, None, None,
        ));
    }
    let command = manager_set_enabled_command(platform, enabled, home);
    let succeeded = runner.run(&command)?;
    if !succeeded {
        return Err(manager_command_error(&command));
    }
    Ok(clock_status_from(
        settings, enabled, enabled, platform, None, None,
    ))
}

/// Observe enough native-manager state to make pause safe and idempotent.
/// A failed status command is inactive only when the manager's diagnostic is
/// a recognized disabled/not-loaded state; transport and ambiguous failures
/// must stop before the control path can mutate the manager.
fn observe_clock_enabled_for_pause(
    platform: ClockPlatform,
    runner: &dyn ClockCommandRunner,
) -> Result<bool, OrbitError> {
    let status_command = manager_status_command(platform);
    let status_output = runner
        .probe(&status_command)
        .map_err(|error| clock_manager_probe_error(platform, &status_command, &error))?;
    if status_output.success {
        return Ok(true);
    }

    match platform {
        ClockPlatform::Systemd if systemd_reports_disabled_or_missing(&status_output) => Ok(false),
        ClockPlatform::Systemd => Err(clock_manager_unavailable_error(
            platform,
            [(&status_command, &status_output)],
            None,
        )),
        ClockPlatform::Launchd if launchd_reports_not_loaded(&status_output) => Ok(false),
        ClockPlatform::Launchd => {
            let manager_command = launchd_manager_probe_command();
            let manager_output = runner
                .probe(&manager_command)
                .map_err(|error| clock_manager_probe_error(platform, &manager_command, &error))?;
            if manager_output.success {
                Ok(false)
            } else {
                Err(clock_manager_unavailable_error(
                    platform,
                    [
                        (&status_command, &status_output),
                        (&manager_command, &manager_output),
                    ],
                    None,
                ))
            }
        }
    }
}

pub fn clock_status(global_root: &Path) -> Result<ClockStatus, OrbitError> {
    clock_status_with(
        global_root,
        ClockPlatform::current(),
        &NativeClockCommandRunner,
    )
}

pub(super) fn clock_status_with(
    global_root: &Path,
    platform: ClockPlatform,
    runner: &dyn ClockCommandRunner,
) -> Result<ClockStatus, OrbitError> {
    let settings = load_clock_settings(global_root)?;
    let status_command = manager_status_command(platform);
    let status_output = runner
        .probe(&status_command)
        .map_err(|error| clock_manager_probe_error(platform, &status_command, &error))?;
    let enabled = match platform {
        ClockPlatform::Launchd if !status_output.success => {
            if launchd_reports_not_loaded(&status_output) {
                false
            } else {
                let manager_command = launchd_manager_probe_command();
                let manager_output = runner.probe(&manager_command).map_err(|error| {
                    clock_manager_probe_error(platform, &manager_command, &error)
                })?;
                if !manager_output.success {
                    return Err(clock_manager_unavailable_error(
                        platform,
                        [
                            (&status_command, &status_output),
                            (&manager_command, &manager_output),
                        ],
                        None,
                    ));
                }
                false
            }
        }
        _ => status_output.success,
    };
    let mut manager_details = None;
    let (schedulable, health_issue) = if platform == ClockPlatform::Systemd {
        match query_systemd_clock_details(runner) {
            Ok(details) => {
                let schedulable = enabled && details.is_schedulable();
                manager_details = Some(details);
                if !enabled {
                    (false, None)
                } else if schedulable {
                    (true, None)
                } else {
                    (
                        false,
                        Some(
                            "systemd timer is enabled but is not active with a finite future trigger; recovery: `orbit clock enable` rewrites a stale installed unit if needed, re-arms it, and verifies the result"
                                .to_string(),
                        ),
                    )
                }
            }
            Err(error) if enabled => (
                false,
                Some(format!(
                    "systemd timer is enabled but its next trigger could not be verified ({error}); recovery: inspect `systemctl --user status orbit-sweep.timer`, then run `orbit clock enable` to rewrite a stale unit if needed, re-arm, and verify it"
                )),
            ),
            Err(_) if systemd_reports_disabled_or_missing(&status_output) => (false, None),
            Err(error) => {
                return Err(clock_manager_unavailable_error(
                    platform,
                    [(&status_command, &status_output)],
                    Some(&error),
                ));
            }
        }
    } else {
        (enabled, None)
    };
    Ok(clock_status_from(
        settings,
        enabled,
        schedulable,
        platform,
        health_issue,
        manager_details,
    ))
}

fn systemd_reports_disabled_or_missing(output: &ManagerCommandOutput) -> bool {
    let diagnostic = format!("{}\n{}", output.stdout, output.stderr).to_ascii_lowercase();
    [
        "disabled",
        "masked",
        "static",
        "indirect",
        "generated",
        "transient",
        "not-found",
        "not found",
        "no such file or directory",
        "could not be found",
    ]
    .iter()
    .any(|marker| diagnostic.contains(marker))
}

fn launchd_reports_not_loaded(output: &ManagerCommandOutput) -> bool {
    let diagnostic = format!("{}\n{}", output.stdout, output.stderr).to_ascii_lowercase();
    [
        "could not find service",
        "service not found",
        "no such process",
    ]
    .iter()
    .any(|marker| diagnostic.contains(marker))
}

fn clock_manager_unavailable_error<'a, const N: usize>(
    platform: ClockPlatform,
    attempts: [(&'a ManagerCommand, &'a ManagerCommandOutput); N],
    detail_error: Option<&OrbitError>,
) -> OrbitError {
    let mut diagnostics = attempts
        .into_iter()
        .map(|(command, output)| manager_probe_diagnostic(command, output))
        .collect::<Vec<_>>();
    if let Some(error) = detail_error {
        diagnostics.push(bounded_manager_text(&error.to_string()));
    }
    OrbitError::Execution(format!(
        "{} clock manager is unavailable; {}. Check that the per-user {} manager is running and that this process can query it",
        platform.name(),
        diagnostics.join("; "),
        platform.name(),
    ))
}

fn manager_probe_failure(command: &ManagerCommand, output: &ManagerCommandOutput) -> OrbitError {
    OrbitError::Execution(manager_probe_diagnostic(command, output))
}

fn clock_manager_probe_error(
    platform: ClockPlatform,
    command: &ManagerCommand,
    error: &OrbitError,
) -> OrbitError {
    OrbitError::Execution(format!(
        "{} clock manager is unavailable; `{}` could not run: {}. Check that the per-user {} manager is installed and running and that this process can query it",
        platform.name(),
        command.display(),
        bounded_manager_text(&error.to_string()),
        platform.name(),
    ))
}

fn manager_probe_diagnostic(command: &ManagerCommand, output: &ManagerCommandOutput) -> String {
    let exit = output.exit_code.map_or_else(
        || "by signal".to_string(),
        |code| format!("with exit code {code}"),
    );
    let stdout = bounded_manager_text(&output.stdout);
    let stderr = bounded_manager_text(&output.stderr);
    let mut detail = Vec::new();
    if !stdout.is_empty() {
        detail.push(format!("stdout: {stdout}"));
    }
    if !stderr.is_empty() {
        detail.push(format!("stderr: {stderr}"));
    }
    if detail.is_empty() {
        format!("`{}` failed {exit} without output", command.display())
    } else {
        format!(
            "`{}` failed {exit} ({})",
            command.display(),
            detail.join(", ")
        )
    }
}

fn bounded_manager_text(value: &str) -> String {
    const MAX_DIAGNOSTIC_CHARS: usize = 512;

    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let bounded = chars
        .by_ref()
        .take(MAX_DIAGNOSTIC_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

#[derive(Debug, Default)]
struct SystemdClockDetails {
    loaded: Option<bool>,
    running: Option<bool>,
    last_tick_at: Option<String>,
    /// Wall-clock next elapse from `NextElapseUSecRealtime`. Monotonic values
    /// are durations from boot, not a next-tick timestamp.
    next_tick_at: Option<String>,
    next_elapse_known: bool,
}

impl SystemdClockDetails {
    fn parse(output: &str) -> Self {
        let property = |name: &str| {
            output.lines().find_map(|line| {
                let (key, value) = line.split_once('=')?;
                (key == name).then(|| useful_manager_value(value)).flatten()
            })
        };
        let next_tick_at = property("NextElapseUSecRealtime");
        let next_elapse_monotonic = property("NextElapseUSecMonotonic");
        Self {
            loaded: property("LoadState").map(|value| value == "loaded"),
            running: property("ActiveState").map(|value| value == "active"),
            last_tick_at: property("LastTriggerUSec"),
            next_elapse_known: next_tick_at.is_some() || next_elapse_monotonic.is_some(),
            next_tick_at,
        }
    }

    fn is_schedulable(&self) -> bool {
        self.loaded == Some(true) && self.running == Some(true) && self.next_elapse_known
    }
}

fn query_systemd_clock_details(
    runner: &dyn ClockCommandRunner,
) -> Result<SystemdClockDetails, OrbitError> {
    let command = systemd_next_trigger_command();
    let output = runner.stdout(&command)?.ok_or_else(|| {
        OrbitError::Execution(format!(
            "{} failed; inspect `systemctl --user status {SYSTEMD_UNIT}.timer`",
            command.display()
        ))
    })?;
    Ok(SystemdClockDetails::parse(&output))
}

fn manager_command_error(command: &ManagerCommand) -> OrbitError {
    OrbitError::Execution(format!(
        "{} failed; recovery: {}",
        command.display(),
        command.display()
    ))
}

fn systemd_unschedulable_error(action: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "systemd timer {action}, but the manager did not report it active with a finite future trigger; inspect `systemctl --user status {SYSTEMD_UNIT}.timer` and `journalctl --user -u {SYSTEMD_UNIT}.timer -u {SYSTEMD_UNIT}.service`"
    ))
}

fn useful_manager_value(raw: &str) -> Option<String> {
    let value = raw.trim();
    (!value.is_empty()
        && !matches!(
            value.to_ascii_lowercase().as_str(),
            "n/a" | "infinity" | "0"
        ))
    .then(|| value.to_string())
}

fn clock_status_from(
    settings: ClockSettings,
    enabled: bool,
    schedulable: bool,
    platform: ClockPlatform,
    health_issue: Option<String>,
    manager_details: Option<SystemdClockDetails>,
) -> ClockStatus {
    let details = manager_details.unwrap_or_default();
    ClockStatus {
        configured_cadence_seconds: settings.cadence_seconds,
        effective_cadence_seconds: (enabled && schedulable).then_some(settings.cadence_seconds),
        enabled,
        loaded: details.loaded.unwrap_or(enabled),
        running: details.running,
        schedulable,
        health_issue,
        last_tick_at: details.last_tick_at,
        next_tick_at: (enabled && schedulable)
            .then_some(details.next_tick_at)
            .flatten(),
        platform: platform.name(),
    }
}

fn home_dir() -> Result<PathBuf, OrbitError> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| OrbitError::InvalidInput("HOME is not set".to_string()))
}
