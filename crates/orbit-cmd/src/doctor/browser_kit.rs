//! The `browser-kit` rows: the host's headless browser kit, the Linux
//! libraries Chromium needs, `node` on the PATH workers get, and the `ui`
//! backlog tasks that wait for a kit this host lacks.
//!
//! A host without a kit is informational: many hosts never run UI checks.
//! It becomes a warning once `ui` tasks this host could otherwise start wait
//! for it, so a single-host user's UI work never waits unexplained.

use std::path::Path;

use orbit_core::OrbitRuntime;
use orbit_core::bootstrap::browser_kit::{KIT_HOME_RELATIVE, KitLayout, WORKER_ENV_NAMES};
use orbit_types::task::BROWSER_KIT_INSTALL_COMMAND;

use super::{WorkspaceDoctorResult, WorkspaceDoctorStatus};
use crate::browser_kit::{KitStatus, NODE_FIX, find_in_path_value, kit_status};

pub(super) fn browser_kit_rows(runtime: &OrbitRuntime) -> Vec<WorkspaceDoctorResult> {
    let mut rows = kit_rows(runtime);
    rows.extend(backlog_row(runtime));
    rows
}

/// A warning when backlog tasks tagged `ui` wait only for this host's kit:
/// its OS satisfies them and its kit is not verified. `None` when nothing
/// waits for it.
fn backlog_row(runtime: &OrbitRuntime) -> Option<WorkspaceDoctorResult> {
    let waiting = match runtime.browser_kit_backlog_waits() {
        Ok(waiting) => waiting,
        Err(error) => {
            return Some(row(
                "browser-kit-backlog",
                WorkspaceDoctorStatus::Warning,
                format!("cannot read the backlog for `ui` tasks: {error}"),
                Some("Rerun `orbit doctor` once the task store is readable.".to_string()),
            ));
        }
    };
    if waiting.is_empty() {
        return None;
    }
    Some(row(
        "browser-kit-backlog",
        WorkspaceDoctorStatus::Warning,
        format!(
            "{} backlog task(s) tagged `ui` wait for a host with a verified browser kit, and this \
             host has none: {}",
            waiting.len(),
            waiting.join(", ")
        ),
        Some(format!(
            "Run `{BROWSER_KIT_INSTALL_COMMAND}` on this host, or leave them for a pull-drain \
             follower that has a verified kit."
        )),
    ))
}

fn kit_rows(runtime: &OrbitRuntime) -> Vec<WorkspaceDoctorResult> {
    let layout = match KitLayout::current_user() {
        Ok(layout) => layout,
        Err(error) => {
            return vec![row(
                "browser-kit",
                WorkspaceDoctorStatus::Warning,
                format!("cannot locate the browser kit: {error}"),
                Some("Set HOME, then rerun `orbit doctor`.".to_string()),
            )];
        }
    };
    let status = kit_status(&layout);
    if !status.inspection.present {
        // Shown as `~/...` rather than the absolute path, so the message does not
        // embed the home directory of whichever process rendered it.
        return vec![row(
            "browser-kit",
            WorkspaceDoctorStatus::Info,
            format!(
                "no headless browser kit at ~/{KIT_HOME_RELATIVE}; workers cannot check a UI in a browser"
            ),
            Some(format!(
                "Run `{BROWSER_KIT_INSTALL_COMMAND}` on a host whose workers change web pages."
            )),
        )];
    }
    let mut rows = vec![kit_row(&status)];
    if let Some(problem) = status.library_problem() {
        rows.push(row(
            "browser-kit-libraries",
            WorkspaceDoctorStatus::Warning,
            problem.problem,
            Some(problem.fix),
        ));
    } else if status.unresolved_libraries.is_some() {
        rows.push(row(
            "browser-kit-libraries",
            WorkspaceDoctorStatus::Ok,
            "Chromium resolves every system library it needs".to_string(),
            None,
        ));
    }
    rows.push(node_row(runtime));
    rows
}

fn kit_row(status: &KitStatus) -> WorkspaceDoctorResult {
    let problems = status.problems();
    if problems.is_empty() {
        let kit = &status.inspection;
        return row(
            "browser-kit",
            WorkspaceDoctorStatus::Ok,
            format!(
                "Playwright {} with chromium_headless_shell-{}, verified {}; workers receive {}",
                kit.playwright_version.as_deref().unwrap_or("?"),
                kit.chromium_revision.as_deref().unwrap_or("?"),
                kit.verification
                    .as_ref()
                    .map_or("?", |verification| verification.verified_at.as_str()),
                WORKER_ENV_NAMES.join(", ")
            ),
            None,
        );
    }
    let mut fixes: Vec<String> = Vec::new();
    for problem in &problems {
        if !fixes.contains(&problem.fix) {
            fixes.push(problem.fix.clone());
        }
    }
    row(
        "browser-kit",
        WorkspaceDoctorStatus::Warning,
        problems
            .iter()
            .map(|problem| problem.problem.as_str())
            .collect::<Vec<_>>()
            .join("; "),
        Some(format!("Run `{}`.", fixes.join("`, then `"))),
    )
}

/// `node` as workers resolve it. Every agent session's PATH ends with the
/// login-shell PATH required validation resolves, so a `node` there reaches
/// workers started by hand and by the clock. The clock service's own PATH is
/// reported beside it.
fn node_row(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let environment = runtime.validation_environment();
    let login = environment.program_paths("node").into_iter().next();
    let service = clock_service_path();
    let service_node = service
        .as_ref()
        .and_then(|(_, path)| find_in_path_value("node", path));
    let login_text = login.as_ref().map_or_else(
        || format!("not on the {} PATH", environment.source.as_str()),
        |path| {
            format!(
                "{} on the {} PATH",
                path.display(),
                environment.source.as_str()
            )
        },
    );
    let service_text = match (&service, &service_node) {
        (None, _) => "no clock unit installed".to_string(),
        (Some((unit, _)), Some(path)) => format!("{} on {unit}'s PATH", path.display()),
        (Some((unit, _)), None) => format!("not on {unit}'s PATH"),
    };
    let message = format!("node: login shell: {login_text}; scheduler service: {service_text}");
    match (&login, &service_node) {
        (Some(_), _) => row("browser-kit-node", WorkspaceDoctorStatus::Ok, message, None),
        (None, Some(path)) => row(
            "browser-kit-node",
            WorkspaceDoctorStatus::Warning,
            message,
            Some(format!(
                "Put {} on PATH in the login profile (~/.profile or ~/.zprofile), or add it to \
                 `workflow.validation_env.path` with `orbit config set`, so drains started by \
                 hand find it too.",
                path.parent().unwrap_or(path).display()
            )),
        ),
        (None, None) => row(
            "browser-kit-node",
            WorkspaceDoctorStatus::Warning,
            message,
            Some(format!(
                "{NODE_FIX} Then make sure its directory is on the login profile's PATH or in \
                 `workflow.validation_env.path`."
            )),
        ),
    }
}

/// The installed clock unit and the PATH it starts workers with.
fn clock_service_path() -> Option<(String, String)> {
    let home = orbit_common::fs::path::home_dir().ok()?;
    let systemd = home.join(".config/systemd/user/orbit-sweep.service");
    if let Ok(unit) = std::fs::read_to_string(&systemd) {
        let path = systemd_unit_path(&unit, &home)
            // The user manager's default when the unit sets none.
            .unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".to_string());
        return Some(("orbit-sweep.service".to_string(), path));
    }
    let launchd = home.join("Library/LaunchAgents/com.orbit.sweep.plist");
    if let Ok(plist) = std::fs::read_to_string(&launchd) {
        let path =
            plist_path(&plist).unwrap_or_else(|| "/usr/bin:/bin:/usr/sbin:/sbin".to_string());
        return Some(("com.orbit.sweep".to_string(), path));
    }
    None
}

/// The last `Environment=PATH=` a systemd unit sets, with `%h` expanded.
fn systemd_unit_path(unit: &str, home: &Path) -> Option<String> {
    unit.lines()
        .filter_map(|line| line.trim().strip_prefix("Environment="))
        .flat_map(|assignments| assignments.split_whitespace())
        .filter_map(|assignment| assignment.trim_matches('"').strip_prefix("PATH="))
        .next_back()
        .map(|path| path.replace("%h", &home.display().to_string()))
}

/// The `PATH` a launchd plist's `EnvironmentVariables` sets.
fn plist_path(plist: &str) -> Option<String> {
    let after_key = plist.split("<key>PATH</key>").nth(1)?;
    let value = after_key.split("<string>").nth(1)?;
    value.split("</string>").next().map(str::to_string)
}

fn row(
    name: &str,
    status: WorkspaceDoctorStatus,
    message: String,
    remediation: Option<String>,
) -> WorkspaceDoctorResult {
    WorkspaceDoctorResult {
        duration_ms: 0,
        check_name: name.to_string(),
        status,
        message,
        remediation,
    }
}
