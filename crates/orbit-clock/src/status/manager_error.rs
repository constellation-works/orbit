//! Bounded native manager diagnostics and recovery errors.

use orbit_common::OrbitError;

use crate::manager::{ClockPlatform, ManagerCommand, ManagerCommandOutput};
use crate::settings::SYSTEMD_UNIT;

pub(crate) fn clock_manager_unavailable_error<'a>(
    platform: ClockPlatform,
    attempts: &[(&'a ManagerCommand, &'a ManagerCommandOutput)],
    detail_error: Option<&OrbitError>,
) -> OrbitError {
    let mut diagnostics = attempts
        .iter()
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

pub(crate) fn manager_probe_failure(
    command: &ManagerCommand,
    output: &ManagerCommandOutput,
) -> OrbitError {
    OrbitError::Execution(manager_probe_diagnostic(command, output))
}

pub(crate) fn clock_manager_probe_error(
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

pub(super) fn manager_probe_diagnostic(
    command: &ManagerCommand,
    output: &ManagerCommandOutput,
) -> String {
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

pub(super) fn bounded_manager_text(value: &str) -> String {
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

pub(crate) fn manager_command_error(command: &ManagerCommand) -> OrbitError {
    OrbitError::Execution(format!(
        "{} failed; recovery: {}",
        command.display(),
        command.display()
    ))
}

pub(crate) fn systemd_unschedulable_error(action: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "systemd timer {action}, but the manager did not report it active with a finite future trigger; inspect `systemctl --user status {SYSTEMD_UNIT}.timer` and `journalctl --user -u {SYSTEMD_UNIT}.timer -u {SYSTEMD_UNIT}.service`"
    ))
}

pub(super) fn useful_manager_value(raw: &str) -> Option<String> {
    let value = raw.trim();
    (!value.is_empty()
        && !matches!(
            value.to_ascii_lowercase().as_str(),
            "n/a" | "infinity" | "0"
        ))
    .then(|| value.to_string())
}
