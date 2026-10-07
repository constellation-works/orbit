use std::collections::BTreeSet;

use chrono::Utc;
use orbit_core::application::job::JobRunListParams;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{JobRun, JobRunState, TaskStatus};
use orbit_types::workflow::{ChildDispatch, PipelineState};
use serde_json::{Value, json};

use super::support::{Fixture, Server, error_code, isolated, json_ok};

#[test]
fn ship_and_resume_refuse_in_flight_duplicates_without_persisting_runs() {
    isolated(
        "workflows::ship_and_resume_refuse_in_flight_duplicates_without_persisting_runs",
        || {
            let fixture = Fixture::new();
            fixture.job("task_auto_pipeline");
            fixture.job("resume_fixture");
            let task = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "HTTP ship dedupe".into(),
                    description: "explicit ship fixture".into(),
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                })
                .unwrap();
            let mut ship =
                fixture.seed_run("jrun-ship-live", "task_auto_pipeline", JobRunState::Pending);
            ship.input = Some(json!({"mode":"local","task_ids":[task.id]}));
            fixture.save_run(&ship);
            let source =
                fixture.seed_run("jrun-resume-source", "resume_fixture", JobRunState::Failed);
            let mut resume =
                fixture.seed_run("jrun-resume-live", "resume_fixture", JobRunState::Pending);
            resume.attempt = 2;
            resume.retry_source_run_id = Some(source.run_id.clone());
            fixture.save_run(&resume);
            let server = fixture.server(false);
            let before = fixture
                .runtime
                .list_job_runs(JobRunListParams::default())
                .unwrap();
            // Repeated clicks must continue naming the incumbent, without another receipt.
            for _ in 0..2 {
                let payload = error_code(
                    server.send(
                        "POST",
                        "/api/workflows/ship?workspace=ws_http_fixture",
                        json!({"task_ids":[task.id],"mode":"local"}),
                    ),
                    409,
                    "ship_run_in_flight",
                );
                assert_eq!(payload["run_id"], ship.run_id);
                assert_eq!(payload["task_id"], task.id);
                let payload = error_code(
                    server.send(
                        "POST",
                        &format!(
                            "/api/job-runs/{}/resume?workspace=ws_http_fixture",
                            source.run_id
                        ),
                        json!({}),
                    ),
                    409,
                    "resume_run_in_flight",
                );
                assert_eq!(payload["run_id"], resume.run_id);
                assert_eq!(payload["source_run_id"], source.run_id);
            }
            let after = fixture
                .runtime
                .list_job_runs(JobRunListParams::default())
                .unwrap();
            assert_eq!(
                after, before,
                "refused ship/resume clicks must not persist or change any run"
            );
        },
    );
}

#[test]
fn auto_stop_is_idempotent_and_preserves_in_flight_children() {
    isolated(
        "workflows::auto_stop_is_idempotent_and_preserves_in_flight_children",
        || {
            let fixture = Fixture::new();
            let coordinator =
                fixture.seed_run("jrun-auto", "workspace_auto_pipeline", JobRunState::Running);
            let child = fixture.seed_run("jrun-child", "task_auto_pipeline", JobRunState::Running);
            let mut state = PipelineState::new(
                coordinator.run_id.clone(),
                coordinator.job_id.clone(),
                json!({}),
            );
            state.record_child_dispatch(ChildDispatch::submitted(
                child.run_id.clone(),
                child.job_id.clone(),
                "invoke_detached".into(),
                false,
                false,
                Utc::now(),
            ));
            fixture
                .runtime
                .write_run_state(&coordinator.run_id, &state)
                .unwrap();
            let server = fixture.server(true);
            let first = json_ok(server.send(
                "POST",
                "/api/workflows/auto/stop?workspace=ws_http_fixture",
                json!({"reason":"first click"}),
            ));
            assert_eq!(first["outcome"], "stopped");
            assert_eq!(first["coordinators"][0]["run_id"], coordinator.run_id);
            assert_eq!(
                first["coordinators"][0]["remaining_children"][0]["run_id"],
                child.run_id
            );
            let stopped = fixture
                .runtime
                .read_run_state(&coordinator.run_id)
                .unwrap()
                .unwrap();
            assert!(stopped.admissions_stopped());
            let repeated = json_ok(server.send(
                "POST",
                "/api/workflows/auto/stop?workspace=ws_http_fixture",
                json!({"reason":"second click"}),
            ));
            assert_eq!(repeated["outcome"], "unchanged");
            assert_eq!(repeated["coordinators"][0]["outcome"], "unchanged");
            assert_eq!(
                repeated["coordinators"][0]["remaining_children"],
                first["coordinators"][0]["remaining_children"]
            );
            assert_eq!(
                fixture
                    .runtime
                    .read_run_state(&coordinator.run_id)
                    .unwrap()
                    .unwrap(),
                stopped,
                "a repeated stop preserves the first stop's evidence"
            );
            for run in [&coordinator, &child] {
                let detail = json_ok(server.get(&format!(
                    "/api/runs/{}?workspace=ws_http_fixture",
                    run.run_id
                )));
                assert_eq!(
                    detail["run"]["state"], "running",
                    "stopping admissions must not cancel a run: {detail}"
                );
            }
        },
    );
}

const WS: &str = "ws_http_fixture";

/// An absent body reached the handler: a 4xx whose text is not a JSON parse error.
fn assert_handler_reached(response: reqwest::blocking::Response, label: &str) {
    let status = response.status().as_u16();
    let value: Value = response
        .json()
        .unwrap_or_else(|_| json!({"error": "non-json"}));
    let error = value["error"].as_str().unwrap_or("");
    let parse_error = error.contains("JSON")
        || error.contains("Content-Type")
        || error.contains("invalid type")
        || error.contains("unknown field");
    assert!(
        (400..500).contains(&status) && !parse_error,
        "{label} must reach the handler and not parse as JSON: {status} {value}"
    );
}

/// A present body that does not parse must be a 400 carrying that parse error.
fn assert_parse_error(response: reqwest::blocking::Response, marker: &str) {
    let status = response.status().as_u16();
    let value: Value = response.json().expect("JSON error body");
    assert_eq!(status, 400, "{value}");
    let error = value["error"].as_str().unwrap_or("");
    assert!(
        error.contains(marker),
        "400 must carry the parse error ({marker}): {error}"
    );
}

fn send_raw(
    server: &Server,
    method: &str,
    path: &str,
    content_type: Option<&str>,
    body: Option<&str>,
) -> reqwest::blocking::Response {
    let mut request = server
        .request(method, path)
        .header("origin", &server.origin);
    if let Some(content_type) = content_type {
        request = request.header("content-type", content_type);
    }
    if let Some(body) = body {
        request = request.body(body.to_string());
    }
    request.send().unwrap()
}

fn run_ids(fixture: &Fixture) -> BTreeSet<String> {
    listed_runs(fixture)
        .into_iter()
        .map(|run| run.run_id)
        .collect()
}

fn listed_runs(fixture: &Fixture) -> Vec<JobRun> {
    fixture
        .runtime
        .list_job_runs(JobRunListParams::default())
        .unwrap()
}

#[test]
fn malformed_bodies_do_not_select_defaults_and_auto_requires_workspace() {
    isolated(
        "workflows::malformed_bodies_do_not_select_defaults_and_auto_requires_workspace",
        || {
            let fixture = Fixture::new();
            fixture.job("task_auto_pipeline");
            let cancel_target = fixture.seed_run(
                "jrun-cancel-target",
                "task_auto_pipeline",
                JobRunState::Running,
            );
            let server = fixture.server(true);
            let ws = format!("?workspace={WS}");
            let before = run_ids(&fixture);

            // Ship: unknown key, form body (curl's default content type), wrong type, bad JSON.
            let ship = format!("/api/workflows/ship{ws}");
            assert_parse_error(
                server.send("POST", &ship, json!({"task_id":"X"})),
                "unknown field `task_id`",
            );
            assert_parse_error(
                send_raw(
                    &server,
                    "POST",
                    &ship,
                    Some("application/x-www-form-urlencoded"),
                    Some(r#"{"task_ids":["ORB-1"]}"#),
                ),
                "Content-Type: application/json",
            );
            assert_parse_error(
                server.send("POST", &ship, json!({"task_ids":"ORB-1"})),
                "invalid type",
            );
            assert_parse_error(
                send_raw(&server, "POST", &ship, Some("application/json"), Some("{")),
                "Failed to parse the request body as JSON",
            );

            let cases = [
                (
                    "POST",
                    format!("/api/workflows/auto{ws}"),
                    r#"{"for_duration":1}"#,
                ),
                (
                    "POST",
                    format!("/api/workflows/auto/stop{ws}"),
                    r#"{"reason":1}"#,
                ),
                (
                    "POST",
                    format!("/api/runs/{}/cancel{ws}", cancel_target.run_id),
                    r#"{"force":"true"}"#,
                ),
                (
                    "POST",
                    format!("/api/tasks/missing/approve{ws}"),
                    r#"{"note":1}"#,
                ),
                (
                    "POST",
                    format!("/api/job-runs/missing/resume{ws}"),
                    r#"{"claim_token":1}"#,
                ),
                (
                    "PATCH",
                    format!("/api/frictions/missing{ws}"),
                    r#"{"status":1}"#,
                ),
            ];
            for (method, path, wrong_type) in &cases {
                assert_parse_error(
                    send_raw(&server, method, path, Some("application/json"), Some("{")),
                    "Failed to parse the request body as JSON",
                );
                assert_parse_error(
                    send_raw(
                        &server,
                        method,
                        path,
                        Some("application/x-www-form-urlencoded"),
                        Some(*wrong_type),
                    ),
                    "Content-Type: application/json",
                );
                assert_parse_error(
                    send_raw(
                        &server,
                        method,
                        path,
                        Some("application/json"),
                        Some(*wrong_type),
                    ),
                    "invalid type",
                );
            }
            assert_eq!(
                run_ids(&fixture),
                before,
                "a rejected body must not create a run"
            );
            assert_eq!(
                run_state(&fixture, &cancel_target.run_id),
                JobRunState::Running,
                "a string force flag must not become a graceful cancel"
            );

            // Absent and empty bodies still take the handler default.
            for body in [None, Some("")] {
                let response = send_raw(
                    &server,
                    "POST",
                    &format!("/api/workflows/auto{ws}"),
                    body.map(|_| "application/json"),
                    body,
                );
                let status = response.status().as_u16();
                let value: Value = response.json().unwrap();
                assert_eq!(status, 400, "{value}");
                assert_eq!(value["error"], "for_duration must not be empty", "{value}");
            }
            let stopped = json_ok(send_raw(
                &server,
                "POST",
                &format!("/api/workflows/auto/stop{ws}"),
                None,
                None,
            ));
            assert_eq!(stopped["outcome"], "idle");
            assert_handler_reached(
                send_raw(
                    &server,
                    "POST",
                    &format!("/api/tasks/missing/approve{ws}"),
                    None,
                    None,
                ),
                "empty approve",
            );
            assert_handler_reached(
                send_raw(
                    &server,
                    "POST",
                    &format!("/api/job-runs/missing/resume{ws}"),
                    None,
                    None,
                ),
                "empty resume",
            );
            let patched = send_raw(
                &server,
                "PATCH",
                &format!("/api/frictions/missing{ws}"),
                Some("application/json"),
                Some(""),
            );
            let patch_status = patched.status().as_u16();
            let patch_body: Value = patched.json().unwrap();
            assert_eq!(patch_status, 400, "{patch_body}");
            assert_eq!(
                patch_body["error"], "request body must include `status`, `tags`, or `title`",
                "{patch_body}"
            );
            let cancelled = json_ok(send_raw(
                &server,
                "POST",
                &format!("/api/runs/{}/cancel{ws}", cancel_target.run_id),
                None,
                None,
            ));
            assert!(
                cancelled["outcome"].as_str().is_some(),
                "empty cancel body is the graceful default: {cancelled}"
            );
            assert_ne!(
                run_state(&fixture, &cancel_target.run_id),
                JobRunState::Running,
                "an empty cancel body still cancels gracefully: {cancelled}"
            );
            assert_eq!(
                run_ids(&fixture),
                before,
                "defaults must not add a run here"
            );

            // Missing ?workspace refuses before any drain starts, even with a server default.
            error_code(
                server.send("POST", "/api/workflows/auto", json!({"for_duration":"1m"})),
                400,
                "workspace_required",
            );
            error_code(
                server.send("POST", "/api/workflows/auto/stop", json!({})),
                400,
                "workspace_required",
            );
            assert_eq!(run_ids(&fixture), before);

            let mut discovered = Vec::new();
            for body in [None, Some("")] {
                let response = send_raw(
                    &server,
                    "POST",
                    &ship,
                    body.map(|_| "application/json"),
                    body,
                );
                let submitted = json_ok(response);
                assert_eq!(submitted["workflow"], "ship");
                assert!(
                    matches!(submitted["state"].as_str(), Some("submitted" | "queued")),
                    "empty ship body must submit discovery mode: {submitted}"
                );
                discovered.push(submitted["run_id"].as_str().unwrap().to_string());
            }
            let recorded = listed_runs(&fixture);
            assert_eq!(recorded.len(), before.len() + discovered.len());
            for run_id in discovered {
                let input = recorded
                    .iter()
                    .find(|run| run.run_id == run_id)
                    .and_then(|run| run.input.clone())
                    .unwrap_or_else(|| json!({}));
                assert!(
                    input.get("task_ids").is_none(),
                    "an empty ship body is backlog discovery, not a smuggled task: {input}"
                );
            }
        },
    );
}

fn run_state(fixture: &Fixture, run_id: &str) -> JobRunState {
    listed_runs(fixture)
        .into_iter()
        .find(|run| run.run_id == run_id)
        .map(|run| run.state)
        .unwrap_or_else(|| panic!("missing run {run_id}"))
}

#[test]
fn replay_returns_a_durable_receipt_while_the_job_runs_in_a_detached_worker() {
    isolated(
        "workflows::replay_returns_a_durable_receipt_while_the_job_runs_in_a_detached_worker",
        || {
            use std::time::{Duration, Instant};
            let fixture = Fixture::new();
            // Longer than the HTTP client's five-second timeout: foreground
            // execution cannot return a receipt for this job in time.
            fixture.sleep_job("replay_fixture", 6);
            let mut source =
                fixture.seed_run("jrun-replay-source", "replay_fixture", JobRunState::Success);
            source.input = Some(json!({
                "marker": "preserved", "completion": "done",
                "trusted_host_admission": {"authorized_by": "old operator"},
                "review": {"contract_version": 0},
            }));
            fixture.save_run(&source);
            let source_before = fixture.runtime.show_job_run(&source.run_id).unwrap();
            let mut source_state =
                PipelineState::new(source.run_id.clone(), source.job_id.clone(), json!({}));
            source_state.step_states.insert(0, JobRunState::Success);
            source_state.next_step_index = 1;
            fixture
                .runtime
                .write_run_state(&source.run_id, &source_state)
                .unwrap();
            let server = fixture.replay_server();
            let receipt = json_ok(server.send(
                "POST",
                &format!("/api/runs/{}/replay?workspace={WS}", source.run_id),
                json!({}),
            ));
            assert_eq!(receipt["state"], "submitted");
            let run_id = receipt["run_id"].as_str().unwrap();
            assert_ne!(run_id, source.run_id);
            let replay = fixture.runtime.show_job_run(run_id).unwrap();
            assert!(
                matches!(replay.state, JobRunState::Pending | JobRunState::Running),
                "{replay:?}"
            );
            assert_eq!(
                replay.retry_source_run_id.as_deref(),
                Some(source.run_id.as_str())
            );
            assert_eq!(replay.attempt, 1);
            assert_eq!(replay.input.as_ref().unwrap()["marker"], "preserved");
            assert_eq!(replay.input.as_ref().unwrap()["completion"], "done");
            assert!(
                replay
                    .input
                    .as_ref()
                    .unwrap()
                    .get("trusted_host_admission")
                    .is_none()
            );
            assert!(replay.input.as_ref().unwrap().get("review").is_none());

            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                let run = fixture.runtime.show_job_run(run_id).unwrap();
                if run.state == JobRunState::Running {
                    assert_ne!(
                        run.pid,
                        Some(server.pid()),
                        "execution belongs to the detached worker"
                    );
                    break;
                }
                assert!(
                    !run.state.is_terminal() && Instant::now() < deadline,
                    "worker never ran: {run:?}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            loop {
                let run = fixture.runtime.show_job_run(run_id).unwrap();
                if run.state.is_terminal() {
                    assert_eq!(run.state, JobRunState::Success, "{run:?}");
                    break;
                }
                assert!(Instant::now() < deadline, "worker did not finish: {run:?}");
                std::thread::sleep(Duration::from_millis(20));
            }
            let replay_state = fixture.runtime.read_run_state(run_id).unwrap().unwrap();
            assert_eq!(
                replay_state.trigger,
                Some(orbit_types::workflow::JobRunTrigger::dashboard())
            );
            assert!(
                replay_state.pipeline["nap"]["slept_seconds"]
                    .as_f64()
                    .unwrap()
                    >= 6.0,
                "replay must execute even a checkpointed source step"
            );
            assert_eq!(
                fixture.runtime.show_job_run(&source.run_id).unwrap(),
                source_before
            );
            assert_eq!(
                fixture
                    .runtime
                    .read_run_state(&source.run_id)
                    .unwrap()
                    .unwrap(),
                source_state
            );
        },
    );
}

#[test]
fn replay_requires_workspace_operator_and_the_workspace_claim_token() {
    isolated(
        "workflows::replay_requires_workspace_operator_and_the_workspace_claim_token",
        || {
            let fixture = Fixture::new();
            fixture.job("replay_fixture");
            let mut source =
                fixture.seed_run("jrun-replay-source", "replay_fixture", JobRunState::Success);
            source.input = Some(json!({"completion": "done"}));
            fixture.save_run(&source);
            let operator = fixture.replay_server();
            let agent = fixture.server(false);
            let path = format!("/api/runs/{}/replay?workspace={WS}", source.run_id);
            let before = run_ids(&fixture);
            error_code(
                operator.send(
                    "POST",
                    &format!("/api/runs/{}/replay", source.run_id),
                    json!({}),
                ),
                400,
                "workspace_required",
            );
            let denial = error_code(
                agent.send("POST", &path, json!({})),
                403,
                "authorization_denied",
            );
            assert_eq!(denial["operation"], "job.run");
            assert_parse_error(
                operator.send("POST", &path, json!({"claim_token": 1})),
                "invalid type",
            );
            let grant = fixture
                .runtime
                .sqlite_store()
                .unwrap()
                .acquire_workspace_claim(&orbit_store::contracts::WorkspaceClaimAcquireParams {
                    workspace_orbit_dir: fixture
                        .runtime
                        .paths()
                        .orbit_dir
                        .to_string_lossy()
                        .into_owned(),
                    workspace_id: Some(WS.into()),
                    actor: "codex".into(),
                    ttl_seconds: 60,
                    machine_id: Some("fixture".into()),
                    session_id: Some("holder".into()),
                })
                .unwrap();
            for body in [json!({}), json!({"claim_token": "wrong"})] {
                error_code(
                    operator.send("POST", &path, body),
                    409,
                    "workspace_claim_held",
                );
            }
            assert_eq!(
                run_ids(&fixture),
                before,
                "refusals cannot persist replay runs"
            );
            let receipt =
                json_ok(operator.send("POST", &path, json!({"claim_token": grant.claim_token})));
            let run_id = receipt["run_id"].as_str().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let run = fixture.runtime.show_job_run(run_id).unwrap();
                if run.state.is_terminal() {
                    assert_eq!(run.state, JobRunState::Success);
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "{run:?}");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        },
    );
}

/// A failed pull window retains its typed cause and pass health in the HTTP
/// detail after terminalization, where the dashboard can show the remedy.
#[test]
fn failed_pull_protocol_is_visible_after_terminalization() {
    isolated(
        "workflows::failed_pull_protocol_is_visible_after_terminalization",
        || {
            let fixture = Fixture::new();
            let mut run =
                fixture.seed_run("jrun-skew", "workspace_pull_pipeline", JobRunState::Failed);
            let now = Utc::now();
            run.steps.push(orbit_types::workflow::JobRunStep {
                step_index: 0,
                target_type: orbit_types::workflow::JobTargetType::Job,
                target_id: run.job_id.clone(),
                started_at: Some(now),
                finished_at: Some(now),
                duration_ms: None,
                exit_code: None,
                agent_response_json: None,
                state: JobRunState::Failed,
                error_code: Some("protocol_skew".into()),
                error_message: Some("caller and owner pull request fingerprints differ".into()),
            });
            fixture.save_run(&run);
            fixture
                .runtime
                .sqlite_store()
                .unwrap()
                .upsert_job_run_step_for_workspace(
                    &fixture.runtime.workspace_id().unwrap(),
                    &run.run_id,
                    &run.steps[0],
                )
                .unwrap();
            let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
            state.drain_last_pass = Some(orbit_types::workflow::DrainAdmissionPass {
                capacity: None,
                recorded_at: now,
                queued: 0,
                deferred: vec![],
                excluded: vec![],
                excluded_total: 0,
                resource_throttle: None,
                last_pass_error_code: Some("protocol_skew".into()),
                last_pass_error: run.steps[0].error_message.clone(),
                consecutive_pass_failures: 1,
                degraded: true,
            });
            fixture
                .runtime
                .write_run_state(&run.run_id, &state)
                .unwrap();
            let server = fixture.server(false);
            let detail = json_ok(server.get("/api/runs/jrun-skew?workspace=ws_http_fixture"));
            assert_eq!(detail["run"]["state"], "failed", "{detail}");
            assert_eq!(detail["run"]["error_code"], "protocol_skew", "{detail}");
            assert_eq!(
                detail["run"]["drain_last_pass"]["last_pass_error_code"],
                "protocol_skew"
            );
            assert_eq!(detail["run"]["drain_last_pass"]["degraded"], true);
        },
    );
}
