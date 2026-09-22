use std::path::PathBuf;

use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::{PluginMigrateRequest, migrate_plugin_sidecars};
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct PluginMigrateArgs {
    /// The executable the v1 sidecars belong to
    pub binary: String,
    /// Sidecar files to fold in (default: every `*.orbit-tool.yaml` beside the binary)
    #[arg(long = "sidecar")]
    pub sidecars: Vec<PathBuf>,
    /// Version for the generated manifest
    #[arg(long, default_value = "0.1.0")]
    pub version: String,
    /// Namespace to use when the v1 tool names do not imply one
    #[arg(long)]
    pub name: Option<String>,
    /// Directory to write `plugin.yaml` into (default: print it)
    #[arg(long)]
    pub out_dir: Option<PathBuf>,
}

impl Execute for PluginMigrateArgs {
    fn execute(self, _runtime: &OrbitRuntime) -> CommandOut {
        let (yaml, path) = migrate_plugin_sidecars(&PluginMigrateRequest {
            backend_command: self.binary,
            sidecars: self.sidecars,
            version: self.version,
            namespace: self.name,
            out_dir: self.out_dir,
        })?;
        let text = match &path {
            Some(path) => format!(
                "Wrote {}\n\nNext steps:\n  orbit plugin validate {}\n  orbit plugin add {}\n\nWarning: migration omits `origin: orbit`; add it only for a source that satisfies the first-party rule.",
                path.display(),
                path.parent().unwrap_or(path).display(),
                path.parent().unwrap_or(path).display()
            ),
            None => yaml.clone(),
        };
        let doc = json!({
            "manifest": yaml,
            "path": path.as_ref().map(|path| path.to_string_lossy().into_owned()),
        });
        Ok(Payload::detail(doc, text).into())
    }
}
