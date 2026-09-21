use std::collections::BTreeMap;

use super::super::grant::{PluginGrant, parse_grants};
use super::super::manifest::{PluginManifest, PluginNetworkPermission, PluginSandbox};
use super::super::template::{PluginTemplateVars, render_template, template_references};

fn manifest(raw: &str) -> PluginManifest {
    serde_yaml::from_str(raw).expect("manifest parses")
}

#[test]
fn required_grants_follow_the_manifest_requests() {
    let plain = manifest(
        "schemaVersion: 2\nkind: Plugin\nmetadata: {name: demo, version: 0.1.0}\nspec:\n  backend: {type: exec, command: bin/demo}\n  tools:\n    - {name: hello, execution_kind: read_only}\n",
    );
    assert!(plain.required_grants().is_empty());
    assert!(plain.missing_grants(&[]).is_empty());

    let mut asking = plain.clone();
    asking.spec.permissions.fs.write = vec!["{{plugin_state}}".into()];
    asking.spec.permissions.network = PluginNetworkPermission::Loopback;
    asking.spec.permissions.orbit_tools = vec!["orbit.task.show".into()];
    asking.spec.backend.sandbox = PluginSandbox::None;
    assert_eq!(
        asking.required_grants(),
        [
            PluginGrant::Fs,
            PluginGrant::Network,
            PluginGrant::OrbitTools,
            PluginGrant::Unsandboxed
        ]
    );
    assert_eq!(
        asking.missing_grants(&["fs".into(), "orbit_tools".into()]),
        [PluginGrant::Network, PluginGrant::Unsandboxed]
    );
    let fs = asking
        .grant_requests()
        .into_iter()
        .find(|request| request.grant == PluginGrant::Fs)
        .expect("fs row");
    assert_eq!(fs.requested.as_deref(), Some("write={{plugin_state}}"));
}

#[test]
fn grant_names_are_validated_and_deduplicated() {
    let grants = parse_grants(&["orbit_tools".into(), "fs".into(), "fs".into()]).expect("valid");
    assert_eq!(grants, [PluginGrant::Fs, PluginGrant::OrbitTools]);
    let error = parse_grants(&["fs".into(), "wifi".into()]).unwrap_err();
    assert!(
        error.contains("wifi") && error.contains("unsandboxed"),
        "{error}"
    );
}

#[test]
fn templates_render_only_the_allowed_variables() {
    assert_eq!(
        template_references("{{workspace}}/x/{{ config.dir }}"),
        ["workspace", "config.dir"]
    );
    let vars = PluginTemplateVars {
        workspace: Some("/ws".into()),
        plugin_root: "/root".into(),
        plugin_state: "/state".into(),
        config: BTreeMap::from([("dir".to_string(), ".cache".to_string())]),
    };
    assert_eq!(
        render_template("{{workspace}}/{{config.dir}}", &vars, "f").expect("renders"),
        "/ws/.cache"
    );
    assert_eq!(
        render_template("{{plugin_root}}:{{plugin_state}}", &vars, "f").expect("renders"),
        "/root:/state"
    );
    let missing = render_template("{{config.other}}", &vars, "f").unwrap_err();
    assert!(missing.message.contains("other"), "{missing}");
    let no_workspace = render_template(
        "{{workspace}}",
        &PluginTemplateVars {
            workspace: None,
            ..vars.clone()
        },
        "f",
    )
    .unwrap_err();
    assert!(
        no_workspace.message.contains("no workspace"),
        "{no_workspace}"
    );
    let unknown = render_template("{{home}}", &vars, "f").unwrap_err();
    assert!(unknown.message.contains("{{home}}"), "{unknown}");
}
