//! Desktop library for the Orbit shell.
//!
//! The dashboard-server supervisor is a library apart from the Tauri binary so
//! tests can drive it with a fake window and a fake `orbit`; `main.rs` adapts
//! it to the real window. Platform-independent dashboard polling is available
//! without the native webview stack.

pub mod sidecar;
pub mod watcher;
