//! `orbit plugin migrate`: fold a set of v1 `*.orbit-tool.yaml` sidecars for
//! one executable into a v2 `plugin.yaml` (§4.8).

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::plugin::{
    MANIFEST_KIND, MANIFEST_SCHEMA_VERSION, ORBIT_NAMESPACE_PREFIX, PluginBackend,
    PluginBackendType, PluginExecutionKind, PluginManifest, PluginMcpScope, PluginMetadata,
    PluginPermissions, PluginRequires, PluginSandbox, PluginSpec, PluginToolSpec,
    is_valid_namespace, is_valid_verb,
};
use orbit_types::tool::ToolParam;
use serde::Deserialize;

use super::schema::input_schema_from_params;

/// The v1 sidecar contract, exactly as `orbit tool add` reads it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SidecarManifest {
    #[serde(rename = "schemaVersion", default = "default_schema_version")]
    pub schema_version: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: Vec<ToolParam>,
}

fn default_schema_version() -> u32 {
    1
}

pub fn load_sidecar_manifest(path: &Path) -> Result<SidecarManifest, OrbitError> {
    let raw = std::fs::read_to_string(path).map_err(|error| {
        OrbitError::InvalidInput(format!("cannot read {}: {error}", path.display()))
    })?;
    let manifest: SidecarManifest = match path.extension().and_then(|value| value.to_str()) {
        Some("json") => serde_json::from_str(&raw).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "invalid sidecar JSON '{}': {error}",
                path.display()
            ))
        })?,
        _ => serde_yaml::from_str(&raw).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "invalid sidecar YAML '{}': {error}",
                path.display()
            ))
        })?,
    };
    if manifest.schema_version != 1 {
        return Err(OrbitError::InvalidInput(format!(
            "'{}' has schemaVersion {}; `orbit plugin migrate` reads v1 sidecars only",
            path.display(),
            manifest.schema_version
        )));
    }
    if manifest.name.trim().is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "'{}' must define a non-empty name",
            path.display()
        )));
    }
    Ok(manifest)
}

/// Build a v2 manifest from v1 sidecars sharing one namespace.
/// `orbit.graph.recommend` becomes namespace `graph` and verb `recommend`, but
/// migration never copies the unverified `orbit.` claim: the generated tool is
/// `graph.recommend`. Operators may add `origin: orbit` only when the plugin
/// satisfies the first-party source rule.
pub fn migrate_sidecars(
    sidecars: &[SidecarManifest],
    backend_command: &str,
    version: &str,
    namespace_override: Option<&str>,
) -> Result<PluginManifest, OrbitError> {
    if sidecars.is_empty() {
        return Err(OrbitError::InvalidInput(
            "no v1 sidecars to migrate; pass --sidecar or --sidecar-dir".to_string(),
        ));
    }
    let mut namespace: Option<String> = None;
    let mut tools = Vec::with_capacity(sidecars.len());
    for sidecar in sidecars {
        let (ns, first_party, verb) = split_tool_name(&sidecar.name, namespace_override)?;
        match &namespace {
            None => namespace = Some(ns.clone()),
            Some(seen) if *seen != ns => {
                return Err(OrbitError::InvalidInput(format!(
                    "sidecars span more than one namespace ('{}' and '{}'); one plugin owns one \
                     namespace, so migrate each set separately",
                    seen,
                    display_namespace(&ns, first_party)
                )));
            }
            Some(_) => {}
        }
        if tools.iter().any(|tool: &PluginToolSpec| tool.name == verb) {
            return Err(OrbitError::InvalidInput(format!(
                "tool '{}' appears in more than one sidecar",
                sidecar.name
            )));
        }
        tools.push(PluginToolSpec {
            name: verb,
            description: sidecar.description.clone(),
            // A v1 sidecar never said; the conservative kind keeps the tool
            // operator-gated until the author reviews the manifest.
            execution_kind: PluginExecutionKind::Mutating,
            mcp_scope: PluginMcpScope::Workspace,
            input_schema: Some(input_schema_from_params(&sidecar.name, &sidecar.parameters)),
            output_schema: None,
            cli: None,
        });
    }
    let namespace = namespace.unwrap_or_default();
    Ok(PluginManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        kind: MANIFEST_KIND.to_string(),
        metadata: PluginMetadata {
            name: namespace,
            version: version.to_string(),
            description: String::new(),
            publisher: None,
            origin: None,
            homepage: None,
        },
        spec: PluginSpec {
            requires: PluginRequires {
                orbit: Some(format!(">={}", env!("CARGO_PKG_VERSION"))),
                host_api: Some(orbit_types::plugin::PLUGIN_HOST_API),
                platforms: Vec::new(),
                programs: Vec::new(),
            },
            backend: PluginBackend {
                backend_type: PluginBackendType::Exec,
                command: backend_command.to_string(),
                args: Vec::new(),
                timeout_ms: None,
                sandbox: PluginSandbox::Default,
            },
            permissions: PluginPermissions::default(),
            tools,
            definitions: None,
            skills: Vec::new(),
            config: None,
            web: None,
            tests: Vec::new(),
        },
    })
}

fn display_namespace(namespace: &str, first_party: bool) -> String {
    if first_party {
        format!("{ORBIT_NAMESPACE_PREFIX}.{namespace}")
    } else {
        namespace.to_string()
    }
}

/// `orbit.<ns>.<verb>` → (`ns`, true, `verb`); `<ns>.<verb>` → (`ns`, false,
/// `verb`). An override names the namespace and takes the remainder as verb.
fn split_tool_name(
    name: &str,
    namespace_override: Option<&str>,
) -> Result<(String, bool, String), OrbitError> {
    let invalid = |detail: String| {
        OrbitError::InvalidInput(format!(
            "cannot derive a plugin namespace from tool '{name}': {detail}"
        ))
    };
    let segments: Vec<&str> = name.split('.').collect();
    let (namespace, first_party, verb) = match namespace_override {
        Some(namespace) => {
            let first_party = segments.first() == Some(&ORBIT_NAMESPACE_PREFIX);
            let expected_prefix = display_namespace(namespace, first_party);
            let verb = name
                .strip_prefix(&format!("{expected_prefix}."))
                .ok_or_else(|| invalid(format!("it does not start with '{expected_prefix}.'")))?;
            (namespace.to_string(), first_party, verb.to_string())
        }
        None => match segments.as_slice() {
            [prefix, namespace, verb] if *prefix == ORBIT_NAMESPACE_PREFIX => {
                ((*namespace).to_string(), true, (*verb).to_string())
            }
            [namespace, verb] => ((*namespace).to_string(), false, (*verb).to_string()),
            _ => {
                return Err(invalid(
                    "expected `<ns>.<verb>` or `orbit.<ns>.<verb>`; pass --name to choose the namespace"
                        .to_string(),
                ));
            }
        },
    };
    if !is_valid_namespace(&namespace) {
        return Err(invalid(format!("'{namespace}' is not a valid namespace")));
    }
    if !is_valid_verb(&verb) {
        return Err(invalid(format!(
            "'{verb}' is not a single valid verb segment; pass --name to choose the namespace"
        )));
    }
    Ok((namespace, first_party, verb))
}
