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
            secrets: vec![],
            build: None,
        },
    }
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

const BUILD_MANIFEST: &str = r#"
schemaVersion: 2
kind: Plugin
metadata:
  name: graph
  version: 0.4.1
spec:
  backend:
    type: exec
    command: bin/orbit-graph
  tools:
    - name: query
      execution_kind: read_only
  build:
    programs: [cargo]
    fetch: [cargo, fetch, --locked]
    command: [cargo, build, --release, --offline, --locked, --target-dir, "{{build_dir}}/target"]
    outputs:
      - from: target/release/orbit-graph
        to: bin/orbit-graph
    timeout_ms: 1200000
"#;

/// The design's own example parses and validates, and a key the schema does
/// not know is refused rather than ignored: a typo'd `comand` must not leave
/// a manifest that silently builds nothing.
#[test]
fn spec_build_parses_and_refuses_unknown_fields() {
    let manifest: PluginManifest = serde_yaml::from_str(BUILD_MANIFEST).expect("parse");
    manifest
        .validate_structure()
        .expect("the design example is valid");
    let build = manifest.spec.build.expect("spec.build is read");
    assert_eq!(
        crate::plugin::render_build_argv(&build.command, "/b")[6],
        "/b/target"
    );

    let typo = BUILD_MANIFEST.replace("    timeout_ms: 1200000", "    shell: true");
    let error = serde_yaml::from_str::<PluginManifest>(&typo).unwrap_err();
    assert!(
        error.to_string().contains("shell"),
        "an unknown spec.build key is refused by name: {error}"
    );
}

/// Whitespace-padded `{{ build_dir }}` in a build argv passes structural
/// validation and is substituted by [`crate::plugin::render_build_argv`].
#[test]
fn spec_build_substitutes_padded_build_dir() {
    let manifest_yaml = BUILD_MANIFEST.replace(
        "\"{{build_dir}}/target\"]",
        "\"{{ build_dir }}/target\", \"--extra={{  build_dir  }}\"]",
    );
    let manifest: PluginManifest =
        serde_yaml::from_str(&manifest_yaml).expect("parse padded build_dir");
    manifest
        .validate_structure()
        .expect("padded build_dir reference is valid");
    let build = manifest.spec.build.expect("spec.build is read");
    let rendered = crate::plugin::render_build_argv(&build.command, "/b");
    assert_eq!(rendered[6], "/b/target");
    assert_eq!(rendered[7], "--extra=/b");
}

/// Each structural refusal names the offending key, so `orbit plugin
/// validate` points at what to fix.
#[test]
fn spec_build_refusals_name_their_field() {
    let cases: &[(&str, &str, &str)] = &[
        ("command: [cargo, build", "command: [", "spec.build.command"),
        (
            "\"{{build_dir}}/target\"]",
            "\"{{workspace}}/target\"]",
            "spec.build.command[6]",
        ),
        (
            "fetch: [cargo, fetch, --locked]",
            "fetch: [\"{{build_dir}}/cargo\"]",
            "spec.build.fetch[0]",
        ),
        (
            "to: bin/orbit-graph",
            "to: plugin.yaml",
            "spec.build.outputs[0].to",
        ),
        (
            "to: bin/orbit-graph",
            "to: ../bin/orbit",
            "spec.build.outputs[0].to",
        ),
        (
            "from: target/release/orbit-graph",
            "from: /etc/passwd",
            "spec.build.outputs[0].from",
        ),
        (
            "timeout_ms: 1200000",
            "timeout_ms: 3600001",
            "spec.build.timeout_ms",
        ),
        (
            "programs: [cargo]",
            "programs: [bin/cargo]",
            "spec.build.programs[0]",
        ),
        (
            "fetch: [cargo, fetch, --locked]",
            "fetch: [curl, https://example.test/payload]",
            "spec.build.fetch[0]",
        ),
    ];
    for (from, to, field) in cases {
        let yaml = BUILD_MANIFEST.replacen(from, to, 1);
        // An empty command list leaves the rest of the original line behind.
        let yaml = yaml.replace(
            "command: [, --release, --offline, --locked, --target-dir, \"{{build_dir}}/target\"]",
            "command: []",
        );
        let manifest: PluginManifest =
            serde_yaml::from_str(&yaml).unwrap_or_else(|error| panic!("{field}: {error}"));
        let error = manifest
            .validate_structure()
            .expect_err(&format!("{field} must be refused"));
        assert_eq!(&error.field, field, "{}", error.message);
    }
}

/// The artifact digest preimage is the same whatever order the outputs were
/// declared or copied in, so two hosts building one commit reproducibly
/// record one value (§3.6).
#[test]
fn artifact_digest_preimage_is_order_independent() {
    use crate::plugin::{PluginBuildOutputRecord, artifact_digest_preimage};
    let a = PluginBuildOutputRecord {
        to: "bin/a".into(),
        mode: 0o755,
        sha256: "aa".into(),
    };
    let b = PluginBuildOutputRecord {
        to: "lib/b".into(),
        mode: 0o644,
        sha256: "bb".into(),
    };
    let forward = artifact_digest_preimage(&[a.clone(), b.clone()]);
    assert_eq!(forward, artifact_digest_preimage(&[b, a]));
    assert_eq!(
        forward,
        "orbit.plugin.build.v1\nbin/a\u{0}755\u{0}aa\nlib/b\u{0}644\u{0}bb\n"
    );
}
