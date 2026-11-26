//! Sibling tests for `secrets.rs`: the host-owned store's versioned get,
//! compare-and-swap put, removal, and on-disk privacy.

use super::super::secrets::{PluginSecretStore, PluginSecretValue};

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
