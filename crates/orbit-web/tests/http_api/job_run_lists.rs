//! Regressions for bounded job-run reads and request-scoped reconciliation.

use chrono::Utc;
use orbit_core::application::job::job_run_to_json;
use orbit_core::{JobRun, JobRunState};
use orbit_types::workflow::{
    ChildDispatch, DrainAdmissionsStop, DrainCancelRequest, DrainWindowExtension, DrainWorkerLimit,
    JobRunTrigger, PipelineState,
};
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

fn seed_live_run(fixture: &Fixture, id: &str, state: JobRunState) -> JobRun {
    let mut run = fixture.seed_run(id, "list-fixture", state);
    run.pid = Some(std::process::id());
    run.pid_start_time =
        orbit_common::process::identity::process_start_identity_token(std::process::id());
    fixture.save_run(&run);
    run
}

fn expected_item(run: &JobRun) -> Value {
    let mut item = job_run_to_json(run, None);
    for key in ["tasks", "task_ids", "parent_run_id", "root_cause"] {
        item[key] = Value::Null;
    }
    item
}

/// Count actual SQLite work through Store's public page boundary. The
/// progress hook sees every VM instruction, including rows discarded before
/// LIMIT; returned row counts alone cannot detect a scan of success history.
fn page_work(
    store: &orbit_store::Store,
    query: &orbit_store::contracts::JobRunQuery,
) -> (Vec<String>, usize) {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    let instructions = Arc::new(AtomicUsize::new(0));
    let counter = instructions.clone();
    store
        .connection()
        .lock()
        .unwrap()
        .progress_handler(
            1,
            Some(move || {
                counter.fetch_add(1, Ordering::Relaxed);
                false
            }),
        )
        .unwrap();
    let runs = store.list_job_runs_for_workspace("ws_http_fixture", query);
    store
        .connection()
        .lock()
        .unwrap()
        .progress_handler(0, None::<fn() -> bool>)
        .unwrap();
    (
        runs.unwrap().into_iter().map(|run| run.run_id).collect(),
        instructions.load(Ordering::Relaxed),
    )
}

#[test]
fn multi_state_pages_do_not_scan_unrelated_success_history() {
    isolated(
        "job_run_lists::multi_state_pages_do_not_scan_unrelated_success_history",
        || {
            use orbit_store::contracts::{JobRunOrder, JobRunQuery};

            let fixture = Fixture::new();
            let template = fixture.seed_run("template", "list-fixture", JobRunState::Success);
            let mut measurements = Vec::new();
            for success_count in [12, 120] {
                let store = orbit_store::Store::open_in_memory().unwrap();
                // Five old matching rows and a tiny, growing unrelated history.
                // Equal timestamps also exercise the run-ID tiebreak across states.
                for (index, state) in [
                    JobRunState::Pending,
                    JobRunState::Running,
                    JobRunState::Failed,
                    JobRunState::Timeout,
                    JobRunState::Interrupted,
                ]
                .into_iter()
                .chain(std::iter::repeat_n(JobRunState::Success, success_count))
                .enumerate()
                {
                    let mut run = template.clone();
                    run.run_id = format!("jrun-{index:03}");
                    run.state = state;
                    run.created_at = template.created_at
                        + chrono::Duration::seconds(if index < 5 { 0 } else { 1 });
                    run.started_at = Some(run.created_at);
                    run.finished_at = state.is_terminal().then_some(run.created_at);
                    store
                        .upsert_job_run_for_workspace("ws_http_fixture", &run, None)
                        .unwrap();
                }
                let mut work = Vec::new();
                for (states, expected) in [
                    (
                        vec![JobRunState::Pending, JobRunState::Running],
                        vec!["jrun-000"],
                    ),
                    (
                        vec![
                            JobRunState::Failed,
                            JobRunState::Timeout,
                            JobRunState::Interrupted,
                        ],
                        vec!["jrun-002"],
                    ),
                    (vec![JobRunState::Held, JobRunState::Cancelled], vec![]),
                ] {
                    // A job filter must not persuade the planner to walk
                    // that job's success history instead of the state index.
                    for job_id in [None, Some("list-fixture"), Some("absent")] {
                        let query = JobRunQuery {
                            job_id: job_id.map(str::to_owned),
                            states: states.clone(),
                            limit: Some(1),
                            order_by: JobRunOrder::Recency,
                            include_steps: false,
                            ..Default::default()
                        };
                        let (ids, instructions) = page_work(&store, &query);
                        if job_id == Some("absent") {
                            assert!(ids.is_empty());
                        } else {
                            assert_eq!(ids, expected);
                        }
                        work.push(instructions);
                    }
                }
                measurements.push(work);
            }
            if let Some(path) = std::env::var_os("ORBIT_HTTP_RUN_QUERY_WORK_EVIDENCE") {
                let evidence = json!({
                    "schema_version": 1,
                    "sqlite_version": rusqlite::version(),
                    "unrelated_success_rows": [12, 120],
                    "page_states": ["active", "failed", "empty (held/cancelled)"],
                    "job_filters_per_page": [null, "list-fixture", "absent"],
                    "vm_instructions": measurements,
                });
                std::fs::write(path, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
            }
            assert_eq!(
                measurements[0], measurements[1],
                "multi-state page work must be independent of unrelated newer success rows (12 versus 120): {measurements:?}"
            );
        },
    );
}

#[test]
fn job_run_filters_reconcile_once_and_preserve_pages() {
    isolated(
        "job_run_lists::job_run_filters_reconcile_once_and_preserve_pages",
        || {
            let fixture = Fixture::new();
            let server = fixture.counted_job_run_server();
            // Open the workspace before seeding: runtime-open reconciliation is
            // separate from the scan performed for each completed HTTP request.
            json_ok(server.get("/api/job-runs?limit=1"));
            let states = [
                JobRunState::Pending,
                JobRunState::Running,
                JobRunState::Success,
                JobRunState::Failed,
                JobRunState::Timeout,
                JobRunState::Interrupted,
                JobRunState::Skipped,
                JobRunState::Retrying,
                JobRunState::Cancelled,
                JobRunState::Held,
            ];
            let now = Utc::now();
            let mut runs = Vec::new();
            for (index, state) in states.into_iter().enumerate() {
                for suffix in ["a", "b"] {
                    let mut run =
                        seed_live_run(&fixture, &format!("jrun-{index:02}-{suffix}"), state);
                    // Creation order opposes recency; pairs tie across a page
                    // boundary and must retain the run-id ascending tiebreak.
                    run.created_at = now - chrono::Duration::seconds(50 - index as i64);
                    let recent = now - chrono::Duration::seconds(index as i64);
                    run.started_at = (state != JobRunState::Pending).then_some(recent);
                    if state == JobRunState::Pending {
                        run.created_at = recent;
                    }
                    run.finished_at = state.is_terminal().then_some(recent);
                    run.duration_ms = state.is_terminal().then_some(0);
                    fixture.save_run(&run);
                    runs.push(run);
                }
            }
            for filter in ["active", "failed", "all", "pending", "running", "terminal"] {
                let matching: Vec<_> = runs
                    .iter()
                    .filter(|run| match filter {
                        "active" => {
                            matches!(run.state, JobRunState::Pending | JobRunState::Running)
                        }
                        "failed" => matches!(
                            run.state,
                            JobRunState::Failed | JobRunState::Timeout | JobRunState::Interrupted
                        ),
                        "pending" => run.state == JobRunState::Pending,
                        "running" => run.state == JobRunState::Running,
                        "terminal" => run.state.is_terminal(),
                        _ => true,
                    })
                    .collect();
                for limit in [1, 3, 100] {
                    let before = fixture.job_run_query_trace().len();
                    let page =
                        json_ok(server.get(&format!("/api/job-runs?state={filter}&limit={limit}")));
                    let trace = fixture.job_run_query_trace();
                    let scans = &trace[before..];
                    assert_eq!(
                        scans.len(),
                        1,
                        "one stale-run scan for state={filter}, limit={limit}: {scans:?}"
                    );
                    assert_eq!(scans[0]["span"]["name"], "stale_job_run_scan");
                    assert_eq!(page["state"], filter);
                    assert_eq!(page["total"], matching.len());
                    assert_eq!(page["truncated"], matching.len() > limit);
                    let expected: Vec<_> = matching
                        .iter()
                        .take(limit)
                        .map(|run| expected_item(run))
                        .collect();
                    assert_eq!(
                        page["items"],
                        json!(expected),
                        "workspace state={filter}, limit={limit}"
                    );

                    if matches!(filter, "active" | "failed" | "all") {
                        let before = trace.len();
                        let aggregate = json_ok(
                            server.get(&format!("/api/job-runs/all?state={filter}&limit={limit}")),
                        );
                        assert_eq!(
                            fixture.job_run_query_trace().len(),
                            before,
                            "aggregate polling must remain observational"
                        );
                        assert_eq!(aggregate["state"], filter);
                        assert_eq!(aggregate["total"], page["total"]);
                        assert_eq!(aggregate["truncated"], page["truncated"]);
                        let expected: Vec<_> = expected
                            .into_iter()
                            .map(|mut item| {
                                item["workspace_id"] = json!("ws_http_fixture");
                                item["workspace_name"] = json!("fixture");
                                item
                            })
                            .collect();
                        assert_eq!(aggregate["items"], json!(expected));
                    }
                }
            }
        },
    );
}

#[test]
#[cfg(unix)]
fn job_run_list_reconciles_a_worker_that_died_after_server_open() {
    isolated(
        "job_run_lists::job_run_list_reconciles_a_worker_that_died_after_server_open",
        || {
            let fixture = Fixture::new();
            let server = fixture.counted_job_run_server();
            json_ok(server.get("/api/job-runs?limit=1"));
            let mut worker = std::process::Command::new("sh")
                .args(["-c", "read line"])
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut run =
                fixture.seed_run("jrun-dead-worker", "list-fixture", JobRunState::Running);
            // A newly started worker has a liveness grace window. Model an
            // established worker dying, without sleeping through that window.
            let started = Utc::now() - chrono::Duration::minutes(1);
            run.created_at = started;
            run.scheduled_at = started;
            run.started_at = Some(started);
            run.pid = Some(worker.id());
            run.pid_start_time =
                orbit_common::process::identity::process_start_identity_token(worker.id());
            drop(worker.stdin.take());
            worker.wait().unwrap();
            fixture.save_run(&run);

            let aggregate = json_ok(server.get("/api/job-runs/all?state=active&limit=1"));
            assert_eq!(aggregate["items"][0]["run_id"], run.run_id);
            assert_eq!(aggregate["items"][0]["state"], "running");
            let before = fixture.job_run_query_trace().len();
            let page = json_ok(server.get("/api/job-runs?state=failed&limit=1"));
            assert_eq!(fixture.job_run_query_trace().len() - before, 1);
            assert_eq!(page["total"], 1);
            assert_eq!(page["truncated"], false);
            assert_eq!(page["items"][0]["run_id"], run.run_id);
            assert_eq!(page["items"][0]["state"], "interrupted");
            assert_eq!(
                fixture
                    .runtime
                    .show_job_run_observed(&run.run_id)
                    .unwrap()
                    .state,
                JobRunState::Interrupted
            );
            let active = json_ok(server.get("/api/job-runs?state=active&limit=1"));
            assert_eq!(active["total"], 0);
            assert_eq!(active["items"], json!([]));
            assert_eq!(active["truncated"], false);
        },
    );
}

/// Diagnostic only: paced HTTP samples protect no extra CI guarantee. The
/// regular regression above counts scans deterministically on shared hosts.
/// Set `ORBIT_HTTP_RUN_LATENCY_EVIDENCE` to retain the samples as JSON.
#[test]
#[ignore = "paced job-run HTTP latency measurement"]
fn job_run_badge_latency_with_26_active_runs() {
    isolated(
        "job_run_lists::job_run_badge_latency_with_26_active_runs",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(false);
            json_ok(server.get("/api/job-runs?limit=1"));
            for index in 0..26 {
                seed_live_run(
                    &fixture,
                    &format!("jrun-live-{index:02}"),
                    if index % 2 == 0 {
                        JobRunState::Running
                    } else {
                        JobRunState::Pending
                    },
                );
            }
            let mut failed = fixture.seed_run("jrun-failed", "list-fixture", JobRunState::Failed);
            failed.duration_ms = Some(0);
            fixture.save_run(&failed);
            let mut measurements = Vec::new();
            for filter in ["active", "failed"] {
                let path = format!("/api/job-runs?state={filter}&limit=1");
                json_ok(server.get(&path));
                let mut samples = Vec::new();
                for _ in 0..20 {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    let start = std::time::Instant::now();
                    let page = json_ok(server.get(&path));
                    samples.push(start.elapsed().as_secs_f64() * 1000.0);
                    assert_eq!(page["total"], if filter == "active" { 26 } else { 1 });
                }
                let mut sorted = samples.clone();
                sorted.sort_by(f64::total_cmp);
                let measurement = json!({"endpoint":path, "active_runs":26, "samples_ms": samples, "p50_ms":sorted[9], "p95_ms":sorted[18]});
                tracing::info!(%measurement, "job-run badge latency");
                measurements.push(measurement);
                assert!(
                    sorted[18] <= 60.0,
                    "{filter} nearest-rank p95 exceeded 60ms: {sorted:?}"
                );
            }
            if let Some(path) = std::env::var_os("ORBIT_HTTP_RUN_LATENCY_EVIDENCE") {
                let evidence = json!({
                    "schema_version": 1,
                    "measured_at": Utc::now(),
                    "workload": "disposable HTTP workspace with 26 active runs sharing a live fixture owner",
                    "warmup_requests_per_endpoint": 1,
                    "pace_ms": 250,
                    "measurements": measurements,
                });
                std::fs::write(path, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
            }
        },
    );
}

/// A live drain whose pipeline state names a dispatched child, lock and
/// dependency waits, its trigger and every operator drain control.
fn drain_state(drain: &JobRun, child: &JobRun) -> PipelineState {
    let now = Utc::now();
    let mut state = PipelineState::new(drain.run_id.clone(), drain.job_id.clone(), json!({}));
    state.record_child_dispatch(ChildDispatch::submitted(
        child.run_id.clone(),
        child.job_id.clone(),
        "leaf_invoke".into(),
        false,
        false,
        now,
    ));
    state.waiting_on_locks = Some(vec!["file:src/lib.rs".into()]);
    state.waiting_on_deps = Some(vec!["ORB-1".into()]);
    state.trigger = Some(JobRunTrigger::dashboard());
    let drain_state = &mut state.pull_drain;
    drain_state.drain_worker_limit = Some(DrainWorkerLimit {
        max_active_leaf_runs: 3,
        previous_max_active_leaf_runs: 2,
        revision: 1,
        actor: "operator".into(),
        reason: None,
        updated_at: now,
    });
    drain_state.drain_admissions_stop = Some(DrainAdmissionsStop {
        actor: "operator".into(),
        reason: Some("maintenance".into()),
        stopped_at: now,
    });
    drain_state.drain_window_extension = Some(DrainWindowExtension {
        deadline: now + chrono::Duration::hours(2),
        previous_deadline: now + chrono::Duration::hours(1),
        revision: 1,
        actor: "operator".into(),
        reason: None,
        extended_at: now,
    });
    drain_state.drain_cancel = Some(DrainCancelRequest {
        actor: "operator".into(),
        source: "dashboard".into(),
        reason: None,
        requested_at: now,
    });
    state
}

/// Both run lists project each row with its pipeline state, so a drain reads
/// the same dispatches, waits, trigger and drain controls as `orbit run
/// history --json`, which projects through the same shared row. When the
/// states cannot be read, the rows keep their stateless shape without
/// `parent_run_id`.
#[test]
fn run_list_rows_carry_their_pipeline_state() {
    isolated(
        "job_run_lists::run_list_rows_carry_their_pipeline_state",
        || {
            let fixture = Fixture::new();
            let child = seed_live_run(&fixture, "jrun-state-child", JobRunState::Running);
            let mut drain = seed_live_run(&fixture, "jrun-state-drain", JobRunState::Running);
            drain.job_id = "workspace_pull_pipeline".into();
            fixture.save_run(&drain);
            let state = drain_state(&drain, &child);
            fixture
                .runtime
                .write_run_state(&drain.run_id, &state)
                .unwrap();
            let expected = job_run_to_json(&drain, Some(&state));
            let server = fixture.server(false);
            let row = |page: &Value, id: &str| {
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["run_id"] == id)
                    .unwrap_or_else(|| panic!("{id} is listed: {page}"))
                    .clone()
            };
            for path in ["/api/job-runs?limit=10", "/api/job-runs/all?limit=10"] {
                let page = json_ok(server.get(path));
                let listed = row(&page, &drain.run_id);
                for key in [
                    "child_dispatches",
                    "waiting_on_deps",
                    "waiting_on_locks",
                    "trigger",
                    "drain_worker_limit",
                    "drain_admissions_stop",
                    "drain_window_extension",
                    "drain_cancel",
                    "agent_invocation",
                    "activity_crew_draws",
                    "env_pass_unset",
                    "root_cause",
                ] {
                    assert_eq!(listed[key], expected[key], "{path} {key}");
                }
                assert_eq!(
                    listed["child_dispatches"][0]["child_run_id"], child.run_id,
                    "{path}"
                );
                assert_eq!(listed["waiting_on_locks"], json!(["file:src/lib.rs"]));
                assert_eq!(listed["trigger"]["kind"], "dashboard", "{path}");
                assert_eq!(listed["drain_worker_limit"]["max_active_leaf_runs"], 3);
                assert_eq!(listed["drain_cancel"]["source"], "dashboard", "{path}");
                assert_eq!(
                    row(&page, &child.run_id)["parent_run_id"],
                    drain.run_id,
                    "{path}"
                );
            }

            fixture
                .runtime
                .sqlite_store()
                .unwrap()
                .connection()
                .lock()
                .unwrap()
                .execute_batch("DROP TABLE job_run_states")
                .unwrap();
            for path in ["/api/job-runs?limit=10", "/api/job-runs/all?limit=10"] {
                let page = json_ok(server.get(path));
                let listed = row(&page, &drain.run_id);
                assert_eq!(listed["child_dispatches"], json!([]), "{path}");
                assert_eq!(listed["drain_worker_limit"], Value::Null, "{path}");
                assert!(
                    row(&page, &child.run_id).get("parent_run_id").is_none(),
                    "{path} omits lineage without states: {page}"
                );
            }
        },
    );
}
