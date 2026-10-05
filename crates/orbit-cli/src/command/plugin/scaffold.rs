use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::plugin::{MANIFEST_FILE_NAME, is_valid_namespace, plugin_root_in};
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

/// The starter tree, one template per file. Every `__ORBIT_PLUGIN_NS__` is
/// replaced with the namespace the operator named.
const NAMESPACE_PLACEHOLDER: &str = "__ORBIT_PLUGIN_NS__";
const MANIFEST_TEMPLATE: &str = include_str!("../../../assets/plugin_templates/plugin.yaml.tmpl");
const BACKEND_TEMPLATE: &str = include_str!("../../../assets/plugin_templates/backend.py.tmpl");
const AUTO_TASK_TEMPLATE: &str =
    include_str!("../../../assets/plugin_templates/auto_task.yaml.tmpl");
const SKILL_TEMPLATE: &str = include_str!("../../../assets/plugin_templates/SKILL.md.tmpl");
const CONFORMANCE_TEMPLATE: &str =
    include_str!("../../../assets/plugin_templates/conformance.yaml.tmpl");

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

impl Execute for PluginScaffoldArgs {
    fn execute(self, _runtime: &OrbitRuntime) -> CommandOut {
        let namespace = self.namespace.trim().to_string();
        if !is_valid_namespace(&namespace) {
            return Err(OrbitError::InvalidInput(format!(
                "'{namespace}' is not a valid plugin namespace: use lowercase letters, digits, \
                 '_' or '-', starting with a letter, and not the reserved 'orbit'"
            )));
        }
        let source = self.dir.unwrap_or_else(|| default_scaffold_dir(&namespace));
        let root = plugin_root_in(&source);
        let files = scaffold_files(&namespace);
        if !self.force {
            for (relative, _, _) in &files {
                let path = root.join(relative);
                if path.exists() {
                    return Err(OrbitError::InvalidInput(format!(
                        "refusing to overwrite existing file '{}'; rerun with --force",
                        path.display()
                    )));
                }
            }
        }
        for (relative, contents, executable) in &files {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    OrbitError::Io(format!("create {}: {error}", parent.display()))
                })?;
            }
            fs::write(&path, contents)
                .map_err(|error| OrbitError::Io(format!("write {}: {error}", path.display())))?;
            if *executable {
                make_executable(&path)?;
            }
        }

        let source_display = source.display().to_string();
        let root_display = root.display().to_string();
        let written: Vec<String> = files
            .iter()
            .map(|(relative, _, _)| root.join(relative).display().to_string())
            .collect();
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

fn default_scaffold_dir(namespace: &str) -> PathBuf {
    PathBuf::from(".").join(namespace)
}

/// `(path relative to the plugin root, contents, executable)`.
fn scaffold_files(namespace: &str) -> Vec<(PathBuf, String, bool)> {
    let render = |template: &str| template.replace(NAMESPACE_PLACEHOLDER, namespace);
    vec![
        (
            PathBuf::from(MANIFEST_FILE_NAME),
            render(MANIFEST_TEMPLATE),
            false,
        ),
        (
            PathBuf::from("bin/backend.py"),
            render(BACKEND_TEMPLATE),
            true,
        ),
        (
            PathBuf::from("definitions/auto_tasks/review.yaml"),
            render(AUTO_TASK_TEMPLATE),
            false,
        ),
        (
            Path::new("skills").join(namespace).join("SKILL.md"),
            render(SKILL_TEMPLATE),
            false,
        ),
        (
            PathBuf::from("tests/conformance/status.yaml"),
            render(CONFORMANCE_TEMPLATE),
            false,
        ),
    ]
}

fn make_executable(path: &Path) -> Result<(), OrbitError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let metadata = fs::metadata(path)
            .map_err(|error| OrbitError::Io(format!("stat {}: {error}", path.display())))?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .map_err(|error| OrbitError::Io(format!("chmod {}: {error}", path.display())))?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}
