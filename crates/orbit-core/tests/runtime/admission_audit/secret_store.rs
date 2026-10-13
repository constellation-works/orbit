//! Sibling tests for `secrets.rs`: the host-owned store's versioned get,
//! compare-and-swap put, removal, and on-disk privacy.

use std::sync::{Arc, Barrier};

use orbit_core::runtime::plugin::paths::plugin_secret_store_dir;
use orbit_core::runtime::plugin::secrets::{
    PluginSecretStore, PluginSecretSwap, PluginSecretValue,
};

fn value(text: &str) -> PluginSecretValue {
    PluginSecretValue::new(text.to_string()).expect("valid secret value")
}

#[test]
fn concurrent_swaps_from_one_version_apply_exactly_once() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "concurrent_swaps_from_one_version_apply_exactly_once",
    ) {
        return;
    }

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

#[cfg(unix)]
#[test]
fn the_store_is_private_to_the_host_user() {
    if !crate::admission_audit::isolate(module_path!(), "the_store_is_private_to_the_host_user") {
        return;
    }

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
    if !crate::admission_audit::isolate(module_path!(), "a_symlinked_store_directory_is_refused") {
        return;
    }

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
