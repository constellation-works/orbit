//! `orbit browser-kit` — install, report, verify and remove the host's
//! headless browser kit.
//!
//! Workers that change a UI check it by scripting headless Chromium through
//! Playwright. The kit at `~/.local/chromium-deps` (layout:
//! [`orbit_core::bootstrap::browser_kit`]) holds the Playwright release this
//! Orbit release pins and the browser it launches; Chromium is downloaded
//! from Playwright's CDN at install time, never bundled. Node is a
//! prerequisite this group reports rather than installs.
//!
//! Every step runs on the host as the user who runs workers, never inside a
//! worker: a worker's sandbox forbids `sudo` and gives each run its own
//! `/tmp`. Once `verify` passes, the CLI runner hands every agent the kit's
//! variables; until then it hands them none.
//!
//! On Linux, Chromium's system libraries come from an explicit `sudo` opt-in
//! (`--with-deps`), or on Debian and Ubuntu from packages unpacked into the
//! kit without root; elsewhere the unresolved libraries and their fix are
//! reported.

mod host;
mod install;
#[cfg(target_os = "linux")]
mod libraries;
mod status;
mod verify;
mod web_ui;

pub use install::{InstallOptions, InstallReport, LibraryOutcome, install};
pub use status::{KitProblem, KitStatus, kit_status, remove};
pub use verify::{VerifyReport, verify};
pub use web_ui::web_ui_marker;

pub(crate) use host::{NODE_FIX, find_in_path_value};
