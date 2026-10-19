//! Reads the app's own dashboard server for the tray summary and native
//! notifications.
//!
//! Every [`INTERVAL`] the watcher reads `api/hosts` once, without probing, then
//! the fleet drain rows, the failed runs of the last day and the tasks in
//! review. Those three routes are read for this host and for each registered
//! remote that is not already known to be unreachable, the remotes through
//! `api/on/<host>/…`. Available workspaces also get a readiness read for
//! baseline-red task holds. The first answer of each only records what is there;
//! later ones notify for failed runs and review tasks that were not in the
//! previous answer, drain transitions to idle, and new fleet or task holds.
//! A host that does not answer keeps that baseline, so
//! recovery does not announce those items again, and its counts stay
//! unavailable instead of becoming zero. Review tasks of a workspace the
//! server marks unavailable stay remembered, so recovery does not announce
//! them again; a task that newly appears in review there still does.
//!
//! Notification text names what the dashboard's lists show for the item (a
//! task title, else the job name), or describes the drain or hold, and names
//! its workspace, never a run or task id.
//! When more than one host is registered, it names the host as well.
//! One task's leaf, gate and auto pipeline are a single failure, so they
//! count and notify once. A click on a failure or review notification opens
//! the dashboard at that run or task; the tray lists the same rows.

use std::sync::Arc;
use std::time::Duration;

use orbit_desktop::watcher::{Notice, PollSummary, Watcher};
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
        let summary = match watcher.poll(
            |path| get(&agent, &base, path),
            |notice| notify_notice(&app, &sidecar, notice),
        ) {
            PollSummary::Unreadable => Summary::Unreadable,
            PollSummary::Live {
                drains,
                failed,
                attention,
            } => Summary::Live {
                drains,
                failed,
                attention,
            },
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

/// Show a native notification. A click opens `notice.route` when it names a
/// task or run, and otherwise shows the dashboard window. Failures are
/// dropped: a notification is advisory and the dashboard holds the same
/// information.
fn notify_notice(app: &AppHandle, sidecar: &Arc<Sidecar>, notice: &Notice) {
    let mut notification = notify_rust::Notification::new();
    notification
        .appname("Orbit")
        .summary(&notice.title)
        .body(&notice.body)
        .action("default", "Open");
    let Ok(handle) = notification.show() else {
        return;
    };
    let app = app.clone();
    let sidecar = Arc::clone(sidecar);
    let route = notice.route.clone();
    // `wait_for_action` blocks until the user clicks or dismisses. The tray
    // keeps polling on this thread, so the wait runs on its own.
    let _ = std::thread::Builder::new()
        .name("notification-click".into())
        .spawn(move || {
            handle.wait_for_action(move |action| {
                if action != "default" {
                    return;
                }
                let app = app.clone();
                let sidecar = Arc::clone(&sidecar);
                let _ = app.run_on_main_thread({
                    let app = app.clone();
                    move || {
                        crate::tray::open_route(&app, &sidecar, route.as_ref());
                    }
                });
            });
        });
}

/// Show a native notification that does not open a route. Used for tray
/// errors such as a login-item change that did not apply.
pub(crate) fn notify(title: &str, body: &str) {
    let _ = notify_rust::Notification::new()
        .appname("Orbit")
        .summary(title)
        .body(body)
        .show();
}
