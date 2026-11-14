use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{JobRun, JobRunState, JobTargetType};
use orbit_types::task::is_valid_orb_task_id;
use orbit_types::workflow::{
    DrainCancelRequest, RunCancellationProvenance, RunRootCause, TaskCancellationPolicy,
};
use serde_json::{Value, json};

use super::support::*;

/// Every scenario runs isolated with a fresh fixture; `name` is the exact
/// libtest path the child re-executes by. Scenario bodies are plain
/// functions so their statements stay at low indentation.
fn scenario(name: &'static str, body: fn(&Fixture)) {
    isolated(name, || body(&Fixture::new()));
}

/// Declare one isolated scenario: `name` is both the test and, prefixed
/// with `runs::`, the exact libtest path the child re-executes by.
macro_rules! scenario_test {
    ($name:ident, $body:ident) => {
        #[test]
        fn $name() {
            scenario(concat!("runs::", stringify!($name)), $body);
        }
    };
}

scenario_test!(
    run_event_tail_pages_include_the_end_and_preserve_legacy_reads,
    run_event_tail_pages_body
);

fn run_event_tail_pages_body(fixture: &Fixture) {
    let run = fixture.seed_run("jrun-events", "fixture", JobRunState::Cancelled);
    let workspace = ws_id(fixture);
    let start = "2026-10-10T06:58:00Z".parse::<DateTime<Utc>>().unwrap();
    for index in 0..252 {
        let kind = match index {
            248 => "run_cancelled",
            249 => "run_finished",
            _ => "activity_started",
        };
        // Other-workspace and other-source rows must not affect the count.
        let ts = start + chrono::Duration::seconds(index);
        let (ws, source) = match index {
            250 => ("other-workspace", "v2_envelope"),
            251 => (workspace.as_str(), "other-source"),
            _ => (workspace.as_str(), "v2_envelope"),
        };
        let payload =
            json!({"event_id": format!("event-{index}"), "body_kind": kind, "ts": ts.to_rfc3339()});
        insert_audit(
            fixture,
            ws,
            source,
            &format!("event-{index}"),
            &run.run_id,
            ts,
            kind,
            "fixture",
            None,
            payload,
        );
    }
    let server = fixture.server(false);
    let path = format!("/api/runs/{}/events", run.run_id);
    let latest = get(&server, &format!("{path}?tail=true&limit=100"));
    assert_eq!(latest["total"], 250);
    assert_eq!(latest["offset"], 0);
    assert_eq!(latest["events"].as_array().unwrap().len(), 100);
    assert_eq!(latest["events"][0]["event_id"], "event-150");
    assert_eq!(latest["events"][98]["body_kind"], "run_cancelled");
    assert_eq!(latest["events"][99]["body_kind"], "run_finished");
    let earlier = get(&server, &format!("{path}?tail=true&limit=100&offset=100"));
    assert_eq!(earlier["events"][0]["event_id"], "event-50");
    assert_eq!(earlier["events"][99]["event_id"], "event-149");
    let first = get(&server, &format!("{path}?tail=true&limit=100&offset=200"));
    assert_eq!(first["events"].as_array().unwrap().len(), 50);
    assert_eq!(first["events"][0]["event_id"], "event-0");
    let beyond = get(&server, &format!("{path}?tail=true&limit=100&offset=999"));
    assert_eq!(beyond["events"], json!([]));
    assert_eq!(beyond["total"], 250);
    let legacy = get(&server, &format!("{path}?limit=100"));
    assert_eq!(legacy.as_array().unwrap().len(), 100);
    assert_eq!(legacy[0]["event_id"], "event-0");
    let filtered = get(&server, &format!("{path}?kind=run_finished"));
    assert_eq!(filtered[0]["event_id"], "event-249");
    assert_eq!(
        server
            .get(&format!("{path}?tail=true&kind=run_finished"))
            .status(),
        reqwest::StatusCode::BAD_REQUEST
    );
}

// Forward pages return each stored envelope as written: invalid JSON is
// skipped, a payload without an `event_id` is kept and no inherited
// `step_id` is added. A kind page the scan budget cannot fill is refused
// with 413; one it fills is served.
scenario_test!(
    run_event_pages_keep_stored_envelopes_and_refuse_an_unfillable_kind_scan,
    run_event_pages_body
);

fn run_event_pages_body(fixture: &Fixture) {
    let run = fixture.seed_run("jrun-event-budget", "fixture", JobRunState::Failed);
    let workspace = ws_id(fixture);
    let start = "2026-10-10T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let mut db = rusqlite::Connection::open(fixture.global.join("orbit.db")).unwrap();
    let tx = db.transaction().unwrap();
    {
        let mut insert = tx.prepare("INSERT INTO v2_audit_events (workspace_id, event_id, source, schema_version, event_type, ts, run_id, agent_identity, payload_json) VALUES (?1, ?2, 'v2_envelope', 1, 'fixture', ?3, ?4, 'test', ?5)").unwrap();
        // One row past the budget, then a match only a larger budget reaches.
        let rows = orbit_core::runtime::audit::run_events::RUN_EVENTS_MAX_SCAN_ROWS + 2;
        for index in 0..rows {
            let payload = match index {
                0 => json!({"event_id": "e0", "body_kind": "step_started", "step_id": "implement"}).to_string(),
                1 => "not json".to_string(),
                2 => json!({"event_id": "e2", "parent_event_id": "e0", "body_kind": "cli_invocation_finished"}).to_string(),
                3 => json!({"body_kind": "activity_progress"}).to_string(),
                _ if index == rows - 1 => json!({"event_id": "late", "body_kind": "late_marker"}).to_string(),
                _ => json!({"event_id": format!("f{index}"), "body_kind": "activity_progress"}).to_string(),
            };
            let ts = (start + chrono::Duration::seconds(index as i64)).to_rfc3339();
            let params =
                rusqlite::params![workspace, format!("row-{index}"), ts, run.run_id, payload];
            insert.execute(params).unwrap();
        }
    }
    tx.commit().unwrap();
    let server = fixture.server(false);
    let path = format!("/api/runs/{}/events", run.run_id);
    // An unfiltered page is a window of four stored rows, one of them invalid.
    let page = get(&server, &format!("{path}?limit=4"));
    assert_eq!(
        page,
        json!([
            {"event_id": "e0", "body_kind": "step_started", "step_id": "implement"},
            {"event_id": "e2", "parent_event_id": "e0", "body_kind": "cli_invocation_finished"},
            {"body_kind": "activity_progress"},
        ]),
        "stored envelopes only: invalid JSON skipped, nothing added"
    );
    let filled = get(&server, &format!("{path}?kind=step_started&limit=1"));
    let first_id = &filled[0]["event_id"];
    assert_eq!(first_id, &json!("e0"), "a page the budget fills is served");
    let refused = server.get(&format!("{path}?kind=late_marker"));
    assert_eq!(refused.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        refused.json::<Value>().unwrap(),
        json!({"error": "run-events audit rows exceed bounded scan budget; narrow the kind filter or reduce offset"})
    );
}

scenario_test!(
    child_dispatches_show_live_run_outcomes_and_timing_over_http,
    child_dispatches_body
);

fn child_dispatches_body(fixture: &Fixture) {
    let parent = fixture.seed_run(
        "jrun-child-outcomes",
        "workspace_auto_pipeline",
        JobRunState::Success,
    );
    let mut pipeline = pipeline_for(&parent);
    let states = [
        JobRunState::Running,
        JobRunState::Success,
        JobRunState::Failed,
        JobRunState::Cancelled,
        JobRunState::Pending,
        JobRunState::Retrying,
        JobRunState::Timeout,
        JobRunState::Interrupted,
        JobRunState::Held,
        JobRunState::Skipped,
    ];
    let children: Vec<JobRun> = states
        .into_iter()
        .map(|state| {
            let id = format!("jrun-child-{state}");
            let mut child = fixture.seed_run(&id, "task_auto_pipeline", state);
            child.started_at = (state != JobRunState::Pending)
                .then_some(Utc::now() - chrono::Duration::seconds(120));
            child.duration_ms = state.is_terminal().then_some(120_000);
            fixture.save_run(&child);
            let mut submitted = dispatch(&child);
            submitted.child_status = Some("running".into());
            pipeline.record_child_dispatch(submitted);
            child
        })
        .collect();
    let unavailable = dispatch_ids("jrun-child-unavailable", "task_auto_pipeline");
    pipeline.record_child_dispatch(unavailable);
    write_state(fixture, &parent, &pipeline);
    let server = fixture.server(false);
    let detail = run_detail(&server, &parent.run_id);
    let dispatches = detail["run"]["child_dispatches"].as_array().unwrap();
    assert_eq!(dispatches.len(), children.len() + 1);
    for (dispatch, child) in dispatches.iter().zip(&children) {
        let stored = fixture.runtime.show_job_run(&child.run_id).unwrap();
        assert_eq!(dispatch["child_run_id"], stored.run_id);
        assert_eq!(dispatch["state"], json!(stored.state));
        assert_eq!(dispatch["started_at"], json!(stored.started_at));
        assert_eq!(dispatch["finished_at"], json!(stored.finished_at));
        assert_eq!(dispatch["duration_ms"], json!(stored.duration_ms));
        assert_eq!(dispatch["phase"], "submitted");
        assert_eq!(dispatch["child_status"], "running");
    }
    let unavailable = dispatches.last().unwrap();
    for field in ["state", "started_at", "finished_at", "duration_ms", "tasks"] {
        assert!(unavailable.get(field).unwrap().is_null(), "{field}");
    }
    // The parent's durable checkpoint never changed, even after the
    // child finishes. A fresh HTTP read must still see its new state.
    let mut finished = children[0].clone();
    finished.state = JobRunState::Success;
    finished.finished_at = Some(Utc::now());
    finished.duration_ms = Some(123_000);
    fixture.save_run(&finished);
    let refreshed = run_detail(&server, &parent.run_id);
    let dispatch = &refreshed["run"]["child_dispatches"][0];
    assert_eq!(dispatch["state"], "success");
    assert_eq!(dispatch["duration_ms"], 123_000);
    assert_eq!(dispatch["finished_at"], json!(finished.finished_at));
    assert_eq!(dispatch["phase"], "submitted");
    assert_eq!(dispatch["child_status"], "running");
}

scenario_test!(
    run_tasks_are_projected_over_http_without_per_row_task_reads,
    run_tasks_body
);

fn run_tasks_body(fixture: &Fixture) {
    let tasks = ["First delivered task", "Second delivered task"].map(|title| {
        fixture
            .runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: "run projection fixture".into(),
                ..Default::default()
            })
            .unwrap()
    });
    let expected = json!([
        {"id": tasks[0].id, "title": tasks[0].title},
        {"id": tasks[1].id, "title": tasks[1].title},
    ]);
    let pr = "task_pr_pipeline";
    let auto = "workspace_auto_pipeline";
    for index in 0..30 {
        let job = [pr, "task_gate_pipeline", "task_auto_pipeline"][index % 3];
        let id = format!("jrun-task-{index}");
        let mut run = fixture.seed_run(&id, job, JobRunState::Success);
        run.input = Some(json!({"task_ids": [tasks[1].id, tasks[0].id, tasks[0].id, "", 5]}));
        fixture.save_run(&run);
    }
    let mut singular = fixture.seed_run("jrun-singular", pr, JobRunState::Failed);
    singular.input = Some(json!({"task_id": tasks[0].id}));
    fixture.save_run(&singular);
    let mut missing = fixture.seed_run("jrun-missing-title", pr, JobRunState::Success);
    missing.input = Some(json!({"task_ids": ["HF-999999", "not a task id"]}));
    fixture.save_run(&missing);
    let plain = fixture.seed_run("jrun-plain", "maintenance", JobRunState::Success);
    let mut coordinator = fixture.seed_run("jrun-coordinator", auto, JobRunState::Success);
    coordinator.input = Some(json!({"task_ids": []}));
    fixture.save_run(&coordinator);
    let submitted = child_dispatch(
        "jrun-task-0",
        "task_pr_pipeline",
        "dispatch",
        false,
        true,
        Utc::now(),
    );
    state_with(fixture, &coordinator, |state| {
        state.record_child_dispatch(submitted)
    });
    let server = fixture.counted_task_server();
    for limit in [1, 25, 100] {
        let before = server.task_query_trace().len();
        let page = get(
            &server,
            &format!("/api/job-runs?job_id=task_pr_pipeline&limit={limit}"),
        );
        let trace = server.task_query_trace();
        let reads = &trace[before..];
        let referenced: BTreeSet<&str> = list_rows(&page)
            .iter()
            .flat_map(|row| row["task_ids"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .filter(|id| is_valid_orb_task_id(id))
            .collect();
        let fresh = operations(reads, "task_index_freshness");
        let meta = operations(reads, "task_metadata_read");
        let bundles = operations(reads, "task_bundle_materialization");
        assert_eq!(
            fresh, 0,
            "labels never validate the workspace task index: {reads:?}"
        );
        assert_eq!(
            meta,
            referenced.len(),
            "one keyed read per referenced task: {reads:?}"
        );
        assert_eq!(bundles, 0, "run lists never hydrate task bodies");
        assert_eq!(list_rows(&page).len(), limit.min(12));
    }
    let page = get(&server, "/api/job-runs?limit=100");
    let aggregate = get(&server, "/api/job-runs/all?limit=100");
    for rows in [&page["items"], &aggregate["items"]] {
        for row in rows.as_array().unwrap() {
            let id = row["run_id"].as_str().unwrap();
            let expected_tasks = match id {
                "jrun-plain" | "jrun-coordinator" => Value::Null,
                "jrun-singular" => json!([{"id": tasks[0].id, "title": tasks[0].title}]),
                "jrun-missing-title" => {
                    json!([{"id": "HF-999999", "title": null}, {"id": "not a task id", "title": null}])
                }
                _ => expected.clone(),
            };
            assert_eq!(row["tasks"], expected_tasks, "list task labels for {id}");
            let ids = expected_tasks.as_array().map(|tasks| {
                tasks
                    .iter()
                    .map(|task| task["id"].clone())
                    .collect::<Vec<_>>()
            });
            assert_eq!(row["task_ids"], json!(ids));
            let detail = run_detail(&server, id);
            assert_eq!(
                detail["run"]["tasks"], row["tasks"],
                "detail labels for {id}"
            );
            assert_eq!(detail["run"]["task_ids"], row["task_ids"]);
            if id == coordinator.run_id {
                assert_eq!(detail["run"]["child_dispatches"][0]["tasks"], expected);
            }
        }
    }
    let before = server.task_query_trace().len();
    let page = get(&server, "/api/job-runs?job_id=maintenance");
    assert_eq!(page["items"][0]["run_id"], plain.run_id);
    let after = server.task_query_trace().len();
    assert_eq!(after, before, "task-free lists need no metadata lookup");
}

// History splits a task's runs per attempt from `parent_run_id` and names a
// cascaded failure's leaf from `root_cause`, so both run lists carry them.
scenario_test!(
    run_lists_link_listed_children_and_name_a_cascaded_failure_leaf,
    run_lists_body
);

fn run_lists_body(fixture: &Fixture) {
    let other = add_workspace(fixture, "ws_http_runs_other", "other");
    let created = Utc::now() - chrono::Duration::seconds(1);
    let seed = |id: &str, job: &str, state: JobRunState, offset_ms: i64| {
        let mut run = fixture.seed_run(id, job, state);
        run.created_at = created + chrono::Duration::milliseconds(offset_ms);
        run.scheduled_at = run.created_at;
        run.started_at = Some(run.created_at);
        run.finished_at = state.is_terminal().then_some(run.created_at);
        run.input = Some(json!({"task_id": "ORB-1"}));
        fixture.save_run(&run);
        run
    };
    let pr = "task_pr_pipeline";
    let gate = seed("jrun-gate", "task_gate_pipeline", JobRunState::Failed, 0);
    let leaf = seed("jrun-leaf", pr, JobRunState::Failed, 1);
    let mut retry = seed("jrun-retry", "task_gate_pipeline", JobRunState::Running, 10);
    retry.retry_source_run_id = Some(gate.run_id.clone());
    fixture.save_run(&retry);
    let retry_leaf = seed("jrun-retry-leaf", pr, JobRunState::Running, 11);
    let cause = RunRootCause {
        leaf_run_id: leaf.run_id.clone(),
        task_id: Some("ORB-1".into()),
        step: Some("implement".into()),
        code: Some("provider_unavailable".into()),
    };
    let mut gate_state = pipeline_for(&gate);
    let submitted = child_dispatch(
        &leaf.run_id,
        &leaf.job_id,
        "invoke_and_wait",
        true,
        false,
        leaf.created_at,
    );
    gate_state.record_child_dispatch(submitted);
    gate_state.root_cause = Some(cause.clone());
    write_state(fixture, &gate, &gate_state);
    // Resume copies the source checkpoint, including its dispatches.
    // The retry then admits its own child without owning the old one.
    let mut retry_state = gate_state.clone();
    retry_state.run_id = retry.run_id.clone();
    retry_state.root_cause = None;
    retry_state.record_child_dispatch(dispatch(&retry_leaf));
    write_state(fixture, &retry, &retry_state);
    let server = fixture.server(false);
    for path in ["/api/job-runs?limit=10", "/api/job-runs/all?limit=10"] {
        let page = get(&server, path);
        let rows = list_rows(&page);
        assert_eq!(rows.len(), 4, "{path}");
        assert_eq!(rows[0]["run_id"], retry_leaf.run_id, "newest first: {path}");
        assert_eq!(
            rows[1]["run_id"], retry.run_id,
            "retry precedes source: {path}"
        );
        let row = |id: &str| {
            rows.iter()
                .find(|row| row["run_id"] == id)
                .unwrap_or_else(|| panic!("{path} lists {id}"))
        };
        // (run, parent_run_id, state, root_cause) for every listed row.
        let cause_json = json!(cause);
        let expected_rows: [(&str, Value, &str, &Value); 4] = [
            ("jrun-gate", Value::Null, "failed", &cause_json),
            ("jrun-leaf", json!("jrun-gate"), "failed", &Value::Null),
            ("jrun-retry", Value::Null, "running", &Value::Null),
            (
                "jrun-retry-leaf",
                json!("jrun-retry"),
                "running",
                &Value::Null,
            ),
        ];
        for (id, parent, state, root) in &expected_rows {
            let row = row(id);
            assert_eq!(row["parent_run_id"], *parent, "{path}: {id}");
            assert_eq!(row["state"], *state, "{path}: {id}");
            assert_eq!(&row["root_cause"], *root, "{path}: {id}");
        }
    }
    // A page boundary must not let a retry claim an inherited child
    // merely because its source is outside the loaded page.
    for path in ["/api/job-runs?limit=3", "/api/job-runs/all?limit=3"] {
        let page = get(&server, path);
        let rows = list_rows(&page);
        assert_eq!(rows.len(), 3, "{path}");
        assert_eq!(rows[2]["run_id"], leaf.run_id, "{path}");
        assert_eq!(rows[2]["parent_run_id"], Value::Null, "{path}");
        assert_eq!(rows[0]["parent_run_id"], retry.run_id, "{path}");
    }
    // When multiple workspaces merge and a parent run falls outside
    // the response limit while its child remains, the child's
    // parent_run_id must be null instead of naming the cut parent.
    seed_other_run(&other, "jrun-other-1", 2, created);
    seed_other_run(&other, "jrun-other-2", 3, created);
    let page = get(&server, "/api/job-runs/all?limit=5");
    let rows = list_rows(&page);
    assert_eq!(rows.len(), 5, "top 5 merged across workspaces");
    assert!(
        !rows.iter().any(|row| row["run_id"] == gate.run_id),
        "older parent gate run was pushed past limit"
    );
    let leaf_row = rows
        .iter()
        .find(|row| row["run_id"] == leaf.run_id)
        .expect("child leaf run remains in top 5");
    assert_eq!(
        leaf_row["parent_run_id"],
        Value::Null,
        "parent_run_id must be null when parent is cut from cross-workspace response"
    );
    for row in rows {
        if let Some(parent) = row.get("parent_run_id").and_then(Value::as_str) {
            assert!(
                rows.iter().any(|r| r["run_id"] == parent),
                "every non-null parent_run_id must name a listed run, got {parent} for {}",
                row["run_id"]
            );
        }
    }
}

scenario_test!(
    job_run_list_filters_by_task_job_and_since,
    job_run_list_filters_body
);

fn job_run_list_filters_body(fixture: &Fixture) {
    let seed = |n: &str, j: &str, s: JobRunState, input: Option<Value>, age: i64| {
        let mut run = fixture.seed_run(n, j, s);
        run.input = input;
        let created = Utc::now() - chrono::Duration::hours(age);
        run.created_at = created;
        run.scheduled_at = created;
        if run.started_at.is_some() {
            run.started_at = Some(created);
        }
        if run.finished_at.is_some() {
            run.finished_at = Some(created);
        }
        fixture.save_run(&run);
    };
    // Seed rows: jrun-h's ids hide under a wrapper, jrun-f is 48h old,
    // jrun-g has no task input; the cases below discriminate on them.
    let auto = "task_auto_pipeline";
    let by_ids = Some(json!({"task_ids": ["ORB-10001"]}));
    let by_id = Some(json!({"task_id": "ORB-10001"}));
    let other = Some(json!({"task_ids": ["ORB-10002"]}));
    let wrapped = Some(json!({"wrapper": {"task_id": "ORB-10001", "task_ids": ["ORB-10001"]}}));
    seed("jrun-a", auto, JobRunState::Failed, by_ids.clone(), 0);
    seed(
        "jrun-b",
        "task_gate_pipeline",
        JobRunState::Timeout,
        by_id,
        0,
    );
    seed("jrun-c", auto, JobRunState::Interrupted, other, 0);
    seed(
        "jrun-d",
        "ci_failure_sweep_pipeline",
        JobRunState::Failed,
        by_ids.clone(),
        0,
    );
    seed("jrun-e", auto, JobRunState::Success, by_ids.clone(), 0);
    seed("jrun-f", auto, JobRunState::Failed, by_ids, 48);
    seed("jrun-g", "maintenance", JobRunState::Failed, None, 0);
    seed("jrun-h", auto, JobRunState::Failed, wrapped, 0);
    let server = fixture.server(false);
    let workspace = "workspace=ws_http_fixture";
    let query = |q: &str| format!("/api/job-runs?{q}&limit=100&{workspace}");
    // Cases name themselves in failure messages; `expected` is row-aligned.
    let cases: [(&str, &str); 5] = [
        (
            "task_id matches top-level task_ids or task_id only",
            "task_id=ORB-10001",
        ),
        (
            "task_id + failed + since=24h",
            "task_id=ORB-10001&state=failed&since=24h",
        ),
        ("job_id only", "job_id=ci_failure_sweep_pipeline"),
        (
            "task_id + job_id + failed + since=24h",
            "task_id=ORB-10001&job_id=task_auto_pipeline&state=failed&since=24h",
        ),
        ("failed window", "state=failed&since=24h"),
    ];
    let expected: [&[&str]; 5] = [
        &["jrun-a", "jrun-b", "jrun-d", "jrun-e", "jrun-f"],
        &["jrun-a", "jrun-b", "jrun-d"],
        &["jrun-d"],
        &["jrun-a"],
        &["jrun-a", "jrun-b", "jrun-c", "jrun-d", "jrun-g", "jrun-h"],
    ];
    let mut pages = Vec::new();
    for (index, (case, q)) in cases.iter().enumerate() {
        let page = get(&server, &query(q));
        assert_eq!(run_ids(&page), id_set(expected[index]), "{case}");
        pages.push(page);
    }
    let (task, recent_failures, failed_window) = (&pages[0], &pages[1], &pages[4]);
    assert_eq!(task["total"], 5);
    assert_eq!(recent_failures["state"], "failed");
    let unbounded = get(&server, &query("task_id=ORB-10001&since=all"));
    assert_eq!(
        run_ids(&unbounded),
        run_ids(task),
        "since=all is not a time bound"
    );
    let blank_task = get(&server, &query("task_id=%20&state=failed&since=24h"));
    assert_eq!(blank_task["total"], failed_window["total"]);
    assert_eq!(failed_window["total"], 6);
    let summary = get(
        &server,
        "/api/audit/summary?since=24h&workspace=ws_http_fixture",
    );
    assert_eq!(
        summary["failed_runs"], failed_window["total"],
        "the rail count and the Failed list share one window and outcome set"
    );
    let aggregate = get(
        &server,
        "/api/job-runs/all?task_id=ORB-10001&state=failed&since=24h&limit=100",
    );
    assert_eq!(run_ids(&aggregate), id_set(&["jrun-a", "jrun-b", "jrun-d"]));
    let refused = server.get(&format!("/api/job-runs?since=not-a-window&{workspace}"));
    assert_eq!(refused.status().as_u16(), 400);
    let body: Value = refused.json().expect("error json");
    let message = body["error"].as_str().expect("error text");
    assert!(message.contains("not-a-window"), "{message}");
    assert!(body.get("code").is_none(), "{body}");
    assert_eq!(
        server
            .get("/api/job-runs/all?since=not-a-window")
            .status()
            .as_u16(),
        400
    );
}

fn save_step(fixture: &Fixture, run: &JobRun, message: &str) {
    let now = at(5, 40);
    let mut step = run_step(
        JobTargetType::Activity,
        "landing_review",
        JobRunState::Cancelled,
    );
    step.step_index = 15;
    step.started_at = Some(now);
    step.finished_at = Some(now);
    step.duration_ms = Some(600_000);
    step.error_message = Some(message.into());
    save_record_step(fixture, run, &step);
}

// [ORB-15897] The run detail chooses its steps by the rule every run view
// shares and names their source. A record with activity steps answers over
// the audit trail; a run-level-only record defers to the trail, whose step
// the run died in reads as interrupted rather than running.
scenario_test!(
    run_detail_steps_follow_the_shared_rule_and_name_their_source,
    run_detail_steps_body
);

fn run_detail_steps_body(fixture: &Fixture) {
    let step = |target_type, target_id: &str, state, lock_wait_ms| {
        let mut step = run_step(target_type, target_id, state);
        step.lock_wait_ms = lock_wait_ms;
        step.started_at = Some(at(6, 0));
        step.finished_at = Some(at(6, 1));
        step.duration_ms = Some(60_000);
        step.error_code = (state == JobRunState::Interrupted).then(|| "worker_terminated".into());
        step
    };
    let pr = "task_pr_pipeline";
    let interrupted = fixture.seed_run("jrun-20261010-0600", pr, JobRunState::Interrupted);
    let job_step = step(JobTargetType::Job, pr, JobRunState::Interrupted, None);
    save_record_step(fixture, &interrupted, &job_step);
    insert_step_audit(fixture, &interrupted.run_id, "prepare", "step_started", 0);
    insert_step_audit(fixture, &interrupted.run_id, "prepare", "step_finished", 1);
    insert_step_audit(fixture, &interrupted.run_id, "implement", "step_started", 2);
    let recorded = fixture.seed_run("jrun-20261010-0610", pr, JobRunState::Success);
    let activity_step = step(
        JobTargetType::Activity,
        "prepare",
        JobRunState::Success,
        Some(7),
    );
    save_record_step(fixture, &recorded, &activity_step);
    insert_step_audit(fixture, &recorded.run_id, "prepare", "step_started", 0);
    insert_step_audit(fixture, &recorded.run_id, "prepare", "step_finished", 1);
    insert_step_audit(fixture, &recorded.run_id, "unrecorded", "step_started", 2);
    let server = fixture.server(false);
    let detail = run_detail(&server, &interrupted.run_id);
    assert_eq!(detail["steps_source"], "audit", "{detail}");
    assert_eq!(detail["run"]["error_code"], "worker_terminated");
    assert!(detail["run"].get("steps").is_none(), "{detail}");
    let steps = detail["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2, "{detail}");
    assert_eq!(steps[0]["state"], "success");
    assert_eq!(steps[0]["lock_wait_ms"], 42);
    assert_eq!(steps[1]["target_id"], "implement");
    assert_eq!(steps[1]["state"], "interrupted");
    assert_eq!(steps[1]["outcome"], "interrupted");
    let detail = run_detail(&server, &recorded.run_id);
    assert_eq!(detail["steps_source"], "record", "{detail}");
    let steps = detail["steps"].as_array().unwrap();
    assert_eq!(
        steps.len(),
        1,
        "the record answers, not the trail: {detail}"
    );
    assert_eq!(steps[0]["target_id"], "prepare");
    assert_eq!(steps[0]["lock_wait_ms"], 7);
}

fn policy(note: &str) -> TaskCancellationPolicy {
    TaskCancellationPolicy {
        block: false,
        note: note.into(),
        provenance: None,
    }
}

/// Seed a cancelled run finished at `at(5, minute)`, with an optional
/// cancellation policy recorded in its pipeline state.
fn cancelled_run(
    fixture: &Fixture,
    id: &str,
    job: &str,
    minute: u32,
    policy: Option<TaskCancellationPolicy>,
) -> JobRun {
    let mut run = fixture.seed_run(id, job, JobRunState::Cancelled);
    run.finished_at = Some(at(5, minute));
    fixture.save_run(&run);
    if let Some(policy) = policy {
        state_with(fixture, &run, |state| {
            state.task_cancellation_policy = Some(policy)
        });
    }
    run
}

// A nested failure names the deepest cancelled descendant, and a cancelled
// run projects who cancelled it. A shallower failure and a failure hidden
// under a successful child must not win.
scenario_test!(
    failure_chain_names_the_deepest_descendant_and_projects_cancellation,
    failure_chain_body
);

fn failure_chain_body(fixture: &Fixture) {
    let seed = |i: &str, j: &str, s: JobRunState| fixture.seed_run(i, j, s);
    let cancelled = |i: &str, j: &str, m: u32, p: Option<TaskCancellationPolicy>| {
        cancelled_run(fixture, i, j, m, p)
    };
    let auto = "task_auto_pipeline";
    let gate_job = "task_gate_pipeline";
    let parent = seed("jrun-20261010-0552-c1", auto, JobRunState::Failed);
    let success = seed("jrun-20261010-0552-c5", gate_job, JobRunState::Success);
    let hidden = seed("jrun-20261010-0552-c6", gate_job, JobRunState::Failed);
    let shallow = seed("jrun-20261010-0552-c4", gate_job, JobRunState::Failed);
    let held = seed("jrun-20261010-0552-c2", gate_job, JobRunState::Held);
    let leaf_policy = Some(policy("run cancelled by dashboard: gate exceeded: retry"));
    let leaf = cancelled("jrun-20261010-0552-c3", gate_job, 42, leaf_policy);
    save_step(fixture, &leaf, "landing review was cancelled");
    link_child(fixture, &success, &hidden);
    link_child(fixture, &held, &leaf);
    // The successful child's failed descendant is dispatched first, so a
    // walk that follows success would pick it over the real leaf.
    state_with(fixture, &parent, |state| {
        state.record_child_dispatch(dispatch(&success));
        state.record_child_dispatch(dispatch(&shallow));
        state.record_child_dispatch(dispatch(&held));
    });
    let lone = seed("jrun-20261010-0552-c7", auto, JobRunState::Failed);
    let empty_policy = Some(policy("unrecognized legacy note"));
    let empty = cancelled("jrun-20261010-0552-c8", auto, 43, empty_policy);
    let drain_run = cancelled("jrun-20261010-0552-c9", "workspace_pull_pipeline", 55, None);
    let requested_at = at(5, 52);
    state_with(fixture, &drain_run, |state| {
        state.pull_drain.drain_cancel = Some(DrainCancelRequest {
            actor: "dashboard".into(),
            source: "web".into(),
            reason: Some("operator stopped the drain".into()),
            requested_at,
        });
        state.task_cancellation_policy =
            Some(policy("run cancelled by not-the-drain: note reason"));
    });
    let audited = cancelled("jrun-20261010-0552-c10", auto, 56, None);
    let audit_at = at(5, 50);
    insert_cancel_audit(fixture, &audited.run_id, audit_at);
    let actor_policy = Some(policy("run cancelled by parent-cascade"));
    let actor_only = cancelled("jrun-20261010-0552-c11", auto, 44, actor_policy);
    let mut typed_policy = policy("run cancelled by wrong: wrong");
    let provenance = RunCancellationProvenance::new("typed", "api", Some("typed: reason"));
    typed_policy.provenance = Some(provenance);
    let typed = cancelled("jrun-20261010-0552-c12", auto, 45, Some(typed_policy));
    let server = fixture.server(false);
    // (case, run, expected failure_root, expected cancellation)
    let leaf_root = json!({"run_id": leaf.run_id, "state": "cancelled", "step": "landing_review", "message": "landing review was cancelled"});
    let leaf_cancel = json!({"actor": "dashboard", "source": Value::Null, "reason": "gate exceeded: retry", "at": at(5, 42)});
    let empty_cancel = json!({"actor": Value::Null, "source": Value::Null, "reason": "no reason recorded", "at": at(5, 43)});
    let drain_cancel = json!({"actor": "dashboard", "source": "web", "reason": "operator stopped the drain", "at": requested_at});
    let audit_cancel = json!({"actor": "on-call", "source": "cli", "reason": "stopped after the gate hung", "at": audit_at});
    let typed_cancel = json!({"actor": "typed", "source": Value::Null, "reason": "typed: reason", "at": typed.finished_at});
    let actor_cancel = json!({"actor": "parent-cascade", "source": Value::Null, "reason": "no reason recorded", "at": at(5, 44)});
    let cases: [(&str, &JobRun, Value, Value); 9] = [
        ("parent names deepest leaf", &parent, leaf_root, Value::Null),
        (
            "leaf projects its canceller",
            &leaf,
            Value::Null,
            leaf_cancel,
        ),
        (
            "successful child is clean",
            &success,
            Value::Null,
            Value::Null,
        ),
        ("lone failure is clean", &lone, Value::Null, Value::Null),
        (
            "unrecognized note falls back",
            &empty,
            Value::Null,
            empty_cancel,
        ),
        (
            "drain cancel wins over policy note",
            &drain_run,
            Value::Null,
            drain_cancel,
        ),
        (
            "audit event names the actor",
            &audited,
            Value::Null,
            audit_cancel,
        ),
        (
            "typed provenance wins over note",
            &typed,
            Value::Null,
            typed_cancel,
        ),
        (
            "actor-only note names the actor",
            &actor_only,
            Value::Null,
            actor_cancel,
        ),
    ];
    for (case, run, failure_root, cancellation) in &cases {
        let detail = run_detail(&server, &run.run_id);
        assert_eq!(detail["run"]["failure_root"], *failure_root, "{case}");
        assert_eq!(detail["run"]["cancellation"], *cancellation, "{case}");
    }
}

// [ORB-16330] Run detail projects provider children through the shared
// `RunProviderProcess::to_json` (activity and stuck-descendant fields, no
// `run_id`) and names the same worker identity MCP already puts on the run.
scenario_test!(
    run_detail_projects_shared_provider_processes_and_worker_identity,
    provider_processes_body
);

fn provider_processes_body(fixture: &Fixture) {
    let mut run = fixture.seed_run(
        "jrun-provider-identity",
        "task_auto_pipeline",
        JobRunState::Running,
    );
    run.pid = Some(4242);
    run.pid_start_time = Some("v1:4242".into());
    run.input = Some(json!({"task_id": "ORB-1"}));
    fixture.save_run(&run);
    let workspace = ws_id(fixture);
    // Events land at 08:00:{second:02}Z on the fixture clock.
    let seed = |event_id: &str, second: u32, parent: Option<&str>, body: Value| {
        let ts = format!("2026-10-10T08:00:{second:02}Z");
        let mut payload = json!({"schemaVersion": 1, "event_type": "test.event", "event_id": event_id, "ts": ts.as_str(), "run_id": run.run_id, "agent_identity": "codex", "parent_event_id": parent});
        let object = payload.as_object_mut().unwrap();
        for (key, value) in body.as_object().unwrap().clone() {
            object.insert(key, value);
        }
        insert_test_event(
            fixture,
            &workspace,
            &run.run_id,
            event_id,
            &ts,
            parent,
            payload,
        );
    };
    seed(
        "evt-step",
        0,
        None,
        json!({"body_kind": "step_started", "step_id": "invoke"}),
    );
    seed(
        "evt-process",
        1,
        Some("evt-step"),
        json!({"body_kind": "cli_invocation_process", "provider": "codex", "pid": 4242, "pid_start_time": "v1:4242", "run_id": "decoy-process-run"}),
    );
    seed(
        "evt-activity",
        11,
        Some("evt-step"),
        json!({"body_kind": "cli_invocation_activity", "provider": "codex", "observed_bytes": 512, "latest_message": "Reading the sweep clock config.", "latest_message_truncated": true}),
    );
    seed(
        "evt-stopped",
        12,
        Some("evt-step"),
        json!({"body_kind": "cli_invocation_stopped_descendant", "pid": 4243, "command": "sleep-forever", "ended": false, "error": "permission denied", "stopped_ms": 600000}),
    );
    let server = fixture.server(false);
    let detail = run_detail(&server, &run.run_id);
    assert_eq!(detail["run"]["pid"], 4242, "{detail}");
    assert_eq!(detail["run"]["pid_start_time"], "v1:4242", "{detail}");
    assert_eq!(detail["run"]["input"]["task_id"], "ORB-1", "{detail}");
    let processes = detail["provider_processes"].as_array().unwrap();
    assert_eq!(processes.len(), 1, "{detail}");
    let process = &processes[0];
    assert!(
        process.get("run_id").is_none(),
        "process objects omit run_id: {process}"
    );
    let keys = "last_activity_at latest_message latest_message_truncated stdout_blob_ref build_budget_waits stopped_descendants blocked_on_stopped_descendant";
    for key in keys.split(' ') {
        assert!(process.get(key).is_some(), "missing {key}: {process}");
    }
    assert_eq!(process["provider"], "codex");
    assert_eq!(process["pid"], 4242);
    assert_eq!(process["step_id"], "invoke");
    assert_eq!(process["latest_message"], "Reading the sweep clock config.");
    assert_eq!(process["latest_message_truncated"], true);
    assert_eq!(process["last_activity_at"], "2026-10-10T08:00:11+00:00");
    assert!(process["stdout_blob_ref"].is_null());
    assert!(process["build_budget_waits"].is_null());
    // A pid that is not a live child cannot be blocked; the key is
    // still present, and the seeded descendant is listed beside it.
    assert!(
        process["blocked_on_stopped_descendant"].is_null(),
        "{process}"
    );
    let descendant = &process["stopped_descendants"][0];
    assert_eq!(descendant["pid"], 4243);
    assert_eq!(descendant["command"], "sleep-forever");
    assert_eq!(descendant["ended"], false);
}

// The run-log API serves the shared projection's HTTP preview profile: every
// record field, streams cut to 120 lines and redacted even when the stored
// capture predates write-time redaction, with truncation flags per stream.
scenario_test!(
    run_logs_serve_the_http_preview_profile_of_each_invocation,
    run_logs_body
);

fn run_logs_body(fixture: &Fixture) {
    let run = fixture.seed_run("jrun-logs", "fixture", JobRunState::Failed);
    let workspace = ws_id(fixture);
    let secret = format!("ghp_{}", "L".repeat(36));
    // A legacy capture written before blobs were redacted on write.
    let blob = |name: char, text: String| {
        let blob_ref = name.to_string().repeat(64);
        let dir = fixture
            .runtime
            .data_root()
            .join("state/audit/blobs")
            .join(&blob_ref[..2]);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(&blob_ref), text).unwrap();
        blob_ref
    };
    let stdout_ref = blob(
        'a',
        (0..130)
            .map(|line| format!("line {line} {secret}\n"))
            .collect(),
    );
    let stderr_ref = blob('b', format!("warning {secret}\n"));
    let step_body = json!({"body_kind": "step_started", "step_id": "invoke"});
    let finished_body = json!({"body_kind": "cli_invocation_finished", "provider": "codex", "exit_code": 1, "timed_out": false, "duration_ms": 5000, "stdout_blob_ref": stdout_ref, "stderr_blob_ref": stderr_ref});
    for (event_id, ts, parent, body) in [
        ("evt-step", "2026-10-10T08:00:00Z", None, step_body),
        (
            "evt-finished",
            "2026-10-10T08:00:05Z",
            Some("evt-step"),
            finished_body,
        ),
    ] {
        let mut payload = body;
        payload["event_id"] = json!(event_id);
        payload["ts"] = json!(ts);
        payload["run_id"] = json!(run.run_id);
        payload["parent_event_id"] = json!(parent);
        insert_test_event(
            fixture,
            &workspace,
            &run.run_id,
            event_id,
            ts,
            parent,
            payload,
        );
    }
    let server = fixture.server(false);
    let logs = get(&server, &format!("/api/runs/{}/logs", run.run_id));
    let records = logs.as_array().expect("bare record array");
    assert_eq!(records.len(), 1, "{logs}");
    let record = records[0].as_object().unwrap();
    let keys: Vec<&str> = record.keys().map(String::as_str).collect();
    assert_eq!(
        keys.join(","),
        "run_id,event_id,ts,step_id,step_index,provider,stdout_blob_ref,stderr_blob_ref,stdout_preview,stderr_preview,stdout_truncated,stderr_truncated,exit_code,timed_out,duration_ms",
        "{logs}"
    );
    assert_eq!(record["run_id"], "jrun-logs");
    assert_eq!(record["event_id"], "evt-finished");
    assert_eq!(record["ts"], "2026-10-10T08:00:05+00:00");
    assert_eq!(record["step_id"], "invoke");
    assert_eq!(record["step_index"], 0);
    assert_eq!(record["stdout_blob_ref"], stdout_ref);
    let outcome = (
        &record["exit_code"],
        &record["timed_out"],
        &record["duration_ms"],
    );
    assert_eq!(outcome, (&json!(1), &json!(false), &json!(5000)));
    let stdout = record["stdout_preview"].as_str().unwrap();
    let stderr = record["stderr_preview"].as_str().unwrap();
    assert!(
        !stdout.contains(&secret) && !stderr.contains(&secret),
        "previews never expose the raw capture: {logs}"
    );
    assert_eq!(stdout.lines().count(), 120, "{stdout}");
    assert!(stdout.starts_with("line 0 ") && stdout.contains("\nline 119 "));
    assert_eq!(record["stdout_truncated"], true);
    assert!(stderr.starts_with("warning ") && stderr.ends_with('\n'));
    assert_eq!(record["stderr_truncated"], false);
}

// A drain that holds requests must expose its live breaker and refused
// settlements over HTTP, including after the run ends. Healthy and ordinary
// runs retain the same response shape with no hold records.
scenario_test!(
    run_detail_exposes_pull_request_holds,
    pull_request_holds_body
);

fn pull_request_holds_body(fixture: &Fixture) {
    use orbit_store::compose::workspace_job_run_store;
    use orbit_store::contracts::{LocalPullPhase, SettlementRefusal};
    use orbit_types::workflow::PullBreaker;

    let pull = "workspace_pull_pipeline";
    let drain = fixture.seed_run("jrun-request-holds", pull, JobRunState::Running);
    let healthy = fixture.seed_run("jrun-healthy-pull", pull, JobRunState::Running);
    let ordinary = fixture.seed_run("jrun-ordinary", "fixture", JobRunState::Success);
    let breaker = PullBreaker {
        failures: 3,
        failure_classes: vec!["candidate".into(), "task_input".into()],
        opened_at: at(8, 0),
        half_open_at: at(8, 15),
    };
    let pull_breaker = Some(breaker.clone());
    state_with(fixture, &drain, |state| {
        state.pull_drain.pull_breaker = pull_breaker
    });
    let workspace = ws_id(fixture);
    let store = store(fixture);
    let jobs = workspace_job_run_store(store.clone(), workspace.clone());
    let destination = pull_destination();
    let request = pull_request(&drain, "request-hold-fixture");
    let allocated = jobs
        .allocate_pull_request(&destination, &request, 4)
        .unwrap();
    let mut admission = allocated.unwrap();
    // Seed a persisted refused delivery without contacting another host.
    admission.phase = LocalPullPhase::Settling;
    admission.leaf_run_id = Some("jrun-refused-leaf".into());
    admission.settlement_refusal = Some(SettlementRefusal {
        reason: "owner validation commands missing".into(),
        code: None,
        refusals: 2,
        first_refused_at: at(8, 1),
        last_refused_at: at(8, 2),
        retry_after: at(8, 3),
    });
    persist_admission(&store, &workspace, &request.request_id, &admission);
    let runtime = &fixture.runtime;
    let settlements = runtime
        .pull_drain_refused_settlements(&drain.run_id)
        .unwrap();
    let expected = serde_json::to_value(settlements).unwrap();
    assert_eq!(expected.as_array().unwrap().len(), 1);
    assert_eq!(expected[0]["reason"], "owner validation commands missing");
    assert!(
        expected[0]["remedy"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    );
    let server = fixture.server(false);
    for run in [&healthy, &ordinary] {
        let detail = run_detail(&server, &run.run_id);
        assert_eq!(detail.get("pull_breaker"), Some(&Value::Null), "{detail}");
        assert_eq!(
            detail.get("refused_settlements"),
            Some(&json!([])),
            "{detail}"
        );
    }
    for terminal in [false, true] {
        if terminal {
            let mut ended = drain.clone();
            ended.state = JobRunState::Cancelled;
            ended.finished_at = Some(Utc::now());
            fixture.save_run(&ended);
        }
        let detail = run_detail(&server, &drain.run_id);
        assert_eq!(detail["pull_breaker"], json!(breaker), "{detail}");
        assert_eq!(detail["refused_settlements"], expected, "{detail}");
    }
}

// The HTTP budget must use the allocator's workspace occupancy, including
// claims with no running leaf, without adding a bound claim to its leaf twice.
scenario_test!(
    pull_budget_tracks_allocator_through_launch_review_and_settlement,
    pull_budget_body
);

fn pull_budget_body(fixture: &Fixture) {
    use orbit_store::compose::workspace_job_run_store;
    use orbit_store::contracts::{LocalPullMutation, LocalPullPhase, SettlementRefusal};
    use orbit_types::workflow::ReviewGateAdmissionOutput;

    let seed = |i: &str, j: &str, s: JobRunState| fixture.seed_run(i, j, s);
    let drain = seed(
        "jrun-budget",
        "workspace_pull_pipeline",
        JobRunState::Running,
    );
    state_with(fixture, &drain, |_| {});
    let workspace = ws_id(fixture);
    let store = store(fixture);
    let jobs = workspace_job_run_store(store.clone(), workspace.clone());
    let destination = pull_destination();
    let request = pull_request(&drain, "budget-claim");
    let server = fixture.server(false);
    let probes = std::cell::Cell::new(0);
    let check = |occupied: usize, reviewing: usize, running: usize| {
        let detail = run_detail(&server, &drain.run_id);
        assert_eq!(
            detail["pull_occupancy"],
            json!({"occupied": occupied, "reviewing": reviewing})
        );
        assert_eq!(detail["claimed_leaves"].as_array().unwrap().len(), running);
        let authoritative = jobs.drain_leaf_occupancy().unwrap();
        let actual = (authoritative.occupied, authoritative.reviewing);
        assert_eq!(actual, (occupied, reviewing));
        // Exercise actual allocation too: with N=1, exactly one review
        // may earn a replacement. Release every successful probe.
        let mut probe = request.clone();
        probes.set(probes.get() + 1);
        probe.request_id = format!("budget-probe-{}", probes.get());
        let admitted = jobs.allocate_pull_request(&destination, &probe, 1).unwrap();
        assert_eq!(
            admitted.is_some(),
            occupied.saturating_sub(reviewing.min(1)) < 1
        );
        if admitted.is_some() {
            let refusal = LocalPullMutation::Refuse("fixture probe complete".into());
            jobs.mutate_local_pull(&destination, &probe.request_id, &refusal)
                .unwrap();
        }
    };
    let allocated = jobs
        .allocate_pull_request(&destination, &request, 1)
        .unwrap();
    let mut admission = allocated.unwrap();
    let persist = |admission: &orbit_store::contracts::LocalPullAdmission| {
        persist_admission(&store, &workspace, &request.request_id, admission);
    };
    admission.phase = LocalPullPhase::Launching;
    persist(&admission);
    check(1, 0, 0);
    let mut leaf = seed(
        "jrun-budget-leaf",
        "task_claimed_pr_pipeline",
        JobRunState::Running,
    );
    admission.leaf_run_id = Some(leaf.run_id.clone());
    admission.phase = LocalPullPhase::Launched;
    persist(&admission);
    check(1, 0, 1);
    let admitted = ReviewGateAdmissionOutput::PreflightPassed {
        lineage_key: "fixture".into(),
    };
    let output = serde_json::to_value(admitted).unwrap();
    state_with(fixture, &leaf, |state| {
        state.record_pipeline_output("landing_review_gate_admit", output)
    });
    check(1, 1, 1);
    leaf.state = JobRunState::Success;
    leaf.finished_at = Some(Utc::now());
    fixture.save_run(&leaf);
    admission.phase = LocalPullPhase::Settling;
    admission.settlement_refusal = Some(SettlementRefusal {
        reason: "owner evidence missing".into(),
        code: None,
        refusals: 1,
        first_refused_at: at(8, 1),
        last_refused_at: at(8, 1),
        retry_after: at(8, 2),
    });
    persist(&admission);
    check(1, 0, 0);
    admission.phase = LocalPullPhase::Settled;
    persist(&admission);
    check(0, 0, 0);
    // A leaf outside this drain still competes for the same capacity,
    // while remaining absent from this drain's running claims table.
    seed("jrun-other-leaf", "task_pr_pipeline", JobRunState::Running);
    check(1, 0, 0);
    let ordinary = seed("jrun-non-pull", "fixture", JobRunState::Success);
    let detail = run_detail(&server, &ordinary.run_id);
    assert_eq!(detail["pull_occupancy"], Value::Null);
}

// [ORB-24160] A pull follower's claimed leaf stays in the host store under
// its workspace id after that workspace is removed from the host. Run detail
// and run events, asked with the dashboard's selected workspace or with the
// removed id, find it there with its steps and events instead of answering
// "run not found".
scenario_test!(
    a_run_recorded_under_an_unregistered_workspace_opens_with_steps_and_events,
    unregistered_workspace_body
);

fn unregistered_workspace_body(fixture: &Fixture) {
    const REMOVED: &str = "ws_removed_follower";
    let store = store(fixture);
    let now = Utc::now();
    let mut run = make_job_run(
        "jrun-20261018-1135-c1",
        "task_claimed_pr_pipeline",
        JobRunState::Success,
        now,
        None,
    );
    run.duration_ms = Some(1_000);
    store
        .upsert_job_run_for_workspace(REMOVED, &run, None)
        .unwrap();
    let mut step = run_step(JobTargetType::Activity, "implement", JobRunState::Success);
    step.started_at = Some(now);
    step.finished_at = Some(now);
    step.duration_ms = Some(1_000);
    store
        .upsert_job_run_step_for_workspace(REMOVED, &run.run_id, &step)
        .unwrap();
    for index in 0..3 {
        let event_id = format!("leaf-{index}");
        let ts = now + chrono::Duration::seconds(index);
        let payload = json!({"event_id": event_id, "body_kind": "activity_started"});
        insert_audit_default(
            fixture,
            REMOVED,
            &format!("leaf-{index}"),
            &run.run_id,
            ts,
            "activity_started",
            payload,
        );
    }
    let server = fixture.server(false);
    let runs = format!("/api/runs/{}", run.run_id);
    let recorded = json!({"workspace_id": REMOVED, "registered": false});
    for workspace in ["ws_http_fixture", REMOVED] {
        let detail = get(&server, &format!("{runs}?workspace={workspace}"));
        assert_eq!(detail["run"]["run_id"], run.run_id, "{workspace}: {detail}");
        assert_eq!(detail["run"]["state"], "success", "{workspace}: {detail}");
        assert_eq!(
            detail["recorded_workspace"], recorded,
            "{workspace}: {detail}"
        );
        let steps = detail["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 1, "{workspace}: {detail}");
        assert_eq!(steps[0]["target_id"], "implement", "{workspace}: {detail}");
        let events = get(
            &server,
            &format!("{runs}/events?tail=true&workspace={workspace}"),
        );
        assert_eq!(events["total"], 3, "{workspace}: {events}");
        assert_eq!(
            events["events"][2]["event_id"], "leaf-2",
            "{workspace}: {events}"
        );
        let forward = get(&server, &format!("{runs}/events?workspace={workspace}"));
        let forward_len = forward.as_array().map(Vec::len);
        assert_eq!(forward_len, Some(3), "{workspace}: {forward}");
        assert_eq!(forward[2]["event_id"], "leaf-2", "{workspace}: {forward}");
        let logs = get(&server, &format!("{runs}/logs?workspace={workspace}"));
        assert_eq!(logs, json!([]), "{workspace}: {logs}");
    }
}

// [ORB-24160] A run this host's store does not hold, which a task binds to
// another machine, is refused with a 404 that names the holding host and
// why the run cannot be shown here, not a bare "run not found".
scenario_test!(
    a_run_held_on_another_host_is_refused_naming_that_host,
    held_on_another_host_body
);

fn held_on_another_host_body(fixture: &Fixture) {
    let task = fixture
        .runtime
        .add_task(TaskAddParams {
            title: "Claimed elsewhere".into(),
            description: "a follower ran it".into(),
            ..Default::default()
        })
        .unwrap();
    bind_task_run_to_host(fixture, &task.id, "jrun-held-c1");
    let server = fixture.server(false);
    let response = server.get("/api/runs/jrun-held-c1?workspace=ws_http_fixture");
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    let body: Value = response.json().unwrap();
    assert_eq!(body["code"], "run_held_elsewhere", "{body}");
    let message = body["error"].as_str().unwrap();
    assert!(message.contains("follower-mini"), "names the host: {body}");
    assert!(
        message.contains("held in that host's store"),
        "says why: {body}"
    );
    assert_eq!(body["holder"]["machine_id"], "hm_follower_mini", "{body}");
    assert_eq!(body["holder"]["task_id"], task.id.as_str(), "{body}");
    // With no task naming it, the run is simply absent.
    let unknown = server.get("/api/runs/jrun-nowhere-c1?workspace=ws_http_fixture");
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);
    let unknown: Value = unknown.json().unwrap();
    assert_eq!(
        unknown["error"], "run not found: jrun-nowhere-c1",
        "{unknown}"
    );
    assert!(unknown.get("code").is_none(), "{unknown}");
}

// [ORB-24178] A run held only under another registered workspace is
// cancelled and replayed in that holding workspace. The dashboard's selected
// workspace names neither the run nor its holder, so both writes must find
// the holder rather than answer "run not found".
scenario_test!(
    cancel_and_replay_act_on_the_registered_workspace_holding_the_run,
    cancel_and_replay_body
);

fn cancel_and_replay_body(fixture: &Fixture) {
    let second = add_workspace(fixture, "ws_second", "second");
    fixture.job("replay_fixture");
    let seed = |run_id: &str, state: JobRunState| {
        let run = make_job_run(run_id, "replay_fixture", state, Utc::now(), None);
        second
            .sqlite_store()
            .unwrap()
            .upsert_job_run_for_workspace(&second.workspace_id().unwrap(), &run, None)
            .unwrap();
    };
    seed("jrun-held-running", JobRunState::Running);
    seed("jrun-held-source", JobRunState::Success);
    let server = fixture.replay_server();
    let selected = "ws_http_fixture";
    json_ok(server.send(
        "POST",
        &format!("/api/runs/jrun-held-running/cancel?workspace={selected}"),
        json!({}),
    ));
    assert_eq!(
        second.show_job_run("jrun-held-running").unwrap().state,
        JobRunState::Cancelled,
        "cancel must act on the holding workspace's run"
    );
    let replay = json_ok(server.send(
        "POST",
        &format!("/api/runs/jrun-held-source/replay?workspace={selected}"),
        json!({}),
    ));
    let replay_id = replay["run_id"].as_str().unwrap();
    assert_ne!(replay_id, "jrun-held-source", "{replay}");
    assert!(
        second.show_job_run(replay_id).is_ok(),
        "replay must be submitted to the holding workspace: {replay}"
    );
    assert!(
        fixture.runtime.show_job_run(replay_id).is_err(),
        "the selected workspace must not receive the replay: {replay}"
    );
}
