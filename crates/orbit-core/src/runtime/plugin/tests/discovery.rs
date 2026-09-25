//! Sibling tests for `discovery.rs`: host-global plugin surfaces resolve the
//! caller's global `[plugins.<ns>]` section.

use std::path::Path;

use orbit_store::Store;

use super::super::discovery::{host_plugin_cli_groups, host_plugin_registry};
use super::super::paths::plugin_install_path;
use super::host::record;

/// A plugin whose `[plugins.<ns>]` schema declares `index_dir`, optionally
/// `required`, and an optional manifest default — the ORB-12826 fixture: a
/// key the operator can only satisfy through `config.toml`'s global section
/// when the schema requires it and the manifest ships no default. The tool is
/// `mcp_scope: global` so it exercises the same registry
/// `execute_global_plugin_tool` dispatches through.
fn write_plugin_with_config(root: &Path, name: &str, required: bool, default: Option<&str>) {
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
    std::fs::create_dir_all(root.join("schemas")).expect("create schema dir");
    let required_line = if required {
        "\"required\": [\"index_dir\"],\n  "
    } else {
        ""
    };
    std::fs::write(
        root.join("schemas/config.json"),
        format!(
            "{{\n  \"type\": \"object\",\n  {required_line}\"properties\": {{\n    \
             \"index_dir\": {{ \"type\": \"string\" }}\n  }}\n}}\n"
        ),
    )
    .expect("write config schema");
    let defaults_line = match default {
        Some(value) => format!("    defaults: {{ index_dir: \"{value}\" }}\n"),
        None => String::new(),
    };
    std::fs::write(
        root.join("plugin.yaml"),
        format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {name}\n  version: 1.0.0\nspec:\n  \
             backend:\n    type: exec\n    command: bin/backend.sh\n  config:\n    schema: \
             schemas/config.json\n{defaults_line}  tools:\n    - name: hello\n      \
             execution_kind: read_only\n      mcp_scope: global\n"
        ),
    )
    .expect("write manifest");
}

/// ORB-12826: `host_plugin_cli_groups` and `host_plugin_registry` used to
/// pass `&BTreeMap::new()` for the plugin config no matter what the caller
/// resolved, so a plugin's `orbit <ns>` group and MCP `tools/list` entry were
/// built without ever consulting `config.toml`'s global `[plugins.<ns>]`
/// section — while the workspace runtime, which threads its resolved config
/// through, always has it. `index_dir` is schema-`required`; the manifest
/// ships a placeholder default only because a schema whose defaults do not
/// self-satisfy it fails to load at all for *any* caller (loader.rs's
/// `resolve_config_section`), so an unsatisfiable-without-config manifest can
/// never be installed to demonstrate a load/no-load split. The reachable,
/// fixed split is the *value* the host surfaces actually carry — this test
/// proves both surfaces exist and both reflect the global section, not the
/// placeholder.
#[test]
fn a_schema_required_key_set_in_the_global_config_section_gets_a_cli_group_and_mcp_listing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    write_plugin_with_config(
        &plugin_install_path(&global_root, "graph", "1.0.0"),
        "graph",
        true,
        Some("__unset__"),
    );

    let audit_db = global_root.join("orbit.db");
    {
        let store = Store::open(&audit_db).expect("open store");
        store
            .with_transaction(|tx| tx.upsert_plugin(&record(&global_root, "graph")))
            .expect("record the install");
    }

    let mut plugin_config = std::collections::BTreeMap::new();
    plugin_config.insert(
        "graph".to_string(),
        serde_json::json!({"index_dir": "/var/graph-index"}),
    );

    let groups = host_plugin_cli_groups(&global_root, &audit_db, &plugin_config)
        .expect("cli groups load without the workspace runtime");
    assert!(
        groups.iter().any(|group| group.namespace == "graph"),
        "the plugin's `orbit graph` CLI group is present when the host resolves the global \
         config section: {groups:?}"
    );

    let (registry, load) =
        host_plugin_registry(&global_root, &audit_db, &plugin_config).expect("registry loads");
    assert!(
        load.is_active("graph"),
        "the plugin registers active: {:?}",
        load.diagnostics
    );
    let advertised: Vec<String> = registry
        .mcp_tool_definitions()
        .expect("mcp definitions")
        .into_iter()
        .map(|definition| definition.schema.name)
        .collect();
    assert!(
        advertised.iter().any(|name| name == "graph.hello"),
        "the plugin's tool is in MCP tools/list: {advertised:?}"
    );

    // Both surfaces are built from the same load pass, so the value they
    // carry proves it came from the passed-in global section, not the
    // manifest's placeholder — an empty map (the bug) would carry
    // "__unset__" here instead.
    let entry = load
        .registered
        .iter()
        .find(|entry| entry.name == "graph")
        .expect("the plugin is registered");
    assert_eq!(
        entry.config_values.get("index_dir").map(String::as_str),
        Some("/var/graph-index"),
        "the CLI group and MCP listing are for a plugin resolved with the global section's \
         value, not the manifest placeholder: {:?}",
        entry.config_values
    );
}

/// ORB-12826: `execute_global_plugin_tool` builds its registry through
/// `host_plugin_registry`, which used to ignore `config.toml`'s global
/// section entirely. A `mcp_scope: global` tool's backend renders
/// `{{config.<key>}}` from `RegisteredPlugin::config_values` — "what the
/// backend and the manifest's templates see" — so the value that dispatch
/// path's child process would receive is exactly this map. Before the fix it
/// held only the manifest default; after the fix it holds the global
/// section's override.
#[test]
fn a_global_scope_tool_renders_config_from_the_global_section_over_the_manifest_default() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    write_plugin_with_config(
        &plugin_install_path(&global_root, "graph", "1.0.0"),
        "graph",
        false,
        Some(".index"),
    );

    let audit_db = global_root.join("orbit.db");
    {
        let store = Store::open(&audit_db).expect("open store");
        store
            .with_transaction(|tx| tx.upsert_plugin(&record(&global_root, "graph")))
            .expect("record the install");
    }

    let mut plugin_config = std::collections::BTreeMap::new();
    plugin_config.insert(
        "graph".to_string(),
        serde_json::json!({"index_dir": "/var/graph-index"}),
    );

    let (_, load) =
        host_plugin_registry(&global_root, &audit_db, &plugin_config).expect("registry loads");
    let entry = load
        .registered
        .iter()
        .find(|entry| entry.name == "graph")
        .expect("the plugin is registered");
    assert_eq!(
        entry.config_values.get("index_dir").map(String::as_str),
        Some("/var/graph-index"),
        "the global section's value reaches the child, not the manifest default: {:?}",
        entry.config_values
    );

    // Without the resolved global config, the same dispatch path fell back to
    // the manifest default — the divergence ORB-12826 describes between the
    // workspace and global-tool execution paths.
    let (_, load_without_config) =
        host_plugin_registry(&global_root, &audit_db, &std::collections::BTreeMap::new())
            .expect("registry still opens");
    let entry_without_config = load_without_config
        .registered
        .iter()
        .find(|entry| entry.name == "graph")
        .expect("the plugin is registered");
    assert_eq!(
        entry_without_config
            .config_values
            .get("index_dir")
            .map(String::as_str),
        Some(".index"),
        "sanity check: an empty config map falls back to the manifest default, confirming the \
         assertion above depends on the fix: {:?}",
        entry_without_config.config_values
    );
}
