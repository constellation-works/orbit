use std::collections::BTreeMap;

use super::super::grant::{PluginGrant, parse_grants, resolve_grant_selection};
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
    assert_eq!(grants.grants(), [PluginGrant::Fs, PluginGrant::OrbitTools]);
    assert_eq!(grants.to_recorded(), ["fs", "orbit_tools"]);
    let error = parse_grants(&["fs".into(), "wifi".into()]).unwrap_err();
    assert!(
        error.contains("wifi") && error.contains("unsandboxed"),
        "{error}"
    );
}

#[test]
fn fs_grants_carry_the_roots_the_operator_scoped_them_to() {
    // Comma separates both grants and roots, so the parser has to put
    // `{{plugin_state}}` on the `fs` entry and still see `network` as a grant.
    let grants = parse_grants(&[
        "fs={{workspace}}/.orbit-graph".into(),
        "{{plugin_state}}".into(),
        "network".into(),
    ])
    .expect("valid");
    assert_eq!(grants.grants(), [PluginGrant::Fs, PluginGrant::Network]);
    assert_eq!(
        grants.fs_roots(),
        Some(
            [
                "{{workspace}}/.orbit-graph".to_string(),
                "{{plugin_state}}".to_string()
            ]
            .as_slice()
        )
    );
    // The recorded form round-trips, which is what the witness hashes.
    assert_eq!(
        grants.to_recorded(),
        ["fs={{workspace}}/.orbit-graph,{{plugin_state}}", "network"]
    );
    assert_eq!(
        parse_grants(&grants.to_recorded()).expect("round trips"),
        grants
    );

    // Plain `fs` is the manifest-request shorthand and records no roots.
    let plain = parse_grants(&["fs".into()]).expect("valid");
    assert!(plain.contains(PluginGrant::Fs));
    assert_eq!(plain.fs_roots(), None);
    assert_eq!(plain.to_recorded(), ["fs"]);
}

#[test]
fn a_scoped_grant_refuses_the_spellings_that_would_silently_widen_it() {
    // A continuation segment that does not look like a path is a mistyped
    // grant name, not a filesystem root: recording `netwrok` as a directory
    // is the failure this rule exists to make loud.
    let error = parse_grants(&["fs=/srv/data".into(), "netwrok".into()]).unwrap_err();
    assert!(error.contains("netwrok"), "{error}");

    // Only `fs` names paths.
    let error = parse_grants(&["network=loopback".into()]).unwrap_err();
    assert!(error.contains("does not take roots"), "{error}");

    // `fs=` with nothing after it is not a quieter way to write `fs`.
    let error = parse_grants(&["fs=".into()]).unwrap_err();
    assert!(error.contains("at least one root"), "{error}");

    // Asking for the whole request and a slice of it at once is ambiguous in
    // the direction that matters, so it is refused rather than resolved.
    let error = parse_grants(&["fs".into(), "fs=/srv/data".into()]).unwrap_err();
    assert!(error.contains("with and without roots"), "{error}");

    // An unknown grant before any scope is still just unknown.
    let error = parse_grants(&["/srv/data".into()]).unwrap_err();
    assert!(error.contains("/srv/data"), "{error}");
}

#[test]
fn a_scoped_row_still_satisfies_the_manifests_request_for_that_grant() {
    let mut asking = manifest(
        "schemaVersion: 2\nkind: Plugin\nmetadata: {name: demo, version: 0.1.0}\nspec:\n  backend: {type: exec, command: bin/demo}\n  tools:\n    - {name: hello, execution_kind: read_only}\n",
    );
    asking.spec.permissions.fs.write = vec!["{{plugin_state}}".into()];
    asking.spec.permissions.network = PluginNetworkPermission::Loopback;
    // `missing_grants` compares capabilities, not spellings: a narrowed `fs`
    // is still `fs` recorded, and the loader must not report it missing and
    // refuse the plugin the operator just scoped.
    assert_eq!(
        asking.missing_grants(&["fs={{plugin_state}}".into()]),
        [PluginGrant::Network]
    );
}

#[test]
fn grant_selection_resolves_the_reserved_spellings() {
    let plain = manifest(
        "schemaVersion: 2\nkind: Plugin\nmetadata: {name: demo, version: 0.1.0}\nspec:\n  backend: {type: exec, command: bin/demo}\n  tools:\n    - {name: hello, execution_kind: read_only}\n",
    );
    let mut requesting = plain.clone();
    requesting.spec.permissions.fs.write = vec!["{{plugin_state}}".into()];
    requesting.spec.backend.sandbox = PluginSandbox::None;

    let selected = |names: &[&str], manifest: &PluginManifest| {
        resolve_grant_selection(
            &names
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>(),
            manifest,
        )
        .map(|set| set.grants())
    };
    assert_eq!(
        selected(&["none"], &requesting),
        Ok(Vec::new()),
        "`none` is an explicit empty set, not the manifest's own request"
    );
    assert_eq!(
        selected(&["all"], &requesting),
        Ok(PluginGrant::ALL.to_vec())
    );
    assert_eq!(
        selected(&["requested"], &requesting),
        Ok(vec![PluginGrant::Fs, PluginGrant::Unsandboxed])
    );
    // None of the three reserved words names a root, so each records the
    // unscoped form of every grant it selects.
    assert_eq!(
        resolve_grant_selection(&["requested".into()], &requesting)
            .expect("resolves")
            .fs_roots(),
        None
    );
    assert_eq!(
        selected(&["requested"], &plain),
        Ok(Vec::new()),
        "a manifest that asks for nothing resolves `requested` to nothing"
    );
    // An ordinary list still validates the way it always did; the reserved
    // words only apply when they are the sole entry.
    assert_eq!(
        selected(&["fs", "network"], &plain),
        Ok(vec![PluginGrant::Fs, PluginGrant::Network])
    );
    let error = resolve_grant_selection(&["none".into(), "fs".into()], &plain).unwrap_err();
    assert!(error.contains("none"), "{error}");
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
