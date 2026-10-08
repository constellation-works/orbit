use super::super::router;
use super::test_support::{body_json, enter_isolated_child};
use crate::state::DashboardState;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use orbit_common::governance::authorization::{DASHBOARD_CLOCK_SERVICE, OPERATOR_OVERRIDE_ENV};
use orbit_core::application::routines::ClockStatus;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::telemetry::AuditEventStatus;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;

pub(super) async fn request_cancel(
    runtime: OrbitRuntime,
    run_id: &str,
    origin: Option<&str>,
    host: Option<&str>,
) -> Response {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(format!("/runs/{run_id}/cancel"));
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    if let Some(host) = host {
        builder = builder.header(header::HOST, host);
    }
    router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(builder.body(Body::empty()).expect("request"))
        .await
        .expect("response")
}

#[tokio::test]
async fn successful_clock_change_with_failed_follow_up_read_is_audited_and_unknown() {
    if !enter_isolated_child(
        module_path!(),
        "successful_clock_change_with_failed_follow_up_read_is_audited_and_unknown",
    ) {
        return;
    }
    let _env = orbit_common::test_env::scoped([
        (OPERATOR_OVERRIDE_ENV, None),
        ("ORBIT_AGENT_NAME", Some("orbit-web-test")),
        ("ORBIT_AGENT_MODEL", Some("orbit-web-test")),
    ]);
    let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));
    orbit_registry::ensure_machine_identity(&runtime.global_root(), || {
        Ok(orbit_registry::NewMachineIdentity {
            name: "clock-fixture".to_string(),
            task_prefix: "CF".to_string(),
        })
    })
    .expect("ensure machine identity");
    let state = DashboardState::single(runtime.clone());
    state.set_operator_session(true);
    let status_reads = Arc::new(AtomicUsize::new(0));
    let reads = status_reads.clone();
    state.set_clock_status_hook(Arc::new(move || {
        if reads.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(ClockStatus {
                configured_cadence_seconds: 300,
                effective_cadence_seconds: Some(300),
                enabled: true,
                loaded: true,
                running: Some(true),
                schedulable: true,
                health_issue: None,
                last_tick_at: None,
                next_tick_at: None,
                platform: "test",
            })
        } else {
            Err(OrbitError::Execution(
                "manager status read failed".to_string(),
            ))
        }
    }));
    state.set_clock_mutation_hook(Arc::new(|| Ok(())));
    let machine_name = orbit_cmd::registry_routines::routine_statuses(&runtime.global_root())
        .expect("routine status")
        .machine_name;
    let request = Request::builder()
        .method(Method::POST)
        .uri("/routines/clock?workspace=default")
        .header(header::HOST, "localhost:7878")
        .header(header::ORIGIN, "http://localhost:7878")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "action": "disable",
                "machine_name": machine_name,
                "expected_enabled": true,
                "expected_cadence_seconds": 300,
            })
            .to_string(),
        ))
        .expect("request");

    let response = router()
        .with_state(state)
        .oneshot(request)
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let result = body_json(response).await;
    assert_eq!(result["clock"]["health"], "unknown");
    assert!(result["clock"]["enabled"].is_null());
    assert!(
        result["clock"]["error"]
            .as_str()
            .expect("clock error")
            .contains("manager status read failed")
    );
    assert!(result["changed"].is_null());

    let clock_audits = runtime
        .list_audit_events(None, None, None, None, 20)
        .expect("list audit events")
        .into_iter()
        .filter(|event| {
            event.command == "dashboard.operations"
                && event.target_type.as_deref() == Some(DASHBOARD_CLOCK_SERVICE.id)
                && event.target_id.as_deref() == Some("clock")
        })
        .collect::<Vec<_>>();
    assert_eq!(clock_audits.len(), 1);
    assert_eq!(clock_audits[0].status, AuditEventStatus::Success);
    assert_eq!(clock_audits[0].error_message, None);
}
