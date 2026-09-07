use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::tracing;
use orbit_policy::resolve_symlinks;
use orbit_types::policy::FsOperation;
use orbit_types::task::{MAX_TASK_ARTIFACT_CONTENT_BYTES, TaskArtifact};
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::{Map, Value, json};

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitTaskArtifactPutTool;

impl Tool for OrbitTaskArtifactPutTool {
    fn schema(&self) -> ToolSchema {
        let mut parameters = super::super::orbit_id_params("task");
        parameters.extend([
            ToolParam {
                name: "source_path".to_string(),
                description: "Source file to store as a task artifact. Resolved against the \
                    caller cwd, then confined to the workspace checkout after symlink \
                    resolution. Absolute paths and symlinks that escape the workspace are \
                    rejected."
                    .to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "path".to_string(),
                description:
                    "Artifact path relative to the task artifacts directory. Defaults to the source file name."
                        .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
        ]);
        parameters.extend(super::super::model_identity_params());

        ToolSchema {
            name: "orbit.task.artifact.put".to_string(),
            description: "Store a source file under a task's artifacts directory. For automation-coverage.json, use the versioned evidence template in the assigned task: exact batch/input/revisions, complete examined commit and delivery lists, examination_complete, concrete checks and findings. Coverage acceptance requires the assigned executor run context; malformed, stale or unauthorized evidence is rejected during evaluation. Attaching ordinary artifacts never advances coverage.".to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::reject_agent_field(&input, "orbit.task.artifact.put")?;
        // A spoke broker reads the caller-local source before crossing the
        // hub link, then sends this path-free private payload under the same
        // canonical tool name. The hub accepts that shape only on ssh-mcp;
        // local/model calls must continue to supply a real source_path.
        if input.get("artifacts").is_some() {
            if ctx.session_context.transport != Some(orbit_types::tool::McpTransport::SshMcp)
                || input.get("source_path").is_some()
                || input.get("sourcePath").is_some()
                || input.get("source-path").is_some()
            {
                return Err(OrbitError::InvalidInput(
                    "preloaded task artifact payloads are accepted only from the authenticated ssh-mcp spoke connector"
                        .to_string(),
                ));
            }
            return super::super::execute_host_action(ctx, input, OrbitBuiltinAction::TaskUpdate);
        }

        let update_input = prepare_remote_payload(input, ctx)?;

        super::super::execute_host_action(ctx, update_input, OrbitBuiltinAction::TaskUpdate)
    }
}

/// Read a caller-local task artifact into the bounded, path-free payload used
/// by the spoke connector. The source path is consumed locally and never
/// appears in the returned coordination frame.
pub(crate) fn prepare_remote_payload(input: Value, ctx: &ToolContext) -> Result<Value, OrbitError> {
    let id = super::super::required_string(&input, &["id"], "id")?;
    let source_path = super::super::required_string(
        &input,
        &["source_path", "sourcePath", "source-path"],
        "source_path",
    )?;
    let artifact_path =
        super::super::optional_string_alias(&input, &["path", "artifact_path", "artifactPath"])?;
    let resolved_source_path = confine_source_path(
        ctx,
        &resolve_source_path(ctx.cwd.as_deref().map(Path::new), &source_path),
    )?;
    let artifact = read_bounded_artifact(&resolved_source_path, artifact_path.as_deref())?;

    let mut update_input = input.as_object().cloned().unwrap_or_else(Map::new);
    update_input.insert("id".to_string(), Value::String(id));
    update_input.remove("source_path");
    update_input.remove("sourcePath");
    update_input.remove("source-path");
    update_input.remove("path");
    update_input.remove("artifact_path");
    update_input.remove("artifactPath");
    update_input.insert(
        "artifacts".to_string(),
        json!([{
            "path": artifact.path,
            "media_type": artifact.media_type,
            "content": artifact.content,
        }]),
    );
    Ok(Value::Object(update_input))
}

fn read_bounded_artifact(
    source_path: &Path,
    artifact_path: Option<&str>,
) -> Result<TaskArtifact, OrbitError> {
    let mut file = File::open(source_path).map_err(|error| {
        OrbitError::Io(format!(
            "read artifact source '{}': {error}",
            source_path.display()
        ))
    })?;
    let mut content = Vec::new();
    file.by_ref()
        .take(MAX_TASK_ARTIFACT_CONTENT_BYTES + 1)
        .read_to_end(&mut content)
        .map_err(|error| {
            OrbitError::Io(format!(
                "read artifact source '{}': {error}",
                source_path.display()
            ))
        })?;
    if content.len() as u64 > MAX_TASK_ARTIFACT_CONTENT_BYTES {
        return Err(OrbitError::InvalidInput(format!(
            "artifact source '{}' exceeds the {} byte content limit",
            source_path.display(),
            MAX_TASK_ARTIFACT_CONTENT_BYTES
        )));
    }

    let path = artifact_path
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            source_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "artifact source '{}' has no file name; provide `path`",
                source_path.display()
            ))
        })?;
    Ok(TaskArtifact {
        media_type: orbit_types::task::media_type_for_artifact_path(&path).to_string(),
        path,
        content,
        created_by: None,
    })
}

fn resolve_source_path(cwd: Option<&Path>, source_path: &str) -> PathBuf {
    let path = PathBuf::from(source_path);
    if path.is_absolute() {
        return path;
    }
    cwd.map(|cwd| cwd.join(&path)).unwrap_or(path)
}

/// Symlink-safe workspace containment for a caller-local artifact source.
///
/// Absolute paths, relative `..` traversal, and in-workspace symlinks that
/// resolve outside `ctx.workspace_root` are rejected as `invalid_input` so a
/// remote `agent` session cannot attach host secrets such as
/// `~/.orbit/mcp-ssh-acceptance/*.toml`. When a filesystem profile is present,
/// `check_resolved` additionally applies deny-read rules to the real path.
fn confine_source_path(ctx: &ToolContext, source_path: &Path) -> Result<PathBuf, OrbitError> {
    let workspace_root = ctx.workspace_root.as_deref().ok_or_else(|| {
        OrbitError::InvalidInput("workspace_root is required to attach a source file".to_string())
    })?;
    let canonical_workspace = workspace_root.canonicalize().map_err(|error| {
        OrbitError::InvalidInput(format!(
            "failed to canonicalize workspace_root '{}': {error}",
            workspace_root.display()
        ))
    })?;
    let resolved = resolve_symlinks(source_path)?;
    if !resolved.starts_with(&canonical_workspace) {
        return Err(OrbitError::InvalidInput(format!(
            "source_path '{}' is outside workspace_root '{}'",
            resolved.display(),
            canonical_workspace.display()
        )));
    }

    if let (Some(policy), Some(profile)) = (ctx.policy_engine.as_ref(), ctx.fs_profile.as_deref()) {
        let evaluation =
            policy.check_resolved(&canonical_workspace, profile, FsOperation::Read, &resolved)?;
        if !evaluation.allowed {
            tracing::warn!(
                target: "orbit.policy.deny",
                tool = "orbit.task.artifact.put",
                path = evaluation.path.as_str(),
                profile = evaluation.profile.as_str(),
                matched_rule = evaluation.matched_rule.as_str(),
            );
            return Err(OrbitError::PolicyDenied(format!(
                "orbit.task.artifact.put path '{}' is denied by fsProfile '{}' (matched rule: {})",
                evaluation.path, evaluation.profile, evaluation.matched_rule
            )));
        }
    }

    Ok(resolved)
}
