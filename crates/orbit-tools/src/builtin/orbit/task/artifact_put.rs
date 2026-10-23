use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::open_read_only_no_follow;
use orbit_common::protocol::tool_input::reject_unknown_tool_fields;
use orbit_common::tracing;
use orbit_policy::resolve_symlinks;
use orbit_types::policy::FsOperation;
use orbit_types::task::{MAX_TASK_ARTIFACT_CONTENT_BYTES, TaskArtifact, canonical_artifact_path};
use orbit_types::workflow::{REVIEW_REPORT_ARTIFACT, ReviewReport};
use serde_json::{Map, Value, json};

use crate::{OrbitBuiltinAction, ToolContext};
use orbit_common::governance::operation::OperationVerb;
use orbit_common::governance::task::TaskVerb;

/// Authenticate the private path-free payload before ordinary spec parsing.
pub(super) fn preempt(ctx: &ToolContext, input: &Value) -> Option<Result<Value, OrbitError>> {
    if let Err(error) = super::super::reject_agent_field(input, "orbit.task.artifact.put") {
        return Some(Err(error));
    }
    if input.get("artifacts").is_none() {
        return source_presence_error(input).map(Err);
    }
    Some((|| {
        if (ctx.session_context.transport != Some(orbit_types::tool::McpTransport::SshMcp)
            && ctx.session_context.worker_invocation.is_none())
            || ["source_path", "sourcePath", "source-path"]
                .iter()
                .any(|key| input.get(*key).is_some())
        {
            return Err(OrbitError::InvalidInput("preloaded task artifact payloads are accepted only from the authenticated ssh-mcp spoke connector".to_string()));
        }
        reject_unknown_tool_fields(input, &["id", "artifacts", "model"])?;
        super::super::execute_host_action(
            ctx,
            input.clone(),
            OrbitBuiltinAction::Task(TaskVerb::ArtifactPut),
        )
    })())
}

/// Keep the shipped source-file remedy on incomplete calls. Unknown fields
/// on complete calls still use the registry's warn mode.
fn source_presence_error(input: &Value) -> Option<OrbitError> {
    let spec = TaskVerb::ArtifactPut.spec();
    let source = spec
        .wire_params()
        .find(|param| param.name == "source_path")?;
    if std::iter::once(source.name)
        .chain(source.aliases.iter().copied())
        .any(|key| input.get(key).is_some())
    {
        return None;
    }
    let allowed = spec
        .wire_params()
        .flat_map(|param| std::iter::once(param.name).chain(param.aliases.iter().copied()))
        .collect::<Vec<_>>();
    let result = (|| {
        reject_unknown_tool_fields(input, &allowed)?;
        orbit_common::protocol::tool_input::required_string(input, &["id"], "id")?;
        Err::<(), _>(OrbitError::InvalidInput("missing `source_path`; `path` is the artifact name, and `source_path` is the local file to attach under `.orbit/tmp/` (`$ORBIT_SCRATCH_DIR`)".to_string()))
    })();
    result.err()
}

/// Read a caller-local task artifact into the bounded, path-free payload used
/// by the spoke connector. The source path is consumed locally and never
/// appears in the returned coordination frame.
pub(crate) fn prepare_remote_payload(input: Value, ctx: &ToolContext) -> Result<Value, OrbitError> {
    // This helper also runs on the worker before the private spoke payload is
    // sent, so validate here rather than relying on the local execute path.
    if let Some(error) = source_presence_error(&input) {
        return Err(error);
    }
    let input = TaskVerb::ArtifactPut.spec().parse_input(input)?;
    let id = input.required_string("id")?;
    let normalized_source_path = input.required_string("source_path")?;
    // Local CLI calls select an exact filesystem name; MCP retains its
    // shipped string normalization. Neither changes artifact-name handling.
    let source_path = if ctx.session_context.transport.is_none() {
        input
            .optional_raw_string("source_path")?
            .unwrap_or(normalized_source_path)
    } else {
        normalized_source_path
    };
    let artifact_path = input.optional_string("path")?;
    let resolved = resolve_source_path(ctx.cwd.as_deref().map(Path::new), &source_path);
    if let Some(error) = checkout_root_source_hint(ctx, &source_path, &resolved) {
        return Err(error);
    }
    let resolved_source_path = confine_source_path(ctx, &resolved)?;
    let artifact = read_bounded_artifact(&resolved_source_path, artifact_path.as_deref())?;
    validate_review_report(&artifact)?;

    // Build the attachment input from attachment and transport fields only.
    // Future task update fields cannot silently widen artifact.put authority.
    let mut update_input = Map::new();
    update_input.insert("id".to_string(), Value::String(id));
    for key in ["model", "workspace", "_meta"] {
        if let Some(value) = input.as_value().get(key) {
            update_input.insert(key.to_string(), value.clone());
        }
    }
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

/// A before-PR reviewer's report is validated on attach, so the reviewer
/// learns of a contract mismatch while it can still fix the file rather than
/// at settlement, where it would make the review incomplete.
fn validate_review_report(artifact: &TaskArtifact) -> Result<(), OrbitError> {
    let path = canonical_artifact_path(&artifact.path)?;
    if path != REVIEW_REPORT_ARTIFACT {
        return Ok(());
    }
    ReviewReport::parse_attachment(&artifact.content).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "{REVIEW_REPORT_ARTIFACT} does not match the review report contract: {error}"
        ))
    })?;
    Ok(())
}

fn read_bounded_artifact(
    source_path: &Path,
    artifact_path: Option<&str>,
) -> Result<TaskArtifact, OrbitError> {
    // `confine_source_path` resolved and checked this path a moment ago, but
    // the workspace belongs to the caller. Refuse a link swapped in since, and
    // a FIFO or device that would block or never end: only a regular file is
    // read.
    let mut file = open_read_only_no_follow(source_path).map_err(|error| {
        OrbitError::Io(format!(
            "read artifact source '{}': {error}",
            source_path.display()
        ))
    })?;
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Err(OrbitError::InvalidInput(format!(
            "artifact source '{}' is not a regular file",
            source_path.display()
        )));
    }
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
            "artifact source '{}' exceeds the {} byte content limit; attach a digest or a \
             bounded, gzipped excerpt instead",
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

/// A relative `source_path` is read from the caller's current directory. An
/// agent working in a scratch folder often repeats the checkout-relative
/// prefix, which doubles the path and misses the file. When the same relative
/// path names a confined regular file at the checkout root, refuse and name
/// that file's absolute path. Nothing is attached, and filename semantics stay
/// exact. Missing at both locations keeps the ordinary read error.
fn checkout_root_source_hint(
    ctx: &ToolContext,
    source_path: &str,
    resolved: &Path,
) -> Option<OrbitError> {
    if Path::new(source_path).is_absolute() || resolved.exists() {
        return None;
    }
    let workspace_root = ctx.workspace_root.as_deref()?.canonicalize().ok()?;
    let candidate = confine_source_path(ctx, &workspace_root.join(source_path)).ok()?;
    if !candidate.is_file() {
        return None;
    }
    Some(OrbitError::InvalidInput(format!(
        "source_path '{source_path}' does not exist under the current directory '{}', but the same relative path names a file at the checkout root; pass the absolute source_path '{}' instead",
        ctx.cwd.as_deref().unwrap_or("."),
        candidate.display()
    )))
}

/// Symlink-safe workspace containment for a caller-local artifact source.
///
/// Absolute paths, relative `..` traversal, and in-workspace symlinks that
/// resolve outside `ctx.workspace_root` are rejected as `invalid_input` so a
/// remote `agent` session cannot attach host secrets such as
/// `~/.ssh/id_ed25519`. When a filesystem profile is present,
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
            "source_path '{}' is outside workspace_root '{}'; write evidence under `.orbit/tmp/` (`$ORBIT_SCRATCH_DIR`)",
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
