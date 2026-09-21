//! Sibling tests for `plugin_host.rs`: one broken plugin must not take the
//! runtime, the built-ins, or another plugin down with it (design §4.9).

use std::path::Path;

use orbit_store::Store;
use orbit_tools::ToolRegistry;
use orbit_types::plugin::{InstalledPlugin, PluginStatus};
use orbit_types::telemetry::AuditEventStatus;

use super::super::plugin_grants::record_authorized_grants;
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

/// The same fixture with `backend.sandbox: none`, so a `unsandboxed` grant in
/// the row is the only thing between it and an unconfined registration.
fn write_plugin_unsandboxed(root: &Path, name: &str) {
    write_plugin(root, name, "");
    let manifest = root.join("plugin.yaml");
    let body = std::fs::read_to_string(&manifest).expect("read manifest");
    std::fs::write(
        &manifest,
        body.replace(
            "    command: bin/backend.sh\n",
            "    command: bin/backend.sh\n    sandbox: none\n",
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
    let load = load_host_plugins(
        &global_root,
        &orbit_dir,
        &store,
        &mut registry,
        &std::collections::BTreeMap::new(),
    );

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
    let load = load_host_plugins(
        &global_root,
        &orbit_dir,
        &store,
        &mut registry,
        &std::collections::BTreeMap::new(),
    );

    assert!(registry.is_active("orbit.task.show"));
    assert_eq!(load.diagnostics.len(), 1);
    assert!(
        load.diagnostics[0].message.contains("no longer loads"),
        "{:?}",
        load.diagnostics
    );
}

/// The escape ORB-12778 closes: a backend that can write `orbit.db` writes its
/// own `plugins` row, injects `unsandboxed`, and is registered unconfined at
/// the next load.
///
/// The injection here is the store's own row write — the same effect as the
/// `UPDATE plugins SET grants_json=…` a plugin with a writable `orbit.db`
/// issues, and deliberately not `orbit plugin enable`, which is the authorizing
/// path.
#[test]
fn grants_injected_into_the_store_row_are_refused_instead_of_registered() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let orbit_dir = temp.path().join("repo/.orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    // `sandbox: none` is the manifest half of running unconfined; `unsandboxed`
    // is the grant half, and the store row is the only place the grant lives.
    write_plugin_unsandboxed(
        &plugin_install_path(&global_root, "loose", "1.0.0"),
        "loose",
    );

    let store = Store::open(&global_root.join("orbit.db")).expect("open store");
    store
        .with_transaction(|tx| tx.upsert_plugin(&record(&global_root, "loose")))
        .expect("record the install");
    store
        .with_transaction(|tx| {
            tx.set_plugin_enabled(
                "loose",
                true,
                &["orbit_tools".to_string(), "unsandboxed".to_string()],
            )
            .map(|_| ())
        })
        .expect("the attacker's row write succeeds; the loader is what refuses it");

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
        registry.is_active("orbit.task.show"),
        "the refusal is per plugin"
    );
    assert!(
        !registry.has("loose.hello"),
        "an unverifiable row contributes no tool, active or inactive"
    );
    let entry = load
        .registered
        .iter()
        .find(|entry| entry.name == "loose")
        .expect("the plugin is reported");
    assert_eq!(entry.status, PluginStatus::Inactive);
    assert!(
        entry.loaded.is_none() && !load.is_active("loose"),
        "nothing about this row reached the active surface"
    );
    let diagnostic = entry.diagnostic.clone().expect("a diagnostic");
    assert!(
        diagnostic.contains("`unsandboxed`")
            && diagnostic.contains("no authorization record exists")
            && diagnostic.contains("orbit plugin enable loose"),
        "the operator is told what was claimed and what settles it: {diagnostic}"
    );
    assert_eq!(load.diagnostics.len(), 1, "{:?}", load.diagnostics);

    // Not a silent skip: the refusal is in the audit trail.
    let denials = store
        .list_audit_events(&orbit_store::contracts::AuditEventFilter {
            target_type: Some("plugin".to_string()),
            status: Some(AuditEventStatus::Denied),
            limit: 10,
            ..Default::default()
        })
        .expect("audit events");
    let denial = denials.first().expect("the refusal was audited");
    assert_eq!(denial.command, "plugin.load");
    assert_eq!(denial.target_id.as_deref(), Some("loose"));
    assert!(
        denial
            .arguments_json
            .as_deref()
            .is_some_and(|arguments| arguments.contains("unsandboxed")),
        "the row's claimed grant set is on the audit row: {:?}",
        denial.arguments_json
    );
    assert_eq!(denial.error_message.as_deref(), Some(diagnostic.as_str()));
}

/// The same plugin, with the same grants, recorded the way `orbit plugin
/// enable` records them: it loads and its tool is active.
#[test]
fn grants_recorded_by_the_authorizing_path_load_unchanged() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let orbit_dir = temp.path().join("repo/.orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    write_plugin_unsandboxed(
        &plugin_install_path(&global_root, "loose", "1.0.0"),
        "loose",
    );

    let store = Store::open(&global_root.join("orbit.db")).expect("open store");
    store
        .with_transaction(|tx| tx.upsert_plugin(&record(&global_root, "loose")))
        .expect("record the install");
    let grants = ["unsandboxed".to_string()];
    store
        .with_transaction(|tx| tx.set_plugin_enabled("loose", true, &grants).map(|_| ()))
        .expect("record the grant");
    record_authorized_grants(&global_root, "loose", true, &grants).expect("authorize the grant");

    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let load = load_host_plugins(
        &global_root,
        &orbit_dir,
        &store,
        &mut registry,
        &std::collections::BTreeMap::new(),
    );

    assert!(registry.is_active("loose.hello"), "{:?}", load.diagnostics);
    assert!(load.is_active("loose"));
    assert!(load.diagnostics.is_empty(), "{:?}", load.diagnostics);
}

/// A row nobody has enabled carries no grants, so there is nothing to verify
/// and nothing to re-authorize: a host that has never run `orbit plugin enable`
/// is not broken by the check.
#[test]
fn a_plugin_enabled_without_any_grant_still_loads_without_a_record() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let orbit_dir = temp.path().join("repo/.orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    write_plugin(
        &plugin_install_path(&global_root, "plain", "1.0.0"),
        "plain",
        "",
    );

    let store = Store::open(&global_root.join("orbit.db")).expect("open store");
    store
        .with_transaction(|tx| tx.upsert_plugin(&record(&global_root, "plain")))
        .expect("record the install");

    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let load = load_host_plugins(
        &global_root,
        &orbit_dir,
        &store,
        &mut registry,
        &std::collections::BTreeMap::new(),
    );
    assert!(registry.is_active("plain.hello"), "{:?}", load.diagnostics);
    assert!(load.diagnostics.is_empty(), "{:?}", load.diagnostics);
}
