use super::super::router;
use super::test_support::body_json;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Method, Request, StatusCode, header};
use axum::response::Response;
use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{OrbitRuntime, TaskStatus};
use orbit_types::task::{
    ArtifactManifestV2, TASK_ARTIFACT_MANIFEST_FILE_NAME, TASK_ARTIFACTS_DIR_NAME, TaskArtifact,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

const SECRET_CONTENT: &str = "TOP-SECRET-OUTSIDE-ARTIFACT-ROOT";

fn patch_json(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::PATCH)
        .uri(uri)
        .header(header::ORIGIN, "http://localhost:7878")
        .header(header::HOST, "localhost:7878")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

fn seed_backlog_task(runtime: &OrbitRuntime, title: &str) -> orbit_core::Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}."),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed backlog task")
}

fn seed_task_with_artifact(runtime: &OrbitRuntime) -> orbit_core::Task {
    seed_task_with_artifact_payload(
        runtime,
        "subdir/file.json",
        "application/json",
        br#"{"ok":true}"#.to_vec(),
    )
}

fn seed_task_with_artifact_payload(
    runtime: &OrbitRuntime,
    path: &str,
    media_type: &str,
    content: Vec<u8>,
) -> orbit_core::Task {
    let task = runtime
        .add_task(TaskAddParams {
            title: "Artifact task".to_string(),
            description: "Fixture task with an artifact.".to_string(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("create task");
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact {
                    path: path.to_string(),
                    media_type: media_type.to_string(),
                    content,
                    created_by: None,
                }],
                ..Default::default()
            },
            Some("codex".to_string()),
            Some(TEST_CODEX_MODEL.to_string()),
        )
        .expect("upsert artifact")
}

async fn request(runtime: OrbitRuntime, uri: &str) -> axum::response::Response {
    request_shared(Arc::new(runtime), uri).await
}

/// `request` against an already-shared runtime, so one fixture can serve several
/// query variants without reseeding (ORB-10400's filter matrix).
async fn request_shared(runtime: Arc<OrbitRuntime>, uri: &str) -> axum::response::Response {
    router()
        .with_state(crate::state::DashboardState::single(runtime))
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .header(header::HOST, "localhost:7878")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

#[tokio::test]
async fn get_task_artifact_serves_subdirectory_bytes_and_media_type() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = seed_task_with_artifact(&runtime);

    let response = request(
        runtime,
        &format!("/tasks/{}/artifacts/subdir/file.json", task.id),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&HeaderValue::from_static("application/json"))
    );
    assert_eq!(
        response.headers().get("x-content-type-options"),
        Some(&HeaderValue::from_static("nosniff"))
    );
    assert_eq!(
        response.headers().get(header::CONTENT_SECURITY_POLICY),
        Some(&HeaderValue::from_static("sandbox; default-src 'none'")),
        "artifact bytes are task-author content and must be sandboxed"
    );
    assert!(
        response
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .is_none()
    );
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    assert_eq!(&bytes[..], br#"{"ok":true}"#);
}

#[tokio::test]
async fn get_task_artifact_downloads_unsafe_media_types() {
    let cases: [(&str, &str, &[u8]); 3] = [
        (
            "reports/payload.html",
            "text/html; charset=utf-8",
            br#"<script>fetch("/api/tasks")</script>"#,
        ),
        (
            "reports/payload.js",
            "application/javascript",
            b"fetch('/api/tasks')",
        ),
        (
            "reports/payload.custom",
            "application/x-orbit-preview",
            b"custom artifact",
        ),
    ];

    for (path, media_type, expected_body) in cases {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        let task =
            seed_task_with_artifact_payload(&runtime, path, media_type, expected_body.to_vec());

        let response = request(runtime, &format!("/tasks/{}/artifacts/{path}", task.id)).await;

        assert_eq!(response.status(), StatusCode::OK, "{media_type}");
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/octet-stream")),
            "{media_type}"
        );
        assert_eq!(
            response.headers().get("x-content-type-options"),
            Some(&HeaderValue::from_static("nosniff")),
            "{media_type}"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_DISPOSITION),
            Some(&HeaderValue::from_static("attachment")),
            "{media_type}"
        );
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        assert_eq!(&bytes[..], expected_body, "{media_type}");
    }
}

/// Plants a secret file *outside* the artifact root (next to the runtime's
/// data root) and returns its absolute path, so adversarial requests have a
/// concrete escape target whose bytes must never appear in a response.
fn plant_secret_outside_artifact_root(runtime: &OrbitRuntime) -> std::path::PathBuf {
    let secret_path = runtime.data_root().join("secret-fixture.txt");
    std::fs::write(&secret_path, SECRET_CONTENT).expect("write secret fixture");
    secret_path
}

async fn assert_artifact_request_denied(response: axum::response::Response, label: &str) {
    let status = response.status();
    assert!(
        status.is_client_error(),
        "{label}: expected 4xx, got {status}"
    );
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let body = String::from_utf8_lossy(&bytes);
    assert!(
        !body.contains(SECRET_CONTENT),
        "{label}: response leaked out-of-root file content"
    );
}

/// ORB-10008: adversarial path shapes against the artifact validator. Every
/// case must be a clean 4xx and must never serve bytes from outside the task's
/// artifact root.
#[tokio::test]
async fn get_task_artifact_rejects_adversarial_paths() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = seed_task_with_artifact(&runtime);
    plant_secret_outside_artifact_root(&runtime);

    let cases: &[(&str, String)] = &[
        (
            "raw dot-dot traversal",
            format!(
                "/tasks/{}/artifacts/../../../../secret-fixture.txt",
                task.id
            ),
        ),
        (
            "encoded dot-dot traversal",
            format!(
                "/tasks/{}/artifacts/subdir/%2e%2e%2f%2e%2e%2fsecret-fixture.txt",
                task.id
            ),
        ),
        (
            "encoded absolute path",
            format!("/tasks/{}/artifacts/%2Fetc%2Fpasswd", task.id),
        ),
        (
            "backslash separators",
            format!(
                "/tasks/{}/artifacts/subdir%5C..%5C..%5Csecret-fixture.txt",
                task.id
            ),
        ),
        (
            "leading current-dir component",
            format!("/tasks/{}/artifacts/.%2Fsubdir%2Ffile.json", task.id),
        ),
    ];
    for (label, uri) in cases {
        let response = request(runtime.clone(), uri).await;
        assert_artifact_request_denied(response, label).await;
    }

    // A double-slash absolute spelling may 400 (validator) or 404 (router);
    // either way it must not leak file contents.
    let response = request(
        runtime.clone(),
        &format!("/tasks/{}/artifacts//etc/passwd", task.id),
    )
    .await;
    let status = response.status();
    assert!(
        status.is_client_error(),
        "double-slash absolute: expected 4xx, got {status}"
    );
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    assert!(!String::from_utf8_lossy(&bytes).contains("root:"));
}

/// Replace the on-disk blob backing `subdir/file.json` with a symlink to
/// `target`, returning the blob path that was swapped.
#[cfg(unix)]
fn swap_artifact_blob_for_symlink(
    runtime: &OrbitRuntime,
    task_id: &str,
    target: &std::path::Path,
) -> std::path::PathBuf {
    let blob_path = find_artifact_blob(&runtime.global_root(), task_id, "subdir/file.json")
        .expect("artifact blob exists on disk");
    assert!(
        blob_path.components().any(|c| c.as_os_str() == "artifacts"),
        "blob must live under the artifact directory: {}",
        blob_path.display()
    );
    std::fs::remove_file(&blob_path).expect("remove artifact blob");
    std::os::unix::fs::symlink(target, &blob_path).expect("plant escaping symlink");
    blob_path
}

/// ORB-10008: a manifest-listed blob replaced on disk by a symlink pointing
/// outside the artifact root must be refused, not followed.
///
/// The v2 bundle read sha256/size-verifies every manifest entry before any
/// artifact is served, so the path-containment check (canonicalize +
/// `starts_with`) is only reachable when the escape target is byte-identical
/// to the recorded artifact. This test plants exactly that worst case and
/// asserts the containment validator still rejects the escaping link.
#[cfg(unix)]
#[tokio::test]
async fn get_task_artifact_refuses_symlink_escaping_artifact_root() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = seed_task_with_artifact(&runtime);
    // Byte-identical to the seeded artifact so integrity checks pass and the
    // containment check itself is exercised.
    let outside_twin = runtime.data_root().join("outside-twin.json");
    std::fs::write(&outside_twin, br#"{"ok":true}"#).expect("write outside twin");
    swap_artifact_blob_for_symlink(&runtime, &task.id, &outside_twin);

    let response = request(
        runtime,
        &format!("/tasks/{}/artifacts/subdir/file.json", task.id),
    )
    .await;

    let status = response.status();
    let body = body_json(response).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unexpected response: {body}"
    );
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|m| m.contains("outside the task artifact directory")),
        "unexpected error body: {body}"
    );
}

/// Resolve the blob for a logical artifact path through its task manifest.
fn find_artifact_blob(
    root: &std::path::Path,
    task_id: &str,
    logical_path: &str,
) -> Option<std::path::PathBuf> {
    let workspaces = root.join("tasks/workspaces");
    for workspace in std::fs::read_dir(workspaces).ok()?.flatten() {
        let bundle_dir = workspace.path().join(task_id);
        let artifact_dir = bundle_dir.join(TASK_ARTIFACTS_DIR_NAME);
        let manifest_path = artifact_dir.join(TASK_ARTIFACT_MANIFEST_FILE_NAME);
        let Ok(raw_manifest) = std::fs::read_to_string(manifest_path) else {
            continue;
        };
        let Ok(manifest) = serde_yaml::from_str::<ArtifactManifestV2>(&raw_manifest) else {
            continue;
        };
        manifest.validate().ok()?;
        if let Some(artifact) = manifest
            .files
            .iter()
            .find(|artifact| artifact.path == logical_path)
        {
            return Some(artifact_dir.join(&artifact.blob));
        }
    }
    None
}

fn seed_task_with_status(
    runtime: &OrbitRuntime,
    title: &str,
    status: TaskStatus,
) -> orbit_core::Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}."),
            status: Some(status),
            ..Default::default()
        })
        .expect("seed task")
}

async fn patch_task(runtime: Arc<OrbitRuntime>, task_id: &str, body: Value) -> Response {
    router()
        .with_state(crate::state::DashboardState::single(runtime))
        .oneshot(patch_json(&format!("/tasks/{task_id}"), body))
        .await
        .expect("response")
}

/// [ORB-12245] The dashboard is an attributed operator surface, so its status
/// edits obey the same lifecycle table as every other one: delivered work is
/// not reopened from a PATCH, and completion still comes from `review`.
#[tokio::test]
async fn update_task_refuses_to_fabricate_or_reopen_a_completion() {
    let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));
    let task = seed_backlog_task(&runtime, "Dashboard status governance");

    let proposed = seed_task_with_status(
        &runtime,
        "Proposed dashboard status governance",
        TaskStatus::Proposed,
    );
    let skipped = patch_task(runtime.clone(), &proposed.id, json!({ "status": "done" })).await;
    assert_eq!(skipped.status(), StatusCode::BAD_REQUEST);

    let missing_plan = patch_task(
        runtime.clone(),
        &proposed.id,
        json!({ "status": "in-progress" }),
    )
    .await;
    assert_eq!(missing_plan.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_json(missing_plan).await["error"]
            .as_str()
            .expect("error message")
            .contains("execution plan")
    );

    let skipped = patch_task(runtime.clone(), &task.id, json!({ "status": "done" })).await;
    assert_eq!(skipped.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_json(skipped).await["error"]
            .as_str()
            .expect("error message")
            .contains("'done' is reachable only from 'review'")
    );

    for (status, body) in [
        (
            "in-progress",
            json!({ "status": "in-progress", "plan": "1) do it" }),
        ),
        ("review", json!({ "status": "review" })),
    ] {
        let response = patch_task(runtime.clone(), &task.id, body).await;
        assert_eq!(response.status(), StatusCode::OK, "{status}");
        assert_eq!(body_json(response).await["status"], json!(status));
    }

    let missing_summary = patch_task(runtime.clone(), &task.id, json!({ "status": "done" })).await;
    assert_eq!(missing_summary.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_json(missing_summary).await["error"]
            .as_str()
            .expect("error message")
            .contains("execution summary")
    );

    let completed = patch_task(
        runtime.clone(),
        &task.id,
        json!({ "status": "done", "execution_summary": "did it" }),
    )
    .await;
    assert_eq!(completed.status(), StatusCode::OK);
    assert_eq!(body_json(completed).await["status"], json!("done"));

    let reopened = patch_task(runtime.clone(), &task.id, json!({ "status": "backlog" })).await;
    assert_eq!(reopened.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_json(reopened).await["error"]
            .as_str()
            .expect("error message")
            .contains("regression_from")
    );

    let fetched = body_json(request_shared(runtime, &format!("/tasks/{}", task.id)).await).await;
    assert_eq!(fetched["status"], json!("done"));
    let history = fetched["history"].as_array().expect("history array");
    assert!(
        history
            .iter()
            .any(|entry| { entry["from_status"] == "review" && entry["to_status"] == "done" })
    );
}

/// Selectors the task's latest creation grant names, `None` when it has none.
fn creation_grant(runtime: &OrbitRuntime, task_id: &str) -> Option<Value> {
    let history = runtime.get_task_history(task_id).expect("task history");
    let grant = history
        .iter()
        .rev()
        .find(|entry| entry.event == "context_creation_authorized")?;
    let note: Value =
        serde_json::from_str(grant.note.as_deref().expect("grant note")).expect("grant JSON");
    Some(note["selectors"].clone())
}

/// The dashboard's `allow_missing_context` records the same durable creation
/// intent as the CLI and tool surfaces, and an ordinary dashboard edit
/// re-sends a declared target without it while staying strict for any other
/// missing selector.
#[tokio::test]
async fn dashboard_allow_missing_context_records_durable_creation_intent() {
    let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));
    let repo_root = runtime.paths().repo_root.clone();
    std::fs::write(repo_root.join("existing.rs"), "pub fn fixture() {}\n").expect("fixture file");

    let created = router()
        .with_state(crate::state::DashboardState::single(runtime.clone()))
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/tasks")
                .header(header::ORIGIN, "http://localhost:7878")
                .header(header::HOST, "localhost:7878")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "title": "Dashboard-declared module",
                        "description": "Creates src/future.rs.",
                        "complexity": "low",
                        "context_files": ["file:existing.rs", "file:src/future.rs"],
                        "allow_missing_context": true,
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(created.status(), StatusCode::OK);
    let id = body_json(created).await["id"]
        .as_str()
        .expect("task id")
        .to_string();
    assert_eq!(
        creation_grant(&runtime, &id),
        Some(json!(["file:src/future.rs"]))
    );

    let resent = patch_task(
        runtime.clone(),
        &id,
        json!({"context_files": ["file:src/future.rs", "file:existing.rs"]}),
    )
    .await;
    assert_eq!(resent.status(), StatusCode::OK);
    assert_eq!(
        creation_grant(&runtime, &id),
        Some(json!(["file:src/future.rs"]))
    );

    let undeclared = patch_task(
        runtime.clone(),
        &id,
        json!({"context_files": ["file:existing.rs", "file:src/other.rs"]}),
    )
    .await;
    assert_eq!(undeclared.status(), StatusCode::BAD_REQUEST);

    let declared = patch_task(
        runtime.clone(),
        &id,
        json!({
            "context_files": ["file:existing.rs", "file:src/other.rs"],
            "allow_missing_context": true,
        }),
    )
    .await;
    assert_eq!(declared.status(), StatusCode::OK);
    assert_eq!(
        creation_grant(&runtime, &id),
        Some(json!(["file:src/other.rs"])),
        "replacing the scope revokes the dropped target's grant"
    );
}
