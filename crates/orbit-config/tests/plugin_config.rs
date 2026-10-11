//! Plugin key admission and write validation through the public config API.
#![allow(clippy::expect_used, missing_docs)]

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_config::{
    ConfigScope, ConfigStore, PluginConfigSchema, admit_config_key, register_plugin_config_schemas,
};
use serde_json::{Value, json};

fn register(namespace: &str, schema: Value, defaults: BTreeMap<String, Value>) {
    register_plugin_config_schemas(vec![PluginConfigSchema {
        namespace: namespace.to_owned(),
        schema: Some(schema),
        defaults,
    }]);
}

#[test]
fn schema_accepted_keys_can_be_read_set_and_unset_despite_named_properties_or_defaults() {
    let contracts = [
        (
            "open-named",
            json!({"type": "object", "properties": {"known": {"type": "integer"}}, "additionalProperties": true}),
        ),
        (
            "open-default",
            json!({"type": "object", "additionalProperties": true}),
        ),
        (
            "open-implicit",
            json!({"type": "object", "properties": {"known": {"type": "integer"}}}),
        ),
        (
            "open-typed",
            json!({"type": "object", "properties": {"known": {"type": "integer"}}, "additionalProperties": {"type": "integer"}}),
        ),
        (
            "patterned",
            json!({"type": "object", "properties": {"known": {"type": "integer"}}, "patternProperties": {"^extra$": {"type": "integer"}}, "additionalProperties": false}),
        ),
        (
            "composed",
            json!({"properties": {"known": {"type": "integer"}}, "anyOf": [{"additionalProperties": true}, {"properties": {"extra": {"type": "string"}}}]}),
        ),
        (
            "referenced",
            json!({"$ref": "#/definitions/config", "definitions": {"config": {"type": "object", "properties": {"known": {"type": "integer"}}, "additionalProperties": true}}}),
        ),
        (
            "conditional",
            json!({"properties": {"known": {"type": "integer"}, "extra": {"type": "integer"}}, "additionalProperties": false, "if": {"properties": {"extra": {"type": "null"}}, "required": ["extra"]}, "then": {"properties": {"known": true}, "additionalProperties": false}}),
        ),
    ];
    for (namespace, schema) in contracts {
        register(
            namespace,
            schema,
            BTreeMap::from([("known".into(), json!(1))]),
        );
        let root = tempfile::tempdir().expect("config root");
        let path = root.path().join("config.toml");
        let key = format!("plugins.{namespace}.extra");
        let mut store = ConfigStore::open(ConfigScope::Global, &path).expect("open store");

        admit_config_key(&key).expect("schema admits additional key");
        assert_eq!(
            store.effective_value(&key).expect("read absent key"),
            Value::Null
        );
        store.set_value(&key, "3").expect("stage additional key");
        store
            .validate_for_set(&key)
            .expect("whole section is valid");
        store.save().expect("save valid write");

        let mut reopened = ConfigStore::open(ConfigScope::Global, &path).expect("reopen store");
        assert!(reopened.is_key_set(&key));
        assert_eq!(
            reopened.effective_value(&key).expect("read saved key"),
            json!(3)
        );
        assert!(reopened.unset_value(&key).expect("unset additional key"));
        reopened
            .validate_for_set(&key)
            .expect("section remains valid");
        reopened.save().expect("save unset");
        let unset = ConfigStore::open(ConfigScope::Global, &path).expect("reopen unset store");
        assert!(!unset.is_key_set(&key));
    }
}

#[test]
fn closed_schemas_refuse_unmatched_keys_before_mutation_and_keep_suggestions() {
    for (namespace, schema, suggestions) in [
        (
            "closed-named",
            json!({"properties": {"known": {"type": "integer"}}, "additionalProperties": false}),
            vec!["plugins.closed-named.known".to_owned()],
        ),
        (
            "closed-pattern",
            json!({"properties": {"known": {"type": "integer"}}, "patternProperties": {"^allowed_": {"type": "integer"}}, "additionalProperties": false}),
            vec!["plugins.closed-pattern.known".to_owned()],
        ),
        (
            "closed-empty",
            json!({"additionalProperties": false}),
            vec![],
        ),
        (
            "closed-reference",
            json!({"$ref": "#/definitions/config", "definitions": {"config": {"properties": {"known": true}, "additionalProperties": false}}}),
            vec![],
        ),
        (
            "closed-conjunction",
            json!({"properties": {"known": true}, "allOf": [{"properties": {"known": true}, "additionalProperties": false}]}),
            vec!["plugins.closed-conjunction.known".to_owned()],
        ),
        ("closed-false", json!(false), vec![]),
    ] {
        register(namespace, schema, BTreeMap::new());
        let root = tempfile::tempdir().expect("config root");
        let mut store = ConfigStore::open(ConfigScope::Global, root.path().join("config.toml"))
            .expect("open store");
        let key = format!("plugins.{namespace}.extra");
        let error = admit_config_key(&key)
            .expect_err(&format!("closed schema {namespace} refuses unmatched key"));
        match error {
            OrbitError::InvalidInputDiagnostic { did_you_mean, .. } => {
                assert_eq!(did_you_mean, suggestions);
                for suggestion in suggestions {
                    admit_config_key(&suggestion).expect("suggested key is admitted");
                }
            }
            OrbitError::InvalidInput(_) if suggestions.is_empty() => {}
            other => panic!("unexpected admission error: {other}"),
        }
        store
            .effective_value(&key)
            .expect_err("read refuses forbidden key");
        store
            .set_value(&key, "3")
            .expect_err("set refuses forbidden key");
        store
            .unset_value(&key)
            .expect_err("unset refuses forbidden key");
        assert!(
            !store.is_key_set(&key),
            "refused set must leave the document untouched"
        );
    }
}

#[test]
fn writes_validate_values_and_the_entire_effective_section_including_layers_and_defaults() {
    let namespace = "whole-section";
    register(
        namespace,
        json!({"type": "object", "properties": {"known": {"type": "integer"}}, "required": ["known"], "additionalProperties": {"type": "integer"}}),
        BTreeMap::from([("known".into(), json!(1))]),
    );
    let global = tempfile::tempdir().expect("global root");
    let workspace = tempfile::tempdir().expect("workspace root");
    let path = workspace.path().join("config.toml");
    let key = format!("plugins.{namespace}.extra");
    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");

    store.set_value(&key, "3").expect("stage additional key");
    store
        .validate_workspace_for_set(&key, global.path())
        .expect("manifest default satisfies required key");
    store.save().expect("save valid effective section");
    let saved = std::fs::read_to_string(&path).expect("saved config");

    store
        .set_value(&key, "\"wrong type\"")
        .expect("name admission permits staging value");
    let error = store
        .validate_workspace_for_set(&key, global.path())
        .expect_err("additional property value must validate");
    assert!(error.to_string().contains(&key), "{error}");
    assert_eq!(
        std::fs::read_to_string(&path).expect("unchanged file"),
        saved
    );

    store
        .set_value(&key, "3")
        .expect("restore valid staged value");
    std::fs::write(
        global.path().join("config.toml"),
        format!("[plugins.{namespace}]\nknown = \"invalid\"\n"),
    )
    .expect("invalid global plugin value");
    let error = store
        .validate_workspace_for_set(&key, global.path())
        .expect_err("a different effective key must validate too");
    assert!(
        error
            .to_string()
            .contains(&format!("plugins.{namespace}.known")),
        "{error}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("unchanged file"),
        saved
    );

    register(
        namespace,
        json!({"properties": {"known": {"type": "integer"}}, "required": ["known"], "additionalProperties": true}),
        BTreeMap::new(),
    );
    std::fs::write(global.path().join("config.toml"), "").expect("clear global fixture");
    store
        .validate_workspace_for_set(&key, global.path())
        .expect_err("missing required key is refused without a default");
}
