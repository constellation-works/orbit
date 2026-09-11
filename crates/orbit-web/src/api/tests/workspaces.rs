//! Tests for the global (cross-workspace) endpoints (ORB-00030).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use chrono::{Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::runtime::WorkspaceRuntimeBinding;
use orbit_core::{ActorIdentity, JobRunState, OrbitRuntime, ShipMode, TaskStatus};
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus};
use serde_json::json;
use tower::ServiceExt;
use tracing_subscriber::Registry;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;

use super::super::*;
use super::test_support::{body_json, seed_run, write_seeded_run};
use crate::state::{DashboardState, RegistrySource, WsEntry};

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .expect("request")
}

/// Create an on-disk workspace under `base/<name>`, seed one in-progress task,
/// and return `(orbit_dir, repo_root)`. The workspace persists after the
/// runtime is dropped, so global mode can reopen it via `from_roots`.
pub(super) fn seed_workspace(global_root: &Path, base: &Path, name: &str) -> (PathBuf, PathBuf) {
    let repo_root = base.join(name);
    let orbit_dir = repo_root.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create .orbit");
    std::fs::write(orbit_dir.join("config.toml"), "").expect("write config");
    std::fs::write(
        orbit_dir.join("config.yaml"),
        format!("schema_version: 1\nworkspace_id: ws_{name}\n"),
    )
    .expect("write workspace identity");
    let runtime = OrbitRuntime::from_roots(global_root, &orbit_dir)
        .expect("build runtime")
        .with_actor(ActorIdentity::human("human"));
    runtime
        .add_task(TaskAddParams {
            title: format!("{name} task"),
            description: "seed".to_string(),
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .expect("add task");
    (orbit_dir, repo_root)
}

pub(super) fn workspace_entry(
    id: &str,
    repo_root: PathBuf,
    orbit_dir: PathBuf,
    active: bool,
) -> WsEntry {
    let binding = active.then(|| WorkspaceRuntimeBinding {
        logical_workspace_id: format!("ws_{id}"),
        workspace_id: format!("ws_{id}"),
        owner_machine_id: None,
        repo_root: repo_root.clone(),
        ship_mode: ShipMode::Local,
        base_branch: None,
    });
    WsEntry {
        id: id.to_string(),
        name: id.to_string(),
        repo_root,
        orbit_dir,
        binding,
        active,
    }
}

#[tokio::test]
async fn workspaces_endpoint_reports_single_default() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let state = DashboardState::single(Arc::new(runtime));
    let response = router()
        .with_state(state)
        .oneshot(get("/workspaces"))
        .await
        .expect("response");
    let body = body_json(response).await;
    let entries = body.as_array().expect("array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["id"], json!("default"));
    assert_eq!(entries[0]["is_default"], json!(true));
    assert_eq!(entries[0]["status"], json!("active"));
}

#[tokio::test]
async fn tasks_all_in_single_mode_tags_default_workspace() {
    let runtime = OrbitRuntime::in_memory()
        .expect("build runtime")
        .with_actor(ActorIdentity::human("human"));
    runtime
        .add_task(TaskAddParams {
            title: "solo".to_string(),
            description: "seed".to_string(),
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .expect("add task");
    let state = DashboardState::single(Arc::new(runtime));
    let response = router()
        .with_state(state)
        .oneshot(get("/tasks/all"))
        .await
        .expect("response");
    let body = body_json(response).await;
    let tasks = body["items"].as_array().expect("items");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["workspace_id"], json!("default"));
    assert_eq!(tasks[0]["workspace_name"], json!("default"));
}

#[tokio::test]
async fn tasks_all_aggregates_active_workspaces_and_skips_inactive() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");

    let (alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    let (beta_orbit, beta_repo) = seed_workspace(&global_root, tmp.path(), "beta");

    let entries = vec![
        workspace_entry("alpha", alpha_repo, alpha_orbit, true),
        workspace_entry("beta", beta_repo, beta_orbit, true),
        workspace_entry(
            "gone",
            tmp.path().join("missing"),
            tmp.path().join("missing/.orbit"),
            false,
        ),
    ];
    let state = DashboardState::global(global_root, entries, Some("alpha".to_string()));

    // Aggregate task list: one task per active workspace, tagged; none from the
    // inactive workspace.
    let response = router()
        .with_state(state.clone())
        .oneshot(get("/tasks/all"))
        .await
        .expect("response");
    let body = body_json(response).await;
    let tasks = body["items"].as_array().expect("items");
    assert_eq!(tasks.len(), 2);
    let ws_ids: HashSet<&str> = tasks
        .iter()
        .map(|t| t["workspace_id"].as_str().expect("workspace_id"))
        .collect();
    assert_eq!(ws_ids, HashSet::from(["alpha", "beta"]));
    assert!(tasks.iter().all(|t| t["workspace_name"].is_string()));
    // ORB-00037: every aggregate task carries its workspace's filesystem path.
    assert!(tasks.iter().all(|t| {
        t["workspace_root"]
            .as_str()
            .is_some_and(|root| root.ends_with(t["workspace_name"].as_str().expect("name")))
    }));
    assert_eq!(body["total"], json!(2));
    assert_eq!(body["limit"], json!(orbit_core::DEFAULT_TASK_LIST_LIMIT));
    assert_eq!(body["truncated"], json!(false));

    // Workspace listing: all three, with status + default flag.
    let response = router()
        .with_state(state)
        .oneshot(get("/workspaces"))
        .await
        .expect("response");
    let body = body_json(response).await;
    let listed = body.as_array().expect("array");
    assert_eq!(listed.len(), 3);
    let gone = listed
        .iter()
        .find(|w| w["id"] == json!("gone"))
        .expect("gone entry");
    assert_eq!(gone["status"], json!("invalid"));
    let alpha = listed
        .iter()
        .find(|w| w["id"] == json!("alpha"))
        .expect("alpha entry");
    assert_eq!(alpha["is_default"], json!(true));
    assert_eq!(alpha["status"], json!("active"));
    // ORB-00037: entries expose the filesystem path (repo root + orbit_dir).
    assert!(
        alpha["root"]
            .as_str()
            .is_some_and(|root| root.ends_with("alpha"))
    );
    assert!(
        alpha["orbit_dir"]
            .as_str()
            .is_some_and(|dir| dir.ends_with(".orbit"))
    );
}

#[tokio::test]
async fn tasks_all_reports_aggregate_total_when_global_limit_truncates() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");

    let mut entries = Vec::new();
    for name in ["alpha", "beta", "gamma"] {
        let (orbit_dir, repo_root) = seed_workspace(&global_root, tmp.path(), name);
        let runtime = OrbitRuntime::from_roots(&global_root, &orbit_dir)
            .expect("build runtime")
            .with_actor(ActorIdentity::human("human"));
        for index in 1..30 {
            runtime
                .add_task(TaskAddParams {
                    title: format!("{name} task {index}"),
                    description: "seed".to_string(),
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                })
                .expect("add task");
        }
        entries.push(workspace_entry(name, repo_root, orbit_dir, true));
    }

    let response = router()
        .with_state(DashboardState::global(
            global_root,
            entries,
            Some("alpha".to_string()),
        ))
        .oneshot(get("/tasks/all"))
        .await
        .expect("response");
    let body = body_json(response).await;

    assert_eq!(body["total"], json!(90));
    assert_eq!(body["truncated"], json!(true));
    assert_eq!(body["items"].as_array().expect("items").len(), 50);
}

#[tokio::test]
async fn tasks_all_pages_globally_without_per_workspace_omissions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let mut entries = Vec::new();
    for name in ["alpha", "beta"] {
        let (orbit_dir, repo_root) = seed_workspace(&global_root, tmp.path(), name);
        let runtime = OrbitRuntime::from_roots(&global_root, &orbit_dir)
            .expect("runtime")
            .with_actor(ActorIdentity::human("human"));
        for index in 1..30 {
            runtime
                .add_task(TaskAddParams {
                    title: format!("global page {name} {index:02}"),
                    description: "seed".to_string(),
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                })
                .expect("seed task");
        }
        entries.push(workspace_entry(name, repo_root, orbit_dir, true));
    }
    let state = DashboardState::global(global_root, entries, Some("alpha".to_string()));
    let mut cursor = None;
    let mut identities = HashSet::new();
    let mut offset = 0;
    loop {
        let uri = cursor.as_deref().map_or_else(
            || "/tasks/all?limit=7&q=global".to_string(),
            |cursor| {
                let query = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("limit", "7")
                    .append_pair("q", "global")
                    .append_pair("cursor", cursor)
                    .finish();
                format!("/tasks/all?{query}")
            },
        );
        let body = body_json(
            router()
                .with_state(state.clone())
                .oneshot(get(&uri))
                .await
                .expect("response"),
        )
        .await;
        assert_eq!(body["total"], json!(58));
        assert_eq!(body["offset"], json!(offset));
        let items = body["items"].as_array().expect("items");
        for item in items {
            let identity = (
                item["workspace_id"]
                    .as_str()
                    .expect("workspace")
                    .to_string(),
                item["id"].as_str().expect("id").to_string(),
            );
            assert!(
                identities.insert(identity),
                "aggregate page duplicated a task"
            );
        }
        offset += items.len();
        cursor = body["next_cursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(identities.len(), 58);
    assert_eq!(offset, 58);
    assert!(identities.iter().any(|(workspace, _)| workspace == "alpha"));
    assert!(identities.iter().any(|(workspace, _)| workspace == "beta"));
}

#[tokio::test]
async fn job_runs_all_preserves_workspace_identity_order_bounds_and_unavailable_sources() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    let (beta_orbit, beta_repo) = seed_workspace(&global_root, tmp.path(), "beta");

    let alpha = OrbitRuntime::from_roots(&global_root, &alpha_orbit).expect("alpha runtime");
    let beta = OrbitRuntime::from_roots(&global_root, &beta_orbit).expect("beta runtime");
    let shared_time = Utc::now();
    let mut alpha_duplicate = seed_run(&alpha, "jrun-shared", "aggregate", JobRunState::Pending);
    alpha_duplicate.created_at = shared_time;
    alpha_duplicate.scheduled_at = shared_time;
    alpha_duplicate.started_at = None;
    write_seeded_run(&alpha, &alpha_duplicate);
    let mut beta_duplicate = seed_run(&beta, "jrun-shared", "aggregate", JobRunState::Failed);
    beta_duplicate.created_at = shared_time;
    beta_duplicate.scheduled_at = shared_time;
    beta_duplicate.started_at = Some(shared_time);
    beta_duplicate.finished_at = Some(shared_time);
    write_seeded_run(&beta, &beta_duplicate);
    let mut old = seed_run(&alpha, "jrun-old", "aggregate", JobRunState::Success);
    old.created_at = shared_time - Duration::days(1);
    old.scheduled_at = old.created_at;
    old.started_at = Some(old.created_at);
    old.finished_at = Some(old.created_at);
    write_seeded_run(&alpha, &old);

    let entries = vec![
        workspace_entry("alpha", alpha_repo, alpha_orbit, true),
        workspace_entry("beta", beta_repo, beta_orbit, true),
        workspace_entry(
            "gone",
            tmp.path().join("missing"),
            tmp.path().join("missing/.orbit"),
            false,
        ),
    ];
    let state = DashboardState::global(global_root, entries, Some("alpha".to_string()));
    let bounded = router()
        .with_state(state.clone())
        .oneshot(get("/job-runs/all?limit=999"))
        .await
        .expect("bounded response");
    let bounded = body_json(bounded).await;
    assert_eq!(bounded["limit"], json!(HISTORY_MAX_LIMIT));
    assert_eq!(bounded["items"].as_array().expect("bounded items").len(), 3);

    let active = router()
        .with_state(state.clone())
        .oneshot(get("/job-runs/all?state=active&limit=2"))
        .await
        .expect("active response");
    let active = body_json(active).await;
    assert_eq!(active["state"], json!("active"));
    assert_eq!(active["items"].as_array().expect("active items").len(), 1);
    assert_eq!(active["items"][0]["workspace_id"], json!("alpha"));

    let failed = router()
        .with_state(state.clone())
        .oneshot(get("/job-runs/all?state=failed&limit=2"))
        .await
        .expect("failed response");
    let failed = body_json(failed).await;
    assert_eq!(failed["state"], json!("failed"));
    assert_eq!(failed["items"].as_array().expect("failed items").len(), 1);
    assert_eq!(failed["items"][0]["workspace_id"], json!("beta"));

    let invalid = router()
        .with_state(state.clone())
        .oneshot(get("/job-runs/all?state=terminal"))
        .await
        .expect("invalid response");
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let response = router()
        .with_state(state)
        .oneshot(get("/job-runs/all?limit=2"))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["limit"], json!(2));
    assert_eq!(body["truncated"], json!(true));
    let rows = body["items"].as_array().expect("items");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["run_id"], json!("jrun-shared"));
    assert_eq!(rows[0]["workspace_id"], json!("alpha"));
    assert_eq!(rows[1]["run_id"], json!("jrun-shared"));
    assert_eq!(rows[1]["workspace_id"], json!("beta"));
    assert!(rows.iter().all(|run| run["workspace_name"].is_string()));
    assert_eq!(body["unavailable"][0]["workspace_id"], json!("gone"));
    assert_eq!(
        body["unavailable"][0]["error"],
        json!("workspace is unavailable")
    );
}

/// [ORB-11251] The store truncates each workspace's contribution to the
/// aggregate list with `LIMIT` before this handler ever merges or re-sorts
/// anything. If that per-workspace truncation still used `created_at`, an
/// old run that only just finished after a newer run already completed
/// could be dropped before its recency was ever compared to that newer
/// run's — even though the dashboard's own recency contract ranks it first.
/// This proves the top displayed run across two workspaces is selected by
/// that same recency ordering *before* the per-workspace limit is applied,
/// not after.
#[tokio::test]
async fn job_runs_all_selects_top_by_recency_before_per_workspace_limit() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    let (beta_orbit, beta_repo) = seed_workspace(&global_root, tmp.path(), "beta");

    let alpha = OrbitRuntime::from_roots(&global_root, &alpha_orbit).expect("alpha runtime");
    let beta = OrbitRuntime::from_roots(&global_root, &beta_orbit).expect("beta runtime");
    let now = Utc::now();

    // Alpha holds both contenders: an old, long-running task that only just
    // finished, and a newer task that finished earlier. Under a naive
    // `created_at`-then-`LIMIT` query, the older/longer-running run would
    // never make it out of alpha's own per-workspace page.
    let mut old_long_running = seed_run(
        &alpha,
        "jrun-old-long-running",
        "aggregate",
        JobRunState::Success,
    );
    old_long_running.created_at = now - Duration::hours(2);
    old_long_running.scheduled_at = old_long_running.created_at;
    old_long_running.started_at = Some(old_long_running.created_at);
    old_long_running.finished_at = Some(now);
    write_seeded_run(&alpha, &old_long_running);

    let mut new_short_running = seed_run(
        &alpha,
        "jrun-new-short-running",
        "aggregate",
        JobRunState::Success,
    );
    new_short_running.created_at = now - Duration::hours(1);
    new_short_running.scheduled_at = new_short_running.created_at;
    new_short_running.started_at = Some(new_short_running.created_at);
    new_short_running.finished_at = Some(now - Duration::minutes(90));
    write_seeded_run(&alpha, &new_short_running);

    // Beta holds an unrelated run that finished before either alpha run, so
    // the merge across workspaces stays exercised without becoming the top
    // result itself.
    let mut beta_run = seed_run(&beta, "jrun-beta-older", "aggregate", JobRunState::Success);
    beta_run.created_at = now - Duration::hours(3);
    beta_run.scheduled_at = beta_run.created_at;
    beta_run.started_at = Some(beta_run.created_at);
    beta_run.finished_at = Some(now - Duration::hours(2) - Duration::minutes(30));
    write_seeded_run(&beta, &beta_run);

    let entries = vec![
        workspace_entry("alpha", alpha_repo, alpha_orbit, true),
        workspace_entry("beta", beta_repo, beta_orbit, true),
    ];
    let state = DashboardState::global(global_root, entries, Some("alpha".to_string()));

    let response = router()
        .with_state(state)
        .oneshot(get("/job-runs/all?limit=1"))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let rows = body["items"].as_array().expect("items");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["run_id"],
        json!("jrun-old-long-running"),
        "the run that finished most recently must win, even though another run in the \
         same workspace was created more recently"
    );
    assert_eq!(rows[0]["workspace_id"], json!("alpha"));
}

/// ORB-10008: workspace selection failures surface over HTTP as clean 4xx
/// JSON bodies (unknown -> 404, inactive -> 400, no default -> 400), never a
/// 500 or a panic.
#[tokio::test]
async fn workspace_selection_errors_are_clean_4xx_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (orbit_dir, repo_root) = seed_workspace(&global_root, tmp.path(), "alpha");
    let entries = vec![
        workspace_entry("alpha", repo_root, orbit_dir, true),
        workspace_entry(
            "stale",
            tmp.path().join("missing"),
            tmp.path().join("missing/.orbit"),
            false,
        ),
    ];
    // No default workspace configured: requests must select one explicitly.
    let state = DashboardState::global(global_root, entries, None);

    // Unknown workspace id -> 404 with a JSON error body.
    let response = router()
        .with_state(state.clone())
        .oneshot(get("/tasks?workspace=ghost"))
        .await
        .expect("response");
    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|m| m.contains("unknown workspace: ghost"))
    );

    // Inactive (stale-path) workspace -> 400, never built.
    let response = router()
        .with_state(state.clone())
        .oneshot(get("/tasks?workspace=stale"))
        .await
        .expect("response");
    assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|m| m.contains("inactive"))
    );

    // No selection and no default -> 400 telling the caller what to pass.
    let response = router()
        .with_state(state.clone())
        .oneshot(get("/tasks"))
        .await
        .expect("response");
    assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|m| m.contains("workspace"))
    );

    // A valid selection on the same state still works.
    let response = router()
        .with_state(state)
        .oneshot(get("/tasks?workspace=alpha"))
        .await
        .expect("response");
    assert_eq!(response.status(), axum::http::StatusCode::OK);
}

/// ORB-10291: a task in one registered workspace depending on a task in
/// another workspace sharing the same global root must resolve that
/// dependency's real status, not `[missing]`. Dependency resolution has to
/// use the coordination registry's global status projection
/// (`OrbitRuntime::task_status_index`) rather than the selected workspace's
/// own task list.
#[tokio::test]
async fn cross_workspace_dependency_resolves_global_status_not_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");

    let (beta_orbit, beta_repo) = seed_workspace(&global_root, tmp.path(), "beta");
    let beta_task = {
        let beta_runtime = OrbitRuntime::from_roots(&global_root, &beta_orbit)
            .expect("build beta runtime")
            .with_actor(ActorIdentity::human("human"));
        beta_runtime
            .add_task(TaskAddParams {
                title: "beta dependency".to_string(),
                description: "seed".to_string(),
                status: Some(TaskStatus::Done),
                ..Default::default()
            })
            .expect("add beta task")
    };

    let alpha_repo = tmp.path().join("alpha");
    let alpha_orbit = alpha_repo.join(".orbit");
    std::fs::create_dir_all(&alpha_orbit).expect("create .orbit");
    std::fs::write(alpha_orbit.join("config.toml"), "").expect("write config");
    std::fs::write(
        alpha_orbit.join("config.yaml"),
        "schema_version: 1\nworkspace_id: ws_alpha\n",
    )
    .expect("write workspace identity");
    let alpha_task = {
        let alpha_runtime = OrbitRuntime::from_roots(&global_root, &alpha_orbit)
            .expect("build alpha runtime")
            .with_actor(ActorIdentity::human("human"));
        alpha_runtime
            .add_task(TaskAddParams {
                title: "alpha dependent".to_string(),
                description: "seed".to_string(),
                status: Some(TaskStatus::InProgress),
                dependencies: vec![beta_task.id.clone()],
                ..Default::default()
            })
            .expect("add alpha task")
    };

    let entries = vec![
        workspace_entry("alpha", alpha_repo, alpha_orbit, true),
        workspace_entry("beta", beta_repo, beta_orbit, true),
    ];
    let state = DashboardState::global(global_root, entries, Some("alpha".to_string()));
    let expected_label = format!("{} [done]", beta_task.id);

    // GET /tasks?workspace=alpha: the cross-workspace dependency resolves to
    // beta's real status, and the response contains only alpha's own task.
    let response = router()
        .with_state(state.clone())
        .oneshot(get("/tasks?workspace=alpha"))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    // ORB-10400: GET /tasks answers `{ items, total, limit, truncated }`.
    let tasks = body["items"].as_array().expect("items");
    assert_eq!(
        tasks.len(),
        1,
        "workspace-local list must contain only alpha's own task, got {tasks:?}"
    );
    let alpha_row = &tasks[0];
    assert_eq!(alpha_row["id"], json!(alpha_task.id));
    let labels: Vec<&str> = alpha_row["resolved_dependencies"]
        .as_array()
        .expect("resolved_dependencies array")
        .iter()
        .map(|value| value.as_str().expect("dependency label"))
        .collect();
    assert_eq!(labels, vec![expected_label.as_str()]);

    // GET /tasks/<alpha-id>?workspace=alpha: the show projection reports the
    // identical cross-workspace dependency status.
    let response = router()
        .with_state(state.clone())
        .oneshot(get(&format!("/tasks/{}?workspace=alpha", alpha_task.id)))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let labels: Vec<&str> = body["resolved_dependencies"]
        .as_array()
        .expect("resolved_dependencies array")
        .iter()
        .map(|value| value.as_str().expect("dependency label"))
        .collect();
    assert_eq!(labels, vec![expected_label.as_str()]);
    let aggregate = router()
        .with_state(state)
        .oneshot(get("/tasks/all"))
        .await
        .expect("aggregate response");
    let body = body_json(aggregate).await;
    let alpha_row = body
        .get("items")
        .and_then(serde_json::Value::as_array)
        .expect("aggregate rows")
        .iter()
        .find(|row| row["id"] == alpha_task.id)
        .expect("alpha row");
    assert_eq!(alpha_row["resolved_dependencies"], json!([expected_label]));
}

/// ORB-00037: the pure display helper collapses `$HOME` to `~` and otherwise
/// renders paths verbatim.
#[test]
fn abbreviate_home_collapses_home_prefix() {
    use super::super::workspaces::abbreviate_home;

    let home = PathBuf::from("/home/dan");
    assert_eq!(
        abbreviate_home(&PathBuf::from("/home/dan/ws/orbit"), Some(&home)),
        "~/ws/orbit"
    );
    // The home directory itself collapses to a bare `~`.
    assert_eq!(abbreviate_home(&home, Some(&home)), "~");
    // Paths outside home are untouched.
    assert_eq!(
        abbreviate_home(&PathBuf::from("/srv/data"), Some(&home)),
        "/srv/data"
    );
    // A sibling that merely shares a name prefix is not under home.
    assert_eq!(
        abbreviate_home(&PathBuf::from("/home/danish/x"), Some(&home)),
        "/home/danish/x"
    );
    // No home configured => render verbatim.
    assert_eq!(
        abbreviate_home(&PathBuf::from("/home/dan/ws"), None),
        "/home/dan/ws"
    );
}

fn post_json(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::ORIGIN, "http://localhost:7878")
        .header(header::HOST, "localhost:7878")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

/// Titles of the dashboard task list for one workspace, via the HTTP surface.
async fn task_titles(state: &DashboardState, workspace: &str) -> Vec<String> {
    let response = router()
        .with_state(state.clone())
        .oneshot(get(&format!("/tasks?workspace={workspace}")))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    // ORB-10400: GET /tasks answers `{ items, total, limit, truncated }`.
    body_json(response).await["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|t| t["title"].as_str().expect("title").to_string())
        .collect()
}

/// ORB-00042: `POST /tasks?workspace=<id>` creates the task in *that*
/// workspace, not the server's default.
#[tokio::test]
async fn create_task_with_workspace_param_binds_to_that_workspace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    let (beta_orbit, beta_repo) = seed_workspace(&global_root, tmp.path(), "beta");
    let entries = vec![
        workspace_entry("alpha", alpha_repo, alpha_orbit, true),
        workspace_entry("beta", beta_repo, beta_orbit, true),
    ];
    // alpha is the default: an unrouted create would land there.
    let state = DashboardState::global(global_root, entries, Some("alpha".to_string()));

    let response = router()
        .with_state(state.clone())
        .oneshot(post_json(
            "/tasks?workspace=beta",
            json!({ "title": "routed to beta", "description": "ORB-00042", "complexity": "low" }),
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let created = body_json(response).await;
    assert!(created["id"].is_string(), "create returns the task");

    let beta_titles = task_titles(&state, "beta").await;
    assert!(
        beta_titles.iter().any(|t| t == "routed to beta"),
        "task must live in the selected workspace, got {beta_titles:?}"
    );
    let alpha_titles = task_titles(&state, "alpha").await;
    assert!(
        !alpha_titles.iter().any(|t| t == "routed to beta"),
        "task must not leak into the default workspace, got {alpha_titles:?}"
    );

    // Omitting `?workspace=` falls back to the configured default (alpha).
    let response = router()
        .with_state(state.clone())
        .oneshot(post_json(
            "/tasks",
            json!({ "title": "defaulted to alpha", "description": "ORB-00042", "complexity": "low" }),
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let alpha_titles = task_titles(&state, "alpha").await;
    assert!(
        alpha_titles.iter().any(|t| t == "defaulted to alpha"),
        "omitted selector must use the default workspace, got {alpha_titles:?}"
    );
}

/// ORB-00042: creating against an unknown workspace is a clean 404 and no
/// task is created anywhere — never a silent fallback to the default.
#[tokio::test]
async fn create_task_with_unknown_workspace_is_404_and_creates_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    let entries = vec![workspace_entry("alpha", alpha_repo, alpha_orbit, true)];
    let state = DashboardState::global(global_root, entries, Some("alpha".to_string()));

    let response = router()
        .with_state(state.clone())
        .oneshot(post_json(
            "/tasks?workspace=ghost",
            json!({ "title": "lost", "description": "should not exist", "complexity": "low" }),
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|m| m.contains("unknown workspace: ghost"))
    );

    let alpha_titles = task_titles(&state, "alpha").await;
    assert!(
        !alpha_titles.iter().any(|t| t == "lost"),
        "nothing may be created in the default workspace, got {alpha_titles:?}"
    );
}

// ---------------------------------------------------------------------------
// ORB-10294: refresh orbit-web workspace state after native registry mutations.
// A registry-backed `DashboardState` reloads `~/.orbit/workspaces.json` at each
// request boundary, so native `orbit workspace init/remove` and binding changes
// are honored without a restart.
// ---------------------------------------------------------------------------

/// Write a registry file at `<global_root>/workspaces.json` binding each
/// `(id, repo_root)` as an active, owner-role workspace.
fn write_registry(global_root: &Path, workspaces: &[(&str, &Path)]) {
    let workspaces: Vec<_> = workspaces
        .iter()
        .map(|(id, repo_root)| (*id, *repo_root, None))
        .collect();
    write_registry_with_ship_modes(global_root, &workspaces);
}

fn write_registry_with_ship_modes(global_root: &Path, workspaces: &[(&str, &Path, Option<&str>)]) {
    let now = Utc::now();
    let mut registry = WorkspaceRegistry::default();
    for (id, repo_root, ship_mode) in workspaces {
        registry.workspaces.push(Workspace {
            id: (*id).to_string(),
            name: (*id).to_string(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: ship_mode.map(str::to_string),
            base_branch: "main".to_string(),
            status: WorkspaceStatus::Active,
            created_at: now,
            updated_at: now,
        });
        registry.checkouts.push(WorkspaceCheckout::owner(
            (*id).to_string(),
            repo_root.to_path_buf(),
            repo_root.join(".orbit"),
        ));
    }
    orbit_registry::workspace_registry::save_registry_to(
        &registry,
        &global_root.join("workspaces.json"),
    )
    .expect("save registry");
}

/// A registry-backed state reloading from `<global_root>/workspaces.json`.
fn registry_state(global_root: &Path) -> DashboardState {
    let source = RegistrySource::new(global_root.join("workspaces.json"), None, None);
    DashboardState::from_registry(global_root.to_path_buf(), source).expect("from_registry")
}

/// Sorted workspace ids as seen over `GET /api/workspaces` (which refreshes).
async fn workspace_ids(state: &DashboardState) -> Vec<String> {
    let response = router()
        .with_state(state.clone())
        .oneshot(get("/workspaces"))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut ids: Vec<String> = body_json(response)
        .await
        .as_array()
        .expect("array")
        .iter()
        .map(|w| w["id"].as_str().expect("id").to_string())
        .collect();
    ids.sort();
    ids
}

async fn route_status(state: &DashboardState, workspace: &str) -> StatusCode {
    router()
        .with_state(state.clone())
        .oneshot(get(&format!("/tasks?workspace={workspace}")))
        .await
        .expect("response")
        .status()
}

/// The cached runtime for `id`, or `None` if the server holds none open.
fn open_runtime(state: &DashboardState, id: &str) -> Option<Arc<OrbitRuntime>> {
    state
        .open_runtimes()
        .into_iter()
        .find(|(ws, _)| ws == id)
        .map(|(_, runtime)| runtime)
}

/// A native workspace add becomes visible through `/api/workspaces` and
/// routable through the workspace-scoped API without a restart.
#[tokio::test]
async fn refresh_surfaces_native_workspace_add() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (_alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    write_registry(&global_root, &[("alpha", &alpha_repo)]);
    let state = registry_state(&global_root);

    assert_eq!(workspace_ids(&state).await, vec!["alpha".to_string()]);
    assert_eq!(route_status(&state, "beta").await, StatusCode::NOT_FOUND);

    // Native add: seed beta on disk and append it to the registry.
    let (_beta_orbit, beta_repo) = seed_workspace(&global_root, tmp.path(), "beta");
    write_registry(
        &global_root,
        &[("alpha", &alpha_repo), ("beta", &beta_repo)],
    );

    assert_eq!(
        workspace_ids(&state).await,
        vec!["alpha".to_string(), "beta".to_string()]
    );
    assert_eq!(route_status(&state, "beta").await, StatusCode::OK);
}

/// A native removal disappears from discovery and routing and evicts its cached
/// runtime, without disturbing another workspace's live runtime.
#[tokio::test]
async fn refresh_removes_workspace_and_evicts_only_its_runtime() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (_alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    let (_beta_orbit, beta_repo) = seed_workspace(&global_root, tmp.path(), "beta");
    write_registry(
        &global_root,
        &[("alpha", &alpha_repo), ("beta", &beta_repo)],
    );
    let state = registry_state(&global_root);

    // Build + cache both runtimes.
    assert_eq!(route_status(&state, "alpha").await, StatusCode::OK);
    assert_eq!(route_status(&state, "beta").await, StatusCode::OK);
    let alpha_runtime = open_runtime(&state, "alpha").expect("alpha open");
    assert!(
        open_runtime(&state, "beta").is_some(),
        "beta should be open"
    );

    // Native remove of beta.
    write_registry(&global_root, &[("alpha", &alpha_repo)]);
    state.refresh();

    assert_eq!(workspace_ids(&state).await, vec!["alpha".to_string()]);
    assert_eq!(route_status(&state, "beta").await, StatusCode::NOT_FOUND);
    // beta's runtime is evicted; alpha's is the *same* handle, never rebuilt.
    assert!(
        open_runtime(&state, "beta").is_none(),
        "beta runtime evicted"
    );
    let alpha_after = open_runtime(&state, "alpha").expect("alpha still open");
    assert!(
        Arc::ptr_eq(&alpha_runtime, &alpha_after),
        "alpha runtime must be untouched by beta's removal"
    );
}

/// A changed root/orbit-dir binding invalidates the old runtime and routes
/// subsequent requests through the new validated binding.
#[tokio::test]
async fn refresh_rebinds_workspace_to_new_checkout() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (_alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    write_registry(&global_root, &[("alpha", &alpha_repo)]);
    let state = registry_state(&global_root);

    let titles = task_titles(&state, "alpha").await;
    assert!(titles.iter().any(|t| t == "alpha task"), "got {titles:?}");
    let before = open_runtime(&state, "alpha").expect("alpha open");
    assert_eq!(
        before
            .workspace_runtime_binding()
            .expect("dashboard runtime binding")
            .workspace_id,
        "ws_alpha"
    );

    // Rebind `alpha` to a different on-disk checkout with a distinct task.
    let (_v2_orbit, v2_repo) = seed_workspace(&global_root, tmp.path(), "alpha_v2");
    write_registry(&global_root, &[("alpha", &v2_repo)]);
    state.refresh();

    let titles = task_titles(&state, "alpha").await;
    assert!(
        titles.iter().any(|t| t == "alpha_v2 task") && !titles.iter().any(|t| t == "alpha task"),
        "rebound workspace must serve the new checkout's tasks, got {titles:?}"
    );
    let after = open_runtime(&state, "alpha").expect("alpha open");
    assert!(
        !Arc::ptr_eq(&before, &after),
        "a rebind must invalidate the old runtime"
    );
    assert_eq!(
        after
            .workspace_runtime_binding()
            .expect("dashboard runtime binding")
            .workspace_id,
        "ws_alpha_v2",
        "logical registry id may differ from the runtime's configured id"
    );
}

/// A registry-only ship-mode change is part of the authoritative runtime
/// binding even when the checkout paths are unchanged. Refresh must therefore
/// evict the cached runtime and rebuild it with the new mode.
#[tokio::test]
async fn refresh_rebuilds_runtime_for_ship_mode_only_change() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (_alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    write_registry_with_ship_modes(&global_root, &[("alpha", &alpha_repo, Some("local"))]);
    let state = registry_state(&global_root);

    assert_eq!(route_status(&state, "alpha").await, StatusCode::OK);
    let before = open_runtime(&state, "alpha").expect("alpha open");
    assert_eq!(
        before
            .workspace_runtime_binding()
            .expect("dashboard runtime binding")
            .ship_mode,
        ShipMode::Local
    );

    write_registry_with_ship_modes(&global_root, &[("alpha", &alpha_repo, Some("pr"))]);
    state.refresh();
    assert!(
        open_runtime(&state, "alpha").is_none(),
        "binding-only change must evict the cached runtime"
    );

    assert_eq!(route_status(&state, "alpha").await, StatusCode::OK);
    let after = open_runtime(&state, "alpha").expect("alpha reopened");
    assert!(
        !Arc::ptr_eq(&before, &after),
        "ship-mode-only change must rebuild the runtime"
    );
    assert_eq!(
        after
            .workspace_runtime_binding()
            .expect("dashboard runtime binding")
            .ship_mode,
        ShipMode::Pr
    );
}

/// A path that disappears after startup is reported inactive on refresh, not
/// left falsely active and not auto-deleted from the registry.
#[tokio::test]
async fn refresh_marks_vanished_path_inactive_without_deleting_record() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (_alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    write_registry(&global_root, &[("alpha", &alpha_repo)]);
    let state = registry_state(&global_root);
    assert_eq!(route_status(&state, "alpha").await, StatusCode::OK);

    // The checkout vanishes after startup; the registry record stays.
    std::fs::remove_dir_all(&alpha_repo).expect("remove checkout");
    state.refresh();

    let response = router()
        .with_state(state.clone())
        .oneshot(get("/workspaces"))
        .await
        .expect("response");
    let listed = body_json(response).await;
    let alpha = listed
        .as_array()
        .expect("array")
        .iter()
        .find(|w| w["id"] == json!("alpha"))
        .expect("alpha still listed");
    assert_eq!(alpha["status"], json!("invalid"), "reported inactive");
    // Routing an inactive workspace is a clean 400, never a rebuild attempt.
    assert_eq!(route_status(&state, "alpha").await, StatusCode::BAD_REQUEST);
    // The operator record is not auto-deleted.
    let content =
        std::fs::read_to_string(global_root.join("workspaces.json")).expect("read registry");
    assert!(
        content.contains("\"alpha\""),
        "registry record must not be auto-deleted, got {content}"
    );
}

/// A malformed or partially-written registry cannot replace the last valid
/// in-memory snapshot; the previous workspace set stays visible and routable.
#[tokio::test]
async fn refresh_retains_last_valid_snapshot_on_malformed_registry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (_alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    write_registry(&global_root, &[("alpha", &alpha_repo)]);
    let state = registry_state(&global_root);
    assert_eq!(workspace_ids(&state).await, vec!["alpha".to_string()]);

    // Corrupt the registry mid-flight.
    std::fs::write(
        global_root.join("workspaces.json"),
        "{ this is not valid json",
    )
    .expect("corrupt registry");

    // The refresh triggered by these requests keeps the last valid snapshot.
    assert_eq!(workspace_ids(&state).await, vec!["alpha".to_string()]);
    assert_eq!(route_status(&state, "alpha").await, StatusCode::OK);
}

/// Refreshes and reads race safely: concurrent `refresh`, `entries`,
/// `runtime_for`, and `open_runtimes` never deadlock or corrupt state, and the
/// runtime cache stays idempotent (construction happens off the lock).
#[test]
fn concurrent_refresh_and_reads_stay_consistent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (_alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    let (_beta_orbit, beta_repo) = seed_workspace(&global_root, tmp.path(), "beta");
    write_registry(
        &global_root,
        &[("alpha", &alpha_repo), ("beta", &beta_repo)],
    );
    let state = registry_state(&global_root);

    // Warm the cache so threads mostly exercise the fast path + refresh race.
    let alpha0 = state.runtime_for("alpha").expect("alpha");
    let _ = state.runtime_for("beta").expect("beta");

    std::thread::scope(|scope| {
        for _ in 0..8 {
            let state = &state;
            scope.spawn(move || {
                for _ in 0..50 {
                    state.refresh();
                    assert_eq!(state.entries().len(), 2);
                    let _ = state.runtime_for("alpha").expect("alpha");
                    let _ = state.open_runtimes();
                }
            });
        }
    });

    assert_eq!(state.entries().len(), 2);
    // The static binding is never evicted, so the cache handle is stable.
    let alpha1 = state.runtime_for("alpha").expect("alpha");
    assert!(Arc::ptr_eq(&alpha0, &alpha1), "idempotent runtime cache");
}

/// Metadata selection used by the aggregate exposes a runtime's binding.
fn runtime_task_titles(runtime: &OrbitRuntime) -> Vec<String> {
    runtime
        .task_candidates(&Default::default(), orbit_core::DEFAULT_TASK_LIST_LIMIT)
        .expect("list candidates")
        .items
        .into_iter()
        .map(|task| task.title)
        .collect()
}

/// Barrier-controlled rebind-during-build: a runtime built against the old
/// binding, paused mid-flight while the registry is rebound and refreshed, must
/// NOT republish as current when it finally reaches the cache. The new-binding
/// runtime stays authoritative, the old build is returned only to its own
/// request, and `open_runtimes` (which every aggregate/health response and
/// `/api/tasks/all` derives from) surfaces exactly the new checkout — never the
/// stale one. Covers finding P1 (stale runtime publication) deterministically.
#[test]
fn stale_build_during_rebind_never_republishes_as_current() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (_alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    write_registry(&global_root, &[("alpha", &alpha_repo)]);
    let state = registry_state(&global_root);

    // The racing thread performs the *first* build, so leave the cache cold.
    let paused = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let fired = Arc::new(AtomicBool::new(false));
    {
        let paused = paused.clone();
        let release = release.clone();
        let fired = fired.clone();
        state.set_pre_publish_hook(Arc::new(move |_id: &str| {
            // Only the first build (the racing old-binding build) pauses; the
            // main thread's later new-binding build passes straight through.
            if !fired.swap(true, Ordering::SeqCst) {
                paused.wait();
                release.wait();
            }
        }));
    }

    let new_runtime = std::thread::scope(|scope| {
        let racer = {
            let state = &state;
            scope.spawn(move || state.runtime_for("alpha").expect("old-binding build"))
        };

        // Wait until the racer has built the old-binding runtime and parked
        // itself just before publication.
        paused.wait();

        // Rebind alpha to a fresh checkout and refresh: a new generation, and
        // the cold cache means nothing is evicted.
        let (_v2_orbit, v2_repo) = seed_workspace(&global_root, tmp.path(), "alpha_v2");
        write_registry(&global_root, &[("alpha", &v2_repo)]);
        state.refresh();

        // Resolve the new binding — builds and publishes the new-generation
        // runtime while the racer is still parked.
        let new_runtime = state.runtime_for("alpha").expect("new-binding build");

        // Release the racer; it now attempts to publish its stale old build.
        release.wait();
        let old_runtime = racer.join().expect("join racer");

        assert!(
            !Arc::ptr_eq(&old_runtime, &new_runtime),
            "old build is a distinct runtime, returned only to its own request"
        );
        // The stale build must not have overwritten the new cache entry.
        let current = state.runtime_for("alpha").expect("current");
        assert!(
            Arc::ptr_eq(&current, &new_runtime),
            "stale old-generation build must never republish as current"
        );
        // The old build serves the old checkout; current serves the new one.
        assert!(
            runtime_task_titles(&old_runtime).contains(&"alpha task".to_string()),
            "old build binds the original checkout"
        );
        assert!(
            runtime_task_titles(&current).contains(&"alpha_v2 task".to_string()),
            "current binds the rebound checkout"
        );
        new_runtime
    });

    // open_runtimes joins by exact binding, so the stale runtime is filtered
    // out entirely: exactly one alpha runtime, and it is the new checkout's.
    let open = state.open_runtimes();
    let alpha_open: Vec<_> = open.iter().filter(|(id, _)| id == "alpha").collect();
    assert_eq!(alpha_open.len(), 1, "one coherent alpha runtime open");
    assert!(
        Arc::ptr_eq(&alpha_open[0].1, &new_runtime),
        "open_runtimes surfaces only the current-binding runtime"
    );
    assert!(
        runtime_task_titles(&alpha_open[0].1).contains(&"alpha_v2 task".to_string()),
        "the open runtime is never tagged as the wrong (old) checkout"
    );
}

/// Detailed `/healthz` derives its checks and its `workspaces_open` count from a
/// single pinned generation: a native removal drops the workspace from both in
/// one coherent step, never probing or counting a lingering checkout. Covers the
/// detailed-health arm of finding P1.
#[tokio::test]
async fn detailed_healthz_reflects_refresh_in_one_coherent_generation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (_alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    let (_beta_orbit, beta_repo) = seed_workspace(&global_root, tmp.path(), "beta");
    write_registry(
        &global_root,
        &[("alpha", &alpha_repo), ("beta", &beta_repo)],
    );
    let state = registry_state(&global_root);

    // Build + cache both runtimes so detailed health has them open.
    assert_eq!(route_status(&state, "alpha").await, StatusCode::OK);
    assert_eq!(route_status(&state, "beta").await, StatusCode::OK);

    let log_dir = tempfile::tempdir().expect("log tempdir");
    let log_path = log_dir.path().join("orbit.jsonl");

    let before =
        body_json(crate::health::detailed_response(&state, Ok(log_path.clone())).await).await;
    assert_eq!(before["workspaces_open"], json!(2));
    assert_eq!(health_workspaces(&before), HashSet::from(["alpha", "beta"]));

    // Native remove of beta; detailed health refreshes and must drop beta from
    // both the count and the per-workspace checks together.
    write_registry(&global_root, &[("alpha", &alpha_repo)]);

    let after = body_json(crate::health::detailed_response(&state, Ok(log_path)).await).await;
    assert_eq!(after["workspaces_open"], json!(1));
    assert_eq!(health_workspaces(&after), HashSet::from(["alpha"]));
}

/// The distinct workspace names named by a detailed `/healthz` body's checks.
fn health_workspaces(body: &serde_json::Value) -> HashSet<&str> {
    body["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .filter_map(|check| check["workspace"].as_str())
        .collect()
}

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl<'a> MakeWriter<'a> for SharedBuf {
    type Writer = SharedBufWriter;
    fn make_writer(&'a self) -> Self::Writer {
        SharedBufWriter(self.0.clone())
    }
}

struct SharedBufWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedBufWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("buffer lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A malformed refresh keeps the last valid snapshot AND emits a credential-safe
/// diagnostic: the captured warning names the registry path and Orbit's error
/// but never echoes the registry file's contents, so a tokenized `git_remote`
/// on disk cannot leak into logs. Covers finding P2's diagnostic-safety arm.
#[test]
fn malformed_refresh_emits_credential_safe_diagnostic() {
    const SECRET: &str = "ghp_SUPERSECRETtoken0xDEADBEEF";

    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (_alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    write_registry(&global_root, &[("alpha", &alpha_repo)]);
    let state = registry_state(&global_root);
    assert_eq!(state.entries().len(), 1);

    // Corrupt the registry with malformed JSON that still embeds a secret-looking
    // token, exactly as a tokenized git_remote would appear on disk.
    let registry_path = global_root.join("workspaces.json");
    std::fs::write(
        &registry_path,
        format!("{{ \"git_remote\": \"https://x-access-token:{SECRET}@github.com/o/r\", INVALID"),
    )
    .expect("corrupt registry");

    let buf = SharedBuf::default();
    let subscriber = Registry::default().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(buf.clone()),
    );
    tracing::subscriber::with_default(subscriber, || {
        // Fail-soft: keeps the last valid snapshot and emits the diagnostic.
        state.refresh();
    });

    let logged = String::from_utf8(buf.0.lock().expect("buffer lock").clone()).expect("utf8 log");
    assert!(
        logged.contains("retaining last valid workspace set"),
        "the keep-last-valid diagnostic must be emitted, got {logged:?}"
    );
    assert!(
        logged.contains("workspaces.json"),
        "the diagnostic must name the registry path, got {logged:?}"
    );
    assert!(
        !logged.contains(SECRET),
        "the diagnostic must never echo registry contents/secrets, got {logged:?}"
    );
    // Keep-last-valid: the previous snapshot is untouched.
    assert_eq!(state.entries().len(), 1);
    assert_eq!(state.entries()[0].id, "alpha");
}

/// A broken registered checkout must not prevent the dashboard from starting
/// for its healthy peers. Its identity is never accepted from the registry:
/// without a readable config it remains listed but unavailable, and the
/// operator receives a diagnostic naming the affected checkout.
#[tokio::test]
async fn registry_startup_skips_unresolvable_checkout_and_serves_healthy_workspace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (_alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    let (beta_orbit, beta_repo) = seed_workspace(&global_root, tmp.path(), "beta");
    std::fs::remove_file(beta_orbit.join("config.yaml")).expect("remove beta identity");
    write_registry(
        &global_root,
        &[("alpha", &alpha_repo), ("beta", &beta_repo)],
    );

    let buf = SharedBuf::default();
    let subscriber = Registry::default().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(buf.clone()),
    );
    let state = tracing::subscriber::with_default(subscriber, || registry_state(&global_root));

    assert_eq!(route_status(&state, "alpha").await, StatusCode::OK);
    assert_eq!(route_status(&state, "beta").await, StatusCode::BAD_REQUEST);

    let response = router()
        .with_state(state)
        .oneshot(get("/workspaces"))
        .await
        .expect("response");
    let listed = body_json(response).await;
    let beta = listed
        .as_array()
        .expect("workspace array")
        .iter()
        .find(|workspace| workspace["id"] == json!("beta"))
        .expect("broken workspace remains visible");
    assert_eq!(beta["status"], json!("invalid"));

    let logged = String::from_utf8(buf.0.lock().expect("buffer lock").clone()).expect("utf8");
    assert!(
        logged.contains("registered workspace is unavailable")
            && logged.contains("beta/.orbit")
            && logged.contains("workspace config is missing"),
        "startup must identify the skipped checkout for operators, got {logged:?}"
    );
}
