//! Run history predicates and tool projection through the public runtime.
//! The store filters before hydration; task identity costs no task reads.
use chrono::{Duration, Utc};
use orbit_core::application::job::JobRunListParams;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{JobRunState, OrbitRuntime};
use orbit_engine::RuntimeHost;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[test]
fn run_pages_filter_in_sql_and_project_task_ids_without_task_reads() {
    if !super::dispatch_admission::isolated(
        "run_history::run_pages_filter_in_sql_and_project_task_ids_without_task_reads",
    ) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Run history fixture".into(),
            description: "Task identity and SQL predicates.".into(),
            ..Default::default()
        })
        .unwrap();
    let task_id = task.id.to_string();
    let now = Utc::now();
    let store = runtime.sqlite_store().unwrap();
    let partition = runtime.workspace_id().unwrap();
    let cases = [
        (
            "old",
            JobRunState::Failed,
            30,
            json!({"task_ids":[task_id]}),
        ),
        (
            "failed",
            JobRunState::Failed,
            3,
            json!({"task_ids":[task_id, task_id]}),
        ),
        ("held", JobRunState::Held, 2, json!({"task_ids":[task_id]})),
        (
            "success",
            JobRunState::Success,
            1,
            json!({"task_ids":[task_id]}),
        ),
        (
            "singular",
            JobRunState::Success,
            2,
            json!({"task_id":task_id}),
        ),
        ("empty", JobRunState::Failed, 0, json!({"task_ids":[]})),
        (
            "malformed",
            JobRunState::Failed,
            0,
            json!({"task_ids":"not-an-array"}),
        ),
    ];
    for (index, (id, state, hours, input)) in cases
        .into_iter()
        .chain((0..193).map(|_| {
            (
                "distractor",
                JobRunState::Failed,
                0,
                json!({"task_ids":["other-task"]}),
            )
        }))
        .enumerate()
    {
        let at = now - Duration::hours(hours) + Duration::milliseconds(index as i64);
        let mut run = runtime
            .insert_job_run("task_pr_pipeline", 1, at, Some(input), None)
            .unwrap();
        run.state = state;
        run.created_at = at;
        // Use a distinguishable job for assertions without changing allocated IDs.
        run.job_id = id.to_owned();
        store
            .upsert_job_run_for_workspace(&partition, &run, None)
            .unwrap();
    }
    let since = now - Duration::hours(24);
    for (params, expected) in [
        (
            JobRunListParams {
                task_id: Some(task_id.clone()),
                ..Default::default()
            },
            "success",
        ),
        (
            JobRunListParams {
                states: vec![JobRunState::Failed, JobRunState::Held],
                ..Default::default()
            },
            "distractor",
        ),
        (
            JobRunListParams {
                since: Some(since),
                ..Default::default()
            },
            "distractor",
        ),
        (
            JobRunListParams {
                task_id: Some(task_id.clone()),
                states: vec![JobRunState::Failed, JobRunState::Held],
                since: Some(since),
                ..Default::default()
            },
            "held",
        ),
        (
            JobRunListParams {
                task_id: Some(task_id.clone()),
                states: vec![JobRunState::Failed, JobRunState::Held],
                state: Some(JobRunState::Failed),
                since: Some(since),
                ..Default::default()
            },
            "failed",
        ),
    ] {
        let params = JobRunListParams {
            limit: Some(1),
            ..params
        };
        let runs = runtime.list_job_runs_observed(params.clone()).unwrap();
        assert_eq!(runs.len(), 1, "{params:?}");
        assert_eq!(runs[0].job_id, expected, "{params:?}");
    }
    let combined = runtime
        .list_job_runs_observed(JobRunListParams {
            task_id: Some(task_id.clone()),
            states: vec![JobRunState::Failed, JobRunState::Held],
            since: Some(since),
            limit: Some(200),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        combined
            .iter()
            .map(|run| run.job_id.as_str())
            .collect::<Vec<_>>(),
        ["held", "failed"]
    );
    // Array and singular bindings both match, each run once; empty,
    // malformed and unbound runs stay out.
    let bound = runtime
        .list_job_runs_observed(JobRunListParams {
            task_id: Some(task_id.clone()),
            since: Some(since),
            limit: Some(200),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(bound.len(), 4);
    assert_eq!(
        bound
            .iter()
            .map(|run| run.job_id.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["failed", "held", "singular", "success"])
    );
    assert!(
        runtime
            .list_job_runs_observed(JobRunListParams {
                since: Some(now + Duration::seconds(1)),
                limit: Some(1),
                ..Default::default()
            })
            .unwrap()
            .is_empty()
    );
    let held = runtime
        .list_job_runs_observed(JobRunListParams {
            states: vec![JobRunState::Held],
            limit: Some(1),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(held[0].job_id, "held");
    // Corrupt an excluded row: client-side filtering would deserialize it and fail.
    let connection = store.connection();
    connection.lock().unwrap().execute(
        "UPDATE job_runs SET knowledge_metrics_json = '{' WHERE workspace_id = ?1 AND job_id = 'distractor'",
        [&partition],
    ).unwrap();
    let matched = runtime
        .list_job_runs_observed(JobRunListParams {
            task_id: Some(task_id.clone()),
            states: vec![JobRunState::Failed, JobRunState::Held],
            since: Some(since),
            limit: Some(1),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(matched[0].job_id, "held");
    connection
        .lock()
        .unwrap()
        .execute(
            "UPDATE job_runs SET knowledge_metrics_json = NULL WHERE workspace_id = ?1",
            [&partition],
        )
        .unwrap();

    let log = root.path().join("task-queries.jsonl");
    tracing_subscriber::fmt()
        .json()
        .with_env_filter("orbit.store.task_query=trace")
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NEW)
        .with_span_list(false)
        .with_writer(std::sync::Mutex::new(std::fs::File::create(&log).unwrap()))
        .init();
    for limit in [1, 200] {
        let before = std::fs::read_to_string(&log).unwrap().lines().count();
        let runs = runtime
            .list_job_runs_observed(JobRunListParams {
                limit: Some(limit),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(runs.len(), limit);
        let output = runtime
            .execute_tool_command_with_session_context(
                "orbit.workflow.run.list",
                json!({"limit": limit}),
                None,
                Some("codex".into()),
                ToolSessionContext {
                    effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                    ..Default::default()
                },
            )
            .unwrap();
        let items = output["items"].as_array().unwrap();
        assert_eq!(items.len(), limit);
        if limit == 200 {
            for item in items {
                let expected = match item["job_id"].as_str().unwrap() {
                    "empty" | "malformed" => Value::Null,
                    "distractor" => json!(["other-task"]),
                    _ => json!([task_id]),
                };
                assert_eq!(item["task_ids"], expected, "{item}");
            }
        }
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().lines().count() - before,
            0,
            "1- and 200-run pages must issue zero task-store queries, including projection"
        );
    }
    // Prove the query counter observes actual reads, rather than a disabled target.
    runtime.get_task(&task_id).unwrap();
    assert!(
        !std::fs::read_to_string(&log).unwrap().is_empty(),
        "task-query tracing must be active"
    );
}
