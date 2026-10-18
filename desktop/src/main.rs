//! Orbit desktop: a native window around the Orbit dashboard.
//!
//! The window first shows a local status page, starts `orbit web serve` from
//! the user's install (see [`sidecar`]), then loads the dashboard from its
//! loopback address, following it if the server re-announces. Loading the served page directly keeps every API call
//! same-origin, so the dashboard's Host/Origin checks apply unchanged, and
//! host switching works exactly as in the browser.
//!
//! The dashboard page is a remote origin to Tauri: no capability grants it a
//! command, and the window never navigates anywhere except the status page
//! and the running dashboard. Other links open in the default browser.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

mod tray;
mod update;
mod watch;

use std::path::PathBuf;
use std::sync::Arc;

use orbit_desktop::sidecar::{self, Shell, Sidecar};
use serde_json::Value;
use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::{AppHandle, Manager, RunEvent, State, Url, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_opener::OpenerExt;

/// Label of the one dashboard window.
pub(crate) const MAIN_WINDOW: &str = "main";

/// The supervisor's view of the app: its one window and its log directory.
struct Window(AppHandle);

impl Shell for Window {
    fn navigate(&self, url: Url) {
        if let Some(window) = self.0.get_webview_window(MAIN_WINDOW) {
            let _ = window.navigate(url);
        }
    }

    fn eval(&self, script: &str) {
        if let Some(window) = self.0.get_webview_window(MAIN_WINDOW) {
            let _ = window.eval(script);
        }
    }

    fn log_path(&self) -> Option<PathBuf> {
        self.0
            .path()
            .app_log_dir()
            .ok()
            .map(|dir| dir.join("dashboard-server.log"))
    }
}

/// The current startup status, for the status page.
#[tauri::command]
fn status(sidecar: State<'_, Arc<Sidecar>>) -> Value {
    sidecar.status()
}

/// Start the dashboard server again now.
#[tauri::command]
fn retry(sidecar: State<'_, Arc<Sidecar>>) {
    sidecar.retry();
}

fn main() {
    let (sidecar, control) = Sidecar::new();
    // A panic can end the app without its exit handler; stop the server
    // first so it is not left running.
    let on_panic = Arc::clone(&sidecar);
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        on_panic.kill_now();
        default_hook(info);
    }));
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        // Only the update thread uses it, from Rust; no page is granted it.
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        // Its commands are not granted to any page; the tray toggles it.
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(Arc::clone(&sidecar))
        .invoke_handler(tauri::generate_handler![status, retry])
        .on_window_event(|window, event| {
            // On macOS closing the window keeps the app in the menu bar, so
            // the tray and notifications carry on; Quit ends it.
            #[cfg(target_os = "macos")]
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (window, event);
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            let navigation = Arc::clone(&sidecar);
            let opener = handle.clone();
            let popups = handle.clone();
            let loads = Arc::clone(&sidecar);
            WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::App("index.html".into()))
                .title("Orbit")
                .inner_size(1440.0, 900.0)
                .min_inner_size(900.0, 600.0)
                .on_navigation(move |url| {
                    if navigation.allows_navigation(url) {
                        return true;
                    }
                    open_external(&opener, url);
                    false
                })
                .on_new_window(move |url, _features| {
                    open_external(&popups, &url);
                    NewWindowResponse::Deny
                })
                .on_page_load(move |_window, payload| {
                    if payload.event() == PageLoadEvent::Started {
                        loads.page_loading(payload.url());
                    }
                })
                .build()?;
            exit_on_signal(handle.clone())?;
            let (updates, update_requests) = update::channel();
            let (tray, update_item) = tray::build(&handle, Arc::clone(&sidecar), updates)?;
            let updater = handle.clone();
            std::thread::Builder::new()
                .name("updates".into())
                .spawn(move || update::run(updater, update_item, update_requests))?;
            let watched = Arc::clone(&sidecar);
            let watcher = handle.clone();
            std::thread::Builder::new()
                .name("dashboard-watch".into())
                .spawn(move || watch::run(watcher, watched, tray))?;
            let supervised = Arc::clone(&sidecar);
            std::thread::Builder::new()
                .name("dashboard-server".into())
                .spawn(move || sidecar::supervise(Window(handle), supervised, control))?;
            Ok(())
        })
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(error) => {
            #[allow(clippy::print_stderr)]
            {
                eprintln!("orbit desktop: {error}");
            }
            std::process::exit(1);
        }
    };
    app.run(|handle, event| match event {
        RunEvent::Exit => handle.state::<Arc<Sidecar>>().shutdown(),
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => tray::show_window(handle),
        _ => {}
    });
}

/// Hand a link the window will not load to the default browser.
fn open_external(app: &AppHandle, url: &Url) {
    if matches!(url.scheme(), "http" | "https" | "mailto") {
        let _ = app.opener().open_url(url.as_str(), None::<&str>);
    }
}

/// Turn SIGTERM, SIGINT and SIGHUP into a normal exit, so the exit handler
/// stops the dashboard server instead of leaving it running.
#[cfg(unix)]
fn exit_on_signal(app: AppHandle) -> std::io::Result<()> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    let mut signals = signal_hook::iterator::Signals::new([SIGTERM, SIGINT, SIGHUP])?;
    std::thread::Builder::new()
        .name("exit-signals".into())
        .spawn(move || {
            if signals.forever().next().is_some() {
                app.exit(0);
            }
        })?;
    Ok(())
}

#[cfg(not(unix))]
fn exit_on_signal(_app: AppHandle) -> std::io::Result<()> {
    Ok(())
}
