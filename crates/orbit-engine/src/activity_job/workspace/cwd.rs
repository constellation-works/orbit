use std::path::{Path, PathBuf};

use serde_json::Value;

use super::DispatchError;

pub fn resolve_subprocess_cwd(
    input: &Value,
    task_ctx: Option<&Value>,
    tool_ctx_workspace_root: Option<&Path>,
) -> Result<Option<PathBuf>, DispatchError> {
    // A *declared* workspace_path must be usable. Fail closed if the key is
    // present but renders to a non-string, null, or empty value — a
    // worktree-based pipeline step whose `{{ ... workspace_path }}` failed to
    // render would otherwise fall through and silently run the agent in the
    // primary checkout, which is the ORB-10134 data-loss hazard. A genuinely
    // absent key (direct, non-worktree runs) still falls back to the tool
    // context's workspace_root below.
    if let Some(resolved) = resolve_declared_workspace_path(Some(input), "activity input")? {
        return Ok(Some(resolved));
    }
    if let Some(resolved) = resolve_declared_workspace_path(task_ctx, "task context")? {
        return Ok(Some(resolved));
    }

    let Some(path) = tool_ctx_workspace_root else {
        return Ok(None);
    };

    if path.is_dir() {
        return Ok(Some(canonicalize_dir(path)));
    }

    tracing::warn!(
        target: "orbit.engine.cli_runner",
        path = %path.display(),
        "tool_ctx workspace_root missing, child will inherit parent cwd"
    );
    Ok(None)
}

/// Resolve a `workspace_path` declared on `container`. Returns:
/// - `Ok(None)` when the key is absent or JSON `null` (caller falls back — the
///   agent envelope / task context always serialize an undeclared
///   workspace_path as `null`, so `null` means "not declared");
/// - `Ok(Some(dir))` when it is a valid, existing directory;
/// - `Err(..)` when it is present but a non-string/non-null value or empty, or
///   names a path that is not a writable directory (fail closed — never fall
///   back to the primary checkout).
fn resolve_declared_workspace_path(
    container: Option<&Value>,
    source: &str,
) -> Result<Option<PathBuf>, DispatchError> {
    let Some(value) = container.and_then(|container| container.get("workspace_path")) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(path) = value.as_str() else {
        return Err(DispatchError::CliInvocationFailed(format!(
            "{source} declared a non-string workspace_path ({value}); \
             refusing to fall back to the repository root"
        )));
    };
    validate_declared_workspace_path(path).map(Some)
}

fn validate_declared_workspace_path(path: &str) -> Result<PathBuf, DispatchError> {
    let path_buf = PathBuf::from(path);
    if path.trim().is_empty() || !path_buf.is_dir() {
        return Err(DispatchError::CliInvocationFailed(format!(
            "workspace path {} is not a writable directory",
            path_buf.display()
        )));
    }
    Ok(canonicalize_dir(&path_buf))
}

// pub(crate) widened for tests/ layout under ORB-00225; test reaches via exposed surface.
pub(crate) fn canonicalize_dir(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}
