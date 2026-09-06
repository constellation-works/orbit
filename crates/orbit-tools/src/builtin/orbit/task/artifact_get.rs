use orbit_common::OrbitError;
use orbit_types::task::MAX_TASK_ARTIFACT_CONTENT_BYTES;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};

pub struct OrbitTaskArtifactGetTool;

impl Tool for OrbitTaskArtifactGetTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::ReadOnly
    }

    fn schema(&self) -> ToolSchema {
        let mut parameters = vec![
            ToolParam {
                name: "id".to_string(),
                description: "Globally unique task ID that owns the artifact.".to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "path".to_string(),
                description: "Artifact path relative to the task artifacts directory, exactly as \
                    it appears in `orbit.task.show` with `field: \"artifacts\"`. Absolute paths \
                    and `..` are rejected."
                    .to_string(),
                param_type: "string".to_string(),
                required: true,
            },
        ];
        parameters.extend(super::super::identity_params());
        parameters.push(ToolParam {
            name: "workspace".to_string(),
            description: "Optional explicit workspace filter. `id` is resolved globally by \
                default; when supplied, a registered workspace name, logical workspace ID \
                (`ws_*`), or absolute local checkout path is fail-closed — a valid workspace that \
                does not own the task returns not-found."
                .to_string(),
            param_type: "string".to_string(),
            required: false,
        });

        ToolSchema {
            name: "orbit.task.artifact.get".to_string(),
            description: format!(
                "Read one stored task artifact by owning task ID and artifact path. Returns \
                 `media_type`, `size`, and the bytes: UTF-8 text as `content`, and everything \
                 else as base64 in `content_base64`. Supported raster images (PNG, JPEG, GIF, \
                 WebP) whose bytes match their declared type are additionally returned as a \
                 viewable image on transports that carry image content, so a multimodal client \
                 can inspect a stored visual reference directly. SVG, HTML, and any payload \
                 whose bytes contradict its media type are returned as opaque base64 and are \
                 never rendered. List available artifacts first with `orbit.task.show` and \
                 `field: \"artifacts\"`; payloads are fetched only through this call. Reads are \
                 bounded to {MAX_TASK_ARTIFACT_CONTENT_BYTES} bytes."
            ),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::TaskArtifactGet)
    }
}
