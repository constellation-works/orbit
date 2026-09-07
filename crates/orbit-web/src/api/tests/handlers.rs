use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use chrono::{Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{JobRunState, OrbitRuntime, TaskStatus};
use serde_json::json;
use tower::ServiceExt;

use super::super::*;
use super::test_support::{body_json, seed_run, write_seeded_run};

async fn request_cancel(runtime: OrbitRuntime, run_id: &str, origin: Option<&str>) -> Response {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(format!("/runs/{run_id}/cancel"));
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(builder.body(Body::empty()).expect("request"))
        .await
        .expect("response")
}

async fn request_tasks(runtime: OrbitRuntime) -> Response {
    router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/tasks")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

async fn request_job_runs(runtime: OrbitRuntime, query: &str) -> Response {
    router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/job-runs?{query}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

async fn patch_task_crew(runtime: OrbitRuntime, task_id: &str, crew: &str) -> Response {
    patch_task_body(runtime, task_id, format!(r#"{{"crew":"{crew}"}}"#)).await
}

async fn patch_task_body(runtime: OrbitRuntime, task_id: &str, body: String) -> Response {
    router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .method(Method::PATCH)
                .uri(format!("/tasks/{task_id}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ORIGIN, "http://localhost:7878")
                .body(Body::from(body))
                .expect("request"),
        )
        .await
        .expect("response")
}

async fn request_crews(runtime: OrbitRuntime) -> Response {
    router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/crews")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

fn runtime_with_custom_crews() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempfile::tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    std::fs::write(
        workspace_root.join("config.toml"),
        r#"
[crews.beta]
model = "codex-beta"
provider = "codex"
backend = "cli"

[crews.alpha]
model = "alpha-model"
provider = "codex"
backend = "cli"

[workflow]
default_crew = "beta"
"#,
    )
    .expect("write config");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime)
}

fn runtime_with_stale_task_crew() -> (tempfile::TempDir, OrbitRuntime, String) {
    let root = tempfile::tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    std::fs::write(
        workspace_root.join("config.toml"),
        r#"
[crews.beta]
model = "codex-beta"
provider = "codex"
backend = "cli"

[crews.all-codex]
model = "all-codex-model"
provider = "codex"
backend = "cli"

[workflow]
default_crew = "beta"
"#,
    )
    .expect("write initial config");
    let initial_runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build initial runtime");
    let task = initial_runtime
        .add_task(TaskAddParams {
            title: "Stale crew task".to_string(),
            description: "Fixture with an explicit crew removed from config.".to_string(),
            status: Some(TaskStatus::Backlog),
            crew: Some("all-codex".to_string()),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("create stale crew task");

    std::fs::write(
        workspace_root.join("config.toml"),
        r#"
[crews.beta]
model = "codex-beta"
provider = "codex"
backend = "cli"

[workflow]
default_crew = "beta"
"#,
    )
    .expect("write reduced config");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build reduced runtime");
    (root, runtime, task.id)
}

fn seed_task(
    runtime: &OrbitRuntime,
    title: &str,
    status: TaskStatus,
    dependencies: Vec<String>,
) -> orbit_core::Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture for {title}."),
            status: Some(status),
            dependencies,
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("create task")
}

#[tokio::test]
async fn job_run_filters_apply_before_limit_and_validate_state() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let since = Utc::now();
    let mut running = seed_run(
        &runtime,
        "jrun-job-runs-running",
        "job_runs_filter",
        JobRunState::Running,
    );
    running.pid = Some(std::process::id());
    write_seeded_run(&runtime, &running);
    for state in [
        JobRunState::Success,
        JobRunState::Failed,
        JobRunState::Cancelled,
        JobRunState::Interrupted,
        JobRunState::Timeout,
    ] {
        seed_run(
            &runtime,
            &format!("jrun-job-runs-terminal-{state}"),
            "job_runs_filter",
            state,
        );
    }
    let mut old_terminal = seed_run(
        &runtime,
        "jrun-job-runs-terminal-old",
        "job_runs_filter",
        JobRunState::Success,
    );
    old_terminal.created_at = since - Duration::days(1);
    write_seeded_run(&runtime, &old_terminal);
    seed_run(
        &runtime,
        "jrun-job-runs-terminal-other-job",
        "other_job",
        JobRunState::Success,
    );

    let response = request_job_runs(runtime.clone(), "state=running&limit=1").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["state"], json!("running"));
    assert_eq!(body["limit"], json!(1));
    let rows = body["items"].as_array().expect("runs items");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["run_id"], json!(running.run_id));

    let response = request_job_runs(runtime.clone(), "state=terminal&limit=10").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let states = body["items"]
        .as_array()
        .expect("runs items")
        .iter()
        .map(|run| run["state"].as_str().expect("state"))
        .collect::<Vec<_>>();
    for state in ["success", "failed", "cancelled", "interrupted", "timeout"] {
        assert!(states.contains(&state), "missing terminal state {state}");
    }

    let since = since.to_rfc3339().replace('+', "%2B");
    let response = request_job_runs(
        runtime.clone(),
        &format!("job_id=job_runs_filter&state=terminal&since={since}&limit=10"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let run_ids = body["items"]
        .as_array()
        .expect("runs items")
        .iter()
        .map(|run| run["run_id"].as_str().expect("run id"))
        .collect::<Vec<_>>();
    assert!(!run_ids.contains(&old_terminal.run_id.as_str()));
    assert!(!run_ids.contains(&"jrun-job-runs-terminal-other-job"));

    let response = request_job_runs(runtime, "state=unknown").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error = body_json(response).await;
    assert_eq!(
        error["error"],
        json!("invalid state; expected one of: all, active, failed, pending, running, terminal")
    );
}

/// Dashboard Recent Runs used to fetch the newest N runs and then filter to
/// `failed` in the browser. A Failed run older than that recent success/active
/// slice disappeared even though the header still counted it. Filter, then
/// limit, so that older failure remains discoverable.
#[tokio::test]
async fn job_runs_failed_filter_keeps_older_failures_outside_the_recent_success_slice() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let now = Utc::now();

    for index in 0..3 {
        let mut recent = seed_run(
            &runtime,
            &format!("jrun-recent-success-{index}"),
            "recent_job",
            JobRunState::Success,
        );
        recent.created_at = now - Duration::minutes(index);
        recent.scheduled_at = recent.created_at;
        recent.started_at = Some(recent.created_at);
        recent.finished_at = Some(now - Duration::minutes(index));
        write_seeded_run(&runtime, &recent);
    }

    let mut older_failed = seed_run(
        &runtime,
        "jrun-older-failed",
        "recent_job",
        JobRunState::Failed,
    );
    older_failed.created_at = now - Duration::hours(2);
    older_failed.scheduled_at = older_failed.created_at;
    older_failed.started_at = Some(older_failed.created_at);
    older_failed.finished_at = Some(now - Duration::hours(2));
    write_seeded_run(&runtime, &older_failed);

    let unfiltered = request_job_runs(runtime.clone(), "limit=3").await;
    assert_eq!(unfiltered.status(), StatusCode::OK);
    let unfiltered = body_json(unfiltered).await;
    assert_eq!(unfiltered["state"], json!("all"));
    assert_eq!(unfiltered["limit"], json!(3));
    assert_eq!(unfiltered["total"], json!(4));
    assert_eq!(unfiltered["truncated"], json!(true));
    let unfiltered_ids = unfiltered["items"]
        .as_array()
        .expect("unfiltered items")
        .iter()
        .map(|run| run["run_id"].as_str().expect("run id"))
        .collect::<Vec<_>>();
    assert_eq!(unfiltered_ids.len(), 3);
    assert!(
        !unfiltered_ids.contains(&"jrun-older-failed"),
        "the unfiltered recent slice must omit the older failure so this fixture still proves filter-after-limit would hide it"
    );

    let failed = request_job_runs(runtime, "state=failed&limit=3").await;
    assert_eq!(failed.status(), StatusCode::OK);
    let failed = body_json(failed).await;
    assert_eq!(failed["state"], json!("failed"));
    assert_eq!(failed["limit"], json!(3));
    assert_eq!(failed["total"], json!(1));
    assert_eq!(failed["truncated"], json!(false));
    let failed_items = failed["items"].as_array().expect("failed items");
    assert_eq!(failed_items.len(), 1);
    assert_eq!(failed_items[0]["run_id"], json!("jrun-older-failed"));
    assert_eq!(failed_items[0]["state"], json!("failed"));
}

#[tokio::test]
async fn tasks_with_stale_explicit_crew_fall_back_to_default_projection() {
    let (_root, runtime, task_id) = runtime_with_stale_task_crew();

    let response = request_tasks(runtime.clone()).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    // ORB-10400: GET /tasks answers `{ items, total, limit, truncated }`.
    let rows = body["items"].as_array().expect("tasks response items");
    let task = rows
        .iter()
        .find(|task| task["id"].as_str() == Some(task_id.as_str()))
        .expect("stale crew task is listed");
    assert_eq!(task["crew"], json!("all-codex"));
    assert_eq!(task["resolved_crew"], json!("beta"));
    assert_eq!(task["crew_model"], json!("codex-beta"));

    let response = patch_task_crew(runtime, &task_id, "all-codex").await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn patch_task_crew_null_clears_stale_explicit_crew_to_default() {
    let (_root, runtime, task_id) = runtime_with_stale_task_crew();

    let response = patch_task_body(runtime, &task_id, r#"{"crew":null}"#.to_string()).await;

    assert_eq!(response.status(), StatusCode::OK);
    let task = body_json(response).await;
    assert_eq!(task["crew"], json!(null));
    assert_eq!(task["resolved_crew"], json!("beta"));
    assert_eq!(task["crew_model"], json!("codex-beta"));
}

#[tokio::test]
async fn crews_endpoint_returns_sorted_runtime_registry() {
    let (_root, runtime) = runtime_with_custom_crews();

    let response = request_crews(runtime).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["default_crew"], json!("beta"));
    let crews = body["crews"].as_array().expect("crews array");
    assert_eq!(crews.len(), 3);
    assert_eq!(crews[0]["name"], json!("alpha"));
    assert_eq!(crews[0]["is_default"], json!(false));
    assert_eq!(crews[0]["model"], json!("alpha-model"));
    assert_eq!(crews[1]["name"], json!("beta"));
    assert_eq!(crews[1]["is_default"], json!(true));
    assert_eq!(crews[1]["model"], json!("codex-beta"));
    assert_eq!(crews[2]["name"], json!("system"));
    assert_eq!(crews[2]["is_default"], json!(false));
    assert_eq!(crews[2]["model"], json!("codex-beta"));
}

#[tokio::test]
async fn require_localhost_origin_rejects_prefix_match() {
    let cases = [
        ("http://localhost.evil.com", "localhost prefix"),
        ("http://127.0.0.1.evil.com", "127.0.0.1 prefix"),
    ];

    for (index, (origin, label)) in cases.into_iter().enumerate() {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        let run = seed_run(
            &runtime,
            &format!("jrun-web-cancel-prefix-{index}"),
            "web_cancel_prefix",
            JobRunState::Pending,
        );

        let response = request_cancel(runtime.clone(), &run.run_id, Some(origin)).await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{label}");
        let stored = runtime.show_job_run(&run.run_id).expect("show run");
        assert_eq!(stored.state, JobRunState::Pending, "{label}");
    }
}

#[tokio::test]
async fn tasks_resolve_dependency_statuses_from_all_tasks() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let done = seed_task(
        &runtime,
        "Completed dependency",
        TaskStatus::Done,
        Vec::new(),
    );
    let archived = seed_task(
        &runtime,
        "Archived dependency",
        TaskStatus::Backlog,
        Vec::new(),
    );
    runtime.archive_task(&archived.id).expect("archive task");
    let rejected = seed_task(
        &runtime,
        "Rejected dependency",
        TaskStatus::Rejected,
        Vec::new(),
    );
    let visible = seed_task(
        &runtime,
        "Visible dependent",
        TaskStatus::Backlog,
        vec![done.id.clone(), archived.id.clone(), rejected.id.clone()],
    );

    let response = request_tasks(runtime).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    // ORB-10400: GET /tasks answers `{ items, total, limit, truncated }`.
    let rows = body["items"].as_array().expect("tasks response items");
    assert!(
        rows.iter()
            .any(|task| task["id"].as_str() == Some(&visible.id))
    );
    assert!(
        rows.iter()
            .any(|task| task["id"].as_str() == Some(&rejected.id))
    );
    // ORB-10310: task listing is status-neutral, so `done` and `archived`
    // dependency tasks now appear in the list too. Their status still resolves
    // from the global index (asserted below), independent of list membership.
    assert!(
        rows.iter()
            .any(|task| task["id"].as_str() == Some(&done.id))
    );
    assert!(
        rows.iter()
            .any(|task| task["id"].as_str() == Some(&archived.id))
    );

    let visible_json = rows
        .iter()
        .find(|task| task["id"].as_str() == Some(&visible.id))
        .expect("visible task row");
    let dependencies = visible_json["resolved_dependencies"]
        .as_array()
        .expect("resolved dependencies array");
    let dependency_labels = dependencies
        .iter()
        .map(|value| value.as_str().expect("dependency label"))
        .collect::<Vec<_>>();
    let done_label = format!("{} [done]", done.id);
    let archived_label = format!("{} [archived]", archived.id);
    let rejected_label = format!("{} [rejected]", rejected.id);
    assert!(dependency_labels.contains(&done_label.as_str()));
    assert!(dependency_labels.contains(&archived_label.as_str()));
    assert!(dependency_labels.contains(&rejected_label.as_str()));
}

#[tokio::test]
async fn require_localhost_origin_rejects_https_origin() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run = seed_run(
        &runtime,
        "jrun-web-cancel-https-origin",
        "web_cancel_https_origin",
        JobRunState::Pending,
    );

    let response = request_cancel(runtime.clone(), &run.run_id, Some("https://localhost")).await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let stored = runtime.show_job_run(&run.run_id).expect("show run");
    assert_eq!(stored.state, JobRunState::Pending);
}

#[tokio::test]
async fn require_localhost_origin_accepts_localhost_with_port() {
    let cases = [
        ("http://localhost:7878", "localhost"),
        ("http://127.0.0.1:7878", "127-0-0-1"),
    ];

    for (origin, label) in cases {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        let run = seed_run(
            &runtime,
            &format!("jrun-web-cancel-origin-port-{label}"),
            "web_cancel_origin_port",
            JobRunState::Pending,
        );

        let response = request_cancel(runtime.clone(), &run.run_id, Some(origin)).await;

        assert_eq!(response.status(), StatusCode::OK, "{label}");
        let stored = runtime.show_job_run(&run.run_id).expect("show run");
        assert_eq!(stored.state, JobRunState::Cancelled, "{label}");
    }
}

#[tokio::test]
async fn require_localhost_origin_blocks_cross_origin_get_with_attacker_origin() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let response = router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/tasks")
                .header(header::ORIGIN, "http://localhost.evil.com")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
