//! Test-only allowlist: the original tests under orbit-cli passed the same lints via
//! the crate-level test harness configuration; duplicated here for the extracted crate.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::super::router;
use super::test_support::body_json;

fn seed_friction(runtime: &OrbitRuntime, body: &str, tags: &[&str]) -> Value {
    runtime
        .run_tool(
            "orbit.friction.add",
            json!({
                "body": body,
                "tags": tags,
                "model": TEST_CODEX_MODEL,
            }),
        )
        .expect("seed friction")
}

async fn request(
    runtime: OrbitRuntime,
    method: Method,
    uri: String,
    origin: Option<&str>,
    body: Option<Value>,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(origin) = origin {
        builder = builder
            .header(header::ORIGIN, origin)
            .header(header::HOST, "localhost:7878");
    }
    let request = if let Some(body) = body {
        builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("request")
    } else {
        builder.body(Body::empty()).expect("request")
    };

    router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(request)
        .await
        .expect("response")
}

#[tokio::test]
async fn patch_requires_localhost_origin() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let friction = seed_friction(&runtime, "# Build snag\nDetails.", &["tooling"]);
    let id = friction["id"].as_str().expect("friction id");

    for (origin, label) in [(None, "missing"), (Some("http://example.com"), "foreign")] {
        let response = request(
            runtime.clone(),
            Method::PATCH,
            format!("/frictions/{id}"),
            origin,
            Some(json!({ "status": "triaged" })),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{label}");
    }

    let response = request(runtime, Method::GET, format!("/frictions/{id}"), None, None).await;
    let payload = body_json(response).await;
    assert_eq!(payload["status"], "open");
}

#[tokio::test]
async fn resolve_requires_localhost_origin() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let friction = seed_friction(&runtime, "# Policy snag\nDetails.", &["policy"]);
    let id = friction["id"].as_str().expect("friction id");

    for (origin, label) in [(None, "missing"), (Some("http://example.com"), "foreign")] {
        let response = request(
            runtime.clone(),
            Method::POST,
            format!("/frictions/{id}/resolve"),
            origin,
            None,
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{label}");
    }

    let response = request(runtime, Method::GET, format!("/frictions/{id}"), None, None).await;
    let payload = body_json(response).await;
    assert_eq!(payload["status"], "open");
}

#[tokio::test]
async fn create_persists_and_echoes_fields() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let response = request(
        runtime.clone(),
        Method::POST,
        "/frictions".to_string(),
        Some("http://localhost:7878"),
        Some(json!({
            "body": "# Slow tool\nThe CLI hung for a minute.",
            "tags": ["tooling"],
            "model": TEST_CODEX_MODEL,
        })),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let created = body_json(response).await;
    let id = created["id"]
        .as_str()
        .expect("created friction id")
        .to_string();
    assert_eq!(
        created["body"],
        json!("# Slow tool\nThe CLI hung for a minute.")
    );
    assert_eq!(created["tags"], json!(["tooling"]));
    assert_eq!(created["status"], "open");

    // Persisted: readable through the existing GET surface.
    let response = request(runtime, Method::GET, format!("/frictions/{id}"), None, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let fetched = body_json(response).await;
    assert_eq!(fetched["id"], json!(id));
    assert_eq!(
        fetched["body"],
        json!("# Slow tool\nThe CLI hung for a minute.")
    );
    assert_eq!(fetched["tags"], json!(["tooling"]));
}

#[tokio::test]
async fn create_without_attribution_defaults_model_to_human() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let response = request(
        runtime,
        Method::POST,
        "/frictions".to_string(),
        Some("http://localhost:7878"),
        Some(json!({
            "body": "# No attribution supplied\nDetails.",
        })),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let created = body_json(response).await;
    assert_eq!(
        created["model"], "human",
        "identity-less writes attribute to the human actor label, not a model constant"
    );
}

#[tokio::test]
async fn create_forwards_explicit_model_attribution() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let response = request(
        runtime,
        Method::POST,
        "/frictions".to_string(),
        Some("http://localhost:7878"),
        Some(json!({
            "body": "# Explicit attribution supplied\nDetails.",
            "model": TEST_CODEX_MODEL,
        })),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let created = body_json(response).await;
    assert_eq!(
        created["model"], TEST_CODEX_MODEL,
        "caller-supplied `model` is forwarded to the tool host unchanged"
    );
}

#[tokio::test]
async fn create_requires_localhost_origin() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    for (origin, label) in [(None, "missing"), (Some("http://example.com"), "foreign")] {
        let response = request(
            runtime.clone(),
            Method::POST,
            "/frictions".to_string(),
            origin,
            Some(json!({
                "body": "# Snag\nDetails.",
                "model": TEST_CODEX_MODEL,
            })),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{label}");
    }
}

#[tokio::test]
async fn create_empty_body_is_rejected_with_error_text() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let response = request(
        runtime,
        Method::POST,
        "/frictions".to_string(),
        Some("http://localhost:7878"),
        Some(json!({
            "body": "   ",
            "model": TEST_CODEX_MODEL,
        })),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = body_json(response).await;
    let error = payload["error"].as_str().expect("error text");
    assert!(
        error.contains("body"),
        "error should name the offending `body` field: {error}"
    );
}

#[tokio::test]
async fn create_round_trips_tags_and_during_task() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let response = request(
        runtime.clone(),
        Method::POST,
        "/frictions".to_string(),
        Some("http://localhost:7878"),
        Some(json!({
            "body": "# Blocked\nCould not run the tool.",
            "tags": ["tooling", "docs"],
            "during_task": "ORB-10260",
            "model": TEST_CODEX_MODEL,
        })),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let created = body_json(response).await;
    let id = created["id"]
        .as_str()
        .expect("created friction id")
        .to_string();
    assert_eq!(created["tags"], json!(["docs", "tooling"]));
    assert_eq!(created["during_task"], json!("ORB-10260"));

    let response = request(runtime, Method::GET, format!("/frictions/{id}"), None, None).await;
    let fetched = body_json(response).await;
    assert_eq!(fetched["tags"], json!(["docs", "tooling"]));
    assert_eq!(fetched["during_task"], json!("ORB-10260"));
}

#[tokio::test]
async fn path_traversal_id_is_rejected() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let response = request(
        runtime,
        Method::GET,
        "/frictions/%2e%2e".to_string(),
        None,
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn patch_round_trip_updates_status_and_tags() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let friction = seed_friction(&runtime, "# Slow docs\nDetails.", &["tooling"]);
    let id = friction["id"].as_str().expect("friction id");

    let response = request(
        runtime.clone(),
        Method::PATCH,
        format!("/frictions/{id}"),
        Some("http://localhost:7878"),
        Some(json!({
            "status": "triaged",
            "tags": ["docs"],
        })),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let response = request(runtime, Method::GET, format!("/frictions/{id}"), None, None).await;
    let payload = body_json(response).await;
    assert_eq!(payload["status"], "triaged");
    assert_eq!(payload["tags"], json!(["docs"]));
}

#[tokio::test]
async fn stats_shape_exposes_triage_counts() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    seed_friction(&runtime, "# Open one\nDetails.", &["tooling"]);
    let triaged = seed_friction(&runtime, "# Triaged one\nDetails.", &["docs"]);
    let resolved = seed_friction(&runtime, "# Resolved one\nDetails.", &["policy"]);
    runtime
        .run_tool(
            "orbit.friction.update",
            json!({
                "id": triaged["id"],
                "status": "triaged",
                "model": TEST_CODEX_MODEL,
            }),
        )
        .expect("triage friction");
    runtime
        .run_tool(
            "orbit.friction.resolve",
            json!({
                "id": resolved["id"],
                "model": TEST_CODEX_MODEL,
            }),
        )
        .expect("resolve friction");

    let response = request(
        runtime,
        Method::GET,
        "/frictions/stats".to_string(),
        None,
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    for key in ["open", "triaged", "resolved_this_month"] {
        assert!(
            payload.get(key).and_then(Value::as_u64).is_some(),
            "{key} should be a u64 in {payload}"
        );
    }
}

#[tokio::test]
async fn list_forwards_empty_multi_word_query_notes() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    seed_friction(
        &runtime,
        "workspace_init lost the parallel env snapshot while EnvGuard was armed.",
        &["tooling"],
    );

    let response = request(
        runtime,
        Method::GET,
        "/frictions?status=open&q=EnvGuard%20workspace_init".to_string(),
        None,
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    let items = payload["items"].as_array().expect("items array");
    assert!(items.is_empty(), "non-contiguous terms are one needle");
    let notes = payload["notes"].as_array().expect("notes array");
    assert_eq!(notes.len(), 1);
    let note = notes[0].as_str().expect("note text");
    assert!(note.contains("single case-insensitive substring"), "{note}");
    assert!(note.contains("not proof the corpus is empty"), "{note}");
}

/// ADR-0209 bearing 1 [ORB-10358]: the dashboard's friction field names come
/// from the operation registry, not from literals. This asserts the coupling —
/// every parameter these routes set is one the registry declares — so a
/// registry rename that the dashboard misses fails here rather than silently
/// dropping a filter at runtime.
#[test]
fn dashboard_friction_parameters_are_declared_by_the_registry() {
    use orbit_common::governance::friction::FrictionVerb;

    let used: &[(FrictionVerb, &[&str])] = &[
        (
            FrictionVerb::List,
            &["status", "tag", "month", "q", "limit", "offset"],
        ),
        (FrictionVerb::Add, &["model", "body", "tags", "during_task"]),
        (FrictionVerb::Show, &["id"]),
        (FrictionVerb::Update, &["id", "status", "tags"]),
        (FrictionVerb::Resolve, &["id"]),
        (FrictionVerb::Stats, &[]),
    ];

    for (verb, params) in used {
        let declared: Vec<&str> = verb.spec().params.iter().map(|param| param.name).collect();
        for param in *params {
            assert!(
                declared.contains(param),
                "{} does not declare `{param}`; declared: {declared:?}",
                verb.tool_name()
            );
        }
    }
}
