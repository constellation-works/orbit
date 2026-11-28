use std::path::PathBuf;

use clap::Args;
use orbit_core::Core;
use orbit_core::app::plugin::{Command, Outcome, exec};
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
pub struct PluginValidateArgs {
    /// Plugin source: a directory holding `.orbit-plugin/plugin.yaml`, or
    /// that `.orbit-plugin` directory itself
    pub dir: PathBuf,
    /// Treat the source as a verified first-party checkout, so an
    /// `origin: orbit` manifest is validated as it would be on install
    #[arg(long)]
    pub first_party: bool,
    /// Print the effective sandbox profile and child environment for the
    /// registered workspace without executing the backend. Outside one,
    /// render with host config; use `--workspace` for workspace config.
    #[arg(long)]
    pub render: bool,
}

impl ExecuteCore for PluginValidateArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Validated(report) = exec(
            core,
            Command::Validate {
                dir: self.dir,
                first_party: self.first_party,
                render: self.render,
            },
        )?
        else {
            unreachable!("plugin command returned an unrelated outcome");
        };
        let mut text = format!(
            "Valid plugin '{}' v{}\n  root:   {}\n  digest: {}\n  tools:",
            report.name, report.version, report.root, report.manifest_digest
        );
        for tool in &report.tools {
            text.push_str(&format!("\n    {tool}"));
        }
        if !report.pages.is_empty() {
            text.push_str("\n  pages:");
            for page in &report.pages {
                text.push_str(&format!("\n    {}  {}", page.id, page.digest));
            }
        }
        for warning in &report.warnings {
            text.push_str(&format!("\n  warning: {warning}"));
        }
        if let Some(rendered) = &report.rendered {
            text.push_str(&format!(
                "\nRendered backend profile for {}\n  read:         {}\n  read denies: {}\n  write:        {}\n  write files:  {}\n  network:      {}\n  unsandboxed:  {}",
                rendered.workspace,
                display_list(&rendered.read),
                display_list(&rendered.read_denies),
                display_list(&rendered.write),
                display_list(&rendered.write_files),
                rendered.network,
                rendered.unsandboxed,
            ));
            for environment in &rendered.environments {
                text.push_str(&format!(
                    "\n  child env{}:",
                    environment
                        .tool
                        .as_deref()
                        .map(|tool| format!(" ({tool})"))
                        .unwrap_or_default()
                ));
                for (name, value) in &environment.variables {
                    text.push_str(&format!("\n    {name}={value}"));
                }
            }
        }
        let rendered = report.rendered.as_ref().map(|rendered| {
            json!({
                "workspace": rendered.workspace,
                "read": rendered.read,
                "read_denies": rendered.read_denies,
                "write": rendered.write,
                "write_files": rendered.write_files,
                "network": rendered.network,
                "unsandboxed": rendered.unsandboxed,
                "environments": rendered.environments.iter().map(|environment| json!({
                    "tool": environment.tool,
                    "variables": environment.variables,
                })).collect::<Vec<_>>(),
            })
        });
        let mut doc = json!({
            "name": report.name,
            "version": report.version,
            "root": report.root,
            "manifest_digest": report.manifest_digest,
            "tools": report.tools,
            "pages": report
                .pages
                .iter()
                .map(|page| json!({ "id": page.id, "digest": page.digest }))
                .collect::<Vec<_>>(),
            "warnings": report.warnings,
        });
        if let Some(rendered) = rendered
            && let Some(fields) = doc.as_object_mut()
        {
            fields.insert("rendered".to_string(), rendered);
        }
        Ok(Payload::detail(doc, text).into())
    }
}

fn display_list(items: &[String]) -> String {
    if items.is_empty() {
        "(none)".to_string()
    } else {
        items.join(", ")
    }
}
