#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "listener and intake fixtures panic when a socket, token or permission setup fails"
)]

/// Open the workspace at `orbit_dir` with path leases enabled, which no
/// configuration can do [ORB-22808].
#[cfg(unix)]
fn enable_leases(orbit_dir: &std::path::Path) {
    use orbit_core::runtime::task::locks::lease::{LeasePolicy, install_test_lease_policy};
    install_test_lease_policy(
        orbit_dir,
        LeasePolicy {
            enabled: true,
            ..LeasePolicy::FIXED
        },
    );
}

#[cfg(unix)]
mod intake;
#[cfg(unix)]
mod listener;
