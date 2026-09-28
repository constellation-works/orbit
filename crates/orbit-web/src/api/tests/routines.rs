//! Tests for the routine-health JSON API [ORB-10138].

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use orbit_common::governance::authorization::OPERATOR_OVERRIDE_ENV;
use orbit_core::application::routines::{ClockStatus, ScheduleDisplayState};
use orbit_core::{OrbitError, OrbitRuntime, RoutineFireRecord, RoutineFireState};
use orbit_registry::{NewMachineIdentity, ensure_machine_identity};
use tower::ServiceExt;

use super::super::router;
use super::super::routines::{
    authorized_caller, clock_json, duration_ms, fire_json, fire_ok, next_evaluation_json,
    unavailable_clock_json,
};
use super::test_support::body_json;
use crate::state::DashboardState;

fn clock_status(enabled: bool) -> ClockStatus {
    ClockStatus {
        configured_cadence_seconds: 300,
        effective_cadence_seconds: enabled.then_some(300),
        enabled,
        loaded: true,
        running: Some(enabled),
        schedulable: enabled,
        health_issue: None,
        last_tick_at: None,
        next_tick_at: enabled.then(|| "2026-09-12T13:00:00+00:00".to_string()),
        platform: "systemd",
    }
}

fn with_clock_status(state: DashboardState, status: ClockStatus) -> DashboardState {
    state.with_clock_status_observer(Arc::new(move |_| Ok(status.clone())))
}

fn fire(state: RoutineFireState, created_at: &str, updated_at: &str) -> RoutineFireRecord {
    RoutineFireRecord {
        routine_name: "ship-sweep".to_string(),
        slot: "2026-07-11T22:00:00+00:00".to_string(),
        attempt: 1,
        state,
        run_id: Some("run-1".to_string()),
        source_workspace: "polaris".to_string(),
        detail: None,
        created_at: created_at.to_string(),
        updated_at: updated_at.to_string(),
    }
}

#[test]
fn fire_ok_classifies_terminal_and_in_flight_states() {
    assert_eq!(fire_ok(RoutineFireState::Succeeded), Some(true));
    assert_eq!(fire_ok(RoutineFireState::Skipped), None);
    assert_eq!(fire_ok(RoutineFireState::Failed), Some(false));
    assert_eq!(fire_ok(RoutineFireState::TimedOut), Some(false));
    assert_eq!(fire_ok(RoutineFireState::Error), Some(false));
    assert_eq!(fire_ok(RoutineFireState::Intent), None);
    assert_eq!(fire_ok(RoutineFireState::Dispatched), None);
}

#[test]
fn duration_ms_spans_intent_to_terminal() {
    let f = fire(
        RoutineFireState::Succeeded,
        "2026-07-11T22:00:00+00:00",
        "2026-07-11T22:00:12.500+00:00",
    );
    assert_eq!(duration_ms(&f), Some(12_500));
}

#[test]
fn duration_ms_is_none_while_in_flight() {
    let f = fire(
        RoutineFireState::Dispatched,
        "2026-07-11T22:00:00+00:00",
        "2026-07-11T22:00:05+00:00",
    );
    assert_eq!(duration_ms(&f), None);
}

#[test]
fn fire_json_surfaces_outcome_and_finish() {
    let f = fire(
        RoutineFireState::Succeeded,
        "2026-07-11T22:00:00+00:00",
        "2026-07-11T22:00:10+00:00",
    );
    let json = fire_json(&f);
    assert_eq!(json["state"], "succeeded");
    assert_eq!(json["ok"], true);
    assert_eq!(json["duration_ms"], 10_000);
    assert_eq!(json["finished_at"], "2026-07-11T22:00:10+00:00");
    assert_eq!(json["run_id"], "run-1");
}

#[test]
fn fire_json_omits_finish_while_in_flight() {
    let f = fire(
        RoutineFireState::Dispatched,
        "2026-07-11T22:00:00+00:00",
        "2026-07-11T22:00:00+00:00",
    );
    let json = fire_json(&f);
    assert!(json["ok"].is_null());
    assert!(json["finished_at"].is_null());
    assert!(json["duration_ms"].is_null());
}

#[test]
fn clock_json_keeps_service_state_and_health_distinct() {
    let healthy = clock_json(&ClockStatus {
        configured_cadence_seconds: 300,
        effective_cadence_seconds: Some(300),
        enabled: true,
        loaded: true,
        running: Some(true),
        schedulable: true,
        health_issue: None,
        last_tick_at: Some("previous".to_string()),
        next_tick_at: Some("next".to_string()),
        platform: "systemd",
    });
    assert_eq!(healthy["health"], "healthy");
    assert_eq!(healthy["enabled"], true);
    assert_eq!(healthy["running"], true);
    assert_eq!(healthy["next_tick_at"], "next");
    assert!(healthy["error"].is_null());

    let missed = clock_json(&ClockStatus {
        configured_cadence_seconds: 300,
        effective_cadence_seconds: None,
        enabled: true,
        loaded: true,
        running: Some(false),
        schedulable: false,
        health_issue: Some("no future trigger".to_string()),
        last_tick_at: None,
        next_tick_at: None,
        platform: "systemd",
    });
    assert_eq!(missed["health"], "missed");
    assert_eq!(missed["enabled"], true, "enabled is not health");
    assert_eq!(missed["running"], false);
    assert!(missed["error"].is_null());

    let active_disabled = clock_json(&ClockStatus {
        configured_cadence_seconds: 300,
        effective_cadence_seconds: Some(300),
        enabled: false,
        loaded: true,
        running: Some(true),
        schedulable: true,
        health_issue: Some("disabled but active".to_string()),
        last_tick_at: None,
        next_tick_at: Some("next".to_string()),
        platform: "systemd",
    });
    assert_eq!(active_disabled["health"], "unhealthy");
    assert_eq!(active_disabled["enabled"], false);
    assert_eq!(active_disabled["running"], true);
    assert_eq!(active_disabled["next_tick_at"], "next");
}

#[test]
fn unavailable_clock_json_is_unknown_not_paused() {
    let json = unavailable_clock_json(
        "execution failed: systemd clock manager is unavailable; fixture transport failure",
    );
    assert_eq!(json["health"], "unknown");
    assert!(json["enabled"].is_null(), "must not invent paused=false");
    assert!(json["loaded"].is_null());
    assert!(json["running"].is_null());
    assert!(json["schedulable"].is_null());
    assert!(json["configured_cadence_seconds"].is_null());
    assert_eq!(
        json["error"],
        "execution failed: systemd clock manager is unavailable; fixture transport failure"
    );
    assert_eq!(json["health_issue"], json["error"]);
    assert_ne!(json["health"], "paused");
    assert_ne!(json["health"], "healthy");
    assert_ne!(json["health"], "missed");
}

#[test]
fn next_evaluation_json_marks_disabled_times_hypothetical() {
    let json = next_evaluation_json(
        ScheduleDisplayState::Disabled,
        Some("2026-09-07T14:15:00-07:00".to_string()),
    );
    assert_eq!(json["state"], "disabled");
    assert_eq!(json["hypothetical"], true);
    assert_eq!(json["at"], "2026-09-07T14:15:00-07:00");

    let waiting = next_evaluation_json(ScheduleDisplayState::Waiting, None);
    assert_eq!(waiting["state"], "waiting");
    assert!(waiting["at"].is_null());
    assert_eq!(waiting["hypothetical"], false);
}

/// End-to-end: the endpoint resolves host-level routine state from the global
/// root and returns a well-formed envelope even when no routines are
/// configured (empty registry). Exercises the wiring, not fixture data.
#[tokio::test]
async fn routines_endpoint_returns_envelope_for_empty_host() {
    let temp = tempfile::tempdir().expect("temp global root");
    ensure_machine_identity(temp.path(), || {
        Ok(NewMachineIdentity {
            name: "dashboard-test".to_string(),
            task_prefix: "DA".to_string(),
        })
    })
    .expect("seed host identity");
    let observations = Arc::new(AtomicUsize::new(0));
    let observed = observations.clone();
    let state = DashboardState::global(temp.path().to_path_buf(), Vec::new(), None)
        .with_clock_status_observer(Arc::new(move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok(clock_status(true))
        }));

    let response = router()
        .with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/routines")
                .header("host", "localhost:7878")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert!(json["generated_at"].is_string());
    assert!(json["machine_name"].is_string());
    assert!(json["clock"]["provider"].is_string());
    assert!(json["clock"]["configured_cadence_seconds"].is_number());
    assert!(json["clock"]["enabled"].is_boolean());
    assert_eq!(json["clock"]["enabled"], true);
    assert_eq!(json["routines"], serde_json::json!([]));
    assert_eq!(json["load_errors"], serde_json::json!([]));
    assert_eq!(observations.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn routines_endpoint_preserves_disabled_clock_observation() {
    let temp = tempfile::tempdir().expect("temp global root");
    ensure_machine_identity(temp.path(), || {
        Ok(NewMachineIdentity {
            name: "dashboard-test".to_string(),
            task_prefix: "DA".to_string(),
        })
    })
    .expect("seed host identity");
    let state = with_clock_status(
        DashboardState::global(temp.path().to_path_buf(), Vec::new(), None),
        clock_status(false),
    );

    let response = routine_request(state, "/routines", None).await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["clock"]["enabled"], false);
    assert_eq!(json["clock"]["health"], "paused");
    assert_eq!(
        json["clock"]["effective_cadence_seconds"],
        serde_json::Value::Null
    );
}

#[tokio::test]
async fn routines_endpoint_preserves_unavailable_clock_manager_error() {
    let temp = tempfile::tempdir().expect("temp global root");
    ensure_machine_identity(temp.path(), || {
        Ok(NewMachineIdentity {
            name: "dashboard-test".to_string(),
            task_prefix: "DA".to_string(),
        })
    })
    .expect("seed host identity");
    let state = DashboardState::global(temp.path().to_path_buf(), Vec::new(), None)
        .with_clock_status_observer(Arc::new(|_| {
            Err(OrbitError::Execution(
                "systemd clock manager is unavailable; fixture transport failure".to_string(),
            ))
        }));

    let response = routine_request(state, "/routines", None).await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert!(json["error"].is_null() || json.get("error").is_none());
    assert!(json["generated_at"].is_string());
    assert!(json["machine_name"].is_string());
    assert_eq!(json["routines"], serde_json::json!([]));
    assert_eq!(json["load_errors"], serde_json::json!([]));
    assert_eq!(json["clock"]["health"], "unknown");
    assert!(
        json["clock"]["enabled"].is_null(),
        "unavailable clock must not invent paused=false: {json}"
    );
    assert_ne!(json["clock"]["health"], "paused");
    assert_ne!(json["clock"]["health"], "healthy");
    assert_eq!(
        json["clock"]["error"],
        "execution failed: systemd clock manager is unavailable; fixture transport failure"
    );
    assert_eq!(json["clock"]["health_issue"], json["clock"]["error"]);
}

/// Pin the process signals `CallerCapabilities::resolve` reads, for the whole
/// request rather than just its construction.
///
/// The guard in `orbit_common::test_env` is process-wide, so it serializes two
/// tests only when *both* take it. Parallel `as_operator` tests in
/// `api::tests::auto_tasks`, `api::tests::runs`, and `api::tests::operation`
/// set `ORBIT_OPERATOR=1` under that lock. This case used to read the variable
/// without holding it, so a sibling could promote the caller to Operator;
/// authorization then succeeded and `routine_statuses` on
/// `DashboardState::single`'s empty global root mapped `InvalidInput` to HTTP
/// 400 instead of the 403 unidentified-caller denial [ORB-10894].
#[allow(clippy::await_holding_lock)]
async fn with_caller_env<'a, T>(
    vars: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    fut: impl std::future::Future<Output = T>,
) -> T {
    let _env = orbit_common::test_env::scoped(vars);
    fut.await
}

#[tokio::test]
async fn routine_mutation_denies_an_unidentified_dashboard_caller() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let state = DashboardState::single(Arc::new(runtime));
    // Agent envelope is *set* rather than cleared: with nothing declared,
    // resolution falls through to the interactive-terminal probe, and a TTY
    // `cargo test` would resolve to Operator for a reason unrelated to the
    // handler. The empty-grants unidentified branch is covered without process
    // state in `orbit_common::governance::tests::authorization`.
    let response = with_caller_env(
        [
            (OPERATOR_OVERRIDE_ENV, None),
            ("ORBIT_AGENT_NAME", Some("orbit-web-test")),
            ("ORBIT_AGENT_MODEL", Some("orbit-web-test")),
        ],
        async {
            router()
                .with_state(state)
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/routines/toggle?workspace=default")
                        .header("origin", "http://localhost:7878")
                        .header("host", "localhost:7878")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            r#"{"name":"nightly","source":"default","target":"job:nightly","machine_name":"host-a","expected_enabled":true,"enabled":false}"#,
                        ))
                        .expect("request"),
                )
                .await
                .expect("response")
        },
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let json = body_json(response).await;
    assert_eq!(json["code"], "authorization_denied");
    assert_eq!(json["operation"], "routine.toggle");
}

#[tokio::test]
async fn operations_mutations_require_an_explicit_workspace() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let state = DashboardState::single(Arc::new(runtime));
    let response = router()
        .with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/routines/clock")
                .header("origin", "http://localhost:7878")
                .header("host", "localhost:7878")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"action":"disable","machine_name":"host-a","expected_enabled":true,"expected_cadence_seconds":60}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let json = body_json(response).await;
    assert_eq!(json["code"], "workspace_required");
}

#[tokio::test]
async fn routine_and_clock_capabilities_use_each_canonical_operation() {
    use super::super::routines::action_capability;
    use orbit_common::governance::authorization::{
        DASHBOARD_CLOCK_CADENCE, DASHBOARD_CLOCK_SERVICE, DASHBOARD_ROUTINE_TOGGLE,
    };

    for operator in [false, true] {
        with_caller_env(
            [
                (OPERATOR_OVERRIDE_ENV, operator.then_some("1")),
                ("ORBIT_AGENT_NAME", Some("orbit-web-test")),
            ],
            async {
                for operation in [
                    &DASHBOARD_ROUTINE_TOGGLE,
                    &DASHBOARD_CLOCK_SERVICE,
                    &DASHBOARD_CLOCK_CADENCE,
                ] {
                    let capability = action_capability(operation, false);
                    assert_eq!(capability["authorized"], operator);
                    if operator {
                        assert!(capability["reason"].is_null());
                    } else {
                        assert!(
                            capability["reason"]
                                .as_str()
                                .expect("denial")
                                .contains(operation.id)
                        );
                    }
                }
            },
        )
        .await;
    }
}

#[test]
fn operator_session_grants_without_tty_or_override() {
    use orbit_common::governance::authorization::DASHBOARD_ROUTINE_TOGGLE;

    let _env = orbit_common::test_env::scoped([
        (OPERATOR_OVERRIDE_ENV, None),
        ("ORBIT_AGENT_NAME", Some("orbit-web-test")),
        ("ORBIT_AGENT_MODEL", Some("orbit-web-test")),
    ]);
    assert!(
        authorized_caller(&DASHBOARD_ROUTINE_TOGGLE, false).is_err(),
        "agent envelope without --operator must remain unauthorized"
    );
    assert!(
        authorized_caller(&DASHBOARD_ROUTINE_TOGGLE, true).is_ok(),
        "--operator must grant the session regardless of TTY or ORBIT_OPERATOR"
    );
}

fn empty_host_state() -> (tempfile::TempDir, DashboardState) {
    let temp = tempfile::tempdir().expect("temp global root");
    ensure_machine_identity(temp.path(), || {
        Ok(NewMachineIdentity {
            name: "dashboard-test".to_string(),
            task_prefix: "DA".to_string(),
        })
    })
    .expect("seed host identity");
    let state = DashboardState::global(temp.path().to_path_buf(), Vec::new(), None);
    (temp, state)
}

#[tokio::test]
async fn operator_session_enables_operations_controls_on_the_list() {
    let (_temp, state) = empty_host_state();
    state.set_operator_session(true);
    let response = with_caller_env(
        [
            (OPERATOR_OVERRIDE_ENV, None),
            ("ORBIT_AGENT_NAME", Some("orbit-web-test")),
            ("ORBIT_AGENT_MODEL", Some("orbit-web-test")),
        ],
        routine_request(state, "/routines", None),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["controls_authorized"], true);
    assert!(
        json["session_explanation"]
            .as_str()
            .expect("explanation")
            .contains("operator authority"),
        "{}",
        json["session_explanation"]
    );
    assert!(
        !json["session_explanation"]
            .as_str()
            .expect("explanation")
            .contains("ORBIT_OPERATOR"),
        "non-operator copy must not appear when the session is authorized"
    );
}

#[tokio::test]
async fn session_explanation_names_serve_operator_not_env_override() {
    let (_temp, state) = empty_host_state();
    let response = with_caller_env(
        [
            (OPERATOR_OVERRIDE_ENV, None),
            ("ORBIT_AGENT_NAME", Some("orbit-web-test")),
            ("ORBIT_AGENT_MODEL", Some("orbit-web-test")),
        ],
        routine_request(state, "/routines", None),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let explanation = json["session_explanation"].as_str().expect("explanation");
    assert!(
        explanation.contains("orbit web serve --operator"),
        "{explanation}"
    );
    assert!(explanation.contains("orbit web connect"), "{explanation}");
    assert!(
        !explanation.contains("ORBIT_OPERATOR=1"),
        "remote users cannot act on an env restart: {explanation}"
    );
}

#[tokio::test]
async fn authorized_routine_toggle_reads_back_and_rejects_stale_or_wrong_selection() {
    use super::workspaces::workspace_entry;
    use chrono::Utc;
    use orbit_registry::workspace_registry;
    use orbit_types::workspace::{
        Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus,
    };

    let temp = tempfile::tempdir().expect("fixture");
    let global = temp.path().join("global");
    std::fs::create_dir_all(&global).expect("global");
    ensure_machine_identity(&global, || {
        Ok(NewMachineIdentity {
            name: "dashboard-test".to_string(),
            task_prefix: "DA".to_string(),
        })
    })
    .expect("identity");
    let repo = temp.path().join("alpha");
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).expect("workspace");
    std::fs::write(
        orbit_dir.join("config.yaml"),
        "schema_version: 1\nworkspace_id: ws_alpha\n",
    )
    .expect("workspace identity");
    super::test_support::write_replay_job_under(&orbit_dir, "noop");
    let routines = orbit_dir.join("routines");
    std::fs::create_dir_all(&routines).expect("routines");
    let path = routines.join("fixture.yaml");
    let original = "schemaVersion: 1\nname: fixture\ndescription: keep this text\n# operator note\nenabled: true # reviewed\ntrigger: {cron: '* * * * *'}\ntarget: job:noop\n";
    std::fs::write(&path, original).expect("definition");
    let now = Utc::now();
    let registry = WorkspaceRegistry {
        workspaces: vec![Workspace {
            id: "ws_alpha".to_string(),
            name: "alpha".to_string(),
            owner_machine_id: Some(
                orbit_registry::machine_identity::load_machine_identity(&global)
                    .expect("identity")
                    .id,
            ),
            git_remote: None,
            ship_mode: None,
            base_branch: "agent-main".to_string(),
            status: WorkspaceStatus::Active,
            created_at: now,
            updated_at: now,
        }],
        checkouts: vec![WorkspaceCheckout::owner(
            "ws_alpha".to_string(),
            repo.clone(),
            orbit_dir.clone(),
        )],
        ..WorkspaceRegistry::default()
    };
    workspace_registry::save_registry_to(
        &registry,
        &workspace_registry::registry_path_for(&global),
    )
    .expect("registry");
    let state = with_clock_status(
        DashboardState::global(
            global,
            vec![workspace_entry("alpha", repo, orbit_dir, true)],
            Some("alpha".to_string()),
        ),
        clock_status(true),
    );

    with_caller_env([(OPERATOR_OVERRIDE_ENV, Some("1"))], async {
        for enabled in [false, true] {
            let body = serde_json::json!({"name":"fixture", "source":"alpha", "target":"job:noop", "machine_name":"dashboard-test", "expected_enabled": !enabled, "enabled":enabled});
            let response = routine_request(state.clone(), "/routines/toggle?workspace=alpha", Some(body.clone())).await;
            assert_eq!(response.status(), StatusCode::OK, "{}", body_json(response).await);
            let persisted: serde_yaml::Value = serde_yaml::from_str(&std::fs::read_to_string(&path).expect("read file")).expect("yaml");
            assert_eq!(persisted["enabled"].as_bool(), Some(enabled));
            let listed = body_json(routine_request(state.clone(), "/routines", None).await).await;
            assert_eq!(listed["routines"][0]["enabled"], enabled, "{listed}");
            assert!(
                listed["routines"]
                    .as_array()
                    .expect("routine rows")
                    .iter()
                    .all(|routine| routine["name"] != "auto_task_scheduler"),
                "auto-task evaluation must not be projected as a routine: {listed}"
            );
            assert_eq!(listed["capabilities"]["routine_toggle"]["authorized"], true);
            let stale = routine_request(state.clone(), "/routines/toggle?workspace=alpha", Some(body)).await;
            assert_eq!(stale.status(), StatusCode::CONFLICT);
        }
        let wrong = routine_request(state.clone(), "/routines/toggle?workspace=alpha", Some(serde_json::json!({
            "name":"fixture", "source":"other", "target":"job:noop", "machine_name":"dashboard-test", "expected_enabled":true, "enabled":false
        }))).await;
        assert_eq!(wrong.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(wrong).await["code"], "workspace_mismatch");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read round trip"),
            original,
            "a disable/enable round trip must keep every other byte, comments included"
        );

        let stale_target = routine_request(state.clone(), "/routines/toggle?workspace=alpha", Some(serde_json::json!({
            "name":"fixture", "source":"alpha", "target":"job:retired", "machine_name":"dashboard-test", "expected_enabled":true, "enabled":false
        }))).await;
        assert_eq!(stale_target.status(), StatusCode::CONFLICT);
        let refused = body_json(stale_target).await;
        assert_eq!(refused["code"], "target_mismatch", "{refused}");
        assert_eq!(refused["actual_target"], "job:noop", "{refused}");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read after stale target"),
            original,
            "a stale target selection must not write"
        );

        let audit = body_json(routine_request(state.clone(), "/audit?workspace=alpha&limit=100", None).await).await;
        let toggles: Vec<&serde_json::Value> = audit
            .as_array()
            .expect("audit rows")
            .iter()
            .filter(|event| event["target_type"] == "routine.toggle")
            .collect();
        let with_status = |status: &str| toggles.iter().filter(|event| event["status"] == status).count();
        assert_eq!(with_status("success"), 2, "only the two changed toggles succeed: {audit}");
        assert_eq!(
            with_status("failure"),
            4,
            "two stale states, the workspace mismatch, and the stale target are refusals: {audit}"
        );
        assert!(
            toggles.iter().any(|event| event["status"] == "failure"
                && event["error_message"].as_str().is_some_and(|message| message.starts_with("target_mismatch"))),
            "the stale target refusal must be audited: {audit}"
        );

        // Enter the canonical clock handler, but reject an invalid cadence before
        // native service writes. Actual service mutation belongs to Core's fake-runner tests.
        let listed = body_json(routine_request(state.clone(), "/routines", None).await).await;
        let invalid_clock = routine_request(state.clone(), "/routines/clock?workspace=alpha", Some(serde_json::json!({
            "action":"set_cadence", "machine_name":"dashboard-test",
            "expected_enabled": listed["clock"]["enabled"],
            "expected_cadence_seconds": listed["clock"]["configured_cadence_seconds"], "cadence_seconds":61
        }))).await;
        assert_eq!(invalid_clock.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(invalid_clock).await["error"].as_str().expect("error").contains("whole minute"));
    }).await;
}

async fn routine_request(
    state: DashboardState,
    uri: &str,
    body: Option<serde_json::Value>,
) -> axum::response::Response {
    let mut request = Request::builder().uri(uri).header("host", "localhost:7878");
    let body = if let Some(body) = body {
        request = request
            .method(Method::POST)
            .header("origin", "http://localhost:7878")
            .header("content-type", "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    router()
        .with_state(state)
        .oneshot(request.body(body).expect("request"))
        .await
        .expect("response")
}

fn parked(
    name: &str,
    source: &str,
    skipped: bool,
) -> orbit_core::application::routines::RetiredRoutine {
    orbit_core::application::routines::RetiredRoutine {
        name: name.to_string(),
        origin: orbit_core::application::routines::RoutineOrigin::Workspace,
        source_workspace: source.to_string(),
        path: std::path::PathBuf::from(format!("/ws/{source}/.orbit/routines/{name}.yaml")),
        job: "graph_refresh_pipeline".to_string(),
        reason: if skipped {
            "seeded by plugin:graph@1.0.0, which is switched off in workspace 'alpha'".to_string()
        } else {
            "targets a retired job".to_string()
        },
        skipped,
    }
}

/// A routine whose plugin is off where it lives is omitted by default and
/// counted per workspace; the opt-in lists it marked inactive. A routine
/// targeting a retired job is listed either way.
#[test]
fn report_hides_inactive_plugin_routines_unless_asked_and_counts_them_per_workspace() {
    let report = orbit_core::application::routines::RoutineStatusReport {
        machine_name: "dashboard-test".to_string(),
        machine_id: "hm_test".to_string(),
        statuses: Vec::new(),
        retired: vec![
            parked("graph-refresh", "alpha", true),
            parked("old-scheduler", "alpha", false),
        ],
        load_errors: Vec::new(),
    };
    let names = |json: &serde_json::Value| {
        json["retired"]
            .as_array()
            .expect("retired")
            .iter()
            .map(|routine| routine["name"].as_str().expect("name").to_string())
            .collect::<Vec<_>>()
    };

    let hidden = super::super::routines::report_json(
        &report,
        clock_json(&clock_status(true)),
        chrono::Utc::now(),
        false,
        false,
    );
    assert_eq!(names(&hidden), vec!["old-scheduler"]);
    assert_eq!(hidden["inactive_plugin_counts"]["alpha"], 1);

    let shown = super::super::routines::report_json(
        &report,
        clock_json(&clock_status(true)),
        chrono::Utc::now(),
        false,
        true,
    );
    assert_eq!(names(&shown), vec!["graph-refresh", "old-scheduler"]);
    assert_eq!(shown["retired"][0]["plugin_inactive"], true);
    assert_eq!(shown["retired"][1]["plugin_inactive"], false);
}

#[tokio::test]
async fn routines_endpoint_accepts_the_inactive_plugin_opt_in() {
    let (_temp, state) = empty_host_state();
    let state = with_clock_status(state, clock_status(true));
    let response = routine_request(state, "/routines?include_inactive_plugins=true", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["inactive_plugin_counts"], serde_json::json!({}));
}
