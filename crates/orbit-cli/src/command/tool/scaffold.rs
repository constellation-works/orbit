use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::tool::ToolParam;

use crate::command::{CommandOut, CommandOutput, Execute};

use super::manifest::{ExternalToolManifest, infer_tool_name, sidecar_manifest_path};

const EXTERNAL_TOOL_TEMPLATE: &str =
    include_str!("../../../assets/tool_templates/external_tool.py.tmpl");
const SCAFFOLD_DEFAULT_DESCRIPTION: &str =
    "Return a greeting and optionally echo Orbit tool context.";

/// What `orbit tool scaffold` prints before it does its work.
///
/// The v1 sidecar form still works and is still supported for one release
/// (design §4.8); this names its replacement at the moment an operator is
/// about to author a new tool, which is the only moment the choice is free.
pub(super) const SCAFFOLD_DEPRECATION: &str = concat!(
    "warning: `orbit tool scaffold` is deprecated and will be removed in a future \
     release. It writes the v1 form: one executable plus one `*.orbit-tool.yaml` \
     sidecar.\n",
    "         `orbit plugin scaffold <namespace>` writes a v2 plugin instead — a \
     manifest, a dashboard panel, a skill stub and passing conformance goldens — \
     and `orbit plugin migrate <binary>` converts existing sidecars."
);

#[derive(Args)]
#[command(
    about = "Generate a starter external tool plugin (deprecated: use `orbit plugin scaffold`)"
)]
pub struct ToolScaffoldArgs {
    /// Path to the starter executable to create
    pub path: String,
    /// Tool name to place in the generated manifest
    #[arg(long)]
    pub name: Option<String>,
    /// Tool description to place in the generated manifest
    #[arg(long, default_value = SCAFFOLD_DEFAULT_DESCRIPTION)]
    pub description: String,
    /// Overwrite existing files
    #[arg(long)]
    pub force: bool,
}

impl Execute for ToolScaffoldArgs {
    fn execute(self, _runtime: &OrbitRuntime) -> CommandOut {
        eprintln!("{SCAFFOLD_DEPRECATION}");
        let script_path = PathBuf::from(&self.path);
        let manifest_path = sidecar_manifest_path(&script_path);
        let tool_name = self
            .name
            .unwrap_or_else(|| infer_tool_name(script_path.as_path()));
        let description = self.description.trim().to_string();

        ensure_scaffold_targets_clear(&script_path, &manifest_path, self.force)?;

        if let Some(parent) = script_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| OrbitError::Io(format!("create {}: {error}", parent.display())))?;
        }

        let script = EXTERNAL_TOOL_TEMPLATE.replace("__ORBIT_TOOL_NAME__", &tool_name);
        fs::write(&script_path, script)
            .map_err(|error| OrbitError::Io(format!("write {}: {error}", script_path.display())))?;
        make_executable(&script_path)?;

        let manifest = ExternalToolManifest {
            schema_version: 1,
            name: tool_name.clone(),
            description,
            parameters: scaffold_parameters(),
        };
        let manifest_yaml = serde_yaml::to_string(&manifest)
            .map_err(|error| OrbitError::InvalidInput(format!("serialize manifest: {error}")))?;
        fs::write(&manifest_path, manifest_yaml).map_err(|error| {
            OrbitError::Io(format!("write {}: {error}", manifest_path.display()))
        })?;

        println!("Created starter plugin:");
        println!("  executable: {}", script_path.display());
        println!("  manifest:   {}", manifest_path.display());
        println!("\nNext steps:");
        println!("  orbit tool add {}", script_path.display());
        println!("  orbit tool show {}", tool_name);
        println!("  orbit mcp serve");
        println!("\nTo author this as a v2 plugin instead: orbit plugin scaffold <namespace>");
        Ok(CommandOutput::Silent)
    }
}

fn scaffold_parameters() -> Vec<ToolParam> {
    vec![
        ToolParam {
            name: "name".to_string(),
            description: "Greeting target echoed back by the example plugin.".to_string(),
            param_type: "string".to_string(),
            required: false,
        },
        ToolParam {
            name: "include_context".to_string(),
            description: "When true, include ORBIT_TOOL_* context values in the response."
                .to_string(),
            param_type: "boolean".to_string(),
            required: false,
        },
    ]
}

fn ensure_scaffold_targets_clear(
    script_path: &Path,
    manifest_path: &Path,
    force: bool,
) -> Result<(), OrbitError> {
    if force {
        return Ok(());
    }

    for path in [script_path, manifest_path] {
        if path.exists() {
            return Err(OrbitError::InvalidInput(format!(
                "refusing to overwrite existing file '{}'; rerun with --force",
                path.display()
            )));
        }
    }
    Ok(())
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
