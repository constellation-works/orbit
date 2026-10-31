use super::support::{Fixture, Server, error_code, isolated, json_ok};
use chrono::Utc;
use orbit_core::JobRunState::{Cancelled, Failed, Pending, Running, Success};
use orbit_core::TaskPriority::{High, Low, Medium};
use orbit_core::TaskStatus::{Backlog, InProgress, Proposed};
use orbit_core::application::job::JobRunListParams;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{JobRun, JobRunState, Task, TaskComplexity, TaskPriority, TaskStatus};
use orbit_types::workflow::{
    BASELINE_RED_HOLD_EVENT, BaselineRedHold, ChildDispatch, PipelineState,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
macro_rules! wf_test {
    ($name:ident, $body:block) => {
        #[test]
        fn $name() {
            isolated(concat!("workflows::", stringify!($name)), || $body);
        }
    };
}
macro_rules! check {
    ($v:expr, $($p:literal : $want:expr),+ $(,)?) => {
        $(assert_eq!($v.pointer($p), Some(&serde_json::json!($want)), "field {} of {}", $p, $v);)+
    };
}
macro_rules! eq {
    ($a:expr => $b:expr) => { assert_eq!($a, $b) };
    ($a:expr => $b:expr, $($m:tt)*) => { assert_eq!($a, $b, $($m)*) };
}
macro_rules! ne {
    ($a:expr => $b:expr) => { assert_ne!($a, $b) };
    ($a:expr => $b:expr, $($m:tt)*) => { assert_ne!($a, $b, $($m)*) };
}
macro_rules! chk {
    ($c:expr => $($m:tt)*) => { assert!($c, $($m)*) };
}
macro_rules! perr {
    ($r:expr => $m:expr) => {
        assert_parse_error($r, $m)
    };
}
macro_rules! reached {
    ($r:expr => $m:expr) => {
        assert_handler_reached($r, $m)
    };
}
macro_rules! ecode {
    ($r:expr => $s:expr, $c:expr) => {
        error_code($r, $s, $c)
    };
}
const WS: &str = "ws_http_fixture";
fn auto_uri() -> String {
    format!("/api/workflows/auto?workspace={WS}")
}
fn readiness_uri() -> String {
    format!("/api/workflows/auto/readiness?workspace={WS}")
}
fn ship_uri() -> String {
    format!("/api/workflows/ship?workspace={WS}")
}
fn stop_uri() -> String {
    format!("/api/workflows/auto/stop?workspace={WS}")
}
fn ok_post(server: &Server, path: &str, body: Value) -> Value {
    json_ok(server.send("POST", path, body))
}
fn ok_get(server: &Server, path: &str) -> Value {
    json_ok(server.get(path))
}
fn status_of(response: &reqwest::blocking::Response) -> u16 {
    response.status().as_u16()
}
fn assert_no_new_runs(fixture: &Fixture, before: &BTreeSet<String>, msg: &str) {
    assert_eq!(&run_ids(fixture), before, "{msg}");
}
fn find_run(fixture: &Fixture, run_id: &str) -> JobRun {
    listed_runs(fixture)
        .into_iter()
        .find(|run| run.run_id == run_id)
        .unwrap_or_else(|| panic!("missing run {run_id}"))
}
fn readiness_entry(payload: &Value, task: &Task) -> Value {
    payload["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["task_id"] == task.id.as_str())
        .cloned()
        .unwrap_or_else(|| panic!("{} missing: {payload:#}", task.id))
}
fn wait_terminal(fixture: &Fixture, run_id: &str, secs: u64) -> JobRun {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        let run = show_run(fixture, run_id);
        if run.state.is_terminal() {
            return run;
        }
        chk!(std::time::Instant::now() < deadline => "run did not finish: {run:?}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
wf_test!(
    ship_and_resume_refuse_in_flight_duplicates_without_persisting_runs,
    {
        let fixture = Fixture::new();
        fixture.job("task_auto_pipeline");
        fixture.job("resume_fixture");
        let task = backlog_task(&fixture, "HTTP ship dedupe", &[]);
        let mut ship = fixture.seed_run("jrun-ship-live", "task_auto_pipeline", Pending);
        ship.input = Some(json!({"mode":"local","task_ids":[task.id]}));
        fixture.save_run(&ship);
        let source = fixture.seed_run("jrun-resume-source", "resume_fixture", Failed);
        let mut resume = fixture.seed_run("jrun-resume-live", "resume_fixture", Pending);
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
            let payload = ecode!(server.send( "POST", &ship_uri(), json!({"task_ids":[task.id],"mode":"local"}), ) => 409, "ship_run_in_flight");
            check!(payload, "/run_id": ship.run_id, "/task_id": task.id);
            let payload = ecode!(server.send( "POST", &format!( "/api/job-runs/{}/resume?workspace=ws_http_fixture", source.run_id ), json!({}), ) => 409, "resume_run_in_flight");
            check!(payload, "/run_id": resume.run_id, "/source_run_id": source.run_id);
        }
        let after = fixture
            .runtime
            .list_job_runs(JobRunListParams::default())
            .unwrap();
        eq!(after => before, "refused ship/resume clicks must not persist or change any run");
    }
);
wf_test!(auto_stop_is_idempotent_and_preserves_in_flight_children, {
    let fixture = Fixture::new();
    let coordinator = fixture.seed_run("jrun-auto", "workspace_auto_pipeline", Running);
    let child = fixture.seed_run("jrun-child", "task_auto_pipeline", Running);
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
    save_state(&fixture, &coordinator.run_id, &state);
    let server = fixture.server(true);
    let refused = server.send(
        "POST",
        "/api/workflows/auto/stop?workspace=ws_http_fixture",
        json!({"auto_concurrency":"enforce"}),
    );
    assert_eq!(refused.status().as_u16(), 400);
    eq!(saved_state(&fixture, &coordinator.run_id) => Some(state.clone()), "an incompatible mode on stop must be refused before changing admissions");
    let first = ok_post(
        &server,
        "/api/workflows/auto/stop?workspace=ws_http_fixture",
        json!({"reason":"first click"}),
    );
    check!(first, "/outcome": "stopped", "/coordinators/0/run_id": coordinator.run_id);
    eq!(first["coordinators"][0] => json!({ "run_id": coordinator.run_id, "job_id": "workspace_auto_pipeline", "outcome": "stopped", "remaining_children": [{ "run_id": child.run_id, "job_name": "task_auto_pipeline", "phase": first["coordinators"][0]["remaining_children"][0]["phase"], "child_status": first["coordinators"][0]["remaining_children"][0]["child_status"], }], "forced_runs": [], }), "the dashboard returns the stop projection CLI and MCP return: {first}");
    let stopped = fixture
        .runtime
        .read_run_state(&coordinator.run_id)
        .unwrap()
        .unwrap();
    assert!(stopped.admissions_stopped());
    let repeated = ok_post(
        &server,
        "/api/workflows/auto/stop?workspace=ws_http_fixture",
        json!({"reason":"second click"}),
    );
    check!(repeated, "/outcome": "unchanged", "/coordinators/0/outcome": "unchanged");
    eq!(repeated["coordinators"][0]["remaining_children"] => first["coordinators"][0]["remaining_children"]);
    eq!(saved_state(&fixture, &coordinator.run_id).unwrap() => stopped, "a repeated stop preserves the first stop's evidence");
    for run in [&coordinator, &child] {
        let run_detail = detail(&server, &run.run_id);
        eq!(run_detail["run"]["state"] => "running", "stopping admissions must not cancel a run: {run_detail}");
    }
});
wf_test!(
    auto_windows_refuse_oversized_input_without_creating_a_run,
    {
        let fixture = Fixture::new();
        fixture.job("workspace_auto_pipeline");
        let server = fixture.server(true);
        let uri = auto_uri();
        let before = run_ids(&fixture);
        for duration in ["2d", "86401s"] {
            let response = server.send("POST", &uri, json!({"for_duration":duration}));
            let status = status_of(&response);
            let error: Value = response.json().unwrap();
            assert_eq!(status, 400, "{error}");
            eq!(error["error"] => "for_seconds must be a whole number from 1 to 86400 (24 hours)");
            assert_no_new_runs(&fixture, &before, "refused input creates no run");
        }
        for body in [json!({}), json!({"for_duration":"0s"})] {
            assert_eq!(server.send("POST", &uri, body).status().as_u16(), 400);
            assert_no_new_runs(&fixture, &before, "minimum-window rules remain");
        }
        for duration in ["1s", "24h"] {
            let receipt = ok_post(&server, &uri, json!({"for_duration":duration}));
            assert_eq!(receipt["job_id"], "workspace_auto_pipeline");
            assert!(receipt["submitted_at"].is_string(), "{receipt}");
            assert!(receipt.get("review").is_some());
            assert_eq!(receipt["auto_fix"], false);
            eq!(run_input(&fixture, &receipt)["for_seconds"] => if duration == "1s" { 1 } else { 86_400 });
        }
    }
);
wf_test!(
    resume_requires_current_completion_authority_and_preserves_review_only_admission,
    {
        let fixture = Fixture::new();
        fixture.job("resume_fixture");
        let mut source = fixture.seed_run("jrun-resume-done", "resume_fixture", Failed);
        source.input = Some(
            json!({"completion":"done","marker":"preserved","auto_concurrency":"enforce","auto_concurrency_source":"explicit"}),
        );
        fixture.save_run(&source);
        let uri = format!("/api/job-runs/{}/resume?workspace={WS}", source.run_id);
        let agent = fixture.server(false);
        let before = listed_runs(&fixture);
        // Authority follows the persisted input, even if a caller tries to downgrade completion in the body or omits the body altogether.
        for response in [
            agent.send("POST", &uri, json!({})),
            agent.send("POST", &uri, json!({"completion":"review"})),
            send_raw(&agent, "POST", &uri, None, None),
        ] {
            let denied = error_code(response, 403, "authorization_denied");
            assert_eq!(denied["operation"], "auto_drain.complete");
            eq!(listed_runs(&fixture) => before, "denied completion resume must not create or change a run");
        }
        let operator = fixture.server(true);
        let receipt = ok_post(&operator, &uri, json!({}));
        assert_eq!(receipt["retry_source_run_id"], source.run_id);
        let resumed = find_run(&fixture, receipt["run_id"].as_str().unwrap());
        assert_eq!(resumed.input, source.input);
        eq!(resumed.retry_source_run_id.as_deref() => Some(source.run_id.as_str()));
        assert_eq!(resumed.attempt, 2);
        assert_eq!(listed_runs(&fixture).len(), before.len() + 1);
        for (id, input) in [
            ("jrun-resume-review", json!({"completion":"review"})),
            ("jrun-resume-default", json!({"marker":"no completion"})),
        ] {
            let mut review = fixture.seed_run(id, "resume_fixture", Failed);
            review.input = Some(input);
            fixture.save_run(&review);
            let before = run_ids(&fixture);
            let receipt = ok_post(
                &agent,
                &format!("/api/job-runs/{id}/resume?workspace={WS}"),
                json!({}),
            );
            let resumed = find_run(&fixture, receipt["run_id"].as_str().unwrap());
            assert_eq!(resumed.input, review.input);
            assert_eq!(resumed.retry_source_run_id.as_deref(), Some(id));
            assert_eq!(resumed.attempt, 2);
            let mut expected = before;
            assert!(expected.insert(resumed.run_id));
            assert_eq!(run_ids(&fixture), expected);
        }
        // An inherited snapshot is retained even when the current host default differs, through the same public resume endpoint.
        fixture.job("workspace_auto_pipeline");
        let mut local = fixture.seed_run(
            "jrun-resume-captured-mode",
            "workspace_auto_pipeline",
            Failed,
        );
        local.input =
            Some(json!({"auto_concurrency":"shadow","auto_concurrency_source":"configured"}));
        fixture.save_run(&local);
        let receipt = ok_post(
            &operator,
            &format!("/api/job-runs/{}/resume?workspace={WS}", local.run_id),
            json!({}),
        );
        let input = run_input(&fixture, &receipt);
        check!(input, "/auto_concurrency": "shadow", "/auto_concurrency_source": "configured");
    }
);
// An absent body reached the handler: a 4xx whose text is not a JSON parse error.
fn assert_handler_reached(response: reqwest::blocking::Response, label: &str) {
    let status = status_of(&response);
    let value: Value = response
        .json()
        .unwrap_or_else(|_| json!({"error": "non-json"}));
    let error = value["error"].as_str().unwrap_or("");
    let parse_error = error.contains("JSON")
        || error.contains("Content-Type")
        || error.contains("invalid type")
        || error.contains("unknown field");
    chk!((400..500).contains(&status) && !parse_error => "{label} must reach the handler and not parse as JSON: {status} {value}");
}
// A present body that does not parse must be a 400 carrying that parse error.
fn assert_parse_error(response: reqwest::blocking::Response, marker: &str) {
    let status = status_of(&response);
    let value: Value = response.json().expect("JSON error body");
    assert_eq!(status, 400, "{value}");
    let error = value["error"].as_str().unwrap_or("");
    chk!(error.contains(marker) => "400 must carry the parse error ({marker}): {error}");
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
fn no_body(server: &Server, method: &str, path: &str) -> reqwest::blocking::Response {
    send_raw(server, method, path, None, None)
}
fn bad_bodies(server: &Server, method: &str, path: &str, wrong_type: &str) {
    perr!(send_raw(server, method, path, Some("application/json"), Some("{")) => "Failed to parse the request body as JSON");
    perr!(send_raw( server, method, path, Some("application/x-www-form-urlencoded"), Some(wrong_type), ) => "Content-Type: application/json");
    perr!(send_raw( server, method, path, Some("application/json"), Some(wrong_type), ) => "invalid type");
}
fn cfg_global(fixture: &Fixture, content: &str) {
    std::fs::write(fixture.global.join("config.toml"), content).unwrap();
}
fn save_state(fixture: &Fixture, run_id: &str, state: &PipelineState) {
    fixture.runtime.write_run_state(run_id, state).unwrap();
}
fn saved_state(fixture: &Fixture, run_id: &str) -> Option<PipelineState> {
    fixture.runtime.read_run_state(run_id).unwrap()
}
fn show_run(fixture: &Fixture, run_id: &str) -> JobRun {
    fixture.runtime.show_job_run(run_id).unwrap()
}
fn fixture_task(
    fixture: &Fixture,
    title: &str,
    status: TaskStatus,
    priority: TaskPriority,
    file: &str,
    crew: Option<&str>,
) -> Task {
    std::fs::write(fixture.runtime.paths().repo_root.join(file), "fixture\n").unwrap();
    fixture
        .runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: format!("Readiness fixture: {title}"),
            acceptance_criteria: vec!["The fixture is observable.".into()],
            plan: "Fixture plan.".into(),
            context_files: vec![format!("file:{file}")],
            priority,
            complexity: TaskComplexity::Medium,
            status: Some(status),
            crew: crew.map(str::to_string),
            ..Default::default()
        })
        .unwrap()
}
fn detail(server: &Server, run_id: &str) -> Value {
    ok_get(server, &format!("/api/runs/{run_id}?workspace={WS}"))
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
wf_test!(
    malformed_bodies_do_not_select_defaults_and_auto_requires_workspace,
    {
        let fixture = Fixture::new();
        // The server opens its own runtime; discovery here tests HTTP defaults.
        std::fs::write(
            fixture.path("global/config.toml"),
            "[workflow.resource_throttle]\nenabled = false\n",
        )
        .unwrap();
        fixture.job("task_auto_pipeline");
        let cancel_target = fixture.seed_run("jrun-cancel-target", "task_auto_pipeline", Running);
        let server = fixture.server(true);
        let ws = format!("?workspace={WS}");
        let before = run_ids(&fixture);
        // Ship: unknown key, form body (curl's default content type), wrong type, bad JSON.
        let ship = ship_uri();
        perr!(server.send("POST", &ship, json!({"task_id":"X"})) => "unknown field `task_id`");
        perr!(send_raw( &server, "POST", &ship, Some("application/x-www-form-urlencoded"), Some(r#"{"task_ids":["ORB-1"]}"#), ) => "Content-Type: application/json");
        perr!(server.send("POST", &ship, json!({"task_ids":"ORB-1"})) => "invalid type");
        perr!(send_raw(&server, "POST", &ship, Some("application/json"), Some("{")) => "Failed to parse the request body as JSON");
        let cancel = format!("/api/runs/{}/cancel{ws}", cancel_target.run_id);
        let (auto_p, stop_p) = (auto_uri(), stop_uri());
        let cases: [(&str, &str, &str); 6] = [
            ("POST", &auto_p, r#"{"for_duration":1}"#),
            ("POST", &stop_p, r#"{"reason":1}"#),
            ("POST", &cancel, r#"{"force":"true"}"#),
            (
                "POST",
                "/api/tasks/missing/approve?workspace=ws_http_fixture",
                r#"{"note":1}"#,
            ),
            (
                "POST",
                "/api/job-runs/missing/resume?workspace=ws_http_fixture",
                r#"{"claim_token":1}"#,
            ),
            (
                "PATCH",
                "/api/frictions/missing?workspace=ws_http_fixture",
                r#"{"status":1}"#,
            ),
        ];
        for (method, path, wrong_type) in &cases {
            bad_bodies(&server, method, path, wrong_type);
        }
        assert_no_new_runs(&fixture, &before, "a rejected body must not create a run");
        eq!(run_state(&fixture, &cancel_target.run_id) => Running, "a string force flag must not become a graceful cancel");
        // Absent and empty bodies still take the handler default.
        for body in [None, Some("")] {
            let response = send_raw(
                &server,
                "POST",
                &format!("/api/workflows/auto{ws}"),
                body.map(|_| "application/json"),
                body,
            );
            let status = status_of(&response);
            let value: Value = response.json().unwrap();
            assert_eq!(status, 400, "{value}");
            assert_eq!(value["error"], "for_duration must not be empty", "{value}");
        }
        let stopped = json_ok(no_body(&server, "POST", &stop_uri()));
        assert_eq!(stopped["outcome"], "idle");
        reached!(no_body(&server, "POST", &format!("/api/tasks/missing/approve{ws}")) => "empty approve");
        reached!(no_body( &server, "POST", &format!("/api/job-runs/missing/resume{ws}"), ) => "empty resume");
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
        eq!(patch_body["error"] => "request body must include `status`, `tags`, or `title`");
        let cancelled = json_ok(no_body(
            &server,
            "POST",
            &format!("/api/runs/{}/cancel{ws}", cancel_target.run_id),
        ));
        chk!(cancelled["outcome"].as_str().is_some() => "empty cancel body is the graceful default: {cancelled}");
        ne!(run_state(&fixture, &cancel_target.run_id) => Running, "an empty cancel body still cancels gracefully: {cancelled}");
        assert_no_new_runs(&fixture, &before, "defaults must not add a run here");
        // Missing ?workspace refuses before any drain starts, even with a server default.
        ecode!(server.send("POST", "/api/workflows/auto", json!({"for_duration":"1m"})) => 400, "workspace_required");
        ecode!(server.send("POST", "/api/workflows/auto/stop", json!({})) => 400, "workspace_required");
        assert_no_new_runs(&fixture, &before, "no new runs");
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
            chk!(matches!(submitted["state"].as_str(), Some("submitted" | "queued")) => "empty ship body must submit discovery mode: {submitted}");
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
            chk!(input.get("task_ids").is_none() => "an empty ship body is backlog discovery, not a smuggled task: {input}");
        }
    }
);
fn run_state(fixture: &Fixture, run_id: &str) -> JobRunState {
    listed_runs(fixture)
        .into_iter()
        .find(|run| run.run_id == run_id)
        .map(|run| run.state)
        .unwrap_or_else(|| panic!("missing run {run_id}"))
}
// The dashboard's queued-window Cancel promises the window has not started. A queued auto window starts under the same run id when the live one ends, so a Cancel sent from a card rendered before that start must be refused, leaving the started window, its owner and its audit trail untouched; a still-queued window cancels, and the general cancel still stops a running window [ORB-20120].
wf_test!(
    queued_only_cancel_refuses_a_window_that_started_after_the_card_rendered,
    {
        use orbit_store::compose::workspace_job_run_store;
        use orbit_store::contracts::AuditEventFilter;
        use orbit_types::workflow::JobRunStartOutcome;
        let fixture = Fixture::new();
        let server = fixture.server(true);
        let jobs = workspace_job_run_store(
            fixture.runtime.sqlite_store().unwrap(),
            fixture.runtime.workspace_id().unwrap(),
        );
        let cancel_path = |run_id: &str| format!("/api/runs/{run_id}/cancel?workspace={WS}");
        let cancel_audits = |run_id: &str| {
            fixture
                .runtime
                .list_audit_events_filtered(&AuditEventFilter {
                    job_run_id: Some(run_id.to_string()),
                    limit: 50,
                    ..AuditEventFilter::default()
                })
                .unwrap()
                .into_iter()
                .filter_map(|event| event.tool_name)
                .filter(|tool| tool.starts_with("pipeline.run.cancel"))
                .collect::<Vec<_>>()
        };
        // Rendered queued, then started by the worker's start arbiter before the Cancel arrived.
        let mut owner = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn stand-in window owner");
        let started = fixture.seed_run(
            "jrun-queued-then-started",
            "workspace_auto_pipeline",
            Pending,
        );
        eq!(jobs.mark_job_run_running(&started.run_id, Utc::now(), owner.id()) .unwrap() => JobRunStartOutcome::Started);
        let before = show_run(&fixture, &started.run_id);
        let refused = server.send(
            "POST",
            &cancel_path(&started.run_id),
            json!({"reason": null, "force": false, "queued_only": true}),
        );
        let status = status_of(&refused);
        let body: Value = refused.json().unwrap();
        eq!(status => 409, "a started window refuses a queued-only cancel: {body}");
        chk!(body["error"] .as_str() .is_some_and(|error| error.contains("no longer queued")) => "the refusal says why: {body}");
        let after = show_run(&fixture, &started.run_id);
        assert_eq!(after.state, Running, "the started window keeps running");
        eq!((after.pid, after.started_at) => (before.pid, before.started_at));
        chk!(owner.try_wait().unwrap().is_none() => "the started window's owner was not stopped");
        eq!(cancel_audits(&started.run_id) => Vec::<String>::new(), "a refused queued-only cancel records no cancellation");
        chk!(saved_state(&fixture, &started.run_id).and_then(|state| state.task_cancellation_policy) .is_none() => "a refused queued-only cancel writes no task cancellation policy");
        let combined = server.send(
            "POST",
            &cancel_path(&started.run_id),
            json!({"queued_only": true, "force": true}),
        );
        assert_eq!(combined.status().as_u16(), 400, "queued_only refuses force");
        assert_eq!(run_state(&fixture, &started.run_id), Running);
        // A window still queued cancels at once.
        let queued = fixture.seed_run("jrun-still-queued", "workspace_auto_pipeline", Pending);
        let cancelled = ok_post(
            &server,
            &cancel_path(&queued.run_id),
            json!({"reason": null, "force": false, "queued_only": true}),
        );
        check!(cancelled, "/outcome": "cancelled", "/previous_state": "pending");
        assert_eq!(run_state(&fixture, &queued.run_id), Cancelled);
        chk!(cancel_audits(&queued.run_id) .iter() .any(|tool| tool == "pipeline.run.cancel.completed") => "the queued cancel keeps its audit trail");
        let again = ok_post(
            &server,
            &cancel_path(&queued.run_id),
            json!({"queued_only": true}),
        );
        assert_eq!(again["outcome"], "already_terminal", "{again}");
        // The general cancel still stops a running window.
        let general = ok_post(
            &server,
            &cancel_path(&started.run_id),
            json!({"reason": null, "force": false}),
        );
        assert_eq!(general["previous_state"], "running", "{general}");
        assert_eq!(run_state(&fixture, &started.run_id), Cancelled);
        let _ = owner.kill();
        let _ = owner.wait();
    }
);
wf_test!(
    replay_returns_a_durable_receipt_while_the_job_runs_in_a_detached_worker,
    {
        use std::time::{Duration, Instant};
        let fixture = Fixture::new();
        // Longer than the HTTP client's five-second timeout: foreground execution cannot return a receipt for this job in time.
        fixture.sleep_job("replay_fixture", 6);
        let mut source = fixture.seed_run("jrun-replay-source", "replay_fixture", Success);
        source.input = Some(json!({
            "marker": "preserved", "completion": "done",
            "trusted_host_admission": {"authorized_by": "old operator"},
            "review": {"contract_version": 0},
        }));
        fixture.save_run(&source);
        let source_before = show_run(&fixture, &source.run_id);
        let mut source_state =
            PipelineState::new(source.run_id.clone(), source.job_id.clone(), json!({}));
        source_state.step_states.insert(0, Success);
        source_state.next_step_index = 1;
        save_state(&fixture, &source.run_id, &source_state);
        let server = fixture.replay_server();
        let receipt = ok_post(
            &server,
            &format!("/api/runs/{}/replay?workspace={WS}", source.run_id),
            json!({}),
        );
        assert_eq!(receipt["state"], "submitted");
        let run_id = receipt["run_id"].as_str().unwrap();
        assert_ne!(run_id, source.run_id);
        let replay = show_run(&fixture, run_id);
        assert!(matches!(replay.state, Pending | Running), "{replay:?}");
        eq!(replay.retry_source_run_id.as_deref() => Some(source.run_id.as_str()));
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
            let run = show_run(&fixture, run_id);
            if run.state == Running {
                ne!(run.pid => Some(server.pid()), "execution belongs to the detached worker");
                break;
            }
            chk!(!run.state.is_terminal() && Instant::now() < deadline => "worker never ran: {run:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
        eq!(wait_terminal(&fixture, run_id, 15).state => Success, "worker did not finish");
        let replay_state = saved_state(&fixture, run_id).unwrap();
        eq!(replay_state.trigger => Some(orbit_types::workflow::JobRunTrigger::dashboard()));
        chk!(replay_state.pipeline["nap"]["slept_seconds"] .as_f64() .unwrap() >= 6.0 => "replay must execute even a checkpointed source step");
        eq!(show_run(&fixture, &source.run_id) => source_before);
        eq!(saved_state(&fixture, &source.run_id).unwrap() => source_state);
    }
);
wf_test!(
    replay_requires_workspace_operator_and_the_workspace_claim_token,
    {
        let fixture = Fixture::new();
        fixture.job("replay_fixture");
        let mut source = fixture.seed_run("jrun-replay-source", "replay_fixture", Success);
        source.input = Some(json!({"completion": "done"}));
        fixture.save_run(&source);
        let operator = fixture.replay_server();
        let agent = fixture.server(false);
        let path = format!("/api/runs/{}/replay?workspace={WS}", source.run_id);
        let before = run_ids(&fixture);
        ecode!(operator.send( "POST", &format!("/api/runs/{}/replay", source.run_id), json!({}), ) => 400, "workspace_required");
        let denial = ecode!(agent.send("POST", &path, json!({})) => 403, "authorization_denied");
        assert_eq!(denial["operation"], "job.run");
        perr!(operator.send("POST", &path, json!({"claim_token": 1})) => "invalid type");
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
            ecode!(operator.send("POST", &path, body) => 409, "workspace_claim_held");
        }
        assert_no_new_runs(&fixture, &before, "refusals cannot persist replay runs");
        let receipt = ok_post(&operator, &path, json!({"claim_token": grant.claim_token}));
        let run_id = receipt["run_id"].as_str().unwrap();
        assert_eq!(wait_terminal(&fixture, run_id, 10).state, Success);
    }
);
// A failed pull window retains its typed cause and pass health in the HTTP detail after terminalization, where the dashboard can show the remedy.
wf_test!(failed_pull_protocol_is_visible_after_terminalization, {
    let fixture = Fixture::new();
    let mut run = fixture.seed_run("jrun-skew", "workspace_pull_pipeline", Failed);
    let now = Utc::now();
    run.steps.push(orbit_types::workflow::JobRunStep {
        lock_wait_ms: None,
        step_index: 0,
        target_type: orbit_types::workflow::JobTargetType::Job,
        target_id: run.job_id.clone(),
        started_at: Some(now),
        finished_at: Some(now),
        duration_ms: None,
        exit_code: None,
        agent_response_json: None,
        state: Failed,
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
    state.pull_drain.drain_last_pass = Some(orbit_types::workflow::DrainAdmissionPass {
        task_pilot_warning: None,
        capacity: None,
        recorded_at: now,
        queued: 0,
        deferred: vec![],
        deferred_total: 0,
        excluded: vec![],
        excluded_total: 0,
        waiting_recorded_at: None,
        waiting_by_reason: Default::default(),
        consecutive_idle_passes: 0,
        resource_throttle: None,
        last_pass_error_code: Some("protocol_skew".into()),
        last_pass_error: run.steps[0].error_message.clone(),
        consecutive_pass_failures: 1,
        admission_refusal: None,
        degraded: true,
        admission_forecast: None,
    });
    save_state(&fixture, &run.run_id, &state);
    let server = fixture.server(false);
    let detail = detail(&server, "jrun-skew");
    check!(detail, "/run/state": "failed", "/run/error_code": "protocol_skew");
    eq!(detail["run"]["drain_last_pass"]["last_pass_error_code"] => "protocol_skew");
    assert_eq!(detail["run"]["drain_last_pass"]["degraded"], true);
});
wf_test!(
    auto_start_approve_proposed_is_opt_in_boolean_and_operator_gated,
    {
        let fixture = Fixture::new();
        fixture.job("workspace_auto_pipeline");
        cfg_global(&fixture, "[machine.auto_concurrency]\nmode = \"shadow\"\n");
        let uri = auto_uri();
        let agent = fixture.server(false);
        let operator = fixture.server(true);
        let before = run_ids(&fixture);
        // Each authority is independently guarded before durable effects.
        for (field, operation) in [
            ("approve_proposed", "auto_drain.approve_proposed"),
            ("auto_fix", "auto_drain.auto_fix"),
        ] {
            let denied = ecode!(agent.send("POST", &uri, json!({"for_duration":"1m",field:true})) => 403, "authorization_denied");
            assert_eq!(denied["operation"], operation);
            assert_no_new_runs(&fixture, &before, "a denied start persists no run");
            for wrong in [json!("true"), json!(1), json!(null)] {
                perr!(operator.send("POST", &uri, json!({"for_duration":"1m",field:wrong})) => "invalid type");
            }
            assert_no_new_runs(&fixture, &before, "a rejected body persists no run");
        }
        for wrong in [
            json!(false),
            json!(true),
            json!(1),
            json!(null),
            json!("bogus"),
        ] {
            let response = operator.send(
                "POST",
                &uri,
                json!({"for_duration":"1m","auto_concurrency":wrong}),
            );
            chk!(!response.status().is_success() => "malformed mode must be refused");
        }
        assert!(
            !operator
                .send(
                    "POST",
                    &format!("/api/workflows/auto/stop?workspace={WS}"),
                    json!({"auto_concurrency":"off"})
                )
                .status()
                .is_success()
        );
        assert!(
            !operator
                .send(
                    "POST",
                    &uri,
                    json!({"for_duration":"1m","autoConcurrency":"enforce"})
                )
                .status()
                .is_success()
        );
        let denied = ecode!(agent.send( "POST", &uri, json!({"for_duration":"1m","auto_concurrency":"off"}), ) => 403, "authorization_denied");
        assert_eq!(denied["operation"], "auto_drain.auto_concurrency");
        assert_no_new_runs(&fixture, &before, "refused modes leave no durable effects");
        let config = std::fs::read(fixture.global.join("config.toml")).ok();
        for mode in ["off", "shadow", "enforce"] {
            let started = ok_post(
                &operator,
                &uri,
                json!({"for_duration":"1m","auto_concurrency":mode}),
            );
            check!(started, "/auto_concurrency/mode": mode, "/auto_concurrency/source": "explicit");
            let shown = detail(&operator, started["run_id"].as_str().unwrap());
            assert_eq!(shown["run"]["auto_concurrency"]["mode"], mode);
            let input = run_input(&fixture, &started);
            check!(input, "/auto_concurrency": mode, "/auto_concurrency_source": "explicit");
        }
        eq!(std::fs::read(fixture.global.join("config.toml")).ok() => config);
        // Omitted and explicit false start the same window as before, from an unauthorized session too.
        for body in [
            json!({"for_duration":"1m"}),
            json!({"for_duration":"1m","approve_proposed":false,"auto_fix":false}),
        ] {
            let started = ok_post(&agent, &uri, body);
            check!(started, "/approve_proposed": false, "/auto_fix": false);
            check!(started, "/auto_concurrency/mode": "shadow", "/auto_concurrency/source": "configured", "/completion": "review");
            let run = run_input(&fixture, &started);
            assert_ne!(run["approve_proposed"], true, "{run}");
            assert_ne!(run["auto_fix"], true, "{run}");
        }
        let started = ok_post(
            &operator,
            &uri,
            json!({"for_duration":"1m","approve_proposed":true}),
        );
        assert_eq!(started["approve_proposed"], true, "{started}");
        eq!(started["completion"] => "review", "approving proposed tasks does not change completion: {started}");
        let run = run_input(&fixture, &started);
        assert_eq!(run["approve_proposed"], true, "{run}");
        for (complete, approve) in [(false, false), (true, true)] {
            let before = run_ids(&fixture);
            let receipt = ok_post(
                &operator,
                &uri,
                json!({"for_duration":"1m","auto_fix":true,"complete":complete,"approve_proposed":approve}),
            );
            check!(receipt, "/auto_fix": true, "/approve_proposed": approve);
            eq!(receipt["completion"] => if complete { "done" } else { "review" });
            let input = run_input(&fixture, &receipt);
            eq!(input["auto_fix"] => true, "the existing coordinator must inherit repair authority");
            eq!(input["completion"].as_str().unwrap_or("review") => receipt["completion"].as_str().unwrap());
            eq!(input["approve_proposed"].as_bool().unwrap_or(false) => approve);
            assert_eq!(run_ids(&fixture).len(), before.len() + 1);
        }
        // A long-lived dashboard must capture the host default at submission, rather than keep inheriting the runtime's startup snapshot.
        let changed = json_ok(operator.send(
            "PUT",
            &format!("/api/config/keys/machine.auto_concurrency.mode?workspace={WS}"),
            json!({"scope":"global","value":"enforce"}),
        ));
        assert_eq!(changed["new_value"], "enforce");
        let readiness = ok_get(&operator, &readiness_uri());
        eq!(readiness["auto_concurrency"]["configured_mode"] => "enforce", "the inherited choice must show the current admitted host setting");
        let inherited = ok_post(&operator, &uri, json!({"for_duration":"1m"}));
        check!(inherited, "/auto_concurrency/mode": "enforce", "/auto_concurrency/source": "configured");
        eq!(run_input(&fixture, &inherited)["auto_concurrency"] => "enforce");
        let earlier = detail(&operator, started["run_id"].as_str().unwrap());
        check!(earlier, "/run/auto_concurrency/mode": "shadow", "/run/auto_concurrency/source": "configured");
    }
);
wf_test!(
    auto_readiness_reports_authorization_and_replica_for_the_start_form,
    {
        let fixture = Fixture::new();
        let uri = readiness_uri();
        let agent = ok_get(&fixture.server(false), &uri);
        check!(agent, "/controls_authorized": false, "/replica": false);
        chk!(agent .get("validation_env_warning") .is_some_and(Value::is_null) => "{agent}");
        let operator = ok_get(&fixture.server(true), &uri);
        check!(operator, "/controls_authorized": true, "/replica": false);
        assert_eq!(operator["approvals"]["enabled"], false, "{operator}");
        let replica = Fixture::replica_of("hm_fixture_remote");
        let server = replica.server(true);
        let readiness = ok_get(&server, &uri);
        assert_eq!(readiness["replica"], true);
        let before = listed_runs(&replica);
        let response = server.send(
            "POST",
            &auto_uri(),
            json!({"for_duration":"1m","auto_fix":true}),
        );
        assert!(!response.status().is_success());
        let refused: Value = response.json().unwrap();
        chk!(refused["error"] .as_str() .is_some_and(|message| !message.is_empty()) => "owner admission must explain its refusal: {refused}");
        eq!(listed_runs(&replica) => before, "replica refusal must precede run creation");
    }
);
// Readiness keeps the shell PATH warning across warm reads and server restarts.
#[cfg(unix)]
wf_test!(
    auto_readiness_reuses_the_validation_warning_without_profiles,
    {
        let fixture = Fixture::new();
        let home = fixture.global.parent().unwrap();
        let marker = home.join("login-probes");
        for name in [".bash_profile", ".profile", ".zprofile"] {
            std::fs::write(home.join(name),
                "printf 'probe\\n' >> \"$HOME/login-probes\"\nexport PATH=\"$HOME/login-tools:/usr/bin:/bin\"\n"
            ).unwrap();
        }
        std::fs::create_dir_all(home.join(".config/fish")).unwrap();
        std::fs::write(home.join(".config/fish/config.fish"),
            "printf 'probe\\n' >> \"$HOME/login-probes\"\nset -gx PATH \"$HOME/login-tools\" /usr/bin /bin\n"
        ).unwrap();
        let configure = |mode: &str| {
            std::fs::write(fixture.work.join("config.toml"), format!(
                "[workflow]\nrequired_validation_commands = [\"make check\"]\n[workflow.validation_env]\npath = [\"/usr/bin\", \"/bin\"]\npath_mode = \"{mode}\"\n"
            )).unwrap();
        };
        configure("replace");
        let uri = readiness_uri();
        let first = {
            let server = fixture.server(false);
            let first = ok_get(&server, &uri);
            chk!(first["validation_env_warning"] .as_str() .unwrap() .contains(home.join("login-tools").to_str().unwrap()) => "{first}");
            let warm = ok_get(&server, &uri);
            eq!(warm["validation_env_warning"] => first["validation_env_warning"]);
            assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
            first
        };
        // Cross-process reuse also prevents a cold probe after a web restart.
        let restarted = ok_get(&fixture.server(false), &uri);
        eq!(restarted["validation_env_warning"] => first["validation_env_warning"]);
        assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
        configure("prepend");
        let kept = ok_get(&fixture.server(false), &uri);
        assert!(kept["validation_env_warning"].is_null(), "{kept}");
        assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 2);
    }
);
// A task ready for admission but for what the test arranges: criteria, a plan, an assessed complexity and one existing file it locks.
fn readiness_task(
    fixture: &Fixture,
    title: &str,
    status: TaskStatus,
    priority: TaskPriority,
    file: &str,
) -> Task {
    fixture_task(fixture, title, status, priority, file, None)
}
// Return `task` to the backlog under a red-base hold, the status change and history note a delivery that met a red base leaves.
fn hold_for_red_base(fixture: &Fixture, task: &str, hold: &BaselineRedHold) {
    let runtime = &fixture.runtime;
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(&runtime.global_root()),
    )
    .unwrap();
    orbit_store::compose::workspace_coordinated_backends(
        registry,
        runtime.workspace_id().unwrap(),
        runtime.sqlite_store().unwrap(),
    )
    .unwrap()
    .task
    .history
    .update_task_history(
        task,
        orbit_store::contracts::TaskHistoryUpdateParams {
            actor: "system".into(),
            status: Some(Backlog),
            status_event: Some(BASELINE_RED_HOLD_EVENT.into()),
            status_note: Some(hold.text("required validation is red on the base")),
            ..Default::default()
        },
    )
    .unwrap();
}
// `?task=` explains exactly the named tasks, so a task detail gets its own answer: waiting on a lock, waiting on capacity, held for a red base, eligible, and a proposed task the bounded backlog listing never reaches.
wf_test!(auto_readiness_explains_the_tasks_it_names, {
    let fixture = Fixture::new();
    let holder = readiness_task(&fixture, "lock holder", InProgress, Medium, "held.rs");
    let locked = readiness_task(&fixture, "waits on the lock", Backlog, Medium, "held.rs");
    let eligible = readiness_task(&fixture, "takes the one slot", Backlog, High, "eligible.rs");
    let saturated = readiness_task(&fixture, "finds no slot", Backlog, Low, "saturated.rs");
    // A delivery that met a red base returns its task to the backlog.
    let red = readiness_task(
        &fixture,
        "held for a red base",
        InProgress,
        Medium,
        "red.rs",
    );
    let hold = BaselineRedHold {
        base_ref: "origin/agent-main".into(),
        base_sha: "1a2b3c4d5e6f".into(),
        command: "make ci-test-affected".into(),
        run_id: "jrun-red".into(),
        selection: None,
        failures: Default::default(),
    };
    hold_for_red_base(&fixture, &red.id, &hold);
    let proposed = readiness_task(&fixture, "awaits approval", Proposed, Medium, "proposed.rs");
    let server = fixture.server(false);
    let named = [&locked, &eligible, &saturated, &red, &proposed]
        .map(|task| task.id.as_str())
        .join(",");
    let payload = ok_get(
        &server,
        &format!("/api/workflows/auto/readiness?workspace={WS}&concurrency=1&task={named}"),
    );
    let entries = payload["tasks"].as_array().unwrap();
    assert_eq!(entries.len(), 5, "exactly the named tasks: {payload:#}");
    let waiting = readiness_entry(&payload, &locked);
    assert_eq!(waiting["reason"], "context_lock_conflict", "{waiting}");
    eq!(waiting["conflicts"][0]["locking_task_id"] => holder.id.as_str());
    assert_eq!(waiting["conflicts"][0]["requested_file"], "file:held.rs");
    let ready = readiness_entry(&payload, &eligible);
    assert_eq!(ready["eligible"], true, "{ready}");
    let full = readiness_entry(&payload, &saturated);
    check!(full, "/eligible": false, "/reason": "capacity_saturated");
    let held = readiness_entry(&payload, &red);
    assert_eq!(held["reason"], "baseline_red_hold", "{held}");
    let detail = held["detail"].as_str().unwrap();
    chk!(detail.contains(&hold.command) && detail.contains(&hold.base_sha) => "the hold names its command and red base: {held}");
    let approval = readiness_entry(&payload, &proposed);
    check!(approval, "/reason": "not_backlog", "/status": "proposed");
    let listing = ok_get(
        &server,
        &format!("/api/workflows/auto/readiness?workspace={WS}&concurrency=1"),
    );
    chk!(listing["tasks"] .as_array() .unwrap() .iter() .all(|entry| entry["task_id"] != proposed.id.as_str()) => "the bounded listing never reaches a proposed task: {listing:#}");
    let combined = ok_get(
        &server,
        &format!(
            "/api/workflows/auto/readiness?workspace={WS}&concurrency=1&also_task={}",
            proposed.id
        ),
    );
    eq!(combined["tasks"] => listing["tasks"], "the bounded list keeps its contents");
    eq!(combined["detail_tasks"].as_array().unwrap().len() => 1, "one selected task is added beside the bounded list");
    check!(combined, "/detail_tasks/0/task_id": proposed.id.as_str(), "/detail_tasks/0/reason": "not_backlog");
    let missing_detail = ok_get(
        &server,
        &format!("/api/workflows/auto/readiness?workspace={WS}&concurrency=1&also_task=HF-404404"),
    );
    eq!(missing_detail["tasks"] => listing["tasks"], "a missing optional detail leaves the bounded snapshot intact");
    check!(missing_detail, "/detail_tasks": json!([]), "/detail_unavailable/task_id": "HF-404404");
    eq!(missing_detail["detail_unavailable"]["reason"] => "task_not_found");
    for bad in ["HF-404404", "not%20an%20id"] {
        let response = server.get(&format!(
            "/api/workflows/auto/readiness?workspace={WS}&task={bad}"
        ));
        assert_eq!(response.status().as_u16(), 400, "{bad}");
    }
});
// Readiness applies the live drain's own `--allow-crew` set: a task it excludes reads `crew_not_allowed` with its crew and the permitted set, and the tasks the drain would admit stay eligible.
wf_test!(auto_readiness_applies_the_live_drains_crew_restriction, {
    let fixture = Fixture::new();
    cfg_global(
        &fixture,
        "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"sol-model\"\n\n[crews.opus]\nprovider = \"claude\"\nmodel = \"opus-model\"\n",
    );
    let excluded = fixture_task(
        &fixture,
        "runs as opus",
        Backlog,
        Medium,
        "opus.rs",
        Some("opus"),
    );
    let permitted = fixture_task(
        &fixture,
        "runs as sol",
        Backlog,
        Medium,
        "sol.rs",
        Some("sol"),
    );
    let defaulted = fixture_task(
        &fixture,
        "runs as the default crew",
        Backlog,
        Medium,
        "default.rs",
        None,
    );
    let server = fixture.server(false);
    let uri = readiness_uri();
    let selected_uri = format!(
        "{uri}&task={},{}&concurrency=7&limit=500&allow_crew=sol",
        excluded.id, permitted.id
    );
    let selected = ok_get(&server, &selected_uri);
    assert_eq!(selected["tasks"].as_array().unwrap().len(), 2, "{selected}");
    check!(selected, "/capacity/max_active_leaf_runs": 7, "/capacity/allowed_crews": json!(["sol"]));
    assert_eq!(
        readiness_entry(&selected, &excluded)["reason"],
        "crew_not_allowed"
    );
    assert_eq!(readiness_entry(&selected, &permitted)["eligible"], true);
    let both_crews = ok_get(
        &server,
        &selected_uri.replace("allow_crew=sol", "allow_crew=sol,opus"),
    );
    assert_eq!(readiness_entry(&both_crews, &excluded)["eligible"], true);
    eq!(both_crews["capacity"]["allowed_crews"] .as_array() .unwrap() .len() => 2);
    eq!(server .get(&selected_uri.replace("limit=500", "limit=1")) .status() => 400, "explicit selections larger than the limit are refused");
    for limit in [0, 501] {
        assert_eq!(server.get(&format!("{uri}&limit={limit}")).status(), 400);
    }
    let idle = ok_get(&server, &uri);
    assert_eq!(idle["capacity"]["allowed_crews"], Value::Null, "{idle:#}");
    for task in [&excluded, &permitted, &defaulted] {
        eq!(readiness_entry(&idle, task)["eligible"] => true, "no drain restricts crews: {idle:#}");
    }
    let mut drain = fixture.seed_run("jrun-auto", "workspace_auto_pipeline", Running);
    drain.input = Some(json!({"max_active_leaf_runs": 4, "allowed_crews": ["sol"]}));
    fixture.save_run(&drain);
    let live = ok_get(&server, &uri);
    assert_eq!(live["capacity"]["drain_run_id"], "jrun-auto", "{live:#}");
    let refused = readiness_entry(&live, &excluded);
    check!(refused, "/eligible": false, "/reason": "crew_not_allowed", "/crew": "opus", "/allowed_crews": json!(["sol"]));
    assert_eq!(live["capacity"]["allowed_crews"], json!(["sol"]));
    for task in [&permitted, &defaulted] {
        let admitted = readiness_entry(&live, task);
        eq!(admitted["eligible"] => true, "the drain admits it: {admitted}");
    }
    // [ORB-16439] The only live drain is a restricted pull drain on a
    // replica: readiness shows its allowed crews and excludes tasks that require other crews.
    drain.state = Success;
    drain.finished_at = Some(Utc::now());
    fixture.save_run(&drain);
    let mut pull_drain = fixture.seed_run("jrun-pull", "workspace_pull_pipeline", Running);
    pull_drain.input = Some(json!({"max_active_leaf_runs": 4, "allowed_crews": ["sol"]}));
    fixture.save_run(&pull_drain);
    let pull_live = ok_get(&server, &uri);
    check!(pull_live, "/capacity/drain_run_id": Value::Null, "/capacity/pull_drain_run_id": "jrun-pull", "/capacity/allowed_crews": json!(["sol"]));
    let pull_refused = readiness_entry(&pull_live, &excluded);
    check!(pull_refused, "/eligible": false, "/reason": "crew_not_allowed", "/crew": "opus", "/allowed_crews": json!(["sol"]));
    for task in [&permitted, &defaulted] {
        let admitted = readiness_entry(&pull_live, task);
        eq!(admitted["eligible"] => true, "the pull drain admits it: {admitted}");
    }
});
// Not `ORBIT_`-prefixed: those names are never reported unset.
const UNSET_PASS: &str = "FIXTURE_HTTP_NOTICE_TOKEN";
// A fixture whose operator passes a variable the server does not hold, configured by `extra`, with both delivery workflows deployed.
fn notice_fixture(extra: &str) -> Fixture {
    let fixture = Fixture::new();
    fixture.job("workspace_auto_pipeline");
    fixture.job("task_auto_pipeline");
    std::fs::write(
        fixture.global.join("config.toml"),
        format!(
            "[execution.env]\npass = [\"HOME\", \"PATH\", \"CLAUDE_CODE_OAUTH_TOKEN\", \"{UNSET_PASS}\"]\n\n{extra}"
        ),
    )
    .unwrap();
    fixture
}
// The notice fixtures pass `CLAUDE_CODE_OAUTH_TOKEN` and hold it, so the Claude credential warning a macOS host without one gets [ORB-16663] stays out of their receipts.
fn held_worker_credential() -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::scoped([("CLAUDE_CODE_OAUTH_TOKEN", Some("fixture-worker-token"))])
}
fn backlog_task(fixture: &Fixture, title: &str, tags: &[&str]) -> Task {
    fixture
        .runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: "submission notice fixture".into(),
            status: Some(Backlog),
            tags: tags.iter().map(ToString::to_string).collect(),
            ..Default::default()
        })
        .unwrap()
}
fn warning_lines(receipt: &Value) -> Vec<String> {
    receipt["warning"]
        .as_str()
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}
// A drain started from the dashboard reports what `orbit run auto` and `orbit.workflow.auto` report: the host throttle, backlog for another OS, unset `execution.env.pass` names and the required-validation note. An explicit ship reports the throttle and the unset names.
wf_test!(auto_start_and_ship_report_the_submission_notices, {
    let _credential = held_worker_credential();
    let fixture = notice_fixture("");
    // A live pull drain recorded a throttle on its latest pass; every admission in this workspace reports it.
    let pull = fixture.seed_run("jrun-pull-throttled", "workspace_pull_pipeline", Running);
    let mut state = PipelineState::new(pull.run_id.clone(), pull.job_id.clone(), json!({}));
    state.pull_drain.drain_last_pass = Some(
        serde_json::from_value(json!({
            "recorded_at": Utc::now(),
            "queued": 0,
            "resource_throttle": {"resources": [{
                "resource": "memory", "percent": 95.0, "high_percent": 90,
                "resume_percent": 85, "since": Utc::now(),
            }]},
        }))
        .unwrap(),
    );
    save_state(&fixture, &pull.run_id, &state);
    let other_os = if cfg!(target_os = "macos") {
        "os:linux"
    } else {
        "os:macos"
    };
    let elsewhere = backlog_task(&fixture, "another OS", &[other_os]);
    let explicit = backlog_task(&fixture, "explicit", &[]);
    let server = fixture.server(false);
    let started = ok_post(&server, &auto_uri(), json!({"for_duration":"1m"}));
    let warnings = warning_lines(&started);
    assert_eq!(warnings.len(), 3, "{started}");
    chk!(warnings[0].starts_with("Admissions throttled: ") => "{started}");
    chk!(warnings[1].contains(&elsewhere.id) && warnings[1].contains("another OS") => "{started}");
    assert!(warnings[2].contains(UNSET_PASS), "{started}");
    chk!(started["resource_throttle"]["resources"] .as_array() .is_some_and(|resources| !resources.is_empty()) => "{started}");
    chk!(started["note"] .as_str() .is_some_and(|note| note.contains("workflow.required_validation_commands")) => "{started}");
    assert!(started.get("validation_env_warning").is_none(), "{started}");
    let shipped = ok_post(
        &server,
        &ship_uri(),
        json!({"task_ids":[explicit.id],"mode":"local"}),
    );
    let warnings = warning_lines(&shipped);
    assert_eq!(warnings.len(), 2, "{shipped}");
    chk!(warnings[0].starts_with("Admissions throttled: ") => "an explicit ship proceeds under the throttle and says so: {shipped}");
    assert!(warnings[1].contains(UNSET_PASS), "{shipped}");
    chk!(shipped.get("resource_throttle").is_none() && shipped.get("note").is_none() => "{shipped}");
});
// Required validation that cannot use the login shell is reported by a dashboard drain start and ship, as by the CLI and MCP.
wf_test!(auto_start_and_ship_report_the_validation_environment, {
    let _credential = held_worker_credential();
    let fixture = notice_fixture(
        "[workflow]\nrequired_validation_commands = [\"make ci-fast\"]\n\n\
             [workflow.validation_env]\nshell_probe = \"off\"\n",
    );
    let explicit = backlog_task(&fixture, "explicit", &[]);
    let server = fixture.server(false);
    for (path, body) in [
        ("auto", json!({"for_duration":"1m"})),
        ("ship", json!({"task_ids":[explicit.id],"mode":"local"})),
    ] {
        let receipt = ok_post(
            &server,
            &format!("/api/workflows/{path}?workspace={WS}"),
            body,
        );
        let warning = receipt["validation_env_warning"]
            .as_str()
            .unwrap_or_else(|| panic!("{path}: {receipt}"));
        chk!(warning.contains("`workflow.validation_env.shell_probe = off`") => "{path}: {receipt}");
        chk!(warning_lines(&receipt).iter().any(|line| line == warning) => "{path}: {receipt}");
        chk!(receipt.get("note").is_none() => "{path}: configured required validation needs no note: {receipt}");
    }
});
fn run_input(fixture: &Fixture, started: &Value) -> Value {
    find_run(fixture, started["run_id"].as_str().expect("run id"))
        .input
        .expect("persisted run input")
}
const REVIEW_CREW: &str = "[crews.reviewers]\nmodel = \"review-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n\n\
     [workflow]\ndefault_crew = \"reviewers\"\n\n[operation]\nreview_crew = \"reviewers\"\n";
// A dashboard drain start can add a review layer for that run, and its receipt names the captured layer; ship refuses a layer local-only delivery cannot run, and a value outside the two layers is a parse error.
wf_test!(
    auto_start_and_ship_accept_a_strengthen_only_review_option,
    {
        let _credential = held_worker_credential();
        let fixture = notice_fixture(REVIEW_CREW);
        let explicit = backlog_task(&fixture, "explicit", &[]);
        let server = fixture.server(false);
        let started = ok_post(
            &server,
            &auto_uri(),
            json!({"for_duration":"1m","review":"before-landing"}),
        );
        eq!(started["review"] => json!({"timing":"before-landing","source":"run-option"}));
        eq!(run_input(&fixture, &started)["review_request"] => "before-landing", "a resume or replay re-resolves from the persisted request: {started}");
        let before = run_ids(&fixture);
        let refused = server.send(
            "POST",
            &ship_uri(),
            json!({"task_ids":[explicit.id],"mode":"local","review":"before-pr"}),
        );
        let status = status_of(&refused);
        let value: Value = refused.json().expect("JSON error body");
        assert!((400..500).contains(&status), "{status} {value}");
        chk!(value["error"] .as_str() .is_some_and(|error| error.contains("--review before-pr")) => "{value}");
        assert_no_new_runs(&fixture, &before, "a refused ship persists no run");
        perr!(server.send( "POST", &auto_uri(), json!({"for_duration":"1m","review":"after-landing"}), ) => "unknown variant");
    }
);
const TWO_CREWS: &str = "[crews.alpha]\nmodel = \"alpha-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n\n\
     [crews.beta]\nmodel = \"beta-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n\n\
     [workflow]\ndefault_crew = \"alpha\"\n";
// The 4xx error text a refused start answers with.
fn refusal(response: reqwest::blocking::Response) -> (u16, String) {
    let status = status_of(&response);
    let value: Value = response.json().expect("JSON error body");
    assert!((400..500).contains(&status), "{status} {value}");
    (
        status,
        value["error"].as_str().unwrap_or_default().to_string(),
    )
}
// A dashboard window restricted to one crew with a tier's pool replaced records the same run input `orbit run auto --allow-crew alpha --low-complexity-crews alpha:2 beta:1 --hard-complexity-crews` does, and a blank or unconfigured crew is refused in the runtime's words before any run exists.
wf_test!(
    auto_start_records_the_crew_restriction_and_pool_overrides,
    {
        let _credential = held_worker_credential();
        let fixture = notice_fixture(TWO_CREWS);
        let server = fixture.server(false);
        let uri = auto_uri();
        let before = run_ids(&fixture);
        let (_, blank) = refusal(server.send(
            "POST",
            &uri,
            json!({"for_duration":"1m","allow_crew":[" "]}),
        ));
        assert_eq!(blank, "crew name in the allowlist must not be empty");
        let (_, unknown) = refusal(server.send(
            "POST",
            &uri,
            json!({"for_duration":"1m","allow_crew":["gamma"]}),
        ));
        assert!(unknown.contains("gamma"), "{unknown}");
        assert_no_new_runs(&fixture, &before, "a refused start persists no run");
        let started = ok_post(
            &server,
            &uri,
            json!({
                "for_duration": "1m",
                "concurrency": 2,
                "allow_crew": ["alpha"],
                "low_complexity_crews": ["alpha:2", "beta:1"],
                "hard_complexity_crews": [],
            }),
        );
        check!(started, "/source": "local", "/allowed_crews": json!(["alpha"]));
        let input = run_input(&fixture, &started);
        check!(input, "/for_seconds": 60, "/max_active_leaf_runs": 2, "/allowed_crews": json!(["alpha"]), "/low_complexity_crews": json!(["alpha:2", "beta:1"]), "/hard_complexity_crews": json!([]));
        chk!(input.get("medium_complexity_crews").is_none() => "an omitted tier keeps the workspace pool: {input}");
    }
);
// `--pull` refuses on the dashboard what it refuses on the command line, in the same words and before any owner is contacted: the flags clap rejects beside it, `--review`, `--host` without it, a blank crew, a pull from the owner checkout and a local window on a replica. Readiness names the same checkout refusals the Start form shows before submission.
wf_test!(pull_starts_refuse_what_the_cli_refuses, {
    let uri = auto_uri();
    let readiness_uri = readiness_uri();
    let replica = Fixture::replica_of("hm_fixture_remote");
    let server = replica.server(true);
    let before = run_ids(&replica);
    let selector = format!("hm_fixture_remote/{WS}");
    for (extra, flag) in [
        (json!({"approve_proposed": true}), "--approve-proposed"),
        (
            json!({"auto_concurrency": "shadow"}),
            "--auto-concurrency <MODE>",
        ),
        (json!({"auto_fix": true}), "--auto-fix"),
        (
            json!({"low_complexity_crews": []}),
            "--low-complexity-crews [<CREW>...]",
        ),
    ] {
        let mut body = json!({"for_duration": "1m", "pull": selector});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let (status, error) = refusal(server.send("POST", &uri, body));
        assert_eq!(status, 400);
        eq!(error => format!("the argument '--pull <SELECTOR>' cannot be used with '{flag}'"));
    }
    let (_, review) = refusal(server.send(
        "POST",
        &uri,
        json!({"for_duration":"1m","pull":selector,"review":"before-pr"}),
    ));
    eq!(review => orbit_core::application::review::pull_review_request_refusal());
    let (_, host) = refusal(server.send(
        "POST",
        &uri,
        json!({"for_duration":"1m","host":"dk-server-2"}),
    ));
    eq!(host => "the following required arguments were not provided: --pull <SELECTOR>");
    let (_, blank) = refusal(server.send(
        "POST",
        &uri,
        json!({"for_duration":"1m","pull":selector,"allow_crew":[""]}),
    ));
    assert_eq!(blank, "crew name in the allowlist must not be empty");
    let options = ok_get(&server, &readiness_uri)["start_options"].clone();
    check!(options, "/checkout": "replica", "/owner_machine_id": "hm_fixture_remote");
    let (status, local) = refusal(server.send("POST", &uri, json!({"for_duration":"1m"})));
    assert_eq!(status, 409);
    assert_eq!(options["local_refusal"], local, "{options}");
    assert_eq!(run_ids(&replica), before, "refusals persist no run");
    let owner = Fixture::new();
    let server = owner.server(true);
    let options = ok_get(&server, &readiness_uri)["start_options"].clone();
    assert_eq!(options["checkout"], "owner", "{options}");
    assert!(options["local_refusal"].is_null(), "{options}");
    let (status, pull) = refusal(server.send(
        "POST",
        &uri,
        json!({"for_duration":"1m","pull":format!("{}/{WS}", owner.machine_id())}),
    ));
    assert_eq!(status, 409);
    assert_eq!(options["pull_refusal"], pull, "{options}");
    chk!(run_ids(&owner).is_empty() => "the owner's refused pull persists no run");
});
