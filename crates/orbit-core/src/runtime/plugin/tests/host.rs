//! Sibling tests for `host.rs`: one broken plugin must not take the
//! runtime, the built-ins, or another plugin down with it (design §4.9).

use std::path::Path;

use orbit_store::Store;
use orbit_tools::ToolRegistry;
use orbit_types::plugin::{InstalledPlugin, PluginStatus};

use super::super::grants::record_authorized_grants;
use super::super::host::load_host_plugins;
use super::super::paths::plugin_install_path;

fn write_plugin(root: &Path, name: &str, requires: &str) {
    write_plugin_verb(root, name, "hello", requires);
}

fn write_plugin_verb(root: &Path, name: &str, verb: &str, requires: &str) {
    std::fs::create_dir_all(root.join("bin")).expect("create bin dir");
    let backend = root.join("bin/backend.sh");
    std::fs::write(
        &backend,
        "#!/bin/sh\ncat >/dev/null\necho '{\"ok\":true,\"output\":{}}'\n",
    )
    .expect("write backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755))
            .expect("chmod backend");
    }
    std::fs::write(
        root.join("plugin.yaml"),
        format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {name}\n  version: 1.0.0\nspec:\n{requires}  backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: {verb}\n      execution_kind: read_only\n      mcp_scope: workspace\n"
        ),
    )
    .expect("write manifest");
}

pub(super) fn record(global_root: &Path, name: &str) -> InstalledPlugin {
    let install_path = plugin_install_path(global_root, name, "1.0.0");
    let manifest_digest = std::fs::read(install_path.join("plugin.yaml"))
        .map(|bytes| orbit_tools::plugin::manifest_digest(&bytes))
        .unwrap_or_else(|_| "0".repeat(64));
    InstalledPlugin {
        name: name.to_string(),
        version: "1.0.0".to_string(),
        source: "fixture".to_string(),
        install_path: install_path.to_string_lossy().into_owned(),
        manifest_digest,
        archive_digest: None,
        enabled: true,
        grants: Vec::new(),
        first_party: false,
        certified_orbit_version: None,
        build: None,
        installed_at: String::new(),
        updated_at: String::new(),
    }
}

/// ORB-12827: `metadata.name` (not the store row's install identity) decides
/// a plugin's namespace, so tampering it on disk after install can make a
/// plugin's manifest claim another plugin's namespace — the digest changes,
/// taking the same digest-mismatch path as
/// `a_rewritten_manifest_is_registered_inactive_naming_both_digests`, but the
/// rewritten name collides with an already-active plugin's tool instead of
/// an unrelated field. `register_entry` must refuse to let the tampered
/// row's inactive entry displace the real owner's active one (§4.9).
#[test]
fn a_tampered_manifest_claiming_another_plugins_namespace_cannot_displace_its_active_tool() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let orbit_dir = temp.path().join("repo/.orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");

    write_plugin(&plugin_install_path(&global_root, "a", "1.0.0"), "a", "");
    write_plugin(&plugin_install_path(&global_root, "b", "1.0.0"), "b", "");

    let store = Store::open(&global_root.join("orbit.db")).expect("open store");
    for name in ["a", "b"] {
        store
            .with_transaction(|tx| tx.upsert_plugin(&record(&global_root, name)))
            .expect("record the install");
    }

    // Tamper plugin b's on-disk manifest to claim plugin a's namespace, so
    // its tool becomes `a.hello` instead of `b.hello`, colliding with the
    // already-installed, already-active plugin a.
    let manifest = plugin_install_path(&global_root, "b", "1.0.0").join("plugin.yaml");
    let body = std::fs::read_to_string(&manifest).expect("read manifest");
    let tampered = body.replace("name: b", "name: a");
    assert_ne!(
        body, tampered,
        "the replace must actually rename the namespace"
    );
    std::fs::write(&manifest, tampered).expect("tamper manifest");

    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let load = load_host_plugins(
        &global_root,
        &orbit_dir,
        &store,
        &mut registry,
        &std::collections::BTreeMap::new(),
    );

    assert!(
        registry.is_active("a.hello"),
        "plugin a's active entry survives the collision"
    );
    assert_eq!(
        registry
            .plugin_binding("a.hello")
            .expect("a.hello is plugin-backed")
            .provenance
            .name,
        "a",
        "the entry still belongs to plugin a, not the tampered plugin b"
    );

    let entry = load
        .registered
        .iter()
        .find(|entry| entry.name == "b")
        .expect("the plugin is reported");
    assert_eq!(
        entry.status,
        PluginStatus::Inactive,
        "the tampered plugin is refused: {:?}",
        entry.diagnostic
    );
    let diagnostic = entry.diagnostic.clone().expect("a diagnostic");
    assert!(
        diagnostic.contains("does not match the stored digest"),
        "{diagnostic}"
    );
}

/// `fs.write` on the plugin root (or a parent, or `/`) would let the backend
/// rewrite `plugin.yaml` under an already-recorded `fs` grant. Registration
/// refuses it independently of the digest check.
#[test]
fn fs_write_covering_the_plugin_root_is_refused_at_registration() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let orbit_dir = temp.path().join("repo/.orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    let install_path = plugin_install_path(&global_root, "wide", "1.0.0");
    write_plugin(&install_path, "wide", "");
    let manifest = install_path.join("plugin.yaml");
    let body = std::fs::read_to_string(&manifest).expect("read manifest");
    std::fs::write(
        &manifest,
        body.replace(
            "  backend:\n",
            "  permissions:\n    fs:\n      write: [\"{{plugin_root}}\"]\n  backend:\n",
        ),
    )
    .expect("request a covering write");

    let store = Store::open(&global_root.join("orbit.db")).expect("open store");
    let mut installed = record(&global_root, "wide");
    installed.grants = vec!["fs".to_string()];
    store
        .with_transaction(|tx| tx.upsert_plugin(&installed))
        .expect("record the install");
    record_authorized_grants(&global_root, "wide", true, &["fs".to_string()])
        .expect("authorize fs");

    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let load = load_host_plugins(
        &global_root,
        &orbit_dir,
        &store,
        &mut registry,
        &std::collections::BTreeMap::new(),
    );
    let entry = load
        .registered
        .iter()
        .find(|entry| entry.name == "wide")
        .expect("the plugin is reported");
    assert_eq!(entry.status, PluginStatus::Inactive);
    let diagnostic = entry.diagnostic.clone().expect("a diagnostic");
    assert!(
        diagnostic.contains("plugin install root")
            && diagnostic.contains("spec.permissions.fs.write[0]"),
        "{diagnostic}"
    );
}

/// Registration refuses a traversal from the one writable global subtree to
/// the host's plugin-grant witnesses before the tool can become active.
#[test]
fn fs_write_traversal_to_a_protected_global_path_is_refused_at_registration() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let orbit_dir = temp.path().join("repo/.orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    let install_path = plugin_install_path(&global_root, "traversal", "1.0.0");
    write_plugin(
        &install_path,
        "traversal",
        "  permissions:\n    fs:\n      write: [\"{{plugin_state}}/../../../plugins/.grants\"]\n",
    );

    let store = Store::open(&global_root.join("orbit.db")).expect("open store");
    let mut installed = record(&global_root, "traversal");
    installed.grants = vec!["fs".to_string()];
    store
        .with_transaction(|tx| tx.upsert_plugin(&installed))
        .expect("record the install");
    record_authorized_grants(&global_root, "traversal", true, &["fs".to_string()])
        .expect("authorize fs");

    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let load = load_host_plugins(
        &global_root,
        &orbit_dir,
        &store,
        &mut registry,
        &std::collections::BTreeMap::new(),
    );
    let entry = load
        .registered
        .iter()
        .find(|entry| entry.name == "traversal")
        .expect("the plugin is reported");
    assert_eq!(entry.status, PluginStatus::Inactive);
    let diagnostic = entry.diagnostic.clone().expect("a diagnostic");
    assert!(
        diagnostic.contains("protected path beneath Orbit global root")
            && diagnostic.contains("spec.permissions.fs.write[0]"),
        "{diagnostic}"
    );
    assert!(
        registry.has("traversal.hello") && !registry.is_active("traversal.hello"),
        "the refused plugin is visible but inactive"
    );
}
