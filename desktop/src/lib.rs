//! The platform-independent parts of the Orbit desktop shell: the
//! dashboard-server supervisor and dashboard polling.
//!
//! They are a library apart from the Tauri binary so tests can drive them
//! without a webview: the supervisor with a fake window and a fake `orbit`,
//! polling with in-memory request and notification adapters. `main.rs` adapts
//! them to the real window.

pub mod sidecar;
pub mod watcher;
