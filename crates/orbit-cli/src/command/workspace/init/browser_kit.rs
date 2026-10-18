//! The browser-kit offer `workspace init` makes for a repository with a web
//! UI. The kit is installed only on `--install-browser-kit` or a `y` typed at
//! a terminal; anything else leaves a hint and installs nothing.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use orbit_cmd::browser_kit::{self, InstallOptions};
use orbit_core::bootstrap::browser_kit::{KitInspection, KitLayout};
use serde_json::{Value, json};

const INSTALL: &str = "orbit browser-kit install";

pub(super) enum BrowserKitOutcome {
    /// No `package.json` or HTML file, so nothing to offer.
    NoWebUi,
    /// A verified kit is already there.
    Ready { marker: String },
    /// Not installed: no terminal to ask, or the operator declined.
    Offered { marker: String, declined: bool },
    /// `install` ran; `ready` is whether workers now receive the kit.
    Installed {
        marker: String,
        ready: bool,
        error: Option<String>,
    },
}

impl BrowserKitOutcome {
    /// An install the operator asked for left no verified kit.
    pub(super) fn failed(&self) -> bool {
        matches!(self, Self::Installed { ready: false, .. })
    }
}

pub(super) fn offer_browser_kit(root: &Path, install_requested: bool) -> BrowserKitOutcome {
    let Some(marker) = browser_kit::web_ui_marker(root) else {
        return BrowserKitOutcome::NoWebUi;
    };
    let layout = match KitLayout::current_user() {
        Ok(layout) => layout,
        Err(error) if install_requested => {
            return BrowserKitOutcome::Installed {
                marker,
                ready: false,
                error: Some(error.to_string()),
            };
        }
        Err(_) => {
            return BrowserKitOutcome::Offered {
                marker,
                declined: false,
            };
        }
    };
    if KitInspection::inspect(&layout).is_verified() {
        return BrowserKitOutcome::Ready { marker };
    }
    let install = install_requested || (interactive() && operator_accepts(&marker));
    if !install {
        return BrowserKitOutcome::Offered {
            declined: interactive(),
            marker,
        };
    }
    match browser_kit::install(&layout, InstallOptions::default(), &mut |step| {
        eprintln!("{step}...");
    }) {
        Ok(report) => BrowserKitOutcome::Installed {
            marker,
            ready: report.ready(),
            error: (!report.ready()).then(|| {
                "the kit is not verified; `orbit browser-kit status` names what is missing"
                    .to_string()
            }),
        },
        Err(error) => BrowserKitOutcome::Installed {
            marker,
            ready: false,
            error: Some(error.to_string()),
        },
    }
}

fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn operator_accepts(marker: &str) -> bool {
    let mut stderr = std::io::stderr();
    let asked = write!(
        stderr,
        "This repository has a web UI ({marker}). Install the headless browser kit workers \
         check it with? Downloads Playwright and Chromium into ~/.local/chromium-deps. [y/N] "
    )
    .and_then(|()| stderr.flush());
    if asked.is_err() {
        return false;
    }
    let mut answer = String::new();
    if std::io::stdin().lock().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

pub(super) fn browser_kit_json(outcome: &BrowserKitOutcome) -> Value {
    match outcome {
        BrowserKitOutcome::NoWebUi => json!({ "status": "no_web_ui" }),
        BrowserKitOutcome::Ready { marker } => json!({ "status": "ready", "web_ui": marker }),
        BrowserKitOutcome::Offered { marker, declined } => json!({
            "status": if *declined { "declined" } else { "offered" },
            "web_ui": marker,
            "next": INSTALL,
        }),
        BrowserKitOutcome::Installed {
            marker,
            ready,
            error,
        } => json!({
            "status": if *ready { "installed" } else { "failed" },
            "web_ui": marker,
            "error": error,
            "next": (!ready).then_some(INSTALL),
        }),
    }
}

pub(super) fn browser_kit_line(outcome: &BrowserKitOutcome) -> Option<String> {
    let line = match outcome {
        BrowserKitOutcome::NoWebUi => return None,
        BrowserKitOutcome::Ready { .. } => "verified; workers receive it".to_string(),
        BrowserKitOutcome::Offered { marker, .. } => format!(
            "not installed; this repository has a web UI ({marker}). Run `{INSTALL}` (or pass \
             --install-browser-kit) so workers can check it in a browser"
        ),
        BrowserKitOutcome::Installed { ready: true, .. } => {
            "installed and verified; workers receive it".to_string()
        }
        BrowserKitOutcome::Installed { error, .. } => format!(
            "install failed: {}; rerun `{INSTALL}`",
            error.as_deref().unwrap_or("unknown error")
        ),
    };
    Some(format!("  browser:   {line}"))
}
