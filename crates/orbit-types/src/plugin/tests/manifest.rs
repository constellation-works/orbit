use super::super::manifest::{
    PluginBackend, PluginBackendType, PluginExecutionKind, PluginManifest, PluginMcpScope,
    PluginMetadata, PluginPermissions, PluginRequires, PluginSandbox, PluginSpec, PluginToolSpec,
};

fn minimal() -> PluginManifest {
    PluginManifest {
        schema_version: 2,
        kind: "Plugin".into(),
        metadata: PluginMetadata {
            name: "demo".into(),
            version: "0.1.0".into(),
            description: String::new(),
            publisher: None,
            origin: None,
            homepage: None,
        },
        spec: PluginSpec {
            requires: PluginRequires::default(),
            backend: PluginBackend {
                backend_type: PluginBackendType::Exec,
                command: "bin/demo".into(),
                args: vec![],
                timeout_ms: None,
                sandbox: PluginSandbox::Default,
            },
            permissions: PluginPermissions::default(),
            tools: vec![PluginToolSpec {
                name: "hello".into(),
                description: String::new(),
                execution_kind: PluginExecutionKind::ReadOnly,
                mcp_scope: PluginMcpScope::Workspace,
                input_schema: None,
                output_schema: None,
                cli: None,
            }],
            definitions: None,
            skills: vec![],
            config: None,
            web: None,
            tests: vec![],
        },
    }
}

#[test]
fn structure_validation_names_the_field() {
    let mut manifest = minimal();
    manifest
        .validate_structure()
        .expect("minimal manifest is valid");

    manifest.schema_version = 1;
    assert_eq!(
        manifest.validate_structure().unwrap_err().field,
        "schemaVersion"
    );
    manifest.schema_version = 2;

    manifest.kind = "Tool".into();
    assert_eq!(manifest.validate_structure().unwrap_err().field, "kind");
    manifest.kind = "Plugin".into();

    manifest.metadata.name = "orbit".into();
    assert_eq!(
        manifest.validate_structure().unwrap_err().field,
        "metadata.name"
    );
    manifest.metadata.name = "Graph.x".into();
    assert_eq!(
        manifest.validate_structure().unwrap_err().field,
        "metadata.name"
    );
    manifest.metadata.name = "demo".into();

    manifest.spec.backend.backend_type = PluginBackendType::Mcp;
    manifest
        .validate_structure()
        .expect("the mcp backend is a valid backend type");
    manifest.spec.backend.backend_type = PluginBackendType::Exec;

    manifest.spec.permissions.fs.write = vec!["{{home}}/cache".into()];
    let error = manifest.validate_structure().unwrap_err();
    assert_eq!(error.field, "spec.permissions.fs.write[0]");
    assert!(error.message.contains("{{home}}"), "{}", error.message);
    manifest.spec.permissions.fs.write = vec!["{{plugin_state}}".into()];
    manifest
        .validate_structure()
        .expect("{{plugin_state}} is an allowed template");
    manifest.spec.permissions.fs.write.clear();

    manifest.spec.tools.push(manifest.spec.tools[0].clone());
    assert_eq!(
        manifest.validate_structure().unwrap_err().field,
        "spec.tools[1].name"
    );
    manifest.spec.tools.pop();

    manifest.spec.tools[0].input_schema = Some(serde_json::json!("string"));
    assert_eq!(
        manifest.validate_structure().unwrap_err().field,
        "spec.tools[0].input_schema"
    );
}

#[test]
fn unknown_keys_are_rejected_everywhere() {
    let raw = "schemaVersion: 2\nkind: Plugin\nmetadata: {name: demo, version: 0.1.0}\nspec:\n  backend: {type: exec, command: bin/demo}\n  tools:\n    - {name: hello, execution_kind: read_only, colour: red}\n";
    let error = serde_yaml::from_str::<PluginManifest>(raw)
        .unwrap_err()
        .to_string();
    assert!(error.contains("colour"), "{error}");
}
