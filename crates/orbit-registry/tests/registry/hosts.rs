//! Host-registry loads refuse a symlinked host file rather than reading
//! outside the selected root.

use orbit_registry::hosts::load_host_registry;

#[cfg(unix)]
#[test]
fn symlinked_hosts_file_is_refused() {
    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    std::fs::write(outside.path().join("hosts.toml"), "schema_version = 1\n").expect("write");
    std::os::unix::fs::symlink(
        outside.path().join("hosts.toml"),
        root.path().join("hosts.toml"),
    )
    .expect("symlink");

    assert!(
        load_host_registry(root.path()).is_err(),
        "a symlinked hosts.toml must be refused rather than read outside the root"
    );
}

#[test]
fn plain_hosts_file_loads() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("hosts.toml"), "schema_version = 1\n").expect("write");

    let registry = load_host_registry(root.path()).expect("load");
    assert!(registry.hosts().entries().is_empty());
}
