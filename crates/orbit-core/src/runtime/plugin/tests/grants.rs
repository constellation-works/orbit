//! Sibling tests for `grants.rs`: the integrity value states which
//! grants were authorized and for which plugin, and a row it does not cover is
//! never verified into authority [ORB-12778].

use orbit_types::plugin::InstalledPlugin;

use super::super::grants::{
    plugin_grant_witness_path, record_authorized_grants, verify_recorded_grants,
};

fn record(name: &str, enabled: bool, grants: &[&str]) -> InstalledPlugin {
    InstalledPlugin {
        name: name.to_string(),
        version: "1.0.0".to_string(),
        source: "fixture".to_string(),
        install_path: format!("/nowhere/{name}"),
        archive_digest: None,
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
fn an_invalid_namespace_cannot_select_or_read_a_grant_witness_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path();
    let witness_dir = global_root.join("plugins/.grants");

    for name in ["../../../escape", "/absolute", "nested/name", "orbit"] {
        let path = plugin_grant_witness_path(global_root, name);
        assert_eq!(
            path.parent(),
            Some(witness_dir.as_path()),
            "invalid namespace '{name}' must stay in the witness directory"
        );
        assert_eq!(
            path.file_name().and_then(|file| file.to_str()),
            Some(".invalid.json"),
            "invalid namespace '{name}' must not select a host file"
        );
        let message = verify_recorded_grants(global_root, &record(name, true, &["fs"]))
            .expect_err("an invalid namespace cannot authorize grants");
        assert!(message.contains("plugin namespace is invalid"), "{message}");
        assert!(
            record_authorized_grants(global_root, name, true, &["fs".to_string()]).is_err(),
            "an invalid namespace cannot create a witness"
        );
    }
    assert!(
        !global_root.join("escape.json").exists(),
        "invalid namespace input must not create a file outside the witness directory"
    );
}
