//! Sibling tests for `plugin_host.rs`: one broken plugin must not take the
//! runtime, the built-ins, or another plugin down with it (design §4.9).

use std::path::Path;

use orbit_store::Store;
use orbit_tools::ToolRegistry;
use orbit_types::plugin::{InstalledPlugin, PLUGIN_HOST_API, PluginStatus};
use orbit_types::telemetry::AuditEventStatus;

use super::super::plugin_grants::{plugin_grant_witness_path, record_authorized_grants};
use super::super::plugin_host::{
    host_plugin_cli_groups, host_plugin_registry, load_host_plugins, plugin_install_path,
};

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
        enabled: true,
        grants: Vec::new(),
        first_party: false,
        certified_orbit_version: None,
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
fn host_api_and_platform_mismatches_register_their_tools_inactive() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let orbit_dir = temp.path().join("repo/.orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");

    write_plugin(
        &plugin_install_path(&global_root, "wrongapi", "1.0.0"),
        "wrongapi",
        &format!(
            "  requires:\n    host_api: {}\n",
            PLUGIN_HOST_API.saturating_add(1)
        ),
    );
    write_plugin(
        &plugin_install_path(&global_root, "wrongplatform", "1.0.0"),
        "wrongplatform",
        "  requires:\n    platforms: [orbit-test-never]\n",
    );

    let store = Store::open(&global_root.join("orbit.db")).expect("open store");
    for name in ["wrongapi", "wrongplatform"] {
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

    for (name, reason) in [("wrongapi", "host_api"), ("wrongplatform", "supports")] {
        let tool = format!("{name}.hello");
        assert!(
            registry.has(&tool) && !registry.is_active(&tool),
            "a requirement mismatch keeps {tool} addressable but inactive"
        );
        assert!(
            registry
                .inactive_diagnostic(&tool)
                .is_some_and(|diagnostic| diagnostic.contains(reason)),
            "the inactive tool explains its {reason} mismatch"
        );
        assert!(
            load.registered
                .iter()
                .any(|entry| entry.name == name && entry.status == PluginStatus::Inactive),
            "the host load reports {name} inactive"
        );
    }
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

/// The composition gap ORB-12785 closes: the witness binds the grant *names*,
/// not the tree they apply to. A backend enabled with `fs,orbit_tools` writes a
/// second plugin tree under one of its own write roots (`state/logs`), then
/// repoints the row's `install_path` and `manifest_digest` at it. Name,
/// enabled and grants are untouched, so the witness still matches; the digest
/// check passes because both sides are now the attacker's; the recorded grant
/// names cover whatever the new manifest requests. Without the install-path
/// check the plugin registers Active with the attacker's backend.
#[test]
fn a_row_repointed_at_a_tree_outside_the_install_root_is_refused_with_the_witness_intact() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let orbit_dir = temp.path().join("repo/.orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    write_plugin(
        &plugin_install_path(&global_root, "demo", "1.0.0"),
        "demo",
        "",
    );

    // The authorizing path, exactly as `orbit plugin enable demo --grant
    // fs,orbit_tools` records it.
    let grants = ["fs".to_string(), "orbit_tools".to_string()];
    let store = Store::open(&global_root.join("orbit.db")).expect("open store");
    let mut installed = record(&global_root, "demo");
    installed.grants = grants.to_vec();
    store
        .with_transaction(|tx| tx.upsert_plugin(&installed))
        .expect("record the install");
    record_authorized_grants(&global_root, "demo", true, &grants).expect("authorize the grants");

    // The attacker's tree, under a directory the `orbit_tools` sandbox lets the
    // backend write. Its manifest asks for more than the granted tree did.
    let evil = global_root.join("state/logs/evil");
    write_plugin_verb(&evil, "demo", "escalate", "");
    let manifest = evil.join("plugin.yaml");
    let body = std::fs::read_to_string(&manifest).expect("read manifest");
    std::fs::write(
        &manifest,
        body.replace(
            "  backend:\n",
            "  permissions:\n    fs:\n      write: [\"/var/tmp\"]\n    orbit_tools: [orbit.task.update]\n  backend:\n",
        ),
    )
    .expect("widen the attacker's manifest");
    let evil_digest =
        orbit_tools::plugin::manifest_digest(&std::fs::read(&manifest).expect("bytes"));

    // The row write: `UPDATE plugins SET install_path=…, manifest_digest=…`
    // through the store's own upsert, leaving name, enabled and grants alone.
    let mut repointed = installed.clone();
    repointed.install_path = evil.to_string_lossy().into_owned();
    repointed.manifest_digest = evil_digest;
    store
        .with_transaction(|tx| tx.upsert_plugin(&repointed))
        .expect("the attacker's row write succeeds; the loader is what refuses it");
    assert!(
        plugin_grant_witness_path(&global_root, "demo").is_file(),
        "the witness is untouched; nothing it measures moved"
    );

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
        !registry.has("demo.escalate") && !registry.has("demo.hello"),
        "nothing from the relocated tree, or the original, reaches the registry"
    );
    let entry = load
        .registered
        .iter()
        .find(|entry| entry.name == "demo")
        .expect("the plugin is reported");
    assert_eq!(
        entry.status,
        PluginStatus::Inactive,
        "{:?}",
        entry.diagnostic
    );
    assert!(
        entry.loaded.is_none() && !entry.grants_authorized && !load.is_active("demo"),
        "the row is refused outright, not loaded and then found wanting"
    );
    let diagnostic = entry.diagnostic.clone().expect("a diagnostic");
    assert!(
        diagnostic.contains(&repointed.install_path)
            && diagnostic.contains(&global_root.join("plugins/demo").display().to_string())
            && diagnostic.contains("orbit plugin remove demo"),
        "the operator is told the recorded path, the expected root and what settles it: \
         {diagnostic}"
    );
    assert_eq!(load.diagnostics.len(), 1, "{:?}", load.diagnostics);

    // Audited like the grant refusal: one `plugin.load` / denied row naming
    // the path the row asked this host to load.
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
    assert_eq!(denial.subcommand.as_deref(), Some("verify_install_path"));
    assert_eq!(denial.target_id.as_deref(), Some("demo"));
    assert!(
        denial
            .arguments_json
            .as_deref()
            .is_some_and(|arguments| arguments.contains(&repointed.install_path)),
        "the row's claimed install path is on the audit row: {:?}",
        denial.arguments_json
    );
    assert_eq!(denial.error_message.as_deref(), Some(diagnostic.as_str()));
}

/// A backend that can write `orbit.db` sets `first_party = 1` on its own
/// row. Validation names tools from the manifest (`command.exec`); using
/// the row would register `orbit.command.exec` over the built-in.
#[test]
fn a_first_party_row_for_a_third_party_manifest_does_not_take_orbit_command_exec() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let orbit_dir = temp.path().join("repo/.orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    write_plugin_verb(
        &plugin_install_path(&global_root, "command", "1.0.0"),
        "command",
        "exec",
        "",
    );

    let store = Store::open(&global_root.join("orbit.db")).expect("open store");
    let mut installed = record(&global_root, "command");
    installed.first_party = true;
    store
        .with_transaction(|tx| tx.upsert_plugin(&installed))
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

    let schema = registry
        .get_schema("orbit.command.exec")
        .expect("the built-in stays registered");
    assert!(
        schema.builtin && registry.plugin_binding("orbit.command.exec").is_none(),
        "orbit.command.exec still resolves to the built-in"
    );
    assert!(
        !registry.has("command.exec"),
        "the refused row contributes no plugin tool"
    );
    let entry = load
        .registered
        .iter()
        .find(|entry| entry.name == "command")
        .expect("the plugin is reported");
    assert_eq!(entry.status, PluginStatus::Inactive);
    let diagnostic = entry.diagnostic.clone().expect("a diagnostic");
    assert!(
        diagnostic.contains("first_party") && diagnostic.contains("origin: orbit"),
        "the operator is told the row claim and the missing manifest origin: {diagnostic}"
    );
    assert_eq!(load.diagnostics.len(), 1, "{:?}", load.diagnostics);
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

/// A hand-edited `plugin.yaml` after install is not the granted manifest:
/// the loader registers the plugin inactive and names both digests plus the
/// re-consent commands (design §4.1).
#[test]
fn a_rewritten_manifest_is_registered_inactive_naming_both_digests() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let orbit_dir = temp.path().join("repo/.orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    let install_path = plugin_install_path(&global_root, "demo", "1.0.0");
    write_plugin(&install_path, "demo", "");

    let store = Store::open(&global_root.join("orbit.db")).expect("open store");
    store
        .with_transaction(|tx| tx.upsert_plugin(&record(&global_root, "demo")))
        .expect("record the install");

    let stored = store
        .get_plugin("demo")
        .expect("row")
        .expect("installed")
        .manifest_digest;
    let manifest = install_path.join("plugin.yaml");
    let body = std::fs::read_to_string(&manifest).expect("read manifest");
    std::fs::write(
        &manifest,
        body.replace("version: 1.0.0", "version: 1.0.0\n  description: tampered"),
    )
    .expect("rewrite manifest");
    let loaded = orbit_tools::plugin::manifest_digest(&std::fs::read(&manifest).expect("bytes"));
    assert_ne!(stored, loaded, "the rewrite must change the digest");

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
        .find(|entry| entry.name == "demo")
        .expect("the plugin is reported");
    assert_eq!(entry.status, PluginStatus::Inactive);
    let diagnostic = entry.diagnostic.clone().expect("a diagnostic");
    assert!(
        diagnostic.contains(&stored)
            && diagnostic.contains(&loaded)
            && diagnostic.contains("orbit plugin add --force")
            && diagnostic.contains("orbit plugin enable demo"),
        "{diagnostic}"
    );
    assert!(
        registry.has("demo.hello") && !registry.is_active("demo.hello"),
        "the rewritten plugin is inactive, not missing"
    );
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
