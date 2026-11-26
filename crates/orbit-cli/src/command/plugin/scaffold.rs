use std::path::PathBuf;

use clap::Args;
use orbit_core::Core;
use orbit_core::app::plugin::{Command, Outcome, exec};
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
pub struct PluginScaffoldArgs {
    /// Namespace for the new plugin: it owns `<ns>.*` tools, `orbit <ns>`,
    /// and `[plugins.<ns>]`
    pub namespace: String,
    /// Source directory to create; the plugin goes in its `.orbit-plugin/`
    /// (defaults to `./<namespace>`)
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Overwrite existing files
    #[arg(long)]
    pub force: bool,
}

impl ExecuteCore for PluginScaffoldArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Scaffolded(scaffold) = exec(
            core,
            Command::Scaffold {
                namespace: self.namespace,
                dir: self.dir,
                force: self.force,
            },
        )?
        else {
            unreachable!("plugin command returned an unrelated outcome");
        };
        let namespace = scaffold.namespace;
        let source_display = scaffold.source;
        let root_display = scaffold.root;
        let written = scaffold.files;
        let text = format!(
            "Created the '{namespace}' plugin in {root_display}:\n{}\n\nNext steps:\n  orbit \
             plugin validate {source_display}\n  orbit plugin add {source_display} --enable\n  \
             orbit plugin test {source_display}\n  orbit {namespace} status\n\nThe seeded \
             auto-task is disabled; review it before switching it on.",
            written
                .iter()
                .map(|path| format!("  {path}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        Ok(Payload::detail(
            json!({
                "name": namespace,
                "source": source_display,
                "root": root_display,
                "files": written,
            }),
            text,
        )
        .into())
    }
}
