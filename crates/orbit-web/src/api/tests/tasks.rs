//! Test-only allowlist: the original tests under orbit-cli passed the same lints via
//! the crate-level test harness configuration; duplicated here for the extracted crate.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Method, Request, StatusCode, header};
use axum::response::Response;
use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus, TaskType};
use orbit_types::task::TaskArtifact;
use orbit_types::task::{TaskRelation, TaskRelationType};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::super::router;
use super::test_support::body_json;

fn post_json(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::ORIGIN, "http://localhost:7878")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

fn patch_json(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::PATCH)
        .uri(uri)
        .header(header::ORIGIN, "http://localhost:7878")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

pub(super) fn seed_backlog_task(runtime: &OrbitRuntime, title: &str) -> orbit_core::Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}."),
            status: Some(TaskStatus::Backlog),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("seed backlog task")
}

pub(super) fn seed_task_with_artifact(runtime: &OrbitRuntime) -> orbit_core::Task {
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
            workspace_path: Some(".".to_string()),
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

fn seed_lock_task(
    runtime: &OrbitRuntime,
    title: &str,
    status: TaskStatus,
    context_files: Vec<&str>,
    job_run_id: Option<&str>,
) -> orbit_core::Task {
    for selector in &context_files {
        if let Some(path) = selector.strip_prefix("file:") {
            let path = runtime.data_root().join(path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create context parent");
            }
            std::fs::write(path, "").expect("write context file");
        }
    }
    let task = runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture for {title}."),
            status: Some(status),
            context_files: context_files.into_iter().map(str::to_string).collect(),
            workspace_path: Some(runtime.data_root().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .expect("create lock task");
    if let Some(job_run_id) = job_run_id {
        runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    job_run_id: Some(Some(job_run_id.to_string())),
                    ..Default::default()
                },
                Some("codex".to_string()),
                Some(TEST_CODEX_MODEL.to_string()),
            )
            .expect("set job run")
    } else {
        task
    }
}

async fn request(runtime: OrbitRuntime, uri: &str) -> axum::response::Response {
    request_shared(Arc::new(runtime), uri).await
}

/// `request` against an already-shared runtime, so one fixture can serve several
/// query variants without reseeding (ORB-10400's filter matrix).
pub(super) async fn request_shared(
    runtime: Arc<OrbitRuntime>,
    uri: &str,
) -> axum::response::Response {
    router()
        .with_state(crate::state::DashboardState::single(runtime))
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

#[tokio::test]
async fn task_locks_endpoint_matches_cli_json_contract() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let review = seed_lock_task(
        &runtime,
        "Review task",
        TaskStatus::Review,
        vec!["file:src/b.rs", "file:src/shared.rs"],
        Some("jrun-review"),
    );
    let in_progress = seed_lock_task(
        &runtime,
        "In progress task",
        TaskStatus::InProgress,
        vec!["file:src/a.rs", "file:src/shared.rs"],
        None,
    );
    seed_lock_task(
        &runtime,
        "Backlog task",
        TaskStatus::Backlog,
        vec!["file:src/ignored.rs"],
        None,
    );
    seed_lock_task(
        &runtime,
        "Done task",
        TaskStatus::Done,
        vec!["file:src/done.rs"],
        None,
    );
    let expected = crate::projections::task_locks_json(&runtime).expect("cli task locks json");

    let response = request(runtime, "/tasks/locks").await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body, expected);
    assert_eq!(
        body["locked_files"],
        json!(["file:src/a.rs", "file:src/b.rs", "file:src/shared.rs"])
    );
    assert_eq!(body["total_locked"], json!(3));
    assert_eq!(body["total_tasks"], json!(2));
    let by_task = body["by_task"].as_array().expect("by_task array");
    assert_eq!(by_task[0]["id"], json!(in_progress.id));
    assert_eq!(by_task[1]["id"], json!(review.id));
    assert!(
        !by_task
            .iter()
            .any(|task| task["status"] == json!("backlog"))
    );
    assert!(!by_task.iter().any(|task| task["status"] == json!("done")));
}

#[tokio::test]
async fn get_task_projects_artifact_manifest_without_content() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = seed_task_with_artifact(&runtime);

    let response = request(runtime, &format!("/tasks/{}", task.id)).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let artifacts = body["artifacts"].as_array().expect("artifacts array");
    assert_eq!(artifacts.len(), 1);
    let artifact = artifacts.first().expect("artifact");
    let object = artifact.as_object().expect("artifact object");
    let keys = object.keys().map(String::as_str).collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec![
            "created_at",
            "created_by",
            "media_type",
            "path",
            "sha256",
            "size_bytes"
        ]
    );
    assert_eq!(
        artifact["path"],
        Value::String("subdir/file.json".to_string())
    );
    assert_eq!(
        artifact["media_type"],
        Value::String("application/json".to_string())
    );
    assert_eq!(
        artifact["size_bytes"],
        Value::Number(serde_json::Number::from(11))
    );
    assert!(artifact.get("content").is_none());
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
async fn get_task_artifact_normalizes_text_plain_and_keeps_it_inline() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = seed_task_with_artifact_payload(
        &runtime,
        "notes/output.txt",
        "Text/Plain; Charset=UTF-8",
        b"plain artifact".to_vec(),
    );

    let response = request(
        runtime,
        &format!("/tasks/{}/artifacts/notes/output.txt", task.id),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&HeaderValue::from_static("text/plain"))
    );
    assert_eq!(
        response.headers().get("x-content-type-options"),
        Some(&HeaderValue::from_static("nosniff"))
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
    assert_eq!(&bytes[..], b"plain artifact");
}

#[tokio::test]
async fn get_task_artifact_downloads_html_instead_of_serving_inline() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = seed_task_with_artifact_payload(
        &runtime,
        "reports/payload.html",
        "text/html; charset=utf-8",
        br#"<script>fetch("/api/tasks")</script>"#.to_vec(),
    );

    let response = request(
        runtime,
        &format!("/tasks/{}/artifacts/reports/payload.html", task.id),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&HeaderValue::from_static("application/octet-stream"))
    );
    assert_eq!(
        response.headers().get("x-content-type-options"),
        Some(&HeaderValue::from_static("nosniff"))
    );
    assert_eq!(
        response.headers().get(header::CONTENT_DISPOSITION),
        Some(&HeaderValue::from_static("attachment"))
    );
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    assert_eq!(&bytes[..], br#"<script>fetch("/api/tasks")</script>"#);
}

#[tokio::test]
async fn get_task_artifact_downloads_script_media_types() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = seed_task_with_artifact_payload(
        &runtime,
        "reports/payload.js",
        "application/javascript",
        b"fetch('/api/tasks')".to_vec(),
    );

    let response = request(
        runtime,
        &format!("/tasks/{}/artifacts/reports/payload.js", task.id),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&HeaderValue::from_static("application/octet-stream"))
    );
    assert_eq!(
        response.headers().get("x-content-type-options"),
        Some(&HeaderValue::from_static("nosniff"))
    );
    assert_eq!(
        response.headers().get(header::CONTENT_DISPOSITION),
        Some(&HeaderValue::from_static("attachment"))
    );
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    assert_eq!(&bytes[..], b"fetch('/api/tasks')");
}

#[tokio::test]
async fn get_task_artifact_downloads_unknown_media_types() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = seed_task_with_artifact_payload(
        &runtime,
        "reports/payload.custom",
        "application/x-orbit-preview",
        b"custom artifact".to_vec(),
    );

    let response = request(
        runtime,
        &format!("/tasks/{}/artifacts/reports/payload.custom", task.id),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&HeaderValue::from_static("application/octet-stream"))
    );
    assert_eq!(
        response.headers().get("x-content-type-options"),
        Some(&HeaderValue::from_static("nosniff"))
    );
    assert_eq!(
        response.headers().get(header::CONTENT_DISPOSITION),
        Some(&HeaderValue::from_static("attachment"))
    );
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    assert_eq!(&bytes[..], b"custom artifact");
}

#[tokio::test]
async fn get_task_artifact_returns_not_found_for_missing_artifact() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = seed_task_with_artifact(&runtime);

    let response = request(
        runtime,
        &format!("/tasks/{}/artifacts/missing.json", task.id),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[test]
fn get_task_artifact_rejects_traversal_path() {
    tokio::runtime::Runtime::new()
        .expect("build tokio runtime")
        .block_on(async {
            let runtime = OrbitRuntime::in_memory().expect("build runtime");
            let task = seed_task_with_artifact(&runtime);

            let response = request(
                runtime,
                &format!("/tasks/{}/artifacts/subdir/%2e%2e/%2e%2e/escape", task.id),
            )
            .await;

            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        });
}

const SECRET_CONTENT: &str = "TOP-SECRET-OUTSIDE-ARTIFACT-ROOT";

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
    target: &std::path::Path,
) -> std::path::PathBuf {
    let blob_path = find_artifact_blob(&runtime.data_root(), "file.json")
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
    swap_artifact_blob_for_symlink(&runtime, &outside_twin);

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

/// ORB-10008: when the escaping symlink points at *different* content, the
/// bundle-read integrity verification (sha256/size against the manifest)
/// fails closed before the containment check is reached. The response is an
/// error either way and must never carry the out-of-root bytes.
#[cfg(unix)]
#[tokio::test]
async fn get_task_artifact_fails_closed_on_symlink_with_foreign_content() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = seed_task_with_artifact(&runtime);
    let secret_path = plant_secret_outside_artifact_root(&runtime);
    swap_artifact_blob_for_symlink(&runtime, &secret_path);

    let response = request(
        runtime,
        &format!("/tasks/{}/artifacts/subdir/file.json", task.id),
    )
    .await;

    let status = response.status();
    assert!(
        !status.is_success(),
        "tampered artifact must not be served: got {status}"
    );
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    assert!(
        !String::from_utf8_lossy(&bytes).contains(SECRET_CONTENT),
        "response leaked out-of-root file content"
    );
}

/// Depth-first search for a file named `name` under `root`, skipping nothing.
/// Test-only helper: the artifact bundle layout is an implementation detail of
/// orbit-store, so the test discovers the blob instead of hardcoding the path.
pub(super) fn find_artifact_blob(root: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_artifact_blob(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|f| f == name)
            && path.components().any(|c| c.as_os_str() == "artifacts")
        {
            return Some(path);
        }
    }
    None
}

/// Exercises PATCH /api/tasks/:id with the dashboard's emitted spelling {"status":"in-progress"}
/// against a backlog task. Before the serde alias fix this produced 422 on JSON extraction;
/// now it succeeds and the response continues to surface status as the display form "in-progress".
#[tokio::test]
async fn patch_api_accepts_in_progress_hyphen_from_dashboard_and_returns_in_progress() {
    use axum::Router;
    use axum::http::{Method, Request, header};

    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let created = runtime
        .add_task(TaskAddParams {
            title: "Dashboard status update test".to_string(),
            description: "backlog task to be moved via PATCH with hyphen spelling".to_string(),
            status: Some(TaskStatus::Backlog),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("seed backlog task");
    let task_id = created.id;

    // Wrap to exercise the literal /api/tasks path per acceptance criteria
    let app = Router::new()
        .nest("/api", router())
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)));

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::PATCH)
                .uri(format!("/api/tasks/{}", task_id))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ORIGIN, "http://localhost:7878")
                .body(Body::from(r#"{"status":"in-progress"}"#))
                .expect("build patch request"),
        )
        .await
        .expect("oneshot");

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "PATCH with in-progress must succeed (not 422)"
    );

    let body = body_json(response).await;
    assert_eq!(body["id"], serde_json::json!(task_id));
    assert_eq!(
        body["status"],
        serde_json::json!("in-progress"),
        "response must continue to expose dashboard display spelling"
    );
}

#[tokio::test]
async fn patch_api_persists_pr_status_with_status_and_execution_summary() {
    use axum::http::{Method, Request, header};

    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = runtime
        .add_task(TaskAddParams {
            title: "Dashboard PR status update".to_string(),
            description: "An HTTP update must persist every supplied field.".to_string(),
            status: Some(TaskStatus::InProgress),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("seed in-progress task");

    let response = router()
        .with_state(crate::state::DashboardState::single(Arc::new(
            runtime.clone(),
        )))
        .oneshot(
            Request::builder()
                .method(Method::PATCH)
                .uri(format!("/tasks/{}", task.id))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ORIGIN, "http://localhost:7878")
                .body(Body::from(
                    json!({
                        "pr_status": "approved",
                        "execution_summary": "Implemented and verified.",
                        "status": "review",
                    })
                    .to_string(),
                ))
                .expect("build patch request"),
        )
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["pr_status"], json!("approved"));
    assert_eq!(
        body["execution_summary"],
        json!("Implemented and verified.")
    );
    assert_eq!(body["status"], json!("review"));

    let persisted = runtime.get_task(&task.id).expect("read updated task");
    assert_eq!(persisted.pr_status.as_deref(), Some("approved"));
    assert_eq!(persisted.execution_summary, "Implemented and verified.");
    assert_eq!(persisted.status, TaskStatus::Review);
}

#[tokio::test]
async fn patch_api_persists_complexity_and_omission_preserves_it() {
    use axum::http::{Method, Request, header};

    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = seed_backlog_task(&runtime, "Dashboard complexity update");
    let app = router().with_state(crate::state::DashboardState::single(Arc::new(
        runtime.clone(),
    )));

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::PATCH)
                .uri(format!("/tasks/{}", task.id))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ORIGIN, "http://localhost:7878")
                .body(Body::from(json!({ "complexity": "medium" }).to_string()))
                .expect("build complexity patch request"),
        )
        .await
        .expect("complexity patch response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["complexity"], json!("medium"));
    assert_eq!(
        runtime
            .get_task(&task.id)
            .expect("read updated task")
            .complexity,
        Some(TaskComplexity::Medium)
    );

    let response = router()
        .with_state(crate::state::DashboardState::single(Arc::new(
            runtime.clone(),
        )))
        .oneshot(
            Request::builder()
                .method(Method::PATCH)
                .uri(format!("/tasks/{}", task.id))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ORIGIN, "http://localhost:7878")
                .body(Body::from(
                    json!({ "title": "Retitled without complexity" }).to_string(),
                ))
                .expect("build omitted complexity patch request"),
        )
        .await
        .expect("omitted complexity patch response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["complexity"], json!("medium"));
    assert_eq!(
        runtime
            .get_task(&task.id)
            .expect("read preserved complexity")
            .complexity,
        Some(TaskComplexity::Medium)
    );
}

/// Contract test: /tasks projection (and /tasks/:id) must include `complexity` string
/// when TaskComplexity is set on the task (low/medium/hard). Null complexity omits the key
/// or yields null (per current projection); this test asserts presence for a hard task.
#[tokio::test]
async fn list_tasks_includes_complexity_when_set() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let with_complexity = runtime
        .add_task(TaskAddParams {
            title: "Hard task for complexity display".to_string(),
            description: "Task with explicit complexity for dashboard test.".to_string(),
            status: Some(TaskStatus::Backlog),
            workspace_path: Some(".".to_string()),
            complexity: TaskComplexity::Hard,
            ..Default::default()
        })
        .expect("seed task with complexity");
    // Also seed one without to ensure list works
    let _without = runtime
        .add_task(TaskAddParams {
            title: "Plain task no complexity".to_string(),
            description: "no complexity set".to_string(),
            status: Some(TaskStatus::Backlog),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("seed plain task");

    let response = request(runtime, "/tasks").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let arr = task_items(&body);
    let found = arr
        .iter()
        .find(|t| t["id"] == serde_json::json!(with_complexity.id))
        .expect("task present in /tasks");
    assert_eq!(
        found.get("complexity"),
        Some(&serde_json::json!("hard")),
        "complexity must be projected as string for dashboard"
    );
}

#[tokio::test]
async fn task_api_projects_orchestrator_separately_from_execution_crew() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = runtime
        .add_task(TaskAddParams {
            title: "Separate task ownership fields".to_string(),
            description: "Project execution selection and orchestration attribution.".to_string(),
            status: Some(TaskStatus::Backlog),
            crew: Some("sol".to_string()),
            orchestrator: Some("terra".to_string()),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("seed attributed task");

    let response = request(runtime.clone(), "/tasks").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let listed = task_items(&body)
        .iter()
        .find(|candidate| candidate["id"] == task.id)
        .expect("task present in list");
    assert_eq!(listed["crew"], "sol");
    assert_eq!(listed["orchestrator"], "terra");
    assert_eq!(listed["resolved_crew"], "sol");

    let response = request(runtime, &format!("/tasks/{}", task.id)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let detail = body_json(response).await;
    assert_eq!(detail["crew"], "sol");
    assert_eq!(detail["orchestrator"], "terra");
}

fn seed_task_with_status(
    runtime: &OrbitRuntime,
    title: &str,
    status: TaskStatus,
) -> orbit_core::Task {
    seed_filterable_task(runtime, title, status, Vec::new(), None)
}

/// Seed a task with the attributes `GET /tasks` can filter on (ORB-10400).
fn seed_filterable_task(
    runtime: &OrbitRuntime,
    title: &str,
    status: TaskStatus,
    tags: Vec<&str>,
    task_type: Option<TaskType>,
) -> orbit_core::Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}."),
            status: Some(status),
            workspace_path: Some(".".to_string()),
            tags: tags.into_iter().map(str::to_string).collect(),
            task_type,
            ..Default::default()
        })
        .expect("seed task")
}

/// `GET /tasks` answers the ORB-10400 page envelope
/// `{ items, total, limit, truncated }`; unwrap `items` for row assertions.
fn task_items(body: &Value) -> &Vec<Value> {
    body["items"].as_array().expect("items is an array")
}

fn task_ids(body: &Value) -> Vec<&str> {
    task_items(body)
        .iter()
        .map(|row| row["id"].as_str().expect("task id"))
        .collect()
}

/// ORB-10310: `GET /tasks` is status-neutral — `done` and `archived` tasks
/// (previously excluded by the hard-coded dashboard status subset) are now
/// discoverable, and the list is ordered newest-first.
#[tokio::test]
async fn list_tasks_includes_done_and_archived_statuses_newest_first() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let proposed = seed_task_with_status(&runtime, "Proposed task", TaskStatus::Proposed);
    let done = seed_task_with_status(&runtime, "Done task", TaskStatus::Done);
    let to_archive = seed_task_with_status(&runtime, "Archived task", TaskStatus::Backlog);
    runtime.archive_task(&to_archive.id).expect("archive task");

    let response = request(runtime, "/tasks").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let rows = task_items(&body);

    for expected in [&proposed.id, &done.id, &to_archive.id] {
        assert!(
            rows.iter().any(|row| row["id"].as_str() == Some(expected)),
            "task {expected} must be discoverable regardless of status: {rows:?}"
        );
    }
    let archived_row = rows
        .iter()
        .find(|row| row["id"].as_str() == Some(to_archive.id.as_str()))
        .expect("archived task present");
    assert_eq!(archived_row["status"], json!("archived"));

    let created_ats = rows
        .iter()
        .map(|row| row["created_at"].as_str().expect("created_at").to_string())
        .collect::<Vec<_>>();
    for pair in created_ats.windows(2) {
        assert!(
            pair[0] >= pair[1],
            "tasks must be ordered newest-first: {created_ats:?}"
        );
    }
}

/// ORB-10310: `GET /tasks` bounds the response to the default limit (50),
/// keeping the newest matching tasks.
#[tokio::test]
async fn list_tasks_is_bounded_to_default_limit() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let mut oldest = None;
    for index in 0..55 {
        let task = seed_task_with_status(
            &runtime,
            &format!("Bounded dashboard task {index:02}"),
            TaskStatus::Backlog,
        );
        if index == 0 {
            oldest = Some(task.id);
        }
    }

    let response = request(runtime, "/tasks").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let rows = task_items(&body);
    assert_eq!(rows.len(), 50, "default limit bounds the response to 50");
    let oldest = oldest.expect("seeded at least one task");
    assert!(
        !rows
            .iter()
            .any(|row| row["id"].as_str() == Some(oldest.as_str())),
        "the oldest task must fall outside the newest 50"
    );
    // ORB-10400: the page metadata is what tells a client the window is partial.
    assert_eq!(
        body["total"],
        json!(55),
        "total counts every match pre-limit"
    );
    assert_eq!(body["limit"], json!(50));
    assert_eq!(body["truncated"], json!(true));
}

/// ORB-10400: every filter predicate is applied *before* the limit. The
/// regression fixture puts the only matching task beyond the first
/// `DEFAULT_TASK_LIST_LIMIT` unfiltered rows — the exact shape that made
/// matches unrecoverable to bridge, which could only filter the already
/// truncated array client-side.
#[tokio::test]
async fn list_tasks_filters_before_limit() {
    let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));
    let buried = seed_filterable_task(
        &runtime,
        "Buried proposed task",
        TaskStatus::Proposed,
        vec!["auto-task:qa-sweep"],
        None,
    );
    // 60 newer tasks bury it well outside the unfiltered 50-row window.
    for index in 0..60 {
        seed_task_with_status(
            &runtime,
            &format!("Newer backlog task {index:02}"),
            TaskStatus::Backlog,
        );
    }

    let unfiltered = body_json(request_shared(runtime.clone(), "/tasks").await).await;
    assert!(
        !task_ids(&unfiltered).contains(&buried.id.as_str()),
        "fixture must bury the match outside the unfiltered window"
    );

    for uri in [
        "/tasks?status=proposed",
        "/tasks?tag=auto-task:qa-sweep",
        "/tasks?status=proposed&tag=auto-task:qa-sweep",
    ] {
        let response = request_shared(runtime.clone(), uri).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let body = body_json(response).await;
        assert_eq!(
            task_ids(&body),
            vec![buried.id.as_str()],
            "{uri} must return the buried match"
        );
        assert_eq!(body["total"], json!(1), "{uri}");
        assert_eq!(
            body["truncated"],
            json!(false),
            "{uri} is a complete result, not a truncated window"
        );
    }
}

/// ORB-10400: a colon-bearing tag is matched as one whole tag through Orbit's
/// `normalize_task_tags`/`task_matches_tags` semantics — never split on `:` into
/// `auto-task` + `qa-sweep`. Only `,` separates values.
#[tokio::test]
async fn list_tasks_matches_colon_bearing_tag_exactly() {
    let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));
    let sweep = seed_filterable_task(
        &runtime,
        "Sweep task",
        TaskStatus::Backlog,
        vec!["auto-task:qa-sweep"],
        None,
    );
    // Decoys that would match if the tag were split on `:` or matched by prefix.
    seed_filterable_task(
        &runtime,
        "Other auto task",
        TaskStatus::Backlog,
        vec!["auto-task"],
        None,
    );
    seed_filterable_task(
        &runtime,
        "Other sweep task",
        TaskStatus::Backlog,
        vec!["qa-sweep"],
        None,
    );
    seed_filterable_task(
        &runtime,
        "Longer sweep tag",
        TaskStatus::Backlog,
        vec!["auto-task:qa-sweep-extended"],
        None,
    );

    let body =
        body_json(request_shared(runtime.clone(), "/tasks?tag=auto-task:qa-sweep").await).await;
    assert_eq!(task_ids(&body), vec![sweep.id.as_str()]);
    assert_eq!(body["total"], json!(1));

    // Percent-encoded `%3A` is the same tag, and tags are normalized (trimmed,
    // lowercased) exactly as the CLI normalizes `--tag`.
    let encoded =
        body_json(request_shared(runtime.clone(), "/tasks?tag=%20AUTO-TASK%3Aqa-sweep").await)
            .await;
    assert_eq!(task_ids(&encoded), vec![sweep.id.as_str()]);

    // Repeated `tag` is AND, so a tag the task lacks yields a complete empty
    // result — distinguishable from truncation by `total: 0`.
    let anded =
        body_json(request_shared(runtime, "/tasks?tag=auto-task:qa-sweep&tag=qa-sweep").await)
            .await;
    assert!(task_items(&anded).is_empty());
    assert_eq!(anded["total"], json!(0));
    assert_eq!(anded["truncated"], json!(false));
}

/// ORB-10400: status is OR across values (repeated and/or comma-separated),
/// type is an equality filter, and `limit` overrides the default — matching
/// `orbit task list` semantics for the same flags.
#[tokio::test]
async fn list_tasks_honors_status_type_and_limit_filters() {
    let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));
    let bug = seed_filterable_task(
        &runtime,
        "Bug task",
        TaskStatus::Proposed,
        Vec::new(),
        Some(TaskType::Bug),
    );
    let feature = seed_filterable_task(
        &runtime,
        "Feature task",
        TaskStatus::Review,
        Vec::new(),
        Some(TaskType::Feature),
    );
    let chore = seed_filterable_task(
        &runtime,
        "Chore task",
        TaskStatus::Backlog,
        Vec::new(),
        Some(TaskType::Chore),
    );

    let csv =
        body_json(request_shared(runtime.clone(), "/tasks?status=proposed,review").await).await;
    let mut ids = task_ids(&csv);
    ids.sort_unstable();
    let mut expected = vec![bug.id.as_str(), feature.id.as_str()];
    expected.sort_unstable();
    assert_eq!(ids, expected, "status is OR across values");

    let repeated =
        body_json(request_shared(runtime.clone(), "/tasks?status=proposed&status=review").await)
            .await;
    assert_eq!(
        task_items(&repeated).len(),
        2,
        "repeated status accumulates"
    );

    let typed = body_json(request_shared(runtime.clone(), "/tasks?type=chore").await).await;
    assert_eq!(task_ids(&typed), vec![chore.id.as_str()]);

    // Filters compose: a type that no `proposed` task carries is empty.
    let composed =
        body_json(request_shared(runtime.clone(), "/tasks?status=proposed&type=chore").await).await;
    assert!(task_items(&composed).is_empty());
    assert_eq!(composed["total"], json!(0));

    // `limit` truncates the filtered set and is reported back with `truncated`.
    let limited = body_json(request_shared(runtime, "/tasks?limit=1").await).await;
    assert_eq!(task_items(&limited).len(), 1);
    assert_eq!(limited["limit"], json!(1));
    assert_eq!(limited["total"], json!(3));
    assert_eq!(limited["truncated"], json!(true));
}

/// ORB-10400: an unparseable filter is a loud 400. Silently dropping it would
/// answer an unfiltered page that a client cannot distinguish from a real match
/// set — the failure mode this task exists to remove.
#[tokio::test]
async fn list_tasks_rejects_invalid_filter_values() {
    for uri in [
        "/tasks?status=nonsense",
        "/tasks?type=nonsense",
        "/tasks?limit=0",
        "/tasks?limit=many",
    ] {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        let response = request(runtime, uri).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        let body = body_json(response).await;
        assert!(
            body["error"].as_str().is_some_and(|msg| !msg.is_empty()),
            "{uri} must explain the rejected value: {body:?}"
        );
    }
}

/// ORB-10400: empty filter values behave like an omitted filter (matching the
/// `?workspace=` convention), and the `?workspace=` selector itself — which
/// shares this query string — is never mistaken for a task filter.
#[tokio::test]
async fn list_tasks_ignores_empty_values_and_the_workspace_selector() {
    let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));
    seed_task_with_status(&runtime, "Only task", TaskStatus::Backlog);

    for uri in [
        "/tasks?status=&tag=&type=&limit=",
        "/tasks?workspace=default",
    ] {
        let response = request_shared(runtime.clone(), uri).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let body = body_json(response).await;
        assert_eq!(task_items(&body).len(), 1, "{uri}");
        assert_eq!(body["limit"], json!(50), "{uri} keeps the default limit");
        assert_eq!(body["truncated"], json!(false), "{uri}");
    }
}

/// ORB-00042: `workspace` is a selector and lives in the query string, never
/// the body. A stray `workspace` body key (the historical bridge mis-key) must
/// be a loud 400 pointing at `?workspace=`, not silently dropped.
#[tokio::test]
async fn create_task_rejects_stray_workspace_body_key() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let response = router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/tasks")
                .header(header::ORIGIN, "http://localhost:7878")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "title": "mis-keyed",
                        "description": "workspace does not belong in the body",
                        "complexity": "low",
                        "workspace": "ws_polaris",
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    let message = body["error"].as_str().expect("error message");
    assert!(message.contains("workspace"), "names the offending key");
    assert!(message.contains("?workspace="), "points at the query param");
}

#[tokio::test]
async fn create_task_only_accepts_creation_legal_statuses_and_ignores_comment() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let app = router().with_state(crate::state::DashboardState::single(Arc::new(runtime)));

    let rejected = app
        .clone()
        .oneshot(post_json(
            "/tasks",
            json!({
                "title": "illegal status",
                "description": "must not start done",
                "complexity": "low",
                "status": "done",
            }),
        ))
        .await
        .expect("response");
    assert!(rejected.status().is_client_error());

    let created = app
        .oneshot(post_json(
            "/tasks",
            json!({
                "title": "legal status",
                "description": "starts in backlog",
                "complexity": "medium",
                "status": "backlog",
                "comment": "retired create input",
            }),
        ))
        .await
        .expect("response");
    assert_eq!(created.status(), StatusCode::OK);
    let body = body_json(created).await;
    assert_eq!(body["status"], json!("backlog"));
    assert_eq!(body["comments"], json!([]));
}

/// ORB-10253: `POST /api/tasks` accepts a `relations` array of {type, target}
/// objects and persists them on the created task (mirroring the native MCP
/// wire shape). Previously `create_task_action` hardcoded `relations: Vec::new()`.
#[tokio::test]
async fn create_task_persists_relations_from_body() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let target = seed_backlog_task(&runtime, "Relation target");
    let app = router().with_state(crate::state::DashboardState::single(Arc::new(runtime)));

    let response = app
        .oneshot(post_json(
            "/tasks",
            json!({
                "title": "task with relations",
                "description": "records a typed relation on create",
                "complexity": "medium",
                "status": "backlog",
                "relations": [{ "type": "related_to", "target": target.id }],
            }),
        ))
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(
        body["relations"],
        json!([{ "type": "related_to", "target": target.id }])
    );
}

/// ORB-10253: a present `relations` array on `PATCH /api/tasks/:id` replaces the
/// existing relation set. Previously `update_task_action` hardcoded
/// `relations: None`, so relations could never be edited over HTTP.
#[tokio::test]
async fn update_task_replaces_relation_set() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let first = seed_backlog_task(&runtime, "First relation target");
    let second = seed_backlog_task(&runtime, "Second relation target");
    let task = runtime
        .add_task(TaskAddParams {
            title: "Task with an initial relation".to_string(),
            description: "relation set will be replaced over HTTP".to_string(),
            status: Some(TaskStatus::Backlog),
            workspace_path: Some(".".to_string()),
            relations: vec![TaskRelation {
                relation_type: TaskRelationType::RelatedTo,
                target: first.id.clone(),
            }],
            ..Default::default()
        })
        .expect("seed task with relation");
    let app = router().with_state(crate::state::DashboardState::single(Arc::new(runtime)));

    let response = app
        .oneshot(patch_json(
            &format!("/tasks/{}", task.id),
            json!({ "relations": [{ "type": "related_to", "target": second.id }] }),
        ))
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(
        body["relations"],
        json!([{ "type": "related_to", "target": second.id }]),
        "present relations array replaces the set (drops the prior target)"
    );
}

/// ORB-10253: an empty `relations` array on `PATCH /api/tasks/:id` clears the
/// relation set, while an absent field leaves it unchanged (covered implicitly
/// by every other PATCH test that omits `relations`).
#[tokio::test]
async fn update_task_clears_relations_with_empty_array() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let target = seed_backlog_task(&runtime, "Relation target to be cleared");
    let task = runtime
        .add_task(TaskAddParams {
            title: "Task whose relations get cleared".to_string(),
            description: "empty array clears the relation set".to_string(),
            status: Some(TaskStatus::Backlog),
            workspace_path: Some(".".to_string()),
            relations: vec![TaskRelation {
                relation_type: TaskRelationType::RelatedTo,
                target: target.id.clone(),
            }],
            ..Default::default()
        })
        .expect("seed task with relation");
    let app = router().with_state(crate::state::DashboardState::single(Arc::new(runtime)));

    let response = app
        .oneshot(patch_json(
            &format!("/tasks/{}", task.id),
            json!({ "relations": [] }),
        ))
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["relations"], json!([]), "empty array clears relations");
}

/// ORB-10253: an invalid relation target must surface Orbit's own validation
/// error as a 4xx — never a silent drop. A malformed target fails
/// `validate_task_relations_for_source` and reaches the client through
/// `map_runtime_error` as a 400.
#[tokio::test]
async fn create_task_rejects_invalid_relation_target() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let app = router().with_state(crate::state::DashboardState::single(Arc::new(runtime)));

    let response = app
        .oneshot(post_json(
            "/tasks",
            json!({
                "title": "task with a bad relation",
                "description": "malformed relation target must be rejected",
                "complexity": "low",
                "status": "backlog",
                "relations": [{ "type": "related_to", "target": "not-a-task-id" }],
            }),
        ))
        .await
        .expect("response");

    assert!(
        response.status().is_client_error(),
        "invalid relation target must be a 4xx, got {}",
        response.status()
    );
    let body = body_json(response).await;
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|m| m.contains("not-a-task-id")),
        "error must carry Orbit's validation text: {body}"
    );
}

/// A runtime whose ambient actor is an *agent* — what the dashboard server
/// process looks like when it runs inside a managed Orbit run.
///
/// The agent-identity env is cleared first so the injected actor is the only
/// agent identity in play however the suite was launched (ORB-10350), then set
/// deterministically. The comment write path is asserted against this runtime
/// because it is the case that used to leak a model constant into `by`.
fn runtime_with_ambient_agent_identity() -> OrbitRuntime {
    let _env =
        orbit_common::test_env::unset(orbit_common::test_env::AGENT_IDENTITY_ENV.iter().copied());
    OrbitRuntime::in_memory()
        .expect("build runtime")
        .with_actor(orbit_core::ActorIdentity::agent("ambient-server-model"))
}

async fn post_task_comment(runtime: OrbitRuntime, task_id: &str, body: Value) -> Response {
    router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(post_json(&format!("/tasks/{task_id}/comments"), body))
        .await
        .expect("response")
}

/// Historical independent-review records are now ordinary comments. They must
/// remain loadable and render unchanged through the dashboard task response.
#[tokio::test]
async fn historical_review_record_renders_as_an_opaque_task_comment() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = seed_backlog_task(&runtime, "commentable task");
    let historical = r#"[independent-review]
{"candidate_head_sha":"abc123","verdict":"approve","criteria":[{"index":1,"verdict":"met"}]}"#;

    let response =
        post_task_comment(runtime.clone(), &task.id, json!({ "message": historical })).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(
        body["comments"][0]["message"].as_str(),
        Some(historical),
        "the response must echo the stored comment: {body}"
    );

    // Durable across the request: re-read from the task's comment store, which
    // is what a subsequent page load renders from.
    let stored = runtime.get_task_comments(&task.id).expect("task comments");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].message, historical);
    assert!(
        runtime
            .get_task(&task.id)
            .expect("task")
            .description
            .contains("Fixture task"),
        "commenting must not disturb the task record itself"
    );
}

/// ORB-10444: the recorded author is a human identity even when the server
/// process's ambient actor is an agent model. This is the regression the write
/// path exists to prevent — an operator's note attributed to a model constant.
#[tokio::test]
async fn task_comment_author_is_human_not_the_ambient_model() {
    let runtime = runtime_with_ambient_agent_identity();
    let task = seed_backlog_task(&runtime, "ambient identity task");

    let response =
        post_task_comment(runtime.clone(), &task.id, json!({ "message": "ship it" })).await;

    assert_eq!(response.status(), StatusCode::OK);
    let stored = runtime.get_task_comments(&task.id).expect("task comments");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].by, "human");
    assert_ne!(stored[0].by, "ambient-server-model");
}

/// A caller-supplied author is honored only when it is a human identity: an
/// agent family or a model constant collapses back to the human label rather
/// than being recorded as the comment's author.
#[tokio::test]
async fn task_comment_author_rejects_model_constants_but_keeps_human_names() {
    let runtime = runtime_with_ambient_agent_identity();

    for author in [
        "claude",
        "codex",
        TEST_CODEX_MODEL,
        "claude-opus-4",
        "system",
    ] {
        let task = seed_backlog_task(&runtime, "authored comment");
        let response = post_task_comment(
            runtime.clone(),
            &task.id,
            json!({ "message": "note", "author": author }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let stored = runtime.get_task_comments(&task.id).expect("task comments");
        assert_eq!(
            stored[0].by, "human",
            "`{author}` is a model/agent identity and must not author a comment"
        );
    }

    let task = seed_backlog_task(&runtime, "operator comment");
    let response = post_task_comment(
        runtime.clone(),
        &task.id,
        json!({ "message": "note", "author": "operator" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let stored = runtime.get_task_comments(&task.id).expect("task comments");
    assert_eq!(stored[0].by, "operator");
}

async fn patch_task(runtime: Arc<OrbitRuntime>, task_id: &str, body: Value) -> Response {
    router()
        .with_state(crate::state::DashboardState::single(runtime))
        .oneshot(patch_json(&format!("/tasks/{task_id}"), body))
        .await
        .expect("response")
}

async fn post_task(runtime: Arc<OrbitRuntime>, body: Value) -> Response {
    router()
        .with_state(crate::state::DashboardState::single(runtime))
        .oneshot(post_json("/tasks", body))
        .await
        .expect("response")
}

/// ORB-10648: `priority` is now a declared update field. It was undeclared
/// while the record layer could persist it, so a re-prioritization was dropped
/// by serde and the endpoint still answered `200` with the old priority — the
/// exact reproduction in the filing. It must be visible in the immediate PATCH
/// response and on the next read.
#[tokio::test]
async fn update_task_applies_priority_and_reads_it_back() {
    let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));
    let task = seed_backlog_task(&runtime, "Re-prioritized task");
    assert_eq!(task.priority.to_string(), "medium", "fixture precondition");

    let response = patch_task(runtime.clone(), &task.id, json!({ "priority": "high" })).await;

    assert_eq!(response.status(), StatusCode::OK);
    let patched = body_json(response).await;
    assert_eq!(patched["priority"], json!("high"));
    let fetched = body_json(request_shared(runtime, &format!("/tasks/{}", task.id)).await).await;
    assert_eq!(fetched["priority"], json!("high"), "read-back agrees");
}

/// ORB-10648: every field a partial update reports back is genuinely persisted.
/// This is the regression guard for the whole class — a field that the body
/// deserializes but the handler never plumbs would show up here as a response
/// value that the next read contradicts.
#[tokio::test]
async fn update_task_response_fields_survive_a_read_back() {
    let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));
    let task = seed_backlog_task(&runtime, "Partially updated task");

    let response = patch_task(
        runtime.clone(),
        &task.id,
        json!({
            "title": "Renamed by PATCH",
            "plan": "1. land the fix",
            "execution_summary": "not yet run",
            "tags": ["alpha", "beta"],
            "priority": "critical",
            "complexity": "hard",
            "task_type": "bug",
            "context_files": ["file:src/lib.rs"],
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let patched = body_json(response).await;
    // (submitted key, projected key, expected value)
    let expected = [
        ("title", "title", json!("Renamed by PATCH")),
        ("plan", "plan", json!("1. land the fix")),
        (
            "execution_summary",
            "execution_summary",
            json!("not yet run"),
        ),
        ("tags", "tags", json!(["alpha", "beta"])),
        ("priority", "priority", json!("critical")),
        ("complexity", "complexity", json!("hard")),
        ("task_type", "type", json!("bug")),
        ("context_files", "context_files", json!(["file:src/lib.rs"])),
    ];
    let fetched = body_json(request_shared(runtime, &format!("/tasks/{}", task.id)).await).await;
    for (submitted, projected, value) in expected {
        assert_eq!(
            patched[projected], value,
            "`{submitted}` must be applied in the PATCH response"
        );
        assert_eq!(
            fetched[projected], patched[projected],
            "`{submitted}` must still be there on read-back"
        );
    }
}

/// ORB-10648: the previously-false-positive case. An undeclared key used to be
/// discarded by serde while the endpoint answered `200` with the task JSON, so
/// a caller comparing its request against that response could report the field
/// as applied. It is now a 400 that names the key, and — the all-or-nothing
/// half of the contract — the declared fields in the same body do not land
/// either.
#[tokio::test]
async fn update_task_rejects_an_undeclared_body_field_without_partial_application() {
    let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));
    let task = seed_backlog_task(&runtime, "Untouched task");

    let response = patch_task(
        runtime.clone(),
        &task.id,
        json!({ "title": "must not land", "urgency": "high" }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let message = body_json(response).await["error"]
        .as_str()
        .expect("error message")
        .to_string();
    assert!(message.contains("urgency"), "names the offending key");
    let fetched = body_json(request_shared(runtime, &format!("/tasks/{}", task.id)).await).await;
    assert_eq!(
        fetched["title"],
        json!("Untouched task"),
        "a rejected body applies nothing"
    );
}

#[tokio::test]
async fn update_task_rejects_required_tools_as_creation_only() {
    let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));
    let task = seed_backlog_task(&runtime, "Immutable tool authority");

    let response = patch_task(
        runtime.clone(),
        &task.id,
        json!({ "required_tools": ["proc.spawn"] }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let message = body_json(response).await["error"]
        .as_str()
        .expect("error message")
        .to_string();
    assert!(message.contains("required_tools"), "{message}");
    assert!(
        runtime
            .get_task(&task.id)
            .expect("read task")
            .required_tools
            .is_empty()
    );
}

#[tokio::test]
async fn create_task_requires_assessed_complexity() {
    let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));

    let missing = post_task(
        runtime.clone(),
        json!({
            "title": "missing complexity",
            "description": "create must require an assessed value",
        }),
    )
    .await;
    assert!(
        missing.status().is_client_error(),
        "missing complexity must fail create"
    );

    let unassessed = post_task(
        runtime.clone(),
        json!({
            "title": "unassessed create",
            "description": "automated non-answer is not a human create value",
            "complexity": "unassessed",
        }),
    )
    .await;
    assert_eq!(unassessed.status(), StatusCode::BAD_REQUEST);
    let message = body_json(unassessed).await["error"]
        .as_str()
        .expect("error message")
        .to_string();
    assert!(
        message.contains("assessed") || message.contains("unassessed"),
        "error should reject unassessed: {message}"
    );
    assert!(
        runtime.list_tasks().expect("list tasks").is_empty(),
        "rejected creates write nothing"
    );
}

/// ORB-10648: the same contract on create — `POST /api/tasks` no longer absorbs
/// keys it cannot apply. The tailored `workspace` diagnostic (ORB-00042) still
/// wins for that key, since it is a declared trap field.
#[tokio::test]
async fn create_task_rejects_an_undeclared_body_field() {
    let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));

    let response = post_task(
        runtime.clone(),
        json!({
            "title": "undeclared input",
            "description": "carries a key the endpoint cannot apply",
            "complexity": "low",
            "assignee": "someone",
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let message = body_json(response).await["error"]
        .as_str()
        .expect("error message")
        .to_string();
    assert!(message.contains("assignee"), "names the offending key");
    assert!(
        runtime.list_tasks().expect("list tasks").is_empty(),
        "a rejected create writes nothing"
    );
}

/// ORB-10648 / L-0103: attribution is `model`-only. An `agent` key is a caller
/// bug, so both task bodies refuse it the way the native tool surface does
/// rather than dropping it and attributing the write to the ambient identity.
#[tokio::test]
async fn task_bodies_reject_the_retired_agent_attribution_key() {
    let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));
    let task = seed_backlog_task(&runtime, "Attribution task");

    for response in [
        patch_task(
            runtime.clone(),
            &task.id,
            json!({ "status": "someday", "agent": "codex" }),
        )
        .await,
        post_task(
            runtime.clone(),
            json!({
                "title": "agent attribution",
                "description": "agent is not an accepted key",
                "complexity": "low",
                "agent": "codex",
            }),
        )
        .await,
    ] {
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let message = body_json(response).await["error"]
            .as_str()
            .expect("error message")
            .to_string();
        assert!(message.contains("agent"), "names the offending key");
        assert!(message.contains("model"), "points at the accepted field");
    }
}

/// ORB-10648: `model` was forwarded by callers, deserialized by neither task
/// body, and still reported as applied — an affirmative false "applied". It is
/// now the write's provenance (as on `POST /api/frictions`),
/// so the report is true: the comment this update appends is authored by the
/// supplied model rather than by the ambient identity.
#[tokio::test]
async fn update_task_applies_model_provenance() {
    let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));
    let task = seed_backlog_task(&runtime, "Attributed task");

    let response = patch_task(
        runtime.clone(),
        &task.id,
        json!({ "comment": "attributed note", "model": TEST_CODEX_MODEL }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let stored = runtime.get_task_comments(&task.id).expect("task comments");
    assert_eq!(stored.len(), 1);
    assert!(
        stored[0].by.contains(TEST_CODEX_MODEL),
        "supplied model must author the write, got `{}`",
        stored[0].by
    );
}

/// An empty (or whitespace-only) comment is a clean 400 rather than an empty
/// row in the thread.
#[tokio::test]
async fn task_comment_endpoint_rejects_blank_messages() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = seed_backlog_task(&runtime, "blank comment task");

    let response = post_task_comment(runtime.clone(), &task.id, json!({ "message": "   " })).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        runtime
            .get_task_comments(&task.id)
            .expect("task comments")
            .is_empty()
    );
}
