use std::fs;
use std::path::{Path, PathBuf};

use orbit_core::OrbitError;
use orbit_types::tool::ToolParam;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ExternalToolManifest {
    #[serde(rename = "schemaVersion", default = "default_manifest_schema_version")]
    pub(super) schema_version: u32,
    pub(super) name: String,
    #[serde(default)]
    pub(super) description: String,
    #[serde(default)]
    pub(super) parameters: Vec<ToolParam>,
}

fn default_manifest_schema_version() -> u32 {
    1
}

pub(super) fn resolve_manifest_path(tool_path: &Path, explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(PathBuf::from(path));
    }

    manifest_candidates(tool_path)
        .into_iter()
        .find(|candidate| candidate.exists())
}

fn manifest_candidates(tool_path: &Path) -> Vec<PathBuf> {
    vec![
        sidecar_manifest_path_with_extension(tool_path, "yaml"),
        sidecar_manifest_path_with_extension(tool_path, "yml"),
        sidecar_manifest_path_with_extension(tool_path, "json"),
    ]
}

const KNOWN_SCRIPT_EXTENSIONS: &[&str] =
    &["sh", "bash", "zsh", "py", "js", "mjs", "cjs", "ts", "rb"];

pub(super) fn tool_file_stem(tool_path: &Path) -> &str {
    let file_name = tool_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("external-tool");
    if let Some((stem, ext)) = file_name.rsplit_once('.')
        && KNOWN_SCRIPT_EXTENSIONS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(ext))
    {
        return stem;
    }
    file_name
}

pub(super) fn sidecar_manifest_path(tool_path: &Path) -> PathBuf {
    sidecar_manifest_path_with_extension(tool_path, "yaml")
}

fn sidecar_manifest_path_with_extension(tool_path: &Path, extension: &str) -> PathBuf {
    let parent = tool_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = tool_file_stem(tool_path);
    parent.join(format!("{stem}.orbit-tool.{extension}"))
}

pub(super) fn infer_tool_name(path: &Path) -> String {
    tool_file_stem(path).to_string()
}

pub(super) fn load_external_tool_manifest(path: &Path) -> Result<ExternalToolManifest, OrbitError> {
    let raw = fs::read_to_string(path).map_err(|error| {
        OrbitError::InvalidInput(format!("cannot read {}: {error}", path.display()))
    })?;
    let manifest = match path.extension().and_then(|value| value.to_str()) {
        Some("json") => serde_json::from_str::<ExternalToolManifest>(&raw).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "invalid manifest JSON '{}': {error}",
                path.display()
            ))
        })?,
        _ => serde_yaml::from_str::<ExternalToolManifest>(&raw).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "invalid manifest YAML '{}': {error}",
                path.display()
            ))
        })?,
    };
    validate_manifest(path, manifest)
}

fn validate_manifest(
    path: &Path,
    manifest: ExternalToolManifest,
) -> Result<ExternalToolManifest, OrbitError> {
    if manifest.schema_version != 1 {
        return Err(OrbitError::InvalidInput(format!(
            "unsupported plugin manifest schemaVersion {} in '{}'",
            manifest.schema_version,
            path.display()
        )));
    }
    if manifest.name.trim().is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "plugin manifest '{}' must define a non-empty name",
            path.display()
        )));
    }
    if manifest
        .parameters
        .iter()
        .any(|parameter| parameter.name.trim().is_empty())
    {
        return Err(OrbitError::InvalidInput(format!(
            "plugin manifest '{}' contains a parameter with an empty name",
            path.display()
        )));
    }
    Ok(manifest)
}
