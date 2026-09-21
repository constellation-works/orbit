use orbit_types::plugin::PluginStatus;

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

    super::super::enable_plugin(&runtime, "demo", &["fs".to_string()]).expect("enable");

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
    // A managed executor may have exported its own activity allowlist into
    // this process; pin the one this call runs under.
    let _activity_tools =
        crate::adapter::command::dispatch_test_support::override_activity_tools_for_test([
            "demo.hello",
        ]);
    let output = runtime
        .execute_tool_command(
            "demo.hello",
            serde_json::json!({ "subject": "orbit" }),
            None,
            None,
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
