//! Sibling tests for `secrets.rs`: the host-owned store's versioned get,
//! compare-and-swap put, removal, and on-disk privacy.

use std::sync::{Arc, Barrier};

use super::super::paths::plugin_secret_store_dir;
use super::super::secrets::{PluginSecretStore, PluginSecretSwap, PluginSecretValue};

fn value(text: &str) -> PluginSecretValue {
    PluginSecretValue::new(text.to_string()).expect("valid secret value")
}

#[test]
fn concurrent_swaps_from_one_version_apply_exactly_once() {
    let root = tempfile::tempdir().expect("tempdir");
    let store = PluginSecretStore::new(root.path());
    let base = store.put("demo", "token", &value("base")).expect("put");

    let writers = 8;
    let barrier = Arc::new(Barrier::new(writers));
    let handles: Vec<_> = (0..writers)
        .map(|index| {
            let store = store.clone();
            let base = base.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .compare_and_swap("demo", "token", &value(&format!("v{index}")), Some(&base))
                    .expect("swap")
            })
        })
        .collect();
    let outcomes: Vec<PluginSecretSwap> = handles
        .into_iter()
        .map(|handle| handle.join().expect("writer thread"))
        .collect();
    let applied = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, PluginSecretSwap::Applied { .. }))
        .count();
    assert_eq!(applied, 1, "{outcomes:?}");
}

#[test]
fn invalid_names_and_values_are_refused_without_echoing_the_value() {
    let root = tempfile::tempdir().expect("tempdir");
    let store = PluginSecretStore::new(root.path());
    assert!(store.put("demo", "Bad Name", &value("x")).is_err());
    assert!(store.put("../demo", "token", &value("x")).is_err());
    assert!(PluginSecretValue::new(String::new()).is_err());
    let oversized = "s".repeat(super::super::secrets::MAX_PLUGIN_SECRET_BYTES + 1);
    let error = PluginSecretValue::new(oversized.clone())
        .expect_err("oversized value")
        .to_string();
    assert!(!error.contains(&oversized));
    assert_eq!(
        format!("{:?}", value("hunter2")),
        "PluginSecretValue(<redacted>)",
        "Debug must never print a value"
    );
}

#[cfg(unix)]
#[test]
fn the_store_is_private_to_the_host_user() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(root.path().join("state")).expect("state dir");
    let store = PluginSecretStore::new(root.path());
    store.put("demo", "token", &value("x")).expect("put");

    let dir = plugin_secret_store_dir(root.path());
    let mode = |path: &std::path::Path| {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join("demo.json")), 0o600);
}

#[cfg(unix)]
#[test]
fn a_symlinked_store_directory_is_refused() {
    let root = tempfile::tempdir().expect("tempdir");
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");
    std::fs::create_dir_all(root.path().join("state")).expect("state dir");
    std::os::unix::fs::symlink(&elsewhere, plugin_secret_store_dir(root.path()))
        .expect("link the store elsewhere");

    let store = PluginSecretStore::new(root.path());
    assert!(store.put("demo", "token", &value("x")).is_err());
    assert!(
        std::fs::read_dir(&elsewhere)
            .expect("read elsewhere")
            .next()
            .is_none(),
        "nothing is written through the link"
    );
}
