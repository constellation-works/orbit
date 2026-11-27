//! Sibling tests for `secrets.rs`: the host-owned store's versioned get,
//! compare-and-swap put, removal, and on-disk privacy.

use crate::host::plugin::paths::plugin_secret_store_dir;
use crate::host::plugin::sandbox_mask::PLUGIN_MASK_SENTINEL_FILE;
use crate::host::plugin::secrets::{PluginSecretStore, PluginSecretValue};

fn value(text: &str) -> PluginSecretValue {
    PluginSecretValue::new(text.to_string()).expect("valid secret value")
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

/// Permission drift is refused without echoing the value, while private
/// files, absent secrets and a restored store keep working.
#[cfg(unix)]
#[test]
fn permission_drift_is_refused_without_echoing_the_value() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("tempdir");
    let store = PluginSecretStore::new(root.path());
    let dir = plugin_secret_store_dir(root.path());
    let file = dir.join("demo.json");

    // Absent secrets read as empty before anything is stored.
    assert!(store.get("demo", "token").expect("absent get").is_none());
    assert!(store.list("demo").expect("absent list").is_empty());

    store.put("demo", "token", &value("s3cret")).expect("put");
    assert_eq!(
        store
            .get("demo", "token")
            .expect("get")
            .expect("stored secret")
            .value
            .expose(),
        "s3cret"
    );

    // Fresh writes stay private.
    let file_mode = std::fs::metadata(&file)
        .expect("file metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(file_mode, 0o600, "secret files are owner-only");
    let dir_mode = std::fs::metadata(&dir)
        .expect("dir metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700, "the store directory is owner-only");

    // A file other users can reach is refused without quoting the value.
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644))
        .expect("drift file mode");
    let error = store
        .get("demo", "token")
        .expect_err("drifted file is refused")
        .to_string();
    assert!(
        !error.contains("s3cret"),
        "the refusal must not echo the value: {error}"
    );
    assert!(
        store.list("demo").is_err(),
        "listing a drifted file is refused"
    );
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))
        .expect("restore file mode");
    assert!(
        store.get("demo", "token").expect("restored get").is_some(),
        "a repaired file loads again"
    );

    // A directory other users can reach is refused the same way.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("drift dir mode");
    let error = store
        .get("demo", "token")
        .expect_err("drifted dir is refused")
        .to_string();
    assert!(
        !error.contains("s3cret"),
        "the refusal must not echo the value: {error}"
    );
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .expect("restore dir mode");
    assert!(
        store.get("demo", "token").expect("restored get").is_some(),
        "a repaired directory loads again"
    );
}

#[test]
fn masked_store_stays_unreadable() {
    let root = tempfile::tempdir().expect("tempdir");
    let store = PluginSecretStore::new(root.path());
    store.put("demo", "token", &value("s3cret")).expect("put");
    std::fs::write(
        plugin_secret_store_dir(root.path()).join(PLUGIN_MASK_SENTINEL_FILE),
        "sentinel",
    )
    .expect("sentinel");
    let error = store
        .get("demo", "token")
        .expect_err("masked store is refused")
        .to_string();
    assert!(
        error.contains("not visible"),
        "masked refusal names the sandbox boundary: {error}"
    );
    assert!(
        !error.contains("s3cret"),
        "the refusal must not echo the value: {error}"
    );
    assert!(
        store.list("demo").is_err(),
        "listing a masked store is refused"
    );
}
