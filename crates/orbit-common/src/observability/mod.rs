//! Tracing subscriber setup, log rotation, and audit execution IDs; imports `fs` and `security`.

/// Correlation IDs shared by audit entries and callbacks on the same dispatch thread.
pub mod audit_id;
pub mod log_rotation;
pub mod logging;
