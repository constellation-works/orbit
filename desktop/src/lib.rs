//! The dashboard-server supervisor behind the Orbit desktop window.
//!
//! It is a library apart from the Tauri binary so tests can drive it with a
//! fake window and a fake `orbit`; `main.rs` adapts it to the real window.

pub mod sidecar;
