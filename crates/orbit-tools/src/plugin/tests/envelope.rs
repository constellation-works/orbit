//! What both dispatch surfaces tell a backend about the call, and what they
//! must never tell anything else.

use std::path::Path;

use orbit_types::plugin::{PluginGrant, PluginPermissions};
use serde_json::json;

use super::super::backend::{PluginBackendSpec, PluginConfigSection};
use super::super::envelope::{call_context, exec_envelope};
use super::super::mcp::tools_call_params;
use super::support::{context, spec};
use crate::ToolContext;

/// A backend spec whose plugin is configured: `[plugins.demo]` over the
/// manifest's defaults, already validated by the host.
fn configured_spec(root: &Path) -> PluginBackendSpec {
    let mut spec = (*spec(
        root.join("bin/backend.sh"),
        root,
        PluginPermissions::default(),
        &[PluginGrant::Fs],
    ))
    .clone();
    spec.config = PluginConfigSection::new(json!({
        "index_dir": "/srv/graph",
        "max_nodes": 500,
        "incremental": true,
        "api_token": "s3cret",
    }));
    spec
}

fn call_ctx(root: &Path, workspace: &Path) -> ToolContext {
    ToolContext {
        workspace_root: Some(workspace.to_path_buf()),
        agent_name: Some("claude".to_string()),
        model_name: Some("opus-5".to_string()),
        ..context(root)
    }
}

/// An `exec` backend has had no view of its own `[plugins.<ns>]` section: it
/// could only read what the manifest interpolated into its arguments through
/// `{{config.<key>}}`. The envelope now carries the section itself, typed.
#[test]
fn the_exec_envelope_carries_the_effective_config_section() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let spec = configured_spec(temp.path());
    let ctx = call_ctx(temp.path(), &workspace);

    let envelope = exec_envelope(&spec, &ctx, "demo.hello", json!({ "name": "world" }));

    assert_eq!(envelope["schema_version"], 1);
    assert_eq!(envelope["tool"], "demo.hello");
    assert_eq!(envelope["input"]["name"], "world");
    assert_eq!(
        envelope["context"],
        json!({
            "workspace_root": workspace.to_string_lossy(),
            "agent": "claude",
            "model": "opus-5",
            "config": {
                "index_dir": "/srv/graph",
                "max_nodes": 500,
                "incremental": true,
                "api_token": "s3cret",
            },
        }),
        "the context is the caller's facts plus the plugin's effective section"
    );
    // Typed, not stringified: a backend reading `max_nodes` gets a number,
    // which is what `{{config.<key>}}` substitution cannot give it.
    assert!(envelope["context"]["config"]["max_nodes"].is_u64());
    assert!(envelope["context"]["config"]["incremental"].is_boolean());
}

/// One plugin configured one way must look the same whichever transport the
/// host chose for it: an operator reading `[plugins.<ns>]` is not told which
/// backend type their plugin declares [ORB-12826].
#[test]
fn both_dispatch_surfaces_send_the_same_config_for_one_plugin_and_workspace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let spec = configured_spec(temp.path());
    let ctx = call_ctx(temp.path(), &workspace);

    let exec = exec_envelope(&spec, &ctx, "demo.hello", json!({ "name": "world" }));
    let mcp = tools_call_params(
        &spec,
        &ctx,
        "demo.hello",
        "hello",
        json!({ "name": "world" }),
    );

    assert_eq!(
        exec["context"]["config"], mcp["_meta"]["orbit"]["config"],
        "one resolution feeds both surfaces"
    );
    assert_eq!(exec["context"]["config"], *spec.config.as_value());

    // And the rest of the context agrees too: `mcp` adds the tool name its
    // shared child has no `ORBIT_TOOL_NAME` for, and nothing else.
    let mut expected = exec["context"].clone();
    expected["tool"] = json!("demo.hello");
    assert_eq!(mcp["_meta"]["orbit"], expected);
    assert_eq!(mcp["name"], "hello");
    assert_eq!(mcp["arguments"]["name"], "world");
}

/// A plugin is configured with its credentials like any other key, so the
/// section goes to the backend process and nowhere else. `Debug` is the one
/// way a value could reach a log line without a caller meaning it to: it
/// prints the key names only.
#[test]
fn the_config_section_is_not_printed_by_debug() {
    let temp = tempfile::tempdir().expect("tempdir");
    let spec = configured_spec(temp.path());

    let rendered = format!("{spec:?}");
    assert!(
        !rendered.contains("s3cret"),
        "a formatted spec must not carry configured values: {rendered}"
    );
    assert!(
        rendered.contains("api_token"),
        "the key names stay, so a diagnostic can still say what was set: {rendered}"
    );
    assert!(!format!("{:?}", spec.config).contains("s3cret"));
}

/// The section a plugin declares nothing for is an empty object rather than
/// an absent key, so a backend can read `context.config` unconditionally.
#[test]
fn an_unconfigured_plugin_still_receives_a_config_object() {
    let temp = tempfile::tempdir().expect("tempdir");
    let spec = (*spec(
        temp.path().join("bin/backend.sh"),
        temp.path(),
        PluginPermissions::default(),
        &[],
    ))
    .clone();
    let ctx = context(temp.path());

    let context = call_context(&spec, &ctx, None);
    assert_eq!(context["config"], json!({}));
    assert!(spec.config_values().is_empty());
}
