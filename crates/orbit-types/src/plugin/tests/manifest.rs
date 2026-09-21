use super::super::manifest::{
    PluginBackend, PluginBackendType, PluginExecutionKind, PluginManifest, PluginMcpScope,
    PluginMetadata, PluginPanelGroup, PluginPanelRender, PluginPermissions, PluginRequires,
    PluginSandbox, PluginSpec, PluginToolSpec, PluginWebLink, PluginWebPanel, PluginWebSection,
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
fn env_pass_refuses_orbit_reserved_names() {
    let mut manifest = minimal();

    for name in [
        "ORBIT_OPERATOR",
        "ORBIT_WORKSPACE_CLAIM_TOKEN",
        "ORBIT_RUN_ID",
    ] {
        manifest.spec.permissions.env_pass = vec![name.to_string()];
        let error = manifest.validate_structure().unwrap_err();
        assert_eq!(error.field, "spec.permissions.env_pass[0]");
        assert!(
            error.message.contains(name),
            "diagnostic must name the key: {}",
            error.message
        );
    }

    manifest.spec.permissions.env_pass = vec!["DATABASE_URL".to_string()];
    manifest
        .validate_structure()
        .expect("a non-orbit name is still an allowed env_pass entry");
}

#[test]
fn unknown_keys_are_rejected_everywhere() {
    let raw = "schemaVersion: 2\nkind: Plugin\nmetadata: {name: demo, version: 0.1.0}\nspec:\n  backend: {type: exec, command: bin/demo}\n  tools:\n    - {name: hello, execution_kind: read_only, colour: red}\n";
    let error = serde_yaml::from_str::<PluginManifest>(raw)
        .unwrap_err()
        .to_string();
    assert!(error.contains("colour"), "{error}");
}

fn panel(id: &str, source: &str) -> PluginWebPanel {
    PluginWebPanel {
        id: id.into(),
        title: "Status".into(),
        source: source.into(),
        render: PluginPanelRender::Kv,
        group: PluginPanelGroup::Diagnostics,
    }
}

/// §4.7: a panel may only read a `read_only` tool, and the refusal names the
/// panel so `orbit plugin validate` can point at it.
#[test]
fn a_panel_over_a_mutating_tool_is_refused_by_name() {
    let mut manifest = minimal();
    manifest.spec.tools.push(PluginToolSpec {
        name: "maintain".into(),
        description: String::new(),
        execution_kind: PluginExecutionKind::Mutating,
        mcp_scope: PluginMcpScope::Workspace,
        input_schema: None,
        output_schema: None,
        cli: None,
    });
    manifest.spec.web = Some(PluginWebSection {
        panels: vec![panel("index", "tool:hello")],
        links: vec![],
    });
    manifest
        .validate_structure()
        .expect("a panel over a read_only tool is valid");

    manifest.spec.web = Some(PluginWebSection {
        panels: vec![panel("index", "tool:maintain")],
        links: vec![],
    });
    let error = manifest.validate_structure().unwrap_err();
    assert_eq!(error.field, "spec.web.panels[0].source");
    assert!(
        error.message.contains("index") && error.message.contains("maintain"),
        "the refusal names the panel and its source: {}",
        error.message
    );
}

#[test]
fn a_panel_source_must_name_a_declared_tool_exactly_once() {
    let mut manifest = minimal();
    manifest.spec.web = Some(PluginWebSection {
        panels: vec![panel("index", "hello")],
        links: vec![],
    });
    let error = manifest.validate_structure().unwrap_err();
    assert_eq!(error.field, "spec.web.panels[0].source");
    assert!(error.message.contains("tool:<verb>"), "{}", error.message);

    manifest.spec.web = Some(PluginWebSection {
        panels: vec![panel("index", "tool:absent")],
        links: vec![],
    });
    assert_eq!(
        manifest.validate_structure().unwrap_err().field,
        "spec.web.panels[0].source"
    );

    manifest.spec.web = Some(PluginWebSection {
        panels: vec![panel("index", "tool:hello"), panel("index", "tool:hello")],
        links: vec![],
    });
    assert_eq!(
        manifest.validate_structure().unwrap_err().field,
        "spec.web.panels[1].id"
    );
}

#[test]
fn a_link_url_may_only_use_the_manifest_template_variables() {
    let mut manifest = minimal();
    manifest.spec.web = Some(PluginWebSection {
        panels: vec![],
        links: vec![PluginWebLink {
            title: "Explorer".into(),
            url: "http://127.0.0.1:{{config.port}}".into(),
        }],
    });
    manifest
        .validate_structure()
        .expect("{{config.<key>}} is an allowed reference");

    manifest.spec.web = Some(PluginWebSection {
        panels: vec![],
        links: vec![PluginWebLink {
            title: "Explorer".into(),
            url: "http://127.0.0.1:{{home}}".into(),
        }],
    });
    assert_eq!(
        manifest.validate_structure().unwrap_err().field,
        "spec.web.links[0].url"
    );
}

#[test]
fn a_link_url_must_be_http_or_https() {
    let mut manifest = minimal();
    for url in [
        "javascript:alert(1)",
        "data:text/html,hi",
        "127.0.0.1:7890",
        "file:///etc",
    ] {
        manifest.spec.web = Some(PluginWebSection {
            panels: vec![],
            links: vec![PluginWebLink {
                title: "Explorer".into(),
                url: url.into(),
            }],
        });
        let error = manifest.validate_structure().unwrap_err();
        assert_eq!(error.field, "spec.web.links[0].url", "{url}");
        assert!(
            error.message.contains("http://"),
            "{url}: {}",
            error.message
        );
    }
    manifest.spec.web = Some(PluginWebSection {
        panels: vec![],
        links: vec![PluginWebLink {
            title: "Explorer".into(),
            url: "HTTPS://example.test/{{config.path}}".into(),
        }],
    });
    manifest
        .validate_structure()
        .expect("the scheme check is case-insensitive");
}
