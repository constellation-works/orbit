//! OS clock integration [ORB-10021, ORB-12237]: the OS owns the wake-up,
//! Orbit owns everything else. `orbit routine init --install-clock` renders
//! the platform unit from the templates in `assets/clock/` and installs it
//! as a per-user unit (launchd agent on macOS, systemd user timer on Linux).
//! There is no resident Orbit daemon.

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
    ClockSettings, DEFAULT_CLOCK_CADENCE_SECONDS, LAUNCHD_LABEL, SYSTEMD_UNIT, clock_settings_path,
    load_clock_settings, save_clock_settings, set_clock_cadence,
};
pub use status::{ClockStatus, clock_status};

#[cfg(test)]
mod tests;
