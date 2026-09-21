use std::collections::BTreeSet;
use std::path::Path;

use orbit_types::plugin::PluginStatus;
use orbit_types::telemetry::AuditEventStatus;

use super::super::{PluginAddOptions, install_plugin, list_plugins, plugin_doctor, show_plugin};
use super::fixture::{PluginFixture, PluginSpecFixture, write_plugin_at};

#[test]
fn add_refuses_a_source_inside_the_repository() {
    let fixture = PluginFixture::new();
    let inside = fixture.repo_root.join("plugins/demo");
    write_plugin_at(&inside, PluginSpecFixture::new("demo", "demo"));

    let error = install_plugin(
        &fixture.runtime,
        inside.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect_err("an in-repository source must be refused");
    let message = error.to_string();
    assert!(message.contains("global-install-only"), "{message}");
    assert!(message.contains(".orbit/plugins.yaml"), "{message}");
    assert!(
        list_plugins(&fixture.runtime).expect("list").is_empty(),
        "the refusal must not record an install"
    );
}

#[test]
fn add_then_enable_puts_the_tool_on_the_surface() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));

    let summary = install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect("install");
    assert_eq!(summary.status, PluginStatus::Disabled);
    assert!(
        summary.install_path.starts_with(
            fixture
                .global_root
                .join("plugins")
                .to_str()
                .expect("utf8 install root")
        ),
        "installed globally: {}",
        summary.install_path
    );
    assert_eq!(summary.manifest_digest.len(), 64);

    // Disabled: the tool is not registered anywhere yet.
    let runtime = fixture.reopen();
    assert!(runtime.show_tool("demo.hello").is_err());
    let doctor = plugin_doctor(&runtime).expect("doctor");
    assert_eq!(doctor.len(), 1);
    assert!(
        doctor[0].message.contains("orbit plugin enable demo"),
        "{doctor:?}"
    );

    super::super::enable_plugin(
        &runtime,
        "demo",
        &super::super::PluginEnableOptions {
            grants: vec!["fs".to_string()],
            force: false,
        },
    )
    .expect("enable");

    let runtime = fixture.reopen();
    let tool = runtime
        .show_tool("demo.hello")
        .expect("plugin tool is registered");
    assert!(tool.active && tool.enabled);
    assert!(!tool.builtin);
    assert!(
        tool.parameters.iter().any(|param| param.name == "subject"),
        "input schema reached the registry: {:?}",
        tool.parameters
    );
    assert!(
        runtime
            .list_mcp_tool_definitions()
            .expect("mcp definitions")
            .iter()
            .any(|definition| definition.schema.name == "demo.hello"),
        "an enabled plugin tool is advertised"
    );

    let shown = show_plugin(&runtime, "demo").expect("show");
    assert_eq!(shown.status, PluginStatus::Active);
    assert_eq!(shown.granted, ["fs"]);
    assert_eq!(shown.tools.len(), 1);
    assert_eq!(
        shown.tools[0].advertised_name.as_deref(),
        Some("demo_hello")
    );
    assert!(
        plugin_doctor(&runtime).expect("doctor")[0]
            .message
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn an_enabled_plugin_tool_executes_through_audited_dispatch() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install");

    let runtime = fixture.reopen();
    let output = fixture
        .call_with_input(
            &runtime,
            "demo.hello",
            serde_json::json!({ "subject": "orbit" }),
        )
        .expect("run the plugin tool");
    assert_eq!(output["plugin"], "demo");
    assert_eq!(output["envelope"]["tool"], "demo.hello");
    assert_eq!(output["envelope"]["input"]["subject"], "orbit");

    let events = runtime
        .list_audit_events(None, Some("demo.hello".to_string()), None, None, 10)
        .expect("audit events");
    let event = events.first().expect("the call was audited");
    let plugin = event
        .plugin
        .as_ref()
        .expect("the audit row names the plugin");
    assert_eq!(plugin.name, "demo");
    assert_eq!(plugin.version, "1.0.0");
    assert_eq!(plugin.manifest_digest.len(), 64);
}

/// Every way a backend can fail short of a valid response is a tool error
/// carrying an audit row with the plugin's provenance and grants, and none of
/// them returns part of the backend's output (design §4.2, §4.4).
#[cfg(unix)]
#[test]
fn a_failing_plugin_call_is_audited_with_plugin_provenance_and_no_partial_output() {
    for (backend, expected, status) in [
        (
            "#!/bin/sh\ncat >/dev/null\necho boom >&2\nexit 7\n",
            "exited with 7",
            AuditEventStatus::Failure,
        ),
        (
            "#!/bin/sh\ncat >/dev/null\nprintf 'almost {\"ok\":true}'\n",
            "invalid JSON output",
            AuditEventStatus::Failure,
        ),
        (
            "#!/bin/sh\ncat >/dev/null\nprintf '{\"ok\":true,\"output\":{\"count\":\"three\"}}\\n'\n",
            "violates its output_schema",
            AuditEventStatus::Failure,
        ),
    ] {
        let fixture = PluginFixture::new();
        let source = fixture.write_plugin(
            PluginSpecFixture::new("demo", "demo")
                .with_backend(backend)
                .with_output_schema(
                    "        type: object\n        required: [count]\n        properties:\n          count: { type: integer }\n",
                ),
        );
        install_plugin(
            &fixture.runtime,
            source.to_str().expect("utf8 path"),
            &PluginAddOptions {
                enable: true,
                ..PluginAddOptions::default()
            },
        )
        .expect("install");

        let runtime = fixture.reopen();
        // The call returning `Err` *is* the no-partial-success property:
        // there is no value for the caller to act on, whichever way the
        // backend failed. The diagnostic may quote the offending value.
        let error = fixture
            .call(&runtime, "demo.hello")
            .expect_err("the backend failed")
            .to_string();
        assert!(error.contains(expected), "{error}");

        let events = runtime
            .list_audit_events(None, Some("demo.hello".to_string()), None, None, 10)
            .expect("audit events");
        let event = events.first().expect("the failed call was audited");
        assert_eq!(event.status, status);
        let plugin = event
            .plugin
            .as_ref()
            .expect("the audit row names the plugin");
        assert_eq!(plugin.name, "demo");
        assert_eq!(plugin.manifest_digest.len(), 64);
    }
}

/// A refusal before the backend starts is audited as `Denied`, still naming
/// the plugin.
#[cfg(unix)]
#[test]
fn an_ungranted_plugin_call_is_audited_as_denied_with_plugin_provenance() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo").requesting_fs_write());
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install");

    let runtime = fixture.reopen();
    fixture
        .call(&runtime, "demo.hello")
        .expect_err("the grant is missing");
    let events = runtime
        .list_audit_events(None, Some("demo.hello".to_string()), None, None, 10)
        .expect("audit events");
    let event = events.first().expect("the refusal was audited");
    assert_eq!(event.status, AuditEventStatus::Denied);
    assert_eq!(
        event.plugin.as_ref().map(|plugin| plugin.name.as_str()),
        Some("demo")
    );
}

#[test]
fn a_pinned_but_uninstalled_plugin_is_reported_without_breaking_the_runtime() {
    let fixture = PluginFixture::new();
    fixture.write_pin_file(
        "schemaVersion: 1\nplugins:\n  - name: absent\n    source: /nowhere/absent\n    enabled: true\n",
    );

    let runtime = fixture.reopen();
    // Built-ins are untouched.
    runtime
        .show_tool("orbit.task.show")
        .expect("builtins still register");

    let summaries = list_plugins(&runtime).expect("list");
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].status, PluginStatus::Missing);
    assert!(summaries[0].pinned);
    let diagnostic = summaries[0].diagnostic.as_deref().unwrap_or_default();
    assert!(diagnostic.contains("orbit plugin sync"), "{diagnostic}");
    let doctor = plugin_doctor(&runtime).expect("doctor");
    assert!(
        doctor[0].message.contains("orbit plugin sync"),
        "{doctor:?}"
    );
}

fn relative_inventory(root: &Path) -> BTreeSet<String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).expect("read inventory") {
            let entry = entry.expect("entry");
            let file_type = entry.file_type().expect("file type");
            let relative = entry
                .path()
                .strip_prefix(root)
                .expect("inventory path is under root")
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(relative);
            if file_type.is_dir() {
                walk(root, &entry.path(), out);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(root, root, &mut out);
    out
}

#[test]
fn install_inventory_matches_the_source_tree() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    let source_inventory = relative_inventory(&source);

    let summary = install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect("install");
    let installed = Path::new(&summary.install_path);
    assert_eq!(
        relative_inventory(installed),
        source_inventory,
        "install must copy the source tree and nothing else"
    );
}

#[cfg(unix)]
#[test]
fn add_refuses_a_source_with_a_symlink_to_a_file_outside_the_tree() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    let secret = fixture.sources.join("outside-secret");
    std::fs::write(&secret, "SECRET-CONTENT-OUTSIDE-PLUGIN-TREE").expect("secret");
    std::os::unix::fs::symlink(&secret, source.join("leaked")).expect("symlink");

    let error = install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect_err("an outside-tree symlink must be refused");
    let message = error.to_string();
    assert!(
        message.contains("leaked"),
        "refusal must name the offending entry: {message}"
    );
    assert!(
        message.contains("symbolic link"),
        "refusal must say why: {message}"
    );
    assert!(
        list_plugins(&fixture.runtime).expect("list").is_empty(),
        "the refusal must not record an install"
    );
    let plugins_root = fixture.global_root.join("plugins");
    if plugins_root.exists() {
        let listing = relative_inventory(&plugins_root);
        assert!(
            listing.iter().all(|path| {
                let bytes = std::fs::read(plugins_root.join(path)).unwrap_or_default();
                !bytes
                    .windows(b"SECRET-CONTENT-OUTSIDE-PLUGIN-TREE".len())
                    .any(|window| window == b"SECRET-CONTENT-OUTSIDE-PLUGIN-TREE")
            }),
            "no installed file may hold the outside-tree target: {listing:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_hand_edited_install_with_a_symlink_cannot_become_active() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    let summary = install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install");

    let secret = fixture.sources.join("outside-secret");
    std::fs::write(&secret, "SECRET-CONTENT-OUTSIDE-PLUGIN-TREE").expect("secret");
    std::os::unix::fs::symlink(&secret, Path::new(&summary.install_path).join("env"))
        .expect("plant symlink in the install tree");

    let runtime = fixture.reopen();
    let shown = show_plugin(&runtime, "demo").expect("show");
    assert_eq!(shown.status, PluginStatus::Inactive);
    let diagnostic = shown.diagnostic.as_deref().unwrap_or_default();
    assert!(
        diagnostic.contains("symbolic link") || diagnostic.contains("env"),
        "load must name the planted link: {diagnostic}"
    );
    assert!(
        runtime.show_tool("demo.hello").is_err(),
        "a tree with a planted symlink must not register tools"
    );
}
