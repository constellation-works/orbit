//! Checks for, and on request installs, a newer release of this app.
//!
//! A minute after launch, then once a day, the app looks for a newer stable
//! release that carries desktop assets (see [`release`]). It authenticates
//! the release's signed checksum manifest against the Orbit release keys in
//! [`trust`] before it says anything. A verified update raises one
//! notification, and the tray's update item becomes "Install Orbit X.Y.Z…".
//! Choosing it asks for confirmation, then downloads, verifies and installs
//! the bundle as [`install`] describes. Nothing is ever installed without
//! that confirmation.
//!
//! An unreachable GitHub is silent unless the user asked for the check. A
//! manifest that fails verification always notifies, because it means the
//! release's desktop assets can't be trusted.

mod install;
mod release;
mod trust;

#[cfg(test)]
mod tests;

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant};

use tauri::menu::MenuItem;
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;

use self::install::{Install, Outcome};
use self::release::{CheckError, Releases, Verified, Version};
use crate::watch::notify;

/// The first check waits this long, so it doesn't compete with the
/// dashboard server's start.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(60);
/// Time between automatic checks.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Room for a few menu clicks while a check or install runs.
const REQUEST_CAPACITY: usize = 4;

/// The tray item's text while no update is waiting.
pub(crate) const CHECK_LABEL: &str = "Check for Updates…";

/// What the tray asks of the update thread.
pub(crate) enum Request {
    /// The update item was chosen: check now, or install the pending update.
    Activate,
}

/// The tray's handle on the update thread.
#[derive(Clone)]
pub(crate) struct Requests(SyncSender<Request>);

impl Requests {
    pub(crate) fn activate(&self) {
        let _ = self.0.try_send(Request::Activate);
    }
}

pub(crate) fn channel() -> (Requests, Receiver<Request>) {
    let (sender, receiver) = mpsc::sync_channel(REQUEST_CAPACITY);
    (Requests(sender), receiver)
}

/// Check and install updates until the app exits.
pub(crate) fn run(app: AppHandle, item: MenuItem<Wry>, requests: Receiver<Request>) {
    let (Some(current), Some(install)) = (Version::current(), Install::detect()) else {
        let _ = item.set_text("Updates Unavailable in This Build");
        let _ = item.set_enabled(false);
        return;
    };
    let Some(asset) = install.asset() else {
        let _ = item.set_text("Updates Unavailable on This Platform");
        let _ = item.set_enabled(false);
        return;
    };
    let mut updater = Updater {
        app,
        item,
        releases: Releases::new(),
        install,
        asset,
        current,
        pending: None,
        announced: None,
    };
    let mut next_check = Instant::now() + FIRST_CHECK_DELAY;
    loop {
        match requests.recv_timeout(next_check.saturating_duration_since(Instant::now())) {
            Ok(Request::Activate) => match updater.pending.clone() {
                Some(update) => updater.offer(&update),
                None => updater.check(true),
            },
            Err(RecvTimeoutError::Timeout) => {
                updater.check(false);
                next_check = Instant::now() + CHECK_INTERVAL;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

struct Updater {
    app: AppHandle,
    item: MenuItem<Wry>,
    releases: Releases,
    install: Install,
    asset: &'static str,
    current: Version,
    /// A verified update waiting for the user.
    pending: Option<Verified>,
    /// The newest version already announced, so daily checks don't repeat it.
    announced: Option<Version>,
}

impl Updater {
    /// Look for an update. `manual` checks report every result; automatic
    /// ones report only a new update or a rejected one.
    fn check(&mut self, manual: bool) {
        let _ = self.item.set_enabled(false);
        let _ = self.item.set_text("Checking for Updates…");
        let today = chrono::Utc::now().date_naive();
        match self
            .releases
            .check(self.current, self.asset, trust::TRUSTED_RELEASE_KEYS, today)
        {
            Ok(Some(update)) => {
                if manual || self.announced != Some(update.version) {
                    notify(
                        &format!("Orbit {} is available", update.version),
                        &format!(
                            "Choose Install Orbit {}… in the Orbit menu to update.",
                            update.version
                        ),
                    );
                }
                self.announced = Some(update.version);
                self.pending = Some(update);
            }
            Ok(None) => {
                self.pending = None;
                if manual {
                    notify(
                        "Orbit is up to date",
                        &format!("Version {} is the newest desktop release.", self.current),
                    );
                }
            }
            Err(CheckError::Unavailable(error)) => {
                if manual {
                    notify("Couldn't check for updates", &error);
                }
            }
            Err(CheckError::Rejected(error)) => {
                self.pending = None;
                notify(
                    "Orbit update rejected",
                    &format!("{error}. Nothing was downloaded or installed."),
                );
            }
        }
        self.show_item();
    }

    /// Ask to install `update`, and install it on a yes.
    fn offer(&mut self, update: &Verified) {
        let confirmed = self
            .app
            .dialog()
            .message(self.install.describe())
            .title(format!("Install Orbit {}?", update.version))
            .kind(MessageDialogKind::Info)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Install".into(),
                "Later".into(),
            ))
            .blocking_show();
        if !confirmed {
            return;
        }
        let _ = self.item.set_enabled(false);
        let _ = self
            .item
            .set_text(format!("Installing Orbit {}…", update.version));
        let downloads = self
            .app
            .path()
            .download_dir()
            .or_else(|_| self.app.path().home_dir())
            .unwrap_or_else(|_| std::env::temp_dir());
        let identifier = self.app.config().identifier.clone();
        match self
            .install
            .install(&self.releases, update, &identifier, &downloads)
        {
            Ok(Outcome::Restart(command)) => match install::relaunch_after_exit(&command) {
                Ok(()) => self.app.exit(0),
                Err(error) => notify(
                    &format!("Orbit {} is installed", update.version),
                    &format!("{error}. Quit and reopen Orbit to use it."),
                ),
            },
            Ok(Outcome::Saved {
                path,
                open,
                message,
            }) => {
                notify(&format!("Downloaded Orbit {}", update.version), &message);
                let opener = self.app.opener();
                let _ = if open {
                    opener.open_path(path.to_string_lossy(), None::<&str>)
                } else {
                    opener.reveal_item_in_dir(&path)
                };
            }
            Err(error) => notify("Orbit update failed", &format!("{error}.")),
        }
        self.show_item();
    }

    fn show_item(&self) {
        let label = match &self.pending {
            Some(update) => format!("Install Orbit {}…", update.version),
            None => CHECK_LABEL.into(),
        };
        let _ = self.item.set_text(label);
        let _ = self.item.set_enabled(true);
    }
}
