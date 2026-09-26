//! Sibling tests for `secrets.rs`: the host-owned store's versioned get,
//! compare-and-swap put, removal, and on-disk privacy.

use std::sync::{Arc, Barrier};

use super::super::paths::plugin_secret_store_dir;
use super::super::secrets::{PluginSecretStore, PluginSecretSwap, PluginSecretValue};

fn value(text: &str) -> PluginSecretValue {
    PluginSecretValue::new(text.to_string()).expect("valid secret value")
}

#[test]
fn every_write_changes_the_version_and_get_returns_the_value_with_it() {
    let root = tempfile::tempdir().expect("tempdir");
    let store = PluginSecretStore::new(root.path());
    assert_eq!(store.get("demo", "token").expect("get"), None);

    let first = store.put("demo", "token", &value("one")).expect("put");
    let stored = store.get("demo", "token").expect("get").expect("stored");
    assert_eq!(stored.value.expose(), "one");
    assert_eq!(stored.version, first);

    let second = store.put("demo", "token", &value("one")).expect("rewrite");
    assert_ne!(
        first, second,
        "rewriting even the same value is a new version"
    );

    assert!(store.remove("demo", "token").expect("remove"));
    let third = store
        .put("demo", "token", &value("one"))
        .expect("set again");
    assert!(
        third != first && third != second,
        "a version is never reused after a remove"
    );
}

#[test]
fn compare_and_swap_applies_only_against_the_current_version() {
    let root = tempfile::tempdir().expect("tempdir");
    let store = PluginSecretStore::new(root.path());

    let created = match store
        .compare_and_swap("demo", "token", &value("a"), None)
        .expect("create when unset")
    {
        PluginSecretSwap::Applied { version } => version,
        other => panic!("expected the create to apply, got {other:?}"),
    };
    assert_eq!(
        store
            .compare_and_swap("demo", "token", &value("b"), None)
            .expect("create again"),
        PluginSecretSwap::Stale {
            current: Some(created.clone())
        },
        "expecting unset fails once a value exists"
    );

    let rotated = match store
        .compare_and_swap("demo", "token", &value("b"), Some(&created))
        .expect("rotate")
    {
        PluginSecretSwap::Applied { version } => version,
        other => panic!("expected the rotation to apply, got {other:?}"),
    };
    assert_eq!(
        store
            .compare_and_swap("demo", "token", &value("c"), Some(&created))
            .expect("stale rotate"),
        PluginSecretSwap::Stale {
            current: Some(rotated.clone())
        }
    );
    let stored = store.get("demo", "token").expect("get").expect("stored");
    assert_eq!(stored.value.expose(), "b", "a stale swap stores nothing");
    assert_eq!(stored.version, rotated);
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
fn plugins_hold_separate_secrets_and_removal_is_scoped() {
    let root = tempfile::tempdir().expect("tempdir");
    let store = PluginSecretStore::new(root.path());
    store
        .put("demo", "token", &value("demo-token"))
        .expect("put");
    store
        .put("demo", "other", &value("demo-other"))
        .expect("put");
    store
        .put("second", "token", &value("second-token"))
        .expect("put");

    assert_eq!(
        store
            .retain("demo", |name| name == "token")
            .expect("retain"),
        vec!["other".to_string()]
    );
    let names: Vec<String> = store
        .list("demo")
        .expect("list")
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, ["token"]);

    assert_eq!(
        store.remove_all("demo").expect("remove all"),
        vec!["token".to_string()]
    );
    assert!(store.list("demo").expect("list").is_empty());
    assert!(
        !plugin_secret_store_dir(root.path())
            .join("demo.json")
            .exists(),
        "an emptied plugin leaves no secret file behind"
    );
    assert_eq!(
        store
            .get("second", "token")
            .expect("get")
            .map(|secret| secret.value.expose().to_string()),
        Some("second-token".to_string()),
        "another plugin's secrets are untouched"
    );
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
