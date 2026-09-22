//! Sibling tests for `plugin_host.rs`: one broken plugin must not take the
//! runtime, the built-ins, or another plugin down with it (design §4.9).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use orbit_store::Store;
use orbit_tools::ToolRegistry;
use orbit_tools::plugin::{load_plugin_dir, refuse_covering_fs_write_roots, render_fs_roots};
use orbit_types::plugin::{
    InstalledPlugin, PLUGIN_HOST_API, PluginGrant, PluginGrantSet, PluginProvenance, PluginStatus,
    PluginTemplateVars,
};
use orbit_types::telemetry::AuditEventStatus;

use crate::OrbitRuntime;
use crate::application::plugin::list_plugins;

use super::super::plugin_grants::{plugin_grant_witness_path, record_authorized_grants};
use super::super::plugin_host::{
    build_plugin_backend, host_plugin_cli_groups, host_plugin_registry, load_host_plugins,
    plugin_backend, plugin_dir_load_count, plugin_install_path, plugin_state_dir,
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

fn write_plugin_with_typed_config_roots(root: &Path, name: &str) {
    std::fs::create_dir_all(root.join("bin")).expect("create bin dir");
    std::fs::create_dir_all(root.join("schemas")).expect("create schema dir");
    std::fs::write(root.join("bin/backend.sh"), "#!/bin/sh\n").expect("write backend");
    std::fs::write(
        root.join("schemas/config.json"),
        "{\n  \"type\": \"object\",\n  \"properties\": {\n    \
         \"directory\": { \"type\": \"string\" },\n    \
         \"port\": { \"type\": \"integer\" },\n    \
         \"enabled\": { \"type\": \"boolean\" }\n  }\n}\n",
    )
    .expect("write config schema");
    std::fs::write(
        root.join("plugin.yaml"),
        format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {name}\n  version: 1.0.0\nspec:\n  \
             backend:\n    type: exec\n    command: bin/backend.sh\n  config:\n    schema: \
             schemas/config.json\n    defaults: {{ directory: default, port: 7, enabled: true }}\n  \
             permissions:\n    fs:\n      read:\n        - \"{{{{config.directory}}}}/read\"\n        \
             - \"{{{{config.port}}}}/read\"\n        - \"{{{{config.enabled}}}}/read\"\n        - \
             relative/read\n      write:\n        - \"{{{{config.directory}}}}/write\"\n        - \
             \"{{{{config.port}}}}/write\"\n        - \"{{{{config.enabled}}}}/write\"\n        - \
             relative/write\n  tools:\n    - name: hello\n      execution_kind: read_only\n      \
             mcp_scope: workspace\n"
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
        archive_digest: None,
        enabled: true,
        grants: Vec::new(),
        first_party: false,
        certified_orbit_version: None,
        installed_at: String::new(),
        updated_at: String::new(),
    }
}

fn capture_errors<F, T>(f: F) -> (T, String)
where
    F: FnOnce() -> T,
{
    use std::io::{self, Write};
    use tracing_subscriber::filter::LevelFilter;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone)]
    struct CaptureMakeWriter(Arc<Mutex<Vec<u8>>>);
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl<'a> MakeWriter<'a> for CaptureMakeWriter {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            CaptureWriter(Arc::clone(&self.0))
        }
    }

    impl Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(CaptureMakeWriter(Arc::clone(&buffer)))
        .with_max_level(LevelFilter::ERROR)
        .with_ansi(false)
        .without_time()
        .finish();
    let result = tracing::subscriber::with_default(subscriber, f);
    let logs =
        String::from_utf8(buffer.lock().expect("capture buffer lock").clone()).expect("utf8 logs");
    (result, logs)
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

#[test]
fn read_only_host_discovery_reports_a_refused_row_without_attempting_its_audit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    write_plugin_unsandboxed(
        &plugin_install_path(&global_root, "loose", "1.0.0"),
        "loose",
    );

    let audit_db = global_root.join("orbit.db");
    let store = Store::open(&audit_db).expect("open store");
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
        .expect("write the unauthorized grants");

    let (groups, errors) = capture_errors(|| {
        host_plugin_cli_groups(&global_root, &audit_db, &BTreeMap::new())
            .expect("read-only CLI discovery completes")
    });
    assert!(
        groups.is_empty(),
        "a refused plugin has no CLI group: {groups:?}"
    );
    assert!(
        errors.is_empty(),
        "read-only CLI discovery must not emit an error for the intentionally skipped audit: \
         {errors}"
    );

    let (_, load) = host_plugin_registry(&global_root, &audit_db, &BTreeMap::new())
        .expect("read-only MCP discovery completes");
    assert!(
        load.diagnostics.iter().any(|diagnostic| {
            diagnostic.plugin == "loose"
                && diagnostic
                    .message
                    .contains("no authorization record exists")
        }),
        "the refusal remains visible: {:?}",
        load.diagnostics
    );

    let denials = store
        .list_audit_events(&orbit_store::contracts::AuditEventFilter {
            target_type: Some("plugin".to_string()),
            status: Some(AuditEventStatus::Denied),
            limit: 10,
            ..Default::default()
        })
        .expect("audit events");
    assert!(
        denials.is_empty(),
        "read-only discovery must not attempt a refusal audit: {denials:?}"
    );

    let mut registry = ToolRegistry::new();
    let writable_load = load_host_plugins(
        &global_root,
        &global_root,
        &store,
        &mut registry,
        &BTreeMap::new(),
    );
    assert!(
        writable_load
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.plugin == "loose"),
        "the writable load still refuses the row"
    );
    assert_eq!(
        store
            .list_audit_events(&orbit_store::contracts::AuditEventFilter {
                target_type: Some("plugin".to_string()),
                status: Some(AuditEventStatus::Denied),
                limit: 10,
                ..Default::default()
            })
            .expect("audit events")
            .len(),
        1,
        "the writable load records the refusal"
    );
}

#[test]
fn plugin_list_reuses_the_cli_tree_load_once_per_plugin() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let workspace_root = temp.path().join("repo/.orbit");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");

    let plugin_roots = ["alpha", "beta", "disabled"].map(|name| {
        let root = plugin_install_path(&global_root, name, "1.0.0");
        write_plugin(&root, name, "");
        root
    });
    let audit_db = global_root.join("orbit.db");
    {
        let store = Store::open(&audit_db).expect("open store");
        for name in ["alpha", "beta"] {
            store
                .with_transaction(|tx| tx.upsert_plugin(&record(&global_root, name)))
                .expect("record the install");
        }
        let mut disabled = record(&global_root, "disabled");
        disabled.enabled = false;
        store
            .with_transaction(|tx| tx.upsert_plugin(&disabled))
            .expect("record the disabled install");
    }

    let groups = host_plugin_cli_groups(&global_root, &audit_db, &BTreeMap::new())
        .expect("build the pre-clap plugin tree");
    assert_eq!(groups.len(), 2, "both plugins contribute a CLI group");

    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root)
        .expect("build the runtime for plugin list");
    let summaries = list_plugins(&runtime).expect("run plugin list");
    assert_eq!(
        summaries.len(),
        3,
        "enabled and disabled plugins are listed"
    );
    assert_eq!(
        summaries
            .iter()
            .find(|summary| summary.name == "disabled")
            .expect("disabled summary")
            .tools
            .len(),
        1,
        "the disabled row keeps its manifest details"
    );

    for root in &plugin_roots {
        assert_eq!(
            plugin_dir_load_count(root),
            1,
            "one `orbit plugin list` invocation must load each plugin directory once: {}",
            root.display()
        );
    }

    let manifest = plugin_roots[0].join("plugin.yaml");
    let mut edited = std::fs::read_to_string(&manifest).expect("read cached manifest");
    edited.push_str("# changed after the first load\n");
    std::fs::write(&manifest, edited).expect("edit cached manifest");
    let groups = host_plugin_cli_groups(&global_root, &audit_db, &BTreeMap::new())
        .expect("rebuild after a manifest edit");
    assert_eq!(groups.len(), 1, "the edited plugin is refused by digest");
    assert_eq!(plugin_dir_load_count(&plugin_roots[0]), 2);
    assert_eq!(
        plugin_dir_load_count(&plugin_roots[1]),
        1,
        "an unchanged sibling remains cached"
    );
    assert_eq!(
        plugin_dir_load_count(&plugin_roots[2]),
        1,
        "the disabled plugin is loaded only by the list projection"
    );
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

/// A row's grants can pass witness verification and still name something no
/// current `PluginGrant` recognizes — retired, renamed, or written by a newer
/// Orbit. Dropping it silently, the way a plain `filter_map` would, runs the
/// plugin under fewer grants than were authorized without saying so; the
/// loader refuses the row instead, and the diagnostic names the grant it
/// could not parse.
#[test]
fn a_row_naming_a_grant_this_build_does_not_recognize_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let orbit_dir = temp.path().join("repo/.orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    write_plugin(
        &plugin_install_path(&global_root, "demo", "1.0.0"),
        "demo",
        "",
    );

    let store = Store::open(&global_root.join("orbit.db")).expect("open store");
    store
        .with_transaction(|tx| tx.upsert_plugin(&record(&global_root, "demo")))
        .expect("record the install");
    // Recorded and authorized exactly the way a real grant would be, so the
    // witness matches; only the name itself is unrecognized.
    let grants = ["wifi".to_string()];
    store
        .with_transaction(|tx| tx.set_plugin_enabled("demo", true, &grants).map(|_| ()))
        .expect("record the grant");
    record_authorized_grants(&global_root, "demo", true, &grants).expect("authorize the grant");

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
        registry.has("demo.hello") && !registry.is_active("demo.hello"),
        "the tool name stays addressable but inactive"
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
    let diagnostic = entry.diagnostic.clone().expect("a diagnostic");
    assert!(
        diagnostic.contains("wifi"),
        "the diagnostic must name the unrecognized grant: {diagnostic}"
    );
    assert_eq!(load.diagnostics.len(), 1, "{:?}", load.diagnostics);
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

#[test]
fn typed_config_and_relative_fs_roots_match_validate_registration_call_and_conformance() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugin_root = temp.path().join("plugin");
    let global_root = temp.path().join("global");
    let workspace_root = temp.path().join("workspace");
    let state_dir = plugin_state_dir(&global_root, "rooted");
    for dir in [&global_root, &workspace_root, &state_dir] {
        std::fs::create_dir_all(dir).expect("create phase root");
    }
    write_plugin_with_typed_config_roots(&plugin_root, "rooted");
    let plugin = load_plugin_dir(&plugin_root).expect("load manifest");

    // The string and boolean are effective operator overrides; the integer
    // stays a typed manifest default. Every phase must stringify all three.
    let config = BTreeMap::from([(
        "rooted".to_string(),
        serde_json::json!({"directory": "../configured", "enabled": false}),
    )]);
    let section = super::super::plugin_config::plugin_config_section(&plugin, &config);
    assert_eq!(
        section.as_value(),
        &serde_json::json!({"directory": "../configured", "enabled": false, "port": 7}),
        "the backend's own view of the section keeps every JSON type"
    );
    let config_values = section.rendered_values();
    assert_eq!(
        config_values,
        BTreeMap::from([
            ("directory".to_string(), "../configured".to_string()),
            ("enabled".to_string(), "false".to_string()),
            ("port".to_string(), "7".to_string()),
        ])
    );

    let grants = PluginGrantSet::from_grants([PluginGrant::Fs]);
    let provenance = || PluginProvenance {
        name: "rooted".to_string(),
        version: plugin.manifest.metadata.version.clone(),
        manifest_digest: plugin.manifest_digest.clone(),
        grants: vec!["fs".to_string()],
    };

    // `validate_plugin_dir` and conformance both construct their backends
    // through this builder. Registration additionally derives the same
    // effective values from the installed row and resolved config.
    let validate = build_plugin_backend(
        &plugin,
        provenance(),
        &state_dir,
        &global_root,
        grants.clone(),
        section.clone(),
    );
    refuse_covering_fs_write_roots(validate.spec(), None).expect("validate roots");

    let installed = InstalledPlugin {
        name: "rooted".to_string(),
        version: plugin.manifest.metadata.version.clone(),
        source: "fixture".to_string(),
        install_path: plugin_root.to_string_lossy().into_owned(),
        archive_digest: None,
        manifest_digest: plugin.manifest_digest.clone(),
        enabled: true,
        grants: vec!["fs".to_string()],
        first_party: false,
        certified_orbit_version: None,
        installed_at: String::new(),
        updated_at: String::new(),
    };
    let registration = plugin_backend(&global_root, &installed, &plugin, &config);
    refuse_covering_fs_write_roots(registration.spec(), None).expect("registration roots");

    let conformance = build_plugin_backend(
        &plugin,
        provenance(),
        &state_dir,
        &global_root,
        grants,
        section,
    );
    refuse_covering_fs_write_roots(conformance.spec(), None).expect("conformance roots");

    let render = |backend: &orbit_tools::plugin::PluginBackend| {
        let spec = backend.spec();
        let vars = PluginTemplateVars {
            workspace: Some(workspace_root.to_string_lossy().into_owned()),
            plugin_root: spec.plugin_root.to_string_lossy().into_owned(),
            plugin_state: spec.state_dir.to_string_lossy().into_owned(),
            config: spec.config_values(),
        };
        render_fs_roots(spec, &vars).expect("render roots")
    };
    let validate_roots = render(&validate);
    let registration_roots = render(&registration);
    let conformance_roots = render(&conformance);

    let call_profile = registration
        .spec()
        .sandbox_profile(Some(&workspace_root))
        .expect("call-time profile");
    assert_eq!(
        call_profile.read[1..],
        registration_roots.read,
        "call time adds only the mandatory plugin-root read before the declared roots"
    );
    assert_eq!(call_profile.write, registration_roots.write);
    assert_eq!(validate_roots, registration_roots);
    assert_eq!(registration_roots, conformance_roots);

    assert_eq!(
        registration_roots.read,
        vec![
            plugin_root.join("../configured/read"),
            plugin_root.join("7/read"),
            plugin_root.join("false/read"),
            plugin_root.join("relative/read"),
        ]
    );
    assert_eq!(
        registration_roots.write,
        vec![
            plugin_root.join("../configured/write"),
            plugin_root.join("7/write"),
            plugin_root.join("false/write"),
            plugin_root.join("relative/write"),
        ]
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
