use std::path::Path;

use clap::Args;
use orbit_core::{OrbitError, OrbitRuntime};

use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

use super::manifest::{infer_tool_name, load_external_tool_manifest, resolve_manifest_path};

#[derive(Args)]
pub struct ToolAddArgs {
    /// Path to the external tool executable
    pub path: String,
    /// Tool name (overrides the manifest or filename-derived default)
    #[arg(long)]
    pub name: Option<String>,
    /// Tool description (overrides the manifest description)
    #[arg(long, default_value = "")]
    pub description: String,
    /// Path to a sidecar plugin manifest (`*.orbit-tool.yaml`, `*.yml`, or `*.json`)
    #[arg(long)]
    pub manifest: Option<String>,
}

impl Execute for ToolAddArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let manifest_path = resolve_manifest_path(Path::new(&self.path), self.manifest.as_deref());
        let manifest = manifest_path
            .as_deref()
            .map(load_external_tool_manifest)
            .transpose()?;

        let name = self
            .name
            .or_else(|| manifest.as_ref().map(|entry| entry.name.clone()))
            .unwrap_or_else(|| infer_tool_name(Path::new(&self.path)));
        if name.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "tool name must not be empty".to_string(),
            ));
        }

        let description = if self.description.trim().is_empty() {
            manifest
                .as_ref()
                .map(|entry| entry.description.clone())
                .unwrap_or_default()
        } else {
            self.description.trim().to_string()
        };
        let parameters = manifest.map(|entry| entry.parameters).unwrap_or_default();

        runtime.add_tool(&name, &self.path, &description, parameters)?;
        let mut text = format!("Added tool '{name}' from {}", self.path);
        if let Some(path) = manifest_path.as_deref() {
            text.push_str(&format!("\nLoaded plugin manifest from {}", path.display()));
        }
        Ok(Payload::detail(
            json!({
                "tool": name,
                "path": self.path,
                "manifest": manifest_path.as_deref().map(|path| path.display().to_string()),
            }),
            text,
        )
        .into())
    }
}
