//! Reads the app's own dashboard server for the tray summary and native
//! notifications.
//!
//! Every [`INTERVAL`] the watcher reads three `/api` routes of the server the
//! [`Sidecar`] runs: the fleet drain rows, the failed runs of the last day and
//! the tasks in review, all across this host's workspaces. The first answer
//! of each only records what is there; later ones notify for failed runs and
//! review tasks that were not in the previous answer. Review tasks of a
//! workspace the server marks unavailable stay remembered, so recovery does
//! not announce them again; a task that newly appears in review there still
//! does.
//!
//! Notification text names what the dashboard's lists show for the item (a
//! task title, else the job name) and its workspace, never a run or task id.

use std::sync::Arc;
use std::time::Duration;

use orbit_desktop::watcher::{PollSummary, Watcher};
use serde_json::Value;
use tauri::{AppHandle, Url};

use crate::sidecar::Sidecar;
use crate::tray::{Summary, Tray};

/// Time between reads.
const INTERVAL: Duration = Duration::from_secs(20);
/// Upper bound on one request, connection included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Read the dashboard server until the app exits.
pub(crate) fn run(app: AppHandle, sidecar: Arc<Sidecar>, tray: Tray) {
    init_notifications(&app);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        // The server is on loopback; a proxy from the environment must not
        // see these requests.
        .proxy(None)
        .build()
        .into();
    let mut watcher = Watcher::default();
    while !sidecar.is_shutting_down() {
        let Some(base) = sidecar.dashboard() else {
            tray.show(&Summary::Offline);
            std::thread::sleep(INTERVAL);
            continue;
        };
        let summary = match watcher.poll(|path| get(&agent, &base, path), notify) {
            PollSummary::Unreadable => Summary::Unreadable,
            PollSummary::Live { drains, failed } => Summary::Live { drains, failed },
        };
        tray.show(&summary);
        std::thread::sleep(INTERVAL);
    }
}

/// GET a dashboard route and parse its JSON body.
fn get(agent: &ureq::Agent, base: &Url, path: &str) -> Option<Value> {
    let url = base.join(path).ok()?;
    let body = agent
        .get(url.as_str())
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;
    serde_json::from_str(&body).ok()
}

/// Name the app notifications appear under. An unbundled development build
/// has no registered bundle, so on macOS it borrows Terminal's, as Tauri's
/// own notification plugin does.
fn init_notifications(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let identifier = if tauri::is_dev() {
            "com.apple.Terminal"
        } else {
            app.config().identifier.as_str()
        };
        let _ = notify_rust::set_application(identifier);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// Show a native notification. Failures are dropped: a notification is
/// advisory and the dashboard holds the same information.
pub(crate) fn notify(title: &str, body: &str) {
    let _ = notify_rust::Notification::new()
        .appname("Orbit")
        .summary(title)
        .body(body)
        .show();
}
