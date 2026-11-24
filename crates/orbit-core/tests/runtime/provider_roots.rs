//! Provider-state override variables the Linux sandbox profile reads before
//! falling back to `$HOME`. A sandbox fixture with a disposable `HOME` must
//! not inherit them from the caller, or resolution would follow the caller's
//! configuration and create provider roots outside the fixture's own state.
//! A fixture that wants an override sets it deliberately after this scrub.
#![cfg(any(target_os = "linux", target_os = "macos"))]

/// Every override the profile resolver consults for a provider root.
pub(crate) const PROVIDER_ROOT_OVERRIDES: [&str; 6] = [
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    "COPILOT_HOME",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
];

/// Drop the inherited overrides from a fixture child's command.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn remove_from(command: &mut std::process::Command) {
    for name in PROVIDER_ROOT_OVERRIDES {
        command.env_remove(name);
    }
}

/// Unset the inherited overrides in this re-executed fixture child until the
/// guard drops.
pub(crate) fn unset_inherited() -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::scoped(PROVIDER_ROOT_OVERRIDES.map(|name| (name, None)))
}
