//! Render and install the platform clock unit, and the sweep log it writes to.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::fs::path::home_dir;

use super::converge::clear_clock_reload_pending;
use super::manager::{ClockCommandRunner, ClockPlatform, ManagerCommand, NativeClockCommandRunner};
use super::program::split_systemd_exec_start;
use super::settings::{ClockSettings, LAUNCHD_LABEL, SYSTEMD_UNIT, load_clock_settings};
use super::status::{query_systemd_clock_details, systemd_unschedulable_error};

const LAUNCHD_PLIST_TEMPLATE: &str = include_str!("../../../../assets/clock/com.orbit.sweep.plist");
const SYSTEMD_SERVICE_TEMPLATE: &str = include_str!("../../../../assets/clock/orbit-sweep.service");
const SYSTEMD_TIMER_TEMPLATE: &str = include_str!("../../../../assets/clock/orbit-sweep.timer");

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
    let report = match platform {
        ClockPlatform::Launchd => install_launchd(global_root, orbit_bin, settings, runner, home)?,
        ClockPlatform::Systemd => install_systemd(orbit_bin, settings, runner, home)?,
    };
    if report.activated {
        clear_clock_reload_pending(global_root)?;
    }
    Ok(report)
}

fn install_launchd(
    global_root: &Path,
    orbit_bin: &str,
    settings: ClockSettings,
    runner: &dyn ClockCommandRunner,
    home: &Path,
) -> Result<ClockInstallReport, OrbitError> {
    let plist_path = write_launchd_unit(global_root, orbit_bin, settings, home)?;
    let activated = reload_launchd_unit(runner, home);
    Ok(ClockInstallReport {
        manual_steps: launchd_manual_steps(activated, &plist_path),
        files_written: vec![plist_path],
        activated,
    })
}

/// Render the launchd agent and write it, without touching the unit manager.
///
/// Split from activation so unit convergence can repair a plist that names a
/// moved or deleted binary without resuming a clock the operator paused.
pub(super) fn write_launchd_unit(
    global_root: &Path,
    orbit_bin: &str,
    settings: ClockSettings,
    home: &Path,
) -> Result<PathBuf, OrbitError> {
    let log_path = validated_sweep_log_path(global_root)?;
    let log_parent = log_path.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "sweep log path has no parent directory: {}",
            log_path.display()
        ))
    })?;
    fs::create_dir_all(log_parent).map_err(|error| OrbitError::Io(error.to_string()))?;
    let plist = LAUNCHD_PLIST_TEMPLATE
        .replace("{{ORBIT_BIN}}", &plist_string(orbit_bin))
        .replace("{{CADENCE_SECONDS}}", &settings.cadence_seconds.to_string())
        .replace("{{LOG_PATH}}", &plist_string(&log_path.to_string_lossy()));

    let agents_dir = home.join("Library/LaunchAgents");
    fs::create_dir_all(&agents_dir).map_err(|error| OrbitError::Io(error.to_string()))?;
    let plist_path = launchd_plist_path(home);
    atomic_write_text(&plist_path, &plist).map_err(|error| {
        OrbitError::Io(format!(
            "failed to write '{}': {error}",
            plist_path.display()
        ))
    })?;
    Ok(plist_path)
}

/// Re-bootstrap the installed agent so a rewritten plist takes effect.
///
/// `launchctl load` is deprecated but still the most portable activation; a
/// stale agent is unloaded first so re-installs pick up the new binary path.
/// launchd keeps running the program the loaded job was registered with, so
/// rewriting the file alone never repairs a job in the penalty box.
pub(super) fn reload_launchd_unit(runner: &dyn ClockCommandRunner, home: &Path) -> bool {
    let plist_path = launchd_plist_path(home);
    let unload = ManagerCommand {
        program: "launchctl",
        args: vec!["unload".into(), plist_path.display().to_string()],
    };
    let _ = runner.run(&unload);
    let load = ManagerCommand {
        program: "launchctl",
        args: vec!["load".into(), plist_path.display().to_string()],
    };
    runner.run(&load).unwrap_or(false)
}

pub(super) fn launchd_manual_steps(activated: bool, plist_path: &Path) -> Vec<String> {
    if activated {
        Vec::new()
    } else {
        vec![format!("launchctl load {}", plist_path.display())]
    }
}

fn install_systemd(
    orbit_bin: &str,
    settings: ClockSettings,
    runner: &dyn ClockCommandRunner,
    home: &Path,
) -> Result<ClockInstallReport, OrbitError> {
    let files_written = write_systemd_units(orbit_bin, settings, home)?;

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

    Ok(ClockInstallReport {
        manual_steps: systemd_manual_steps(activated),
        files_written,
        activated,
    })
}

/// Render both systemd units and write them, without touching the manager.
pub(super) fn write_systemd_units(
    orbit_bin: &str,
    settings: ClockSettings,
    home: &Path,
) -> Result<Vec<PathBuf>, OrbitError> {
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
    atomic_write_text(&timer_path, &render_systemd_timer(settings)).map_err(|error| {
        OrbitError::Io(format!(
            "failed to write '{}': {error}",
            timer_path.display()
        ))
    })?;
    Ok(vec![service_path, timer_path])
}

/// Reload the manager and re-arm the installed timer, leaving its
/// enabled/disabled state alone.
pub(super) fn restart_systemd_unit(runner: &dyn ClockCommandRunner) -> bool {
    runner
        .run(&systemd_daemon_reload_command())
        .unwrap_or(false)
        && runner.run(&systemd_restart_command()).unwrap_or(false)
}

pub(super) fn systemd_manual_steps(activated: bool) -> Vec<String> {
    if activated {
        Vec::new()
    } else {
        vec![
            "systemctl --user daemon-reload".to_string(),
            format!("systemctl --user enable {SYSTEMD_UNIT}.timer"),
            format!("systemctl --user restart {SYSTEMD_UNIT}.timer"),
        ]
    }
}

/// Render the systemd service independently of the user manager environment.
pub(super) fn render_systemd_service(orbit_bin: &str) -> String {
    SYSTEMD_SERVICE_TEMPLATE.replace("{{ORBIT_BIN}}", &systemd_exec_program(orbit_bin))
}

/// Escape a value for a plist `<string>`; [`super::program`] reverses it.
fn plist_string(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `path` as the `ExecStart=` program. systemd expands `%` specifiers and
/// splits on whitespace, so `%` is doubled and a path that needs it is quoted
/// with C-style escapes. An ordinary path renders unchanged.
fn systemd_exec_program(path: &str) -> String {
    let escaped = path.replace('%', "%%");
    if escaped
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\'))
    {
        format!("\"{}\"", escaped.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        escaped
    }
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

pub(super) fn systemd_daemon_reload_command() -> ManagerCommand {
    ManagerCommand {
        program: "systemctl",
        args: vec!["--user".into(), "daemon-reload".into()],
    }
}

pub(super) fn systemd_enable_command() -> ManagerCommand {
    ManagerCommand {
        program: "systemctl",
        args: vec![
            "--user".into(),
            "enable".into(),
            format!("{SYSTEMD_UNIT}.timer"),
        ],
    }
}

pub(super) fn systemd_restart_command() -> ManagerCommand {
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
pub(super) fn migrate_stale_systemd_timer(
    home: &Path,
    settings: ClockSettings,
) -> Result<(), OrbitError> {
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
pub(super) fn migrate_stale_systemd_service(home: &Path) -> Result<(), OrbitError> {
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
    let (orbit_bin, _) = split_systemd_exec_start(&installed).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "installed clock service '{}' has no ExecStart program",
            service_path.display()
        ))
    })?;
    let expected = render_systemd_service(&orbit_bin);
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
