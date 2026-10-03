use super::super::router;
use super::test_support::body_json;
use crate::state::DashboardState;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use orbit_common::governance::authorization::OPERATOR_OVERRIDE_ENV;
use orbit_core::{AutoTaskAddParams, OrbitRuntime};
use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};
use std::sync::Arc;
use tower::ServiceExt;

fn runtime() -> OrbitRuntime {
    OrbitRuntime::in_memory().expect("build runtime")
}

fn chore_params(name: &str) -> AutoTaskAddParams {
    AutoTaskAddParams {
        name: name.to_string(),
        description: format!("Definition {name}"),
        schedule: AutoTaskSchedule::Interval { every_minutes: 60 },
        template: AutoTaskTemplate {
            title: format!("Chore {name}"),
            description: "Recurring chore body.".to_string(),
            acceptance_criteria: vec!["The chore is observable.".to_string()],
            task_type: TaskType::Chore,
            tags: vec![],
            required_tools: Vec::new(),
            priority: TaskPriority::Medium,
            complexity: Some(TaskComplexity::Medium),
            crew: None,
            status: TaskStatus::Backlog,
        },
        dedupe: DedupePolicy::SkipIfOpen,
    }
}

fn state(runtime: OrbitRuntime) -> (DashboardState, Arc<OrbitRuntime>) {
    let runtime = Arc::new(runtime);
    (DashboardState::single(runtime.clone()), runtime)
}

async fn send(
    state: DashboardState,
    method: Method,
    uri: &str,
    body: Option<&str>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method.clone())
        .uri(uri)
        .header("host", "localhost:7878");
    if !matches!(method, Method::GET) {
        builder = builder
            .header("origin", "http://localhost:7878")
            .header("content-type", "application/json");
    }
    router()
        .with_state(state)
        .oneshot(
            builder
                .body(Body::from(body.unwrap_or("").to_string()))
                .expect("request"),
        )
        .await
        .expect("response")
}

/// Pin the process signals `CallerCapabilities::resolve` reads, for the whole
/// request rather than just its construction.
///
/// The guard in `orbit_common::test_env` is process-wide, so it serializes two
/// tests only when *both* take it. Every case whose expected status depends on
/// caller identity therefore goes through this helper — a case that merely
/// reads `ORBIT_OPERATOR` without holding the guard can observe the override a
/// sibling set concurrently and turn an expected 403 into a 200 [ORB-10894].
#[allow(clippy::await_holding_lock)]
async fn with_caller_env<'a, T>(
    vars: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    fut: impl std::future::Future<Output = T>,
) -> T {
    let _env = orbit_common::test_env::scoped(vars);
    fut.await
}

/// Resolve as an agent: override cleared, agent envelope declared.
///
/// The envelope is *set* rather than cleared on purpose. With nothing declared,
/// resolution falls through to its interactive-terminal probe, and `cargo test`
/// run from a terminal inherits both handles — the caller would resolve to
/// `Operator` and the denial assertion would fail for a reason that has nothing
/// to do with the code under test. Declaring the agent stops resolution one
/// rule earlier, so the expected 403 holds in CI, in a managed run, and in an
/// interactive shell alike. The unidentified-caller branch is covered without
/// process state in `orbit_common::governance::tests::authorization`.
async fn as_agent<T>(fut: impl std::future::Future<Output = T>) -> T {
    with_caller_env(
        [
            (OPERATOR_OVERRIDE_ENV, None),
            ("ORBIT_AGENT_NAME", Some("orbit-web-test")),
            ("ORBIT_AGENT_MODEL", Some("orbit-web-test")),
        ],
        fut,
    )
    .await
}

#[tokio::test]
async fn toggle_denies_a_caller_without_operator_capability() {
    let runtime = runtime();
    runtime.auto_task_add(chore_params("nightly")).expect("add");
    let (state, runtime) = state(runtime);
    let response = as_agent(send(
        state,
        Method::POST,
        "/auto-tasks/toggle?workspace=default",
        Some(r#"{"name":"nightly","expected_enabled":true,"enabled":false}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let json = body_json(response).await;
    assert_eq!(json["code"], "authorization_denied");
    assert_eq!(json["operation"], "auto_task.toggle");
    assert!(
        runtime
            .auto_task_show("nightly")
            .expect("show")
            .expect("present")
            .enabled
    );
}
