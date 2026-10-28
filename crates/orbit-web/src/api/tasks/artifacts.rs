//! Task artifact reads and response policy.

use crate::api::{bad_request, blocking, validate_id};
use crate::state::Ws;
use axum::body::Body;
use axum::extract::Path;
use axum::http::{HeaderName, HeaderValue, header};
use axum::response::Response;
use orbit_types::task::{inline_safe_artifact_media_type, validate_relative_artifact_path};

struct ArtifactResponsePolicy {
    content_type: &'static str,
    attachment: bool,
}

pub(in crate::api) async fn get_task_artifact(
    Ws(runtime): Ws,
    Path((id, path)): Path<(String, String)>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let path = match validate_artifact_request_path(&path) {
        Ok(path) => path,
        Err(message) => return bad_request(message),
    };
    let id = id.to_string();
    let path_owned = path.to_string();
    let missing = format!("artifact not found: {id}/{path_owned}");
    match blocking("task artifact", move || {
        runtime.get_task_artifact(&id, &path_owned)
    })
    .await
    {
        Ok(Some(artifact)) => {
            let policy = artifact_response_policy(&artifact.media_type);
            let mut response = Response::new(Body::from(artifact.content));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(policy.content_type),
            );
            response.headers_mut().insert(
                HeaderName::from_static("x-content-type-options"),
                HeaderValue::from_static("nosniff"),
            );
            // Artifacts are task-author content served from the dashboard's
            // origin: sandbox them so an inline-rendered one (SVG, HTML-ish
            // media) cannot run script or reach the API as this origin.
            response.headers_mut().insert(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("sandbox; default-src 'none'"),
            );
            if policy.attachment {
                response.headers_mut().insert(
                    header::CONTENT_DISPOSITION,
                    HeaderValue::from_static("attachment"),
                );
            }
            response
        }
        Ok(None) => crate::api::not_found(missing),
        Err(response) => *response,
    }
}

fn artifact_response_policy(media_type: &str) -> ArtifactResponsePolicy {
    // The inline allowlist is the shared artifact policy, not a web-only rule:
    // the dashboard and `orbit.task.artifact.get` must agree on which media
    // types may be rendered rather than downloaded.
    let content_type = inline_safe_artifact_media_type(media_type);
    ArtifactResponsePolicy {
        content_type: content_type.unwrap_or("application/octet-stream"),
        attachment: content_type.is_none(),
    }
}

fn validate_artifact_request_path(path: &str) -> Result<String, String> {
    validate_relative_artifact_path(path).map_err(|error| error.to_string())?;
    Ok(path.to_string())
}
