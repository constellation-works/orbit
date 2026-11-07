#![deny(clippy::print_stderr, clippy::print_stdout)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
//! OS clock integration [ORB-10021, ORB-12237]: the OS owns the wake-up,
//! Orbit owns everything else. `orbit routine init --install-clock` renders
//! the platform unit from the templates in `assets/clock/` and installs it
//! as a per-user unit (launchd agent on macOS, systemd user timer on Linux).
//! There is no resident Orbit daemon.
//!
//! # Role
//! The host's sweep clock: `clock.toml` settings, unit rendering, install,
//! pause/resume, cadence, convergence, inspection and health. It depends only on
//! `orbit-common`, so it compiles beside the `orbit-store` → `orbit-engine` →
//! `orbit-runtime` chain instead of inside the serial `orbit-core` library
//! [ORB-22688]. `orbit-core` re-exports it as `application::routines::clock`
//! and its settings through `application::contracts`; the sweep pass that the
//! clock wakes stays in `orbit-core`.

mod clock_settings;
mod converge;
mod enable;
mod inspect;
mod install;
mod manager;
mod program;
mod settings;
mod status;

pub use converge::{
    ClockUnitConvergence, ClockUnitDrift, ClockUnitReload, ClockUnitRewrite,
    clock_unit_drift_warning, converge_clock_unit,
};
pub use enable::set_clock_enabled;
pub use inspect::{
    ClockUnitInspection, ClockUnitVerdict, RunningBinary, inspect_clock_unit, probe_program_version,
};
pub use install::{ClockInstallReport, install_clock, sweep_log_path};
pub use settings::{
    ClockSettings, DEFAULT_CLOCK_CADENCE_SECONDS, LAUNCHD_LABEL, SYSTEMD_UNIT, load_clock_settings,
    save_clock_settings, set_clock_cadence,
};
/// Cooperative budget for a host scheduler tick. The native manager supplies
/// an independent hard ceiling for an operation that never returns. The clock's
/// health reads it to judge an overdue tick; `orbit-automation` re-exports it
/// for the sweep pass that spends it.
pub const TICK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5 * 60);

pub use status::{ClockHealth, ClockStatus, clock_status, doctor_clock_not_running_issue};

// A bare `cargo test` from a managed-run shell must match one from a clean shell.
#[cfg(test)]
orbit_common::isolate_test_process!();

#[cfg(test)]
mod tests;
