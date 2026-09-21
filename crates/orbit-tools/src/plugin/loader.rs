//! Read and validate one plugin directory.
//!
//! Fail closed per plugin (§4.9): every problem here is reported as a
//! [`PluginLoadError`] naming the manifest field, and the caller decides
//! whether that refuses `orbit plugin add` or registers the plugin inactive.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::security::child_env::allowlisted_child_env;
use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};
use orbit_types::plugin::{
    FIRST_PARTY_PUBLISHER, MANIFEST_FILE_NAME, PluginExecutionKind, PluginManifest,
    PluginManifestError, PluginMcpScope, RESERVED_CLI_COMMANDS, namespace_collides_with_tool,
    plugin_tool_name,
};
use orbit_types::tool::ToolParam;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use super::schema::params_from_input_schema;
use crate::{TIMEOUT_FAST_MS, ToolRegistry};

/// Manifest digests of first-party plugins that may claim `orbit.<ns>.*`
/// without a constellation-works source. Empty until a release bundles one.
pub const FIRST_PARTY_MANIFEST_DIGESTS: &[&str] = &[];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PluginLoadError {
    #[error("{0}")]
    Manifest(#[from] PluginManifestError),
    #[error("{0}")]
    Io(String),
}

impl From<PluginLoadError> for OrbitError {
    fn from(error: PluginLoadError) -> Self {
        OrbitError::InvalidInput(error.to_string())
    }
}

/// A manifest rejection is invalid input at every surface that surfaces it.
pub fn manifest_refusal(error: PluginManifestError) -> OrbitError {
    OrbitError::InvalidInput(error.to_string())
}

/// One `spec.tools[]` entry with its schemas resolved and flattened.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPluginTool {
    /// The manifest verb.
    pub verb: String,
    pub description: String,
    pub execution_kind: PluginExecutionKind,
    pub mcp_scope: PluginMcpScope,
    pub input_schema: Value,
    /// Whether the manifest declared `input_schema` (as opposed to the
    /// empty-object default). An `mcp` backend's server is held to a
    /// declared schema only.
    pub input_schema_declared: bool,
    pub output_schema: Option<Value>,
    pub parameters: Vec<ToolParam>,
}

/// The definition files one plugin ships, resolved to absolute paths inside
/// the plugin root (§4.5). Activities and jobs become the `plugin:<ns>`
/// catalog layer; routines and auto-tasks are seeded on enable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginDefinitionFiles {
    pub activities: Vec<PathBuf>,
    pub jobs: Vec<PathBuf>,
    pub routines: Vec<PathBuf>,
    pub auto_tasks: Vec<PathBuf>,
}

impl PluginDefinitionFiles {
    pub fn is_empty(&self) -> bool {
        self.activities.is_empty()
            && self.jobs.is_empty()
            && self.routines.is_empty()
            && self.auto_tasks.is_empty()
    }
}

/// A plugin directory whose manifest parsed, whose `$ref`s resolved inside
/// the root, and whose backend command exists.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedPlugin {
    pub root: PathBuf,
    pub manifest: PluginManifest,
    pub manifest_digest: String,
    pub backend_command: PathBuf,
    pub tools: Vec<ResolvedPluginTool>,
    /// `spec.definitions.*` resolved to files inside the root.
    pub definitions: PluginDefinitionFiles,
    /// `spec.skills[]` resolved to directories holding a `SKILL.md`.
    pub skills: Vec<PathBuf>,
    /// `spec.config.schema` resolved and read, when the manifest declares one.
    pub config_schema: Option<Value>,
    /// `spec.config.defaults`, flattened to one entry per declared key.
    pub config_defaults: BTreeMap<String, Value>,
}

impl LoadedPlugin {
    pub fn namespace(&self) -> &str {
        &self.manifest.metadata.name
    }

    /// Canonical registry name of one of this plugin's tools.
    pub fn tool_name(&self, verb: &str, first_party: bool) -> String {
        plugin_tool_name(self.namespace(), verb, first_party)
    }
}

/// SHA-256 of the manifest bytes, hex encoded.
pub fn manifest_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Walk `root` without following links and refuse any symbolic link, naming
/// the relative entry.
///
/// `std::fs::copy` follows a link and writes the *target's* bytes as a
/// regular file. The install root is always readable to the plugin backend,
/// so a link to `/proc/self/environ` (or any other readable path) would leak
/// into a location the sandbox is supposed to hide.
pub fn refuse_plugin_tree_symlinks(root: &Path) -> Result<(), PluginLoadError> {
    refuse_plugin_tree_symlinks_in(root, root)
}

fn refuse_plugin_tree_symlinks_in(root: &Path, dir: &Path) -> Result<(), PluginLoadError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| PluginLoadError::Io(format!("read {}: {error}", dir.display())))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| PluginLoadError::Io(format!("read {}: {error}", dir.display())))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| PluginLoadError::Io(format!("stat {}: {error}", path.display())))?;
        if file_type.is_symlink() {
            let target = std::fs::read_link(&path).ok();
            return Err(PluginLoadError::Io(plugin_symlink_refusal(
                path.strip_prefix(root).unwrap_or(&path),
                target.as_deref(),
            )));
        }
        if file_type.is_dir() {
            refuse_plugin_tree_symlinks_in(root, &path)?;
        }
    }
    Ok(())
}

/// Diagnostic that names the offending plugin-tree entry.
pub fn plugin_symlink_refusal(entry: &Path, target: Option<&Path>) -> String {
    match target {
        Some(target) => format!(
            "refusing '{}': it is a symbolic link to '{}'; plugin trees cannot contain \
             symbolic links because they would be copied as regular files into the install root",
            entry.display(),
            target.display()
        ),
        None => format!(
            "refusing '{}': it is a symbolic link; plugin trees cannot contain symbolic \
             links because they would be copied as regular files into the install root",
            entry.display()
        ),
    }
}

/// Read `<root>/plugin.yaml`, validate its structure, resolve schema `$ref`s
/// inside the root, and locate the backend command.
pub fn load_plugin_dir(root: &Path) -> Result<LoadedPlugin, PluginLoadError> {
    let root = std::fs::canonicalize(root).map_err(|error| {
        PluginLoadError::Io(format!("plugin root '{}': {error}", root.display()))
    })?;
    refuse_plugin_tree_symlinks(&root)?;
    let manifest_path = root.join(MANIFEST_FILE_NAME);
    let bytes = std::fs::read(&manifest_path).map_err(|error| {
        PluginLoadError::Io(format!("cannot read {}: {error}", manifest_path.display()))
    })?;
    let manifest: PluginManifest = serde_yaml::from_slice(&bytes).map_err(|error| {
        PluginManifestError::new(
            manifest_field_from_yaml_error(&error),
            format!("invalid {MANIFEST_FILE_NAME}: {error}"),
        )
    })?;
    manifest.validate_structure()?;
    let manifest_digest = manifest_digest(&bytes);

    let backend_command = resolve_backend_command(&root, &manifest.spec.backend.command)?;

    let mut tools = Vec::with_capacity(manifest.spec.tools.len());
    for (index, tool) in manifest.spec.tools.iter().enumerate() {
        let input_schema = match &tool.input_schema {
            Some(schema) => {
                resolve_schema(&root, schema, &format!("spec.tools[{index}].input_schema"))?
            }
            None => serde_json::json!({ "type": "object", "properties": {} }),
        };
        let output_schema = tool
            .output_schema
            .as_ref()
            .map(|schema| {
                resolve_schema(&root, schema, &format!("spec.tools[{index}].output_schema"))
            })
            .transpose()?;
        tools.push(ResolvedPluginTool {
            verb: tool.name.clone(),
            description: tool.description.clone(),
            execution_kind: tool.execution_kind,
            mcp_scope: tool.mcp_scope,
            parameters: params_from_input_schema(&input_schema),
            input_schema,
            input_schema_declared: tool.input_schema.is_some(),
            output_schema,
        });
    }

    let definitions = resolve_definitions(&root, &manifest)?;
    let skills = resolve_skills(&root, &manifest)?;
    let (config_schema, config_defaults) = resolve_config_section(&root, &manifest)?;

    Ok(LoadedPlugin {
        root,
        manifest,
        manifest_digest,
        backend_command,
        tools,
        definitions,
        skills,
        config_schema,
        config_defaults,
    })
}

/// Resolve every `spec.definitions` pattern against the plugin root.
fn resolve_definitions(
    root: &Path,
    manifest: &PluginManifest,
) -> Result<PluginDefinitionFiles, PluginLoadError> {
    let Some(declared) = manifest.spec.definitions.as_ref() else {
        return Ok(PluginDefinitionFiles::default());
    };
    Ok(PluginDefinitionFiles {
        activities: resolve_patterns(root, &declared.activities, "spec.definitions.activities")?,
        jobs: resolve_patterns(root, &declared.jobs, "spec.definitions.jobs")?,
        routines: resolve_patterns(root, &declared.routines, "spec.definitions.routines")?,
        auto_tasks: resolve_patterns(root, &declared.auto_tasks, "spec.definitions.auto_tasks")?,
    })
}

/// Expand one list of manifest patterns into files inside the plugin root.
///
/// A pattern is either a literal path — which must exist — or a directory
/// plus a `*` wildcard in its final component. A wildcard whose directory is
/// absent matches nothing: a manifest may declare the conventional layout for
/// a kind it ships none of.
fn resolve_patterns(
    root: &Path,
    patterns: &[String],
    field: &str,
) -> Result<Vec<PathBuf>, PluginLoadError> {
    let mut resolved = Vec::new();
    for (index, pattern) in patterns.iter().enumerate() {
        let field = format!("{field}[{index}]");
        let pattern = pattern.trim();
        let (directory, file_pattern) = match pattern.rsplit_once('/') {
            Some((directory, file)) => (root.join(directory), file.to_string()),
            None => (root.to_path_buf(), pattern.to_string()),
        };
        if !file_pattern.contains('*') {
            let path = contained_path(root, &directory.join(&file_pattern), &field)?;
            if !path.is_file() {
                return Err(PluginManifestError::new(
                    field,
                    format!("'{pattern}' does not name a file inside the plugin root"),
                )
                .into());
            }
            resolved.push(path);
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut matched = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !wildcard_matches(&file_pattern, name) {
                continue;
            }
            let path = contained_path(root, &entry.path(), &field)?;
            if path.is_file() {
                matched.push(path);
            }
        }
        matched.sort();
        resolved.extend(matched);
    }
    resolved.dedup();
    Ok(resolved)
}

/// Match one `*`-wildcard file pattern against a file name. `*` matches any
/// run of characters, including none; every other character is literal.
fn wildcard_matches(pattern: &str, name: &str) -> bool {
    let mut segments = pattern.split('*');
    let Some(first) = segments.next() else {
        return false;
    };
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let segments: Vec<&str> = segments.collect();
    let Some((last, middle)) = segments.split_last() else {
        return rest.is_empty();
    };
    for segment in middle {
        match rest.find(segment) {
            Some(at) => rest = &rest[at + segment.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// Canonicalise `candidate` and refuse anything outside the plugin root.
fn contained_path(root: &Path, candidate: &Path, field: &str) -> Result<PathBuf, PluginLoadError> {
    let resolved = std::fs::canonicalize(candidate).map_err(|error| {
        PluginManifestError::new(
            field,
            format!("'{}' cannot be read: {error}", candidate.display()),
        )
    })?;
    if !resolved.starts_with(root) {
        return Err(PluginManifestError::new(
            field,
            format!(
                "'{}' escapes the plugin root {}",
                resolved.display(),
                root.display()
            ),
        )
        .into());
    }
    Ok(resolved)
}

/// Resolve `spec.skills[]`: each entry is a directory holding a `SKILL.md`.
fn resolve_skills(root: &Path, manifest: &PluginManifest) -> Result<Vec<PathBuf>, PluginLoadError> {
    let mut skills = Vec::with_capacity(manifest.spec.skills.len());
    for (index, declared) in manifest.spec.skills.iter().enumerate() {
        let field = format!("spec.skills[{index}]");
        let path = contained_path(root, &root.join(declared.trim()), &field)?;
        if !path.join("SKILL.md").is_file() {
            return Err(PluginManifestError::new(
                field,
                format!("'{declared}' must be a directory containing SKILL.md"),
            )
            .into());
        }
        skills.push(path);
    }
    Ok(skills)
}

/// Read `spec.config.schema` and flatten `spec.config.defaults`.
///
/// A schema that does not compile, or defaults the schema itself rejects, is
/// a manifest error: the plugin would otherwise install a `[plugins.<ns>]`
/// section no value can satisfy (§4.9).
fn resolve_config_section(
    root: &Path,
    manifest: &PluginManifest,
) -> Result<(Option<Value>, BTreeMap<String, Value>), PluginLoadError> {
    let Some(config) = manifest.spec.config.as_ref() else {
        return Ok((None, BTreeMap::new()));
    };
    let schema = match config.schema.as_deref() {
        Some(relative) => {
            let path = contained_path(root, &root.join(relative.trim()), "spec.config.schema")?;
            let bytes = std::fs::read(&path).map_err(|error| {
                PluginManifestError::new(
                    "spec.config.schema",
                    format!("'{relative}' cannot be read: {error}"),
                )
            })?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
                PluginManifestError::new(
                    "spec.config.schema",
                    format!("'{relative}' is not valid JSON: {error}"),
                )
            })?;
            if !value.is_object() {
                return Err(PluginManifestError::new(
                    "spec.config.schema",
                    format!("'{relative}' must contain a JSON Schema object"),
                )
                .into());
            }
            Some(value)
        }
        None => None,
    };
    let defaults: BTreeMap<String, Value> = config
        .defaults
        .as_ref()
        .and_then(|defaults| defaults.as_object())
        .map(|defaults| {
            defaults
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    if let Some(schema) = &schema {
        let compiled = jsonschema::JSONSchema::compile(schema).map_err(|error| {
            PluginManifestError::new(
                "spec.config.schema",
                format!("is not a compilable JSON Schema: {error}"),
            )
        })?;
        let rendered = Value::Object(
            defaults
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );
        if let Err(errors) = compiled.validate(&rendered) {
            let details = errors.map(|error| error.to_string()).collect::<Vec<_>>();
            return Err(PluginManifestError::new(
                "spec.config.defaults",
                format!("are rejected by spec.config.schema: {}", details.join("; ")),
            )
            .into());
        }
    }
    Ok((schema, defaults))
}

/// serde_yaml names an unknown key in its message; surface the closest
/// dotted path it reports so the diagnostic points at a field.
fn manifest_field_from_yaml_error(error: &serde_yaml::Error) -> String {
    let message = error.to_string();
    if let Some(rest) = message.split("unknown field `").nth(1)
        && let Some((field, _)) = rest.split_once('`')
    {
        return field.to_string();
    }
    if let Some(rest) = message.split("missing field `").nth(1)
        && let Some((field, _)) = rest.split_once('`')
    {
        return field.to_string();
    }
    "manifest".to_string()
}

fn resolve_backend_command(root: &Path, command: &str) -> Result<PathBuf, PluginLoadError> {
    let raw = Path::new(command);
    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        root.join(raw)
    };
    let resolved = std::fs::canonicalize(&candidate).map_err(|error| {
        PluginManifestError::new(
            "spec.backend.command",
            format!("'{}' is not present: {error}", candidate.display()),
        )
    })?;
    if !raw.is_absolute() && !resolved.starts_with(root) {
        return Err(PluginManifestError::new(
            "spec.backend.command",
            format!(
                "'{command}' resolves to {} outside the plugin root {}",
                resolved.display(),
                root.display()
            ),
        )
        .into());
    }
    Ok(resolved)
}

/// Resolve a top-level `{ $ref: <path> }` against the plugin root, refusing
/// any target that leaves it. An inline schema is returned as-is.
fn resolve_schema(root: &Path, schema: &Value, field: &str) -> Result<Value, PluginLoadError> {
    let Some(reference) = schema.get("$ref") else {
        return Ok(schema.clone());
    };
    let Some(reference) = reference.as_str() else {
        return Err(
            PluginManifestError::new(format!("{field}.$ref"), "must be a string path").into(),
        );
    };
    let raw = Path::new(reference);
    if raw.is_absolute() {
        return Err(PluginManifestError::new(
            format!("{field}.$ref"),
            format!("'{reference}' is absolute; a $ref must stay inside the plugin root"),
        )
        .into());
    }
    let candidate = root.join(raw);
    let resolved = std::fs::canonicalize(&candidate).map_err(|error| {
        PluginManifestError::new(
            format!("{field}.$ref"),
            format!("'{reference}' cannot be read: {error}"),
        )
    })?;
    if !resolved.starts_with(root) {
        return Err(PluginManifestError::new(
            format!("{field}.$ref"),
            format!(
                "'{reference}' escapes the plugin root {} (resolves to {})",
                root.display(),
                resolved.display()
            ),
        )
        .into());
    }
    let bytes = std::fs::read(&resolved).map_err(|error| {
        PluginManifestError::new(
            format!("{field}.$ref"),
            format!("'{reference}' cannot be read: {error}"),
        )
    })?;
    let value: Value = match resolved.extension().and_then(|value| value.to_str()) {
        Some("yaml" | "yml") => serde_yaml::from_slice(&bytes).map_err(|error| {
            PluginManifestError::new(
                format!("{field}.$ref"),
                format!("'{reference}' is not valid YAML: {error}"),
            )
        })?,
        _ => serde_json::from_slice(&bytes).map_err(|error| {
            PluginManifestError::new(
                format!("{field}.$ref"),
                format!("'{reference}' is not valid JSON: {error}"),
            )
        })?,
    };
    if !value.is_object() {
        return Err(PluginManifestError::new(
            format!("{field}.$ref"),
            format!("'{reference}' must contain a JSON Schema object"),
        )
        .into());
    }
    Ok(value)
}

/// What the host knows when it decides whether a loaded plugin may register.
#[derive(Debug, Clone)]
pub struct PluginValidationPolicy {
    /// The source resolved to a constellation-works repository, or the
    /// manifest digest is bundled as first-party.
    pub first_party_verified: bool,
    /// Every built-in tool name, from a registry with builtins registered.
    pub builtin_tools: Vec<String>,
    pub reserved_cli_commands: &'static [&'static str],
}

impl PluginValidationPolicy {
    /// The policy for a host that has not verified the plugin's origin.
    pub fn host_default() -> Self {
        let mut registry = ToolRegistry::new();
        registry.register_builtins();
        Self {
            first_party_verified: false,
            builtin_tools: registry
                .all_schemas()
                .into_iter()
                .map(|schema| schema.name)
                .collect(),
            reserved_cli_commands: RESERVED_CLI_COMMANDS,
        }
    }

    pub fn with_first_party_verified(mut self, verified: bool) -> Self {
        self.first_party_verified = verified;
        self
    }
}

/// Apply the namespace rules (§1): first-party claims need verification,
/// and a namespace may not shadow a built-in tool or a CLI command.
pub fn validate_loaded_plugin(
    plugin: &LoadedPlugin,
    policy: &PluginValidationPolicy,
) -> Result<(), PluginManifestError> {
    let namespace = plugin.namespace();
    if plugin.manifest.claims_first_party_namespace() {
        if plugin.manifest.metadata.publisher.as_deref() != Some(FIRST_PARTY_PUBLISHER) {
            return Err(PluginManifestError::new(
                "metadata.publisher",
                format!(
                    "`origin: orbit` requires `publisher: {FIRST_PARTY_PUBLISHER}`; this manifest \
                     declares '{}'",
                    plugin.manifest.metadata.publisher.as_deref().unwrap_or("")
                ),
            ));
        }
        if !policy.first_party_verified {
            return Err(PluginManifestError::new(
                "metadata.origin",
                format!(
                    "`origin: orbit` claims the reserved `orbit.{namespace}.*` namespace, but the \
                     plugin source is not a constellation-works repository and its manifest digest \
                     is not in the bundled first-party list; remove `origin` to register as \
                     `{namespace}.*`"
                ),
            ));
        }
    }
    let first_party = plugin.manifest.claims_first_party_namespace();
    if policy.reserved_cli_commands.contains(&namespace) {
        return Err(PluginManifestError::new(
            "metadata.name",
            format!(
                "'{namespace}' is a built-in `orbit {namespace}` command and cannot be a plugin namespace"
            ),
        ));
    }
    if let Some(builtin) = policy
        .builtin_tools
        .iter()
        .find(|builtin| namespace_collides_with_tool(namespace, first_party, builtin))
    {
        return Err(PluginManifestError::new(
            "metadata.name",
            format!(
                "namespace '{}' collides with the built-in tool '{builtin}'",
                if first_party {
                    format!("orbit.{namespace}")
                } else {
                    namespace.to_string()
                }
            ),
        ));
    }
    for (index, tool) in plugin.tools.iter().enumerate() {
        let name = plugin.tool_name(&tool.verb, first_party);
        if policy.builtin_tools.contains(&name) {
            return Err(PluginManifestError::new(
                format!("spec.tools[{index}].name"),
                format!("'{name}' is a built-in tool"),
            ));
        }
    }
    Ok(())
}

/// Whether `source` (the `orbit plugin add` argument) resolves to a
/// constellation-works repository: a `git+` URL under that organisation, or a
/// local checkout whose `origin` remote is.
pub fn first_party_source(source: &str, root: &Path) -> bool {
    if let Some(url) = source.strip_prefix("git+") {
        return is_first_party_remote(url);
    }
    let output = run_process(
        &ExecRequest {
            program: "git".to_string(),
            args: vec![
                "-C".to_string(),
                root.to_string_lossy().into_owned(),
                "remote".to_string(),
                "get-url".to_string(),
                "origin".to_string(),
            ],
            current_dir: None,
            timeout_ms: Some(TIMEOUT_FAST_MS),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(allowlisted_child_env(&[], &[])),
            debug: false,
        },
        &NoSandbox,
    );
    match output {
        Ok(result) if result.success => is_first_party_remote(result.stdout.trim()),
        _ => false,
    }
}

fn is_first_party_remote(url: &str) -> bool {
    let url = url.trim().trim_end_matches(".git");
    url.contains(&format!("github.com/{FIRST_PARTY_PUBLISHER}/"))
        || url.contains(&format!("github.com:{FIRST_PARTY_PUBLISHER}/"))
}
