//! Sibling tests for `plugin_grants.rs`: the integrity value states which
//! grants were authorized and for which plugin, and a row it does not cover is
//! never verified into authority [ORB-12778].

use orbit_types::plugin::InstalledPlugin;

use super::super::plugin_grants::{
    plugin_grant_witness_path, plugin_grants_digest, record_authorized_grants,
    verify_recorded_grants,
};

fn record(name: &str, enabled: bool, grants: &[&str]) -> InstalledPlugin {
    InstalledPlugin {
        name: name.to_string(),
        version: "1.0.0".to_string(),
        source: "fixture".to_string(),
        install_path: format!("/nowhere/{name}"),
        manifest_digest: "0".repeat(64),
        enabled,
        grants: grants.iter().map(|grant| (*grant).to_string()).collect(),
        first_party: false,
        certified_orbit_version: None,
        installed_at: String::new(),
        updated_at: String::new(),
    }
}

#[test]
fn the_value_names_the_authorized_set_and_not_the_order_it_was_recorded_in() {
    let ordered = plugin_grants_digest("demo", true, &["fs".into(), "network".into()]);
    assert_eq!(
        ordered,
        plugin_grants_digest("demo", true, &["network".into(), "fs".into()]),
        "grant order is not part of what was authorized"
    );
    assert_eq!(
        ordered,
        plugin_grants_digest("demo", true, &["fs".into(), "fs".into(), "network".into()]),
        "a repeated grant authorizes nothing extra"
    );
    assert_eq!(ordered.len(), 64, "hex SHA-256: {ordered}");

    for other in [
        plugin_grants_digest("demo", true, &["fs".into()]),
        plugin_grants_digest(
            "demo",
            true,
            &["fs".into(), "network".into(), "unsandboxed".into()],
        ),
        plugin_grants_digest("other", true, &["fs".into(), "network".into()]),
        plugin_grants_digest("demo", false, &["fs".into(), "network".into()]),
    ] {
        assert_ne!(
            ordered, other,
            "the value is keyed on the namespace, the enable flag and the set"
        );
    }
}

#[test]
fn a_row_holding_grants_with_no_authorization_record_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path();

    // Nothing authorized, nothing claimed: every plugin enabled without
    // `--grant` is in this state, and refusing it would refuse a host that has
    // never granted anything.
    verify_recorded_grants(global_root, &record("demo", true, &[]))
        .expect("an empty grant set needs no record");

    let message = verify_recorded_grants(global_root, &record("demo", true, &["fs"]))
        .expect_err("grants with no record are not authority");
    assert!(
        message.contains("no authorization record exists")
            && message.contains("`fs`")
            && message.contains("orbit plugin enable demo"),
        "{message}"
    );
}

#[test]
fn a_recorded_set_verifies_and_any_change_to_the_row_does_not() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path();
    record_authorized_grants(global_root, "demo", true, &["fs".to_string()])
        .expect("record the authorized set");
    assert!(
        plugin_grant_witness_path(global_root, "demo").is_file(),
        "the record lives beside the installs, outside orbit.db"
    );

    verify_recorded_grants(global_root, &record("demo", true, &["fs"]))
        .expect("the authorized set verifies");

    for tampered in [
        record("demo", true, &["fs", "unsandboxed"]),
        record("demo", true, &["unsandboxed"]),
        record("demo", true, &[]),
    ] {
        let message = verify_recorded_grants(global_root, &tampered)
            .expect_err("a set this host never authorized is refused");
        assert!(
            message.contains("do not match the set this host authorized"),
            "{message}"
        );
    }
}

#[test]
fn a_record_that_cannot_be_read_as_this_hosts_authorization_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path();
    let path = plugin_grant_witness_path(global_root, "demo");
    std::fs::create_dir_all(path.parent().expect("witness dir")).expect("create witness dir");

    for (body, expected) in [
        ("not json at all", "does not parse"),
        (
            r#"{"schema_version":99,"plugin":"demo","grants_digest":"x","authorized_at":"now"}"#,
            "schema 99",
        ),
        (
            &format!(
                r#"{{"schema_version":1,"plugin":"other","grants_digest":"{}","authorized_at":"now"}}"#,
                plugin_grants_digest("demo", true, &["unsandboxed".to_string()])
            ),
            "written for 'other'",
        ),
    ] {
        std::fs::write(&path, body).expect("write witness");
        let message = verify_recorded_grants(global_root, &record("demo", true, &["unsandboxed"]))
            .expect_err("an unusable record is not authority");
        assert!(message.contains(expected), "{message}");
    }
}
