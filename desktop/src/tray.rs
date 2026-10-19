//! The menu-bar (macOS) or system-tray (Linux) icon.
//!
//! It shows how many drains are running and how many distinct task failures
//! were in the last day, as [`crate::watch`] last read them from the app's
//! dashboard server, plus a bounded list of those failures and of tasks in
//! review. Each of those rows opens the dashboard at that run or task. It also
//! holds the app's global actions: open the window, restart the server, check
//! for or install an update, launch at login and quit.

use std::sync::{Arc, Mutex};

use orbit_desktop::watcher::{AttentionItem, DashboardRoute};
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_autostart::ManagerExt;

use crate::MAIN_WINDOW;
use crate::sidecar::Sidecar;
use crate::update::{self, Requests};

const OPEN: &str = "open";
const RESTART: &str = "restart";
const UPDATE: &str = "update";
const LOGIN: &str = "launch-at-login";
const QUIT: &str = "quit";

/// What the tray reports.
pub(crate) enum Summary {
    /// The dashboard server is not running yet, or is restarting.
    Offline,
    /// The server is running but did not answer.
    Unreadable,
    /// Counts from the server; `None` where that read failed.
    Live {
        drains: Option<usize>,
        failed: Option<u64>,
        attention: Vec<AttentionItem>,
    },
}

impl Summary {
    /// The menu's status line.
    fn line(&self) -> String {
        match self {
            Self::Offline => "Dashboard server not running".into(),
            Self::Unreadable => "Dashboard server not responding".into(),
            Self::Live { drains, failed, .. } => {
                let drains = match drains {
                    Some(1) => "1 drain running".into(),
                    Some(n) => format!("{n} drains running"),
                    None => "Drains unavailable".into(),
                };
                let failed = match failed {
                    Some(1) => "1 failure in 24 h".into(),
                    Some(n) => format!("{n} failures in 24 h"),
                    None => "failures unavailable".into(),
                };
                format!("{drains} · {failed}")
            }
        }
    }

    /// The short text beside the icon; nothing when all is quiet.
    fn badge(&self) -> Option<String> {
        let Self::Live { drains, failed, .. } = self else {
            return None;
        };
        let mut parts = Vec::new();
        if let Some(drains) = drains.filter(|count| *count > 0) {
            parts.push(format!("▶{drains}"));
        }
        if let Some(failed) = failed.filter(|count| *count > 0) {
            parts.push(format!("✕{failed}"));
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }
}

/// The tray icon and the menu item it updates.
pub(crate) struct Tray {
    app: AppHandle,
    icon: TrayIcon,
    status: MenuItem<Wry>,
    open: MenuItem<Wry>,
    restart: MenuItem<Wry>,
    update: MenuItem<Wry>,
    login: CheckMenuItem<Wry>,
    quit: MenuItem<Wry>,
    /// Routes for `attention-<index>` menu ids, replaced on each refresh.
    routes: Arc<Mutex<Vec<DashboardRoute>>>,
}

impl Tray {
    pub(crate) fn show(&self, summary: &Summary) {
        let line = summary.line();
        let _ = self.status.set_text(&line);
        let _ = self.icon.set_tooltip(Some(format!("Orbit: {line}")));
        let _ = self.icon.set_title(summary.badge());
        let attention = match summary {
            Summary::Live { attention, .. } => attention.as_slice(),
            Summary::Offline | Summary::Unreadable => &[],
        };
        self.install_menu(attention);
    }

    /// Rebuild the menu so failed and review rows match this poll.
    /// The status line is already updated; a menu that fails to build keeps
    /// the previous rows.
    fn install_menu(&self, attention: &[AttentionItem]) {
        let mut routes = Vec::new();
        let mut rows = Vec::new();
        for (index, item) in attention.iter().enumerate() {
            let id = format!("attention-{index}");
            let Ok(row) = MenuItem::with_id(&self.app, &id, item.menu_label(), true, None::<&str>)
            else {
                continue;
            };
            routes.push(item.route.clone());
            rows.push(row);
        }
        let Ok(separator_attention) = PredefinedMenuItem::separator(&self.app) else {
            return;
        };
        let Ok(separator_actions) = PredefinedMenuItem::separator(&self.app) else {
            return;
        };
        let Ok(separator_update) = PredefinedMenuItem::separator(&self.app) else {
            return;
        };
        let Ok(separator_quit) = PredefinedMenuItem::separator(&self.app) else {
            return;
        };
        let mut entries: Vec<&dyn IsMenuItem<Wry>> = Vec::new();
        entries.push(&self.status);
        if !rows.is_empty() {
            entries.push(&separator_attention);
            for row in &rows {
                entries.push(row);
            }
        }
        entries.push(&separator_actions);
        entries.push(&self.open);
        entries.push(&self.restart);
        entries.push(&separator_update);
        entries.push(&self.update);
        entries.push(&self.login);
        entries.push(&separator_quit);
        entries.push(&self.quit);
        let Ok(menu) = Menu::with_items(&self.app, &entries) else {
            return;
        };
        // Publish routes before the menu that names them, so a click cannot
        // land on a new row whose route is not stored yet.
        match self.routes.lock() {
            Ok(mut guard) => *guard = routes,
            Err(poisoned) => *poisoned.into_inner() = routes,
        }
        let _ = self.icon.set_menu(Some(menu));
    }
}

/// Build the tray. The returned item is the update entry, which the update
/// thread relabels.
pub(crate) fn build(
    app: &AppHandle,
    sidecar: Arc<Sidecar>,
    updates: Requests,
) -> tauri::Result<(Tray, MenuItem<Wry>)> {
    let status = MenuItem::with_id(app, "status", Summary::Offline.line(), false, None::<&str>)?;
    let open = MenuItem::with_id(app, OPEN, "Open Orbit", true, None::<&str>)?;
    let restart = MenuItem::with_id(app, RESTART, "Restart Dashboard Server", true, None::<&str>)?;
    let update = MenuItem::with_id(app, UPDATE, update::CHECK_LABEL, true, None::<&str>)?;
    let login = CheckMenuItem::with_id(
        app,
        LOGIN,
        "Launch at Login",
        true,
        app.autolaunch().is_enabled().unwrap_or(false),
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, QUIT, "Quit Orbit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &restart,
            &PredefinedMenuItem::separator(app)?,
            &update,
            &login,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    let routes = Arc::new(Mutex::new(Vec::new()));
    let clicked_routes = Arc::clone(&routes);
    let clicked_sidecar = Arc::clone(&sidecar);
    let clicked_login = login.clone();
    let mut builder = TrayIconBuilder::with_id("orbit")
        .menu(&menu)
        .tooltip("Orbit")
        .on_menu_event(move |app, event| match event.id().as_ref() {
            OPEN => show_window(app),
            RESTART => clicked_sidecar.restart(),
            UPDATE => updates.activate(),
            LOGIN => toggle_launch_at_login(app, &clicked_login),
            QUIT => app.exit(0),
            id => {
                let Some(index) = id
                    .strip_prefix("attention-")
                    .and_then(|rest| rest.parse::<usize>().ok())
                else {
                    return;
                };
                let route = match clicked_routes.lock() {
                    Ok(guard) => guard.get(index).cloned(),
                    Err(poisoned) => poisoned.into_inner().get(index).cloned(),
                };
                if let Some(route) = route {
                    open_route(app, &clicked_sidecar, Some(&route));
                }
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let icon = builder.build(app)?;
    Ok((
        Tray {
            app: app.clone(),
            icon,
            status,
            open,
            restart,
            update: update.clone(),
            login,
            quit,
            routes,
        },
        update,
    ))
}

/// Show the dashboard window, and when `route` is set load that task or run.
pub(crate) fn open_route(app: &AppHandle, sidecar: &Sidecar, route: Option<&DashboardRoute>) {
    show_window(app);
    let Some(route) = route else {
        return;
    };
    let Some(base) = sidecar.dashboard() else {
        return;
    };
    let url = route.location(&base);
    if !sidecar.allows_navigation(&url) {
        return;
    }
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.navigate(url);
    }
}

/// Show, restore and focus the dashboard window.
pub(crate) fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Flip the login item, then show its real state: the menu toggles its check
/// mark on click whether or not the change took.
fn toggle_launch_at_login(app: &AppHandle, item: &CheckMenuItem<Wry>) {
    let autolaunch = app.autolaunch();
    let enabled = autolaunch.is_enabled().unwrap_or(false);
    let changed = if enabled {
        autolaunch.disable()
    } else {
        autolaunch.enable()
    };
    let _ = item.set_checked(autolaunch.is_enabled().unwrap_or(enabled));
    if let Err(error) = changed {
        crate::watch::notify(
            "Could not change Launch at Login",
            &format!("The login item could not be updated: {error}"),
        );
    }
}
