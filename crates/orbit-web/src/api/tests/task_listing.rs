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
                    .header("host", "localhost:7878")
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
                .header("host", "localhost:7878")
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
    let artifact = find_artifact_blob(&alpha.global_root(), "file.json").unwrap();
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
                .header("host", "localhost:7878")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let values = body_json(response).await;
    let rows = values["items"].as_array().unwrap();
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
        let artifact = find_artifact_blob(&runtime.global_root(), "file.json").unwrap();
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

/// DANI-10391: a list row is a summary — no prose bodies, counts in place of
/// the comment/history/artifact logs, governed transitions without their
/// evidence requirement, and a crew read from the registry alone — while the
/// detail endpoint keeps the full projection the list used to duplicate per row.
#[tokio::test]
async fn list_rows_are_summaries_and_detail_carries_the_bodies() {
    let runtime = Arc::new(OrbitRuntime::in_memory().unwrap());
    let task = seed_task_with_artifact(&runtime);
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                comment: Some("Nonempty review evidence".to_string()),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    let statuses = runtime.task_status_index().unwrap();
    let task = runtime.get_task(&task.id).unwrap();
    let comments = runtime.get_task_comments(&task.id).unwrap();
    let history = runtime.get_task_history(&task.id).unwrap();
    let artifacts = runtime.get_task_artifact_manifest(&task.id).unwrap();
    assert!(!comments.is_empty());
    assert!(!history.is_empty());
    assert!(!artifacts.is_empty());

    let mut detail_expected = crate::projections::task_to_json(&task, &statuses);
    detail_expected["comments"] = serde_json::to_value(&comments).unwrap();
    detail_expected["history"] = serde_json::to_value(&history).unwrap();
    detail_expected["artifacts"] = crate::projections::task_artifact_manifest_to_json(&artifacts);
    detail_expected["status_transitions"] = json!([
        { "status": "in-progress", "required_field": null },
        { "status": "blocked", "required_field": null },
        { "status": "proposed", "required_field": null },
        { "status": "someday", "required_field": null },
        { "status": "rejected", "required_field": null },
        { "status": "archived", "required_field": null },
    ]);
    if let Some(crew) = runtime.resolved_crew_projection(&task).unwrap() {
        detail_expected["resolved_crew"] = json!(crew.name);
        detail_expected["crew_model"] = json!(crew.model);
    }
    // ORB-12516: the detail says whether `#runs?run_id=` resolves in *this*
    // checkout. A task with no run has nothing to navigate to. The list rows
    // below keep the summary shape and carry no such key.
    detail_expected["job_run_navigable"] = json!(false);

    let mut summary_expected = crate::projections::task_to_json(&task, &statuses);
    let summary_object = summary_expected.as_object_mut().unwrap();
    for body in [
        "description",
        "plan",
        "execution_summary",
        "acceptance_criteria",
    ] {
        assert!(
            summary_object.remove(body).is_some(),
            "{body} is a task field"
        );
    }
    summary_object.insert("projection".into(), json!("summary"));
    summary_object.insert("comment_count".into(), json!(comments.len()));
    summary_object.insert("history_count".into(), json!(history.len()));
    summary_object.insert("artifact_count".into(), json!(artifacts.len()));
    summary_object.insert(
        "status_transitions".into(),
        json!([
            { "status": "in-progress" },
            { "status": "blocked" },
            { "status": "proposed" },
            { "status": "someday" },
            { "status": "rejected" },
            { "status": "archived" },
        ]),
    );
    let registry = runtime.configured_crew_registry_projection();
    if let Some(default_crew) = registry
        .default_crew
        .as_deref()
        .and_then(|name| registry.crews.iter().find(|crew| crew.name == name))
    {
        summary_object.insert("resolved_crew".into(), json!(default_crew.name));
        summary_object.insert("crew_model".into(), json!(default_crew.model));
    }

    let list = body_json(request_shared(runtime.clone(), "/tasks").await).await;
    let detail = body_json(request_shared(runtime, &format!("/tasks/{}", task.id)).await).await;
    assert_eq!(list["items"][0], summary_expected);
    assert_eq!(detail, detail_expected);
}

/// The list path resolves a task's crew from the registry it built once for the
/// page, never from the task's job run; the run-recorded crew (which wins on
/// the detail projection) needs a store read per row.
#[tokio::test]
async fn list_rows_resolve_crew_without_reading_the_task_job_run() {
    use super::test_support::write_seeded_run;
    use orbit_core::JobRunState;

    let runtime = Arc::new(OrbitRuntime::in_memory().unwrap());
    let task = super::tasks::seed_backlog_task(&runtime, "Run-attributed task");
    let mut run = super::test_support::seed_run(&runtime, "jrun-crew", "job", JobRunState::Success);
    run.resolved_crew = Some("run-recorded-crew".to_string());
    run.crew_model = Some("run-recorded-model".to_string());
    write_seeded_run(&runtime, &run);
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                job_run_id: Some(Some(run.run_id.clone())),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();

    let detail =
        body_json(request_shared(runtime.clone(), &format!("/tasks/{}", task.id)).await).await;
    assert_eq!(detail["resolved_crew"], json!("run-recorded-crew"));
    assert_eq!(detail["crew_model"], json!("run-recorded-model"));

    let list = body_json(request_shared(runtime.clone(), "/tasks").await).await;
    let row = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == json!(task.id))
        .unwrap();
    assert_eq!(row["projection"], json!("summary"));
    assert_ne!(row["resolved_crew"], json!("run-recorded-crew"));
    let registry = runtime.configured_crew_registry_projection();
    let expected = registry
        .default_crew
        .as_deref()
        .and_then(|name| registry.crews.iter().find(|crew| crew.name == name))
        .map(|crew| json!(crew.name))
        .unwrap_or(serde_json::Value::Null);
    assert_eq!(row["resolved_crew"], expected);
    assert!(row.get("description").is_none());
    assert!(row.get("comments").is_none());
    assert!(row.get("history").is_none());
    assert!(row.get("artifacts").is_none());
    assert!(row.get("review").is_none());
    assert_eq!(row["comment_count"], json!(0));
    assert_eq!(row["artifact_count"], json!(0));
    assert!(
        row["status_transitions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|transition| transition.get("required_field").is_none()),
        "summary transitions leave the requirement to the detail endpoint"
    );
}
