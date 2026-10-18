//! The platform-independent parts of the Orbit desktop shell: the
//! dashboard-server supervisor, dashboard polling, and update installer.
//!
//! They are a library apart from the Tauri binary so tests can drive them
//! without a webview: the supervisor with a fake window and a fake `orbit`,
//! polling with in-memory request and notification adapters. `main.rs` adapts
//! them to the real window.
//! The update installer lives here too, so a test can replace an AppImage
//! without starting a window.

pub mod sidecar;
pub mod watcher;

// The installer stays beside the binary's update flow. It is compiled into
// this library so `tests/update_install.rs` can drive it; the binary supplies
// the verified download.
#[path = "update/install.rs"]
pub mod update_install;
