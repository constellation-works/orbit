/// Open the workspace at `orbit_dir` with path leases enabled, which no
/// configuration can do [ORB-22808].
#[cfg(unix)]
pub(super) fn enable_leases(orbit_dir: &std::path::Path) {
    use orbit_core::runtime::stored::locks::lease::{LeasePolicy, install_test_lease_policy};
    install_test_lease_policy(
        orbit_dir,
        LeasePolicy {
            enabled: true,
            ..LeasePolicy::FIXED
        },
    );
}
