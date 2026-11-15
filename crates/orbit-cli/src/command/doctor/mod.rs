//! `orbit doctor` — workspace diagnostics, provider CLI checks and filesystem
//! access dry-runs.

mod admission_receipts;
mod command;
mod fs_access;
mod providers;
mod support;
mod tool_telemetry;
mod workspace_checks;

pub use command::{DoctorCommand, DoctorSubcommand};
