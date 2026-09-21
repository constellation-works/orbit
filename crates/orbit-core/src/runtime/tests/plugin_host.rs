//! Sibling tests for `plugin_host.rs`: one broken plugin must not take the
//! runtime, the built-ins, or another plugin down with it (design §4.9).

use std::path::Path;

use orbit_store::Store;
use orbit_tools::ToolRegistry;
use orbit_types::plugin::{InstalledPlugin, PluginStatus};

use super::super::plugin_host::{load_host_plugins, plugin_install_path};

fn write_plugin(root: &Path, name: &str, requires: &str) {
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
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {name}\n  version: 1.0.0\nspec:\n{requires}  backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: hello\n      execution_kind: read_only\n      mcp_scope: workspace\n"
        ),
    )
    .expect("write manifest");
}

fn record(global_root: &Path, name: &str) -> InstalledPlugin {
    InstalledPlugin {
        name: name.to_string(),
        version: "1.0.0".to_string(),
        source: "fixture".to_string(),
        install_path: plugin_install_path(global_root, name, "1.0.0")
            .to_string_lossy()
            .into_owned(),
        manifest_digest: "0".repeat(64),
        enabled: true,
        grants: Vec::new(),
        first_party: false,
        installed_at: String::new(),
        updated_at: String::new(),
    }
}

#[test]
fn one_broken_plugin_does_not_disturb_the_builtins_or_a_healthy_plugin() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let orbit_dir = temp.path().join("repo/.orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");

    write_plugin(
        &plugin_install_path(&global_root, "good", "1.0.0"),
        "good",
        "",
    );
    write_plugin(
        &plugin_install_path(&global_root, "broken", "1.0.0"),
        "broken",
        "  requires:\n    orbit: \">=99.0.0\"\n",
    );

    let store = Store::open(&global_root.join("orbit.db")).expect("open store");
    for name in ["good", "broken"] {
        store
            .with_transaction(|tx| tx.upsert_plugin(&record(&global_root, name)))
            .expect("record the install");
    }

    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let load = load_host_plugins(&global_root, &orbit_dir, &store, &mut registry);

    assert!(
        registry.is_active("orbit.task.show"),
        "builtins keep working"
    );
    assert!(
        registry.is_active("good.hello"),
        "the healthy plugin is active"
    );
    assert!(
        registry.has("broken.hello") && !registry.is_active("broken.hello"),
        "the broken plugin is registered inactive, not missing"
    );
    let diagnostic = registry
        .inactive_diagnostic("broken.hello")
        .expect("the inactive entry carries its reason");
    assert!(
        diagnostic.contains("requires orbit >=99.0.0"),
        "{diagnostic}"
    );

    let statuses: Vec<(String, PluginStatus)> = load
        .registered
        .iter()
        .map(|entry| (entry.name.clone(), entry.status))
        .collect();
    assert!(
        statuses.contains(&("good".to_string(), PluginStatus::Active)),
        "{statuses:?}"
    );
    assert!(
        statuses.contains(&("broken".to_string(), PluginStatus::Inactive)),
        "{statuses:?}"
    );
    assert_eq!(
        load.diagnostics.len(),
        1,
        "one deduped diagnostic: {:?}",
        load.diagnostics
    );
    assert_eq!(load.diagnostics[0].plugin, "broken");

    let advertised: Vec<String> = registry
        .mcp_tool_definitions()
        .expect("mcp definitions")
        .into_iter()
        .map(|definition| definition.schema.name)
        .collect();
    assert!(advertised.iter().any(|name| name == "good.hello"));
    assert!(
        !advertised.iter().any(|name| name == "broken.hello"),
        "an inactive plugin tool is not advertised"
    );
}

#[test]
fn a_plugin_whose_install_tree_vanished_is_reported_rather_than_fatal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let orbit_dir = temp.path().join("repo/.orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    std::fs::create_dir_all(&global_root).expect("create global root");

    let store = Store::open(&global_root.join("orbit.db")).expect("open store");
    store
        .with_transaction(|tx| tx.upsert_plugin(&record(&global_root, "vanished")))
        .expect("record the install");

    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let load = load_host_plugins(&global_root, &orbit_dir, &store, &mut registry);

    assert!(registry.is_active("orbit.task.show"));
    assert_eq!(load.diagnostics.len(), 1);
    assert!(
        load.diagnostics[0].message.contains("no longer loads"),
        "{:?}",
        load.diagnostics
    );
}
