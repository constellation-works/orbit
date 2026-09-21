use std::path::PathBuf;

use clap::Args;
use orbit_core::OrbitRuntime;
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct PluginValidateArgs {
    /// Plugin directory holding `plugin.yaml`
    pub dir: PathBuf,
    /// Treat the source as a verified first-party checkout, so an
    /// `origin: orbit` manifest is validated as it would be on install
    #[arg(long)]
    pub first_party: bool,
}

impl Execute for PluginValidateArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let report = runtime.validate_plugin_dir(&self.dir, self.first_party)?;
        let mut text = format!(
            "Valid plugin '{}' v{}\n  root:   {}\n  digest: {}\n  tools:",
            report.name, report.version, report.root, report.manifest_digest
        );
        for tool in &report.tools {
            text.push_str(&format!("\n    {tool}"));
        }
        for warning in &report.warnings {
            text.push_str(&format!("\n  warning: {warning}"));
        }
        let doc = json!({
            "name": report.name,
            "version": report.version,
            "root": report.root,
            "manifest_digest": report.manifest_digest,
            "tools": report.tools,
            "warnings": report.warnings,
        });
        Ok(Payload::detail(doc, text).into())
    }
}
