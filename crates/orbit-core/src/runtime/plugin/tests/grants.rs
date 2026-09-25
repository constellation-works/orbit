//! Sibling tests for `grants.rs`: the integrity value states which
//! grants were authorized and for which plugin, and a row it does not cover is
//! never verified into authority [ORB-12778].

use std::path::Path;

use orbit_types::plugin::InstalledPlugin;

use super::super::grants::{
    plugin_grant_witness_path, plugin_grants_digest, record_authorized_grants, verify_install_path,
    verify_recorded_grants,
};
use super::super::paths::plugin_install_path;

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

/// The witness is written rename-into-place, not a plain `fs::write`, so a
/// crash mid-write leaves either the previous witness or the new one, never a
/// truncated file that refuses the plugin until an operator notices. The
/// staged-write path creates the file private (`0o600`) and cleans up its
/// temp file on success; a plain `fs::write` would instead leave the mode at
/// the process umask (typically `0o644`).
#[cfg(unix)]
#[test]
fn the_witness_is_written_through_the_atomic_private_write_path() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path();
    record_authorized_grants(global_root, "demo", true, &["fs".to_string()])
        .expect("record the authorized set");
    // Overwrite, to also exercise the replace path, not only the first write.
    record_authorized_grants(
        global_root,
        "demo",
        true,
        &["fs".to_string(), "network".to_string()],
    )
    .expect("overwrite the authorized set");

    let path = plugin_grant_witness_path(global_root, "demo");
    let mode = std::fs::metadata(&path)
        .expect("witness metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "the witness must go through the atomic private-file write path"
    );

    let siblings: Vec<_> = std::fs::read_dir(path.parent().expect("witness dir"))
        .expect("read witness dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        siblings,
        vec!["demo.json"],
        "the atomic write leaves no `.tmp` staging file behind: {siblings:?}"
    );
}

fn record_at(name: &str, install_path: &Path) -> InstalledPlugin {
    let mut installed = record(name, true, &[]);
    installed.install_path = install_path.to_string_lossy().into_owned();
    installed
}

/// The witness does not cover `install_path` (module docs), so the path is
/// held to the install root structurally: a row may name any tree under
/// `plugins/<ns>/`, and nothing else [ORB-12785].
#[test]
fn an_install_path_is_accepted_only_strictly_beneath_the_namespace_install_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path();
    let versioned = plugin_install_path(global_root, "demo", "1.0.0");
    std::fs::create_dir_all(&versioned).expect("create install dir");

    verify_install_path(global_root, &record_at("demo", &versioned))
        .expect("the versioned install directory is where `orbit plugin add` places the tree");
    verify_install_path(
        global_root,
        &record_at("demo", &plugin_install_path(global_root, "demo", "2.0.0")),
    )
    .expect("a version directory that does not exist yet is still beneath the install root");

    #[cfg(unix)]
    {
        // A link left inside the namespace directory — by an operator, or by
        // the `current` entry an Orbit before ORB-12823 wrote there — is
        // resolved physically, so its target is what decides.
        let alias = super::super::paths::plugin_namespace_dir(global_root, "demo").join("previous");
        std::os::unix::fs::symlink(&versioned, &alias).expect("link alias");
        verify_install_path(global_root, &record_at("demo", &alias))
            .expect("a link onto a version directory resolves into the install root");
    }

    let elsewhere = global_root.join("state/logs/evil");
    std::fs::create_dir_all(&elsewhere).expect("create a tree under a backend write root");
    for (recorded, why) in [
        (elsewhere.clone(), "a tree under a backend write root"),
        (
            plugin_install_path(global_root, "demo", "1.0.0").join("../../../state/logs/evil"),
            "a `..` escape spelled beneath the install root",
        ),
        (
            plugin_install_path(global_root, "other", "1.0.0"),
            "another namespace's install",
        ),
        (
            global_root.join("plugins").join("demo"),
            "the namespace directory itself rather than a tree beneath it",
        ),
        (
            global_root.join("plugins").join("demo-2").join("1.0.0"),
            "a sibling whose name merely extends the namespace",
        ),
        (
            Path::new("plugins/demo/1.0.0").to_path_buf(),
            "a relative path",
        ),
    ] {
        let message =
            verify_install_path(global_root, &record_at("demo", &recorded)).expect_err(why);
        assert!(
            message.contains(&recorded.to_string_lossy().into_owned())
                && message.contains(&global_root.join("plugins/demo").display().to_string())
                && message.contains("orbit plugin remove demo"),
            "{why}: the operator is told the recorded and the expected path: {message}"
        );
    }
}
