//! The menu-bar (macOS) or system-tray (Linux) icon.
//!
//! It shows how many drains are running and how many runs failed in the last
//! day, as [`crate::watch`] last read them from the app's dashboard server,
//! and holds the app's global actions: open the window, restart the server,
//! launch at login and quit.

use std::sync::Arc;

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_autostart::ManagerExt;

use crate::MAIN_WINDOW;
use crate::sidecar::Sidecar;

const OPEN: &str = "open";
const RESTART: &str = "restart";
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
    },
}

impl Summary {
    /// The menu's status line.
    fn line(&self) -> String {
        match self {
            Self::Offline => "Dashboard server not running".into(),
            Self::Unreadable => "Dashboard server not responding".into(),
            Self::Live { drains, failed } => {
                let drains = match drains {
                    Some(1) => "1 drain running".into(),
                    Some(n) => format!("{n} drains running"),
                    None => "Drains unavailable".into(),
                };
                let failed = match failed {
                    Some(1) => "1 failed run in 24 h".into(),
                    Some(n) => format!("{n} failed runs in 24 h"),
                    None => "failed runs unavailable".into(),
                };
                format!("{drains} · {failed}")
            }
        }
    }

    /// The short text beside the icon; nothing when all is quiet.
    fn badge(&self) -> Option<String> {
        let Self::Live { drains, failed } = self else {
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
    icon: TrayIcon,
    status: MenuItem<Wry>,
}

impl Tray {
    pub(crate) fn show(&self, summary: &Summary) {
        let line = summary.line();
        let _ = self.status.set_text(&line);
        let _ = self.icon.set_tooltip(Some(format!("Orbit: {line}")));
        let _ = self.icon.set_title(summary.badge());
    }
}

pub(crate) fn build(app: &AppHandle, sidecar: Arc<Sidecar>) -> tauri::Result<Tray> {
    let status = MenuItem::with_id(app, "status", Summary::Offline.line(), false, None::<&str>)?;
    let open = MenuItem::with_id(app, OPEN, "Open Orbit", true, None::<&str>)?;
    let restart = MenuItem::with_id(app, RESTART, "Restart Dashboard Server", true, None::<&str>)?;
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
            &login,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    let mut builder = TrayIconBuilder::with_id("orbit")
        .menu(&menu)
        .tooltip("Orbit")
        .on_menu_event(move |app, event| match event.id().as_ref() {
            OPEN => show_window(app),
            RESTART => sidecar.restart(),
            LOGIN => toggle_launch_at_login(app, &login),
            QUIT => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let icon = builder.build(app)?;
    Ok(Tray { icon, status })
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
