//! [ORB-10709, ADR-0352] The exclusive workspace claim and the gate it puts on
//! workflow dispatch.
//!
//! The gate is asserted against the shared submission path itself, with no HTTP
//! or protocol adapter in the picture — that placement is the whole point, and a
//! test that went through one surface would not prove the others inherit it.

use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::AuditEventStatus;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::tool_host::test_support::{
    create_context_task, run_tool_as_operator, test_runtime, unmanaged_tool_env_guard,
};
use crate::application::task::TaskAddParams;
use crate::application::workflow::{CompletionPolicy, ShipMode};
use crate::runtime::workspace_claim::CLAIM_TOKEN_ENV;

/// Acquire the claim and return its token.
fn acquire_claim(runtime: &OrbitRuntime, actor: &str) -> String {
    let result = run_tool_as_operator(
        runtime,
        "orbit.workspace.claim.acquire",
        json!({ "model": actor, "machine_id": "machine-1", "session_id": "session-1" }),
    )
    .expect("acquire workspace claim");
    assert_eq!(result["acquired"], json!(true));
    result["claim_token"]
        .as_str()
        .expect("claim grant carries a token")
        .to_string()
}

fn ship_error(runtime: &OrbitRuntime, claim_token: Option<&str>) -> OrbitError {
    runtime
        .submit_ship_run(
            ShipMode::Local,
            Some("main"),
            &[],
            CompletionPolicy::Review,
            &[],
            Some("test"),
            claim_token,
        )
        .expect_err("this fixture deploys no job asset, so submission always ends in an error")
}

fn add_backlog_task(runtime: &OrbitRuntime) -> String {
    runtime
        .add_task(TaskAddParams {
            title: "Workspace claim fixture".to_string(),
            description: "A task selected by a workspace-claim test.".to_string(),
            ..Default::default()
        })
        .expect("create backlog task")
        .id
}

#[test]
fn dispatch_without_the_claim_is_refused_with_the_holder_and_expiry() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo) = test_runtime();
    acquire_claim(&runtime, "claude");

    let error = ship_error(&runtime, None);
    let OrbitError::WorkspaceClaimHeld(claim) = &error else {
        panic!("expected WorkspaceClaimHeld, got {error:?}");
    };
    assert_eq!(claim.operation, "orbit.workflow.ship");
    assert_eq!(claim.holder, "claude");
    assert!(claim.claim_id.starts_with("claim-"));
    assert!(
        !claim.expires_at.is_empty(),
        "a refusal must name the instant the claim lapses"
    );

    let message = error.to_string();
    assert!(message.contains("claude"), "unexpected message: {message}");
    assert!(
        message.contains(&claim.expires_at),
        "unexpected message: {message}"
    );
}

#[test]
fn the_holder_dispatches_and_a_second_operator_does_not() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo) = test_runtime();
    let token = acquire_claim(&runtime, "claude");

    // The holder passes the gate; the fixture then fails on the missing job
    // asset, which is exactly how we know the claim check let it through.
    let holder = ship_error(&runtime, Some(&token));
    assert!(
        matches!(holder, OrbitError::NotFound { .. }),
        "the claim holder must pass the gate, got {holder:?}"
    );

    let stranger = ship_error(&runtime, Some("wsclaim-some-other-token"));
    assert!(
        matches!(stranger, OrbitError::WorkspaceClaimHeld(_)),
        "a second operator must be refused, got {stranger:?}"
    );
}

#[test]
fn resume_takes_the_same_gate_as_ship() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo) = test_runtime();
    acquire_claim(&runtime, "claude");

    let error = runtime
        .submit_resume_run("jrun-does-not-exist", Some("test"), None)
        .expect_err("resume must not reach run lookup while the claim refuses it");
    let OrbitError::WorkspaceClaimHeld(claim) = &error else {
        panic!("expected WorkspaceClaimHeld, got {error:?}");
    };
    assert_eq!(claim.operation, "orbit.workflow.run.resume");
}

/// The gap the duplicate-dispatch guard structurally cannot cover: an auto /
/// backlog-discovery submission carries no task ids, so a guard keyed on task id
/// has nothing to key on. The claim check is keyed on neither task ids nor the
/// recent-run window.
#[test]
fn a_discovery_mode_submission_carrying_no_task_ids_is_covered() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo) = test_runtime();
    acquire_claim(&runtime, "claude");

    let error = runtime
        .submit_ship_run(
            ShipMode::Local,
            Some("main"),
            &[],
            CompletionPolicy::Review,
            &[],
            Some("test"),
            None,
        )
        .expect_err("a discovery submission must be gated by the claim");
    assert!(
        matches!(error, OrbitError::WorkspaceClaimHeld(_)),
        "discovery mode must be gated, got {error:?}"
    );
}

#[test]
fn the_token_may_arrive_through_the_environment() {
    let (_root, runtime, _repo) = test_runtime();
    let token = {
        let _env = unmanaged_tool_env_guard();
        acquire_claim(&runtime, "claude")
    };
    // One guard, not two: `test_env` guards share a process-wide lock, so
    // holding the unmanaged guard while taking a second one would deadlock.
    let _env = orbit_common::test_env::scoped([
        ("ORBIT_MANAGED_RUN_CONTEXT", None),
        ("ORBIT_TASK_ID", None),
        ("ORBIT_ACTIVE_TASK_ID", None),
        ("ORBIT_AGENT_NAME", None),
        ("ORBIT_AGENT_MODEL", None),
        ("ORBIT_RUN_ID", None),
        ("ORBIT_ACTIVITY_ID", None),
        ("ORBIT_STEP_INDEX", None),
        (CLAIM_TOKEN_ENV, Some(token.as_str())),
    ]);

    let error = ship_error(&runtime, None);
    assert!(
        matches!(error, OrbitError::NotFound { .. }),
        "the environment fallback must satisfy the gate, got {error:?}"
    );
}

/// The claim serializes *what starts*, and nothing else. Several people working
/// different features in one workspace is the intended behaviour.
#[test]
fn non_workflow_operations_succeed_while_a_claim_is_held() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    acquire_claim(&runtime, "claude");

    let created = run_tool_as_operator(
        &runtime,
        "orbit.task.add",
        json!({
            "title": "Filing a task while the workspace is claimed",
            "description": "Filing, reading, and updating stay concurrent under ADR-0352.",
            "workspace": repo.display().to_string(),
            "model": "codex",
            "complexity": "medium",
        }),
    )
    .expect("filing a task must not be gated by the workspace claim");
    let task_id = created["id"].as_str().expect("new task id").to_string();

    run_tool_as_operator(&runtime, "orbit.task.show", json!({ "id": task_id }))
        .expect("reading a task must not be gated by the workspace claim");
    run_tool_as_operator(
        &runtime,
        "orbit.task.update",
        json!({ "id": task_id, "description": "updated under a held claim", "model": "codex" }),
    )
    .expect("updating a task must not be gated by the workspace claim");
    run_tool_as_operator(&runtime, "orbit.search", json!({ "query": "claim" }))
        .expect("search must not be gated by the workspace claim");

    // Worker file reservations are a different dimension entirely.
    let task = create_context_task(
        &runtime,
        &repo,
        TaskStatus::Backlog,
        &["file:src/reserved.rs"],
    );
    let reserved = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({ "task_ids": [task.id], "model": "codex" }),
    )
    .expect("reserving files must not be gated by the workspace claim");
    assert_eq!(
        reserved["reserved"],
        json!(true),
        "an active workspace claim must not block a worker reservation: {reserved}"
    );
}

#[test]
fn an_expired_claim_stops_blocking_without_intervention() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo) = test_runtime();
    run_tool_as_operator(
        &runtime,
        "orbit.workspace.claim.acquire",
        json!({ "model": "claude", "ttl_seconds": 1 }),
    )
    .expect("acquire a short-lived claim");

    assert!(matches!(
        ship_error(&runtime, None),
        OrbitError::WorkspaceClaimHeld(_)
    ));

    std::thread::sleep(std::time::Duration::from_millis(1100));

    let after = ship_error(&runtime, None);
    assert!(
        matches!(after, OrbitError::NotFound { .. }),
        "an expired claim must stop blocking with no manual release, got {after:?}"
    );
}

#[test]
fn force_release_is_audited_with_who_forced_it_and_whom_they_displaced() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo) = test_runtime();
    acquire_claim(&runtime, "claude");

    let refused = run_tool_as_operator(
        &runtime,
        "orbit.workspace.claim.release",
        json!({ "claim_token": "wsclaim-wrong", "model": "codex" }),
    )
    .expect("a stale token is a refusal, not an error");
    assert_eq!(refused["released"], json!(false));

    let forced = run_tool_as_operator(
        &runtime,
        "orbit.workspace.claim.release",
        json!({ "force": true, "model": "codex" }),
    )
    .expect("force release");
    assert_eq!(forced["released"], json!(true));
    assert_eq!(forced["forced"], json!(true));

    let audited = claim_audit_payloads(&runtime, "workspace.claim.force_released");
    let payload = audited.first().expect("force release is audited");
    assert_eq!(payload["forced"], json!(true));
    assert_eq!(payload["released_by"], json!("codex"));
    assert_eq!(payload["displaced_holder"], json!("claude"));

    // ...and the workspace is dispatchable again.
    assert!(matches!(
        ship_error(&runtime, None),
        OrbitError::NotFound { .. }
    ));
}

#[test]
fn a_refused_dispatch_is_recorded_as_denied_without_the_holders_token() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo) = test_runtime();
    let token = acquire_claim(&runtime, "claude");
    let task_id = add_backlog_task(&runtime);

    let _ = runtime.submit_ship_run(
        ShipMode::Local,
        Some("main"),
        std::slice::from_ref(&task_id),
        CompletionPolicy::Review,
        &[],
        Some("test"),
        None,
    );

    let events = runtime
        .list_audit_events(None, None, None, None, 200)
        .expect("read audit events");
    let denial = events
        .iter()
        .find(|event| event.command == "workspace.claim.dispatch.denied")
        .expect("a refused dispatch is audited");
    assert_eq!(denial.status, AuditEventStatus::Denied);
    let arguments = denial.arguments_json.as_deref().unwrap_or_default();
    assert!(
        arguments.contains("orbit.workflow.ship"),
        "the denial names the refused operation: {arguments}"
    );
    assert!(
        !arguments.contains(&token),
        "an audit reader is not the holder; the token must never be recorded"
    );
}

#[test]
fn a_second_operator_cannot_take_a_held_claim() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo) = test_runtime();
    acquire_claim(&runtime, "claude");

    let contended = run_tool_as_operator(
        &runtime,
        "orbit.workspace.claim.acquire",
        json!({ "model": "codex" }),
    )
    .expect("contention is a refusal, not an error");
    assert_eq!(contended["acquired"], json!(false));
    assert_eq!(contended["claim"]["actor"], json!("claude"));
    assert!(
        contended["claim_token"].is_null(),
        "a refused contender must not learn the incumbent's token: {contended}"
    );
}

#[test]
fn claim_status_reports_the_holder_and_then_the_release() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo) = test_runtime();
    let token = acquire_claim(&runtime, "claude");

    let held = run_tool_as_operator(&runtime, "orbit.workspace.claim.show", json!({}))
        .expect("claim status");
    assert_eq!(held["claimed"], json!(true));
    assert_eq!(held["claim"]["actor"], json!("claude"));
    assert_eq!(held["claim"]["machine_id"], json!("machine-1"));
    assert_eq!(held["claim"]["session_id"], json!("session-1"));

    run_tool_as_operator(
        &runtime,
        "orbit.workspace.claim.release",
        json!({ "claim_token": token, "model": "claude" }),
    )
    .expect("release with the holder's token");

    let free = run_tool_as_operator(&runtime, "orbit.workspace.claim.show", json!({}))
        .expect("claim status after release");
    assert_eq!(free["claimed"], json!(false));
}

#[test]
fn releasing_without_a_token_or_force_is_rejected_as_invalid_input() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo) = test_runtime();
    acquire_claim(&runtime, "claude");

    let error = run_tool_as_operator(
        &runtime,
        "orbit.workspace.claim.release",
        json!({ "model": "codex" }),
    )
    .expect_err("a release with neither token nor force is malformed");
    assert!(
        matches!(error, OrbitError::InvalidInput(_)),
        "got {error:?}"
    );
}

fn claim_audit_payloads(runtime: &OrbitRuntime, command: &str) -> Vec<Value> {
    runtime
        .list_audit_events(None, None, None, None, 200)
        .expect("read audit events")
        .into_iter()
        .filter(|event| event.command == command)
        .filter_map(|event| {
            event
                .arguments_json
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok())
        })
        .collect()
}
