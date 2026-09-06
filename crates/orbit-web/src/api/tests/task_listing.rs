//! Bounded listing and blocking-I/O regression coverage (ORB-11205).
use super::tasks::{find_artifact_blob, request_shared, seed_task_with_artifact};
use super::test_support::body_json;

#[tokio::test(flavor = "current_thread")]
async fn cold_workspace_resolution_leaves_unrelated_requests_runnable() {
    use super::workspaces::{seed_workspace, workspace_entry};
    use crate::state::DashboardState;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use tower::ServiceExt;
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    std::fs::create_dir_all(&global).unwrap();
    let (orbit, repo) = seed_workspace(&global, temp.path(), "alpha");
    let state = DashboardState::global(
        global,
        vec![workspace_entry("alpha", repo, orbit, true)],
        Some("alpha".to_string()),
    );
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let entered_tx = Mutex::new(Some(entered_tx));
    let release_rx = Mutex::new(release_rx);
    let progressed = Arc::new(AtomicBool::new(false));
    let observed = progressed.clone();
    state.set_pre_publish_hook(Arc::new(move |_| {
        if let Some(sender) = entered_tx.lock().unwrap().take() {
            sender.send(()).unwrap();
            observed.store(
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .is_ok(),
                Ordering::SeqCst,
            );
        }
    }));
    let pending_state = state.clone();
    let pending = tokio::spawn(async move {
        super::super::router()
            .with_state(pending_state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/tasks?workspace=alpha")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    entered_rx.await.unwrap();
    let unrelated = super::super::router()
        .with_state(state)
        .oneshot(
            axum::http::Request::builder()
                .uri("/workspaces")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unrelated.status(), StatusCode::OK);
    let _ = release_tx.send(());
    assert_eq!(pending.await.unwrap().status(), StatusCode::OK);
    assert!(progressed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn aggregate_selects_global_newest_rows_before_reading_off_page_workspace_bodies() {
    use super::workspaces::{seed_workspace, workspace_entry};
    use crate::state::DashboardState;
    use tower::ServiceExt;
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    std::fs::create_dir_all(&global).unwrap();
    let (alpha_orbit, alpha_repo) = seed_workspace(&global, temp.path(), "alpha");
    let alpha = OrbitRuntime::from_roots(&global, &alpha_orbit).unwrap();
    let corrupt = seed_task_with_artifact(&alpha);
    let artifact = find_artifact_blob(&alpha.data_root(), "file.json").unwrap();
    std::fs::write(artifact, "invalid artifact content").unwrap();
    assert!(alpha.get_task(&corrupt.id).is_err());
    let (beta_orbit, beta_repo) = seed_workspace(&global, temp.path(), "beta");
    let beta = OrbitRuntime::from_roots(&global, &beta_orbit).unwrap();
    let mut ids = Vec::new();
    for n in 0..55 {
        ids.push(super::tasks::seed_backlog_task(&beta, &format!("Beta {n}")).id);
    }
    let state = DashboardState::global(
        global,
        vec![
            workspace_entry("alpha", alpha_repo, alpha_orbit, true),
            workspace_entry("beta", beta_repo, beta_orbit, true),
        ],
        Some("alpha".to_string()),
    );
    let response = super::super::router()
        .with_state(state)
        .oneshot(
            axum::http::Request::builder()
                .uri("/tasks/all")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let values = body_json(response).await;
    let rows = values.as_array().unwrap();
    assert_eq!(rows.len(), 50);
    assert!(
        rows.iter()
            .all(|row| row["workspace_id"] == "beta" && row["workspace_name"] == "beta")
    );
    assert_eq!(
        rows.iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ids.iter()
            .rev()
            .take(50)
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
}
use axum::http::StatusCode;
use orbit_core::{OrbitRuntime, application::task::TaskUpdateParams};
use serde_json::json;
use std::sync::Arc;

/// A FIFO writer's successful open proves the task reader reached blocked I/O.
/// The OS-thread timeout only releases a broken implementation; readiness has
/// no sleeps and this test runs with a single Tokio worker (ORB-11205).
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn task_list_detail_and_aggregate_leave_async_requests_runnable_during_blocked_io() {
    use std::io::Write;
    for endpoint in ["list", "detail", "aggregate"] {
        let runtime = Arc::new(OrbitRuntime::in_memory().unwrap());
        let task = seed_task_with_artifact(&runtime);
        let artifact = find_artifact_blob(&runtime.data_root(), "file.json").unwrap();
        let description = artifact
            .ancestors()
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name == task.id.as_str())
            })
            .unwrap()
            .join("description.md");
        std::fs::remove_file(&description).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&description)
                .status()
                .unwrap()
                .success()
        );
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let writer = std::thread::spawn(move || {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(description)
                .unwrap();
            entered_tx.send(()).unwrap();
            let progressed = release_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .is_ok();
            file.write_all(b"Fixture task body").unwrap();
            progressed
        });
        let uri = match endpoint {
            "list" => "/tasks".to_string(),
            "detail" => format!("/tasks/{}", task.id),
            _ => "/tasks/all".to_string(),
        };
        let request_runtime = runtime.clone();
        let pending = tokio::spawn(async move { request_shared(request_runtime, &uri).await });
        entered_rx.await.unwrap();
        let unrelated = request_shared(runtime, "/workspaces").await;
        assert_eq!(unrelated.status(), StatusCode::OK);
        let _ = release_tx.send(());
        assert_eq!(pending.await.unwrap().status(), StatusCode::OK);
        assert!(
            writer.join().unwrap(),
            "{endpoint} blocked the async worker"
        );
    }
}

#[tokio::test]
async fn list_and_detail_reuse_bundle_sidecars_with_response_parity() {
    let runtime = Arc::new(OrbitRuntime::in_memory().unwrap());
    let task = seed_task_with_artifact(&runtime);
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                comment: Some("Nonempty review evidence".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    let statuses = runtime.task_status_index().unwrap();
    let task = runtime.get_task(&task.id).unwrap();
    let mut expected = crate::projections::task_to_json(&task, &statuses);
    expected["comments"] =
        serde_json::to_value(runtime.get_task_comments(&task.id).unwrap()).unwrap();
    expected["history"] =
        serde_json::to_value(runtime.get_task_history(&task.id).unwrap()).unwrap();
    expected["artifacts"] = crate::projections::task_artifact_manifest_to_json(
        &runtime.get_task_artifact_manifest(&task.id).unwrap(),
    );
    if let Some(crew) = runtime.resolved_crew_projection(&task).unwrap() {
        expected["resolved_crew"] = json!(crew.name);
        expected["crew_model"] = json!(crew.model);
    }
    assert!(!expected["comments"].as_array().unwrap().is_empty());
    assert!(!expected["history"].as_array().unwrap().is_empty());
    assert!(!expected["artifacts"].as_array().unwrap().is_empty());
    let list = body_json(request_shared(runtime.clone(), "/tasks").await).await;
    let detail = body_json(request_shared(runtime, &format!("/tasks/{}", task.id)).await).await;
    assert_eq!(list["items"][0], expected);
    assert_eq!(detail, expected);
}
