use super::super::manifest::{
    PluginBackend, PluginBackendType, PluginCliShape, PluginDefinitions, PluginExecutionKind,
    PluginManifest, PluginMcpScope, PluginMetadata, PluginPanelGroup, PluginPanelRender,
    PluginPermissions, PluginRequires, PluginSandbox, PluginSpec, PluginToolSpec, PluginWebLink,
    PluginWebPanel, PluginWebSection, validate_plugin_relative_path,
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
fn schema_properties_with_colliding_or_empty_cli_flags_are_refused() {
    let mut manifest = minimal();
    manifest.spec.tools[0].input_schema = Some(serde_json::json!({
        "type": "object",
        "properties": {
            "task_id": { "type": "string" },
            "taskId": { "type": "string" }
        }
    }));
    let error = manifest
        .validate_structure()
        .expect_err("colliding flags refuse the manifest");
    assert!(
        error.message.contains("task_id") && error.message.contains("taskId"),
        "the diagnostic names both colliding properties: {}",
        error.message
    );

    manifest.spec.tools[0].input_schema = Some(serde_json::json!({
        "type": "object",
        "properties": { "###": { "type": "object" } }
    }));
    let error = manifest
        .validate_structure()
        .expect_err("an empty flag refuses the manifest");
    assert!(error.message.contains("###"), "{}", error.message);

    manifest.spec.tools[0].input_schema = Some(serde_json::json!({
        "type": "object",
        "properties": { "task_id": { "type": "string" } }
    }));
    manifest
        .validate_structure()
        .expect("a well-formed schema remains valid");
}

/// A `*` outside the final path component is not a directory glob:
/// `resolve_patterns` looks up a literal directory named `*`, finds none,
/// and the pattern would otherwise silently match nothing.
#[test]
fn a_wildcard_outside_the_final_component_is_refused() {
    let error = validate_plugin_relative_path("definitions/*/x.yaml", "spec.definitions.jobs[0]")
        .expect_err("a directory-component wildcard must be refused");
    assert!(error.message.contains('*'), "{}", error.message);

    validate_plugin_relative_path("definitions/*.yaml", "spec.definitions.jobs[0]")
        .expect("a wildcard in the final component is a legitimate glob");
    validate_plugin_relative_path("definitions/job.yaml", "spec.definitions.jobs[0]")
        .expect("a literal path has no wildcard to refuse");

    let mut manifest = minimal();
    manifest.spec.definitions = Some(PluginDefinitions {
        activities: vec!["definitions/*/x.yaml".to_string()],
        ..PluginDefinitions::default()
    });
    assert_eq!(
        manifest.validate_structure().unwrap_err().field,
        "spec.definitions.activities[0]"
    );
}

/// §4.6: one `orbit <ns> <verb>` dispatches to one tool. Two tools claiming
/// the same subcommand would register it twice on the host's clap tree, so
/// the manifest is refused naming both tools.
#[test]
fn colliding_or_invalid_cli_verbs_are_refused_by_name() {
    let mut manifest = minimal();
    manifest.spec.tools.push(PluginToolSpec {
        name: "search".into(),
        description: String::new(),
        execution_kind: PluginExecutionKind::ReadOnly,
        mcp_scope: PluginMcpScope::Workspace,
        input_schema: None,
        output_schema: None,
        cli: Some(PluginCliShape {
            verb: Some("hello".into()),
            positional: vec![],
        }),
    });
    let error = manifest
        .validate_structure()
        .expect_err("an override colliding with another tool refuses the manifest");
    assert_eq!(error.field, "spec.tools[1].cli.verb");
    assert!(
        error.message.contains("hello") && error.message.contains("search"),
        "the refusal names both tools: {}",
        error.message
    );

    manifest.spec.tools[0].cli = Some(PluginCliShape {
        verb: Some("run".into()),
        positional: vec![],
    });
    manifest.spec.tools[1].cli = Some(PluginCliShape {
        verb: Some("run".into()),
        positional: vec![],
    });
    let error = manifest
        .validate_structure()
        .expect_err("two overrides claiming one subcommand refuse the manifest");
    assert_eq!(error.field, "spec.tools[1].cli.verb");
    assert!(error.message.contains("run"), "{}", error.message);

    manifest.spec.tools[1].cli = Some(PluginCliShape {
        verb: Some("Run Fast".into()),
        positional: vec![],
    });
    let error = manifest
        .validate_structure()
        .expect_err("an unusable subcommand spelling refuses the manifest");
    assert_eq!(error.field, "spec.tools[1].cli.verb");
    assert!(
        error.message.contains("search") && error.message.contains("Run Fast"),
        "the refusal names the tool and the verb: {}",
        error.message
    );

    manifest.spec.tools[1].cli = Some(PluginCliShape {
        verb: Some("run-fast".into()),
        positional: vec![],
    });
    manifest
        .validate_structure()
        .expect("distinct, well-spelled overrides are valid");
}

/// A `cli.positional` entry the CLI adapter cannot fill is silently dropped,
/// so the manifest is refused instead, naming the tool and the entry.
#[test]
fn a_positional_must_name_a_top_level_input_property_once() {
    let mut manifest = minimal();
    manifest.spec.tools[0].cli = Some(PluginCliShape {
        verb: None,
        positional: vec!["query".into()],
    });
    let error = manifest
        .validate_structure()
        .expect_err("a positional with no input_schema refuses the manifest");
    assert_eq!(error.field, "spec.tools[0].cli.positional[0]");
    assert!(
        error.message.contains("hello") && error.message.contains("query"),
        "the refusal names the tool and the property: {}",
        error.message
    );

    manifest.spec.tools[0].input_schema = Some(serde_json::json!({
        "type": "object",
        "properties": { "query": { "type": "string" } }
    }));
    manifest
        .validate_structure()
        .expect("a positional naming a declared property is valid");

    manifest.spec.tools[0].cli = Some(PluginCliShape {
        verb: None,
        positional: vec!["query".into(), "depth".into()],
    });
    let error = manifest
        .validate_structure()
        .expect_err("a positional outside `properties` refuses the manifest");
    assert_eq!(error.field, "spec.tools[0].cli.positional[1]");
    assert!(error.message.contains("depth"), "{}", error.message);

    manifest.spec.tools[0].cli = Some(PluginCliShape {
        verb: None,
        positional: vec!["query".into(), "query".into()],
    });
    let error = manifest
        .validate_structure()
        .expect_err("a repeated positional refuses the manifest");
    assert_eq!(error.field, "spec.tools[0].cli.positional[1]");
    assert!(error.message.contains("twice"), "{}", error.message);

    // A `{ $ref }` schema is read at load; the manifest cannot resolve it and
    // must not reject a positional it simply cannot see yet.
    manifest.spec.tools[0].input_schema = Some(serde_json::json!({ "$ref": "schemas/hello.json" }));
    manifest.spec.tools[0].cli = Some(PluginCliShape {
        verb: None,
        positional: vec!["query".into()],
    });
    manifest
        .validate_structure()
        .expect("a $ref schema defers the positional check to load");
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
        refresh_ms: None,
    }
}

#[test]
fn a_panel_refresh_window_is_optional_and_bounded() {
    let mut manifest = minimal();
    let mut web_panel = panel("index", "tool:hello");
    web_panel.refresh_ms = Some(super::super::manifest::MIN_PANEL_REFRESH_MS);
    manifest.spec.web = Some(PluginWebSection {
        panels: vec![web_panel.clone()],
        links: vec![],
    });
    manifest
        .validate_structure()
        .expect("the minimum panel refresh window is valid");

    web_panel.refresh_ms = Some(super::super::manifest::MIN_PANEL_REFRESH_MS - 1);
    manifest.spec.web = Some(PluginWebSection {
        panels: vec![web_panel],
        links: vec![],
    });
    assert_eq!(
        manifest.validate_structure().unwrap_err().field,
        "spec.web.panels[0].refresh_ms"
    );
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
