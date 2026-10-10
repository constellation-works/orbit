//! Native read surfaces for task pilots, independent of MCP grants.

use serde_json::Value;

use super::super::dispatcher::DispatchError;

/// Prepare native tools before allocating a checkout or starting a session.
/// A null revision still denotes a non-Git inspection.
pub(super) fn prepare_inspection_tools(
    provider: &str,
    input: &Value,
    fs_profile: Option<&str>,
    static_args: &[String],
) -> Result<Option<InspectionTools>, DispatchError> {
    // These are the required task-pilot input fields. Other read-only
    // inspection callers retain their existing provider/tool contracts.
    if input.get("inspection_revision").is_none() || input.get("partition_index").is_none() {
        return Ok(None);
    }
    let unavailable = |reason: &str| DispatchError::InspectionToolsUnavailable {
        provider: provider.to_string(),
        reason: reason.to_string(),
    };
    if fs_profile != Some("reviewer") {
        return Err(unavailable(
            "native inspection requires the reviewer filesystem profile",
        ));
    }
    let surface = match provider {
        "codex" => InspectionTools {
            // Invocation overrides also cover a user config that disables
            // the shell. The outer reviewer sandbox remains authoritative.
            args: vec![
                "--config".into(),
                "features.shell_tool=true".into(),
                "--config".into(),
                "features.unified_exec=true".into(),
            ],
            instruction: "Provider inspection surface: Codex native shell tools are enabled. Use the native shell announced by this harness to read files with cat or sed and run read-only git/rg commands. These native tools are separate from the Orbit MCP tool list.",
        },
        "claude" => {
            let selected = option_values(static_args, &["--tools"]);
            if let Some(selected) = &selected
                && !selected.contains(&"default")
                && !["Read", "Bash"]
                    .iter()
                    .all(|required| selected.contains(required))
            {
                return Err(unavailable(
                    "Claude --tools must include Read and Bash (or default)",
                ));
            }
            let denied = option_values(static_args, &["--disallowedTools", "--disallowed-tools"]);
            if denied
                .iter()
                .flatten()
                .any(|tool| matches!(*tool, "Read" | "Bash"))
            {
                return Err(unavailable("Claude executor disallows Read or Bash"));
            }
            InspectionTools {
                args: if selected.is_none() {
                    vec!["--tools".into(), "Read,Bash,Glob,Grep".into()]
                } else {
                    vec![]
                },
                instruction: "Provider inspection surface: Claude native Read and Bash tools are enabled. Use Read for candidate files and Bash for read-only git/rg commands. These native tools are separate from the Orbit MCP tool list.",
            }
        }
        _ => {
            return Err(unavailable(
                "this provider has no supported native file-read and command surface; configure a Codex or Claude inspection crew",
            ));
        }
    };
    Ok(Some(surface))
}

pub(super) struct InspectionTools {
    pub(super) args: Vec<String>,
    pub(super) instruction: &'static str,
}

/// Claude accepts comma-separated or multiple tool values, including an
/// explicit empty value which disables the native tool set.
fn option_values<'a>(args: &'a [String], names: &[&str]) -> Option<Vec<&'a str>> {
    let mut values = None;
    let mut collecting = false;
    for arg in args {
        if names.contains(&arg.as_str()) {
            values = Some(Vec::new());
            collecting = true;
        } else if let Some((name, value)) = arg.split_once('=')
            && names.contains(&name)
        {
            values = Some(value.split(',').collect());
            collecting = false;
        } else if arg.starts_with('-') {
            collecting = false;
        } else if collecting && let Some(values) = &mut values {
            values.extend(arg.split(','));
        }
    }
    values
}
