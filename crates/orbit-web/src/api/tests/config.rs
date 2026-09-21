//! Tests for the dashboard configuration JSON API [ORB-12724].
//!
//! The two cases worth pinning are the ones an operator cannot reconstruct
//! from a flat value dump: a workspace value that shadows a global one, and a
//! global `execution.*` value the workspace file did not inherit. Both are
//! asserted against the layered view the tab renders, not against the raw
//! files.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use orbit_common::governance::authorization::OPERATOR_OVERRIDE_ENV;
use orbit_core::OrbitRuntime;
use serde_json::Value;
use tower::ServiceExt;

use super::super::router;
use super::test_support::body_json;
use crate::state::DashboardState;

fn runtime() -> OrbitRuntime {
    OrbitRuntime::in_memory().expect("build runtime")
}

fn state(runtime: OrbitRuntime) -> (DashboardState, Arc<OrbitRuntime>) {
    let runtime = Arc::new(runtime);
    (DashboardState::single(runtime.clone()), runtime)
}

fn write_global_config(runtime: &OrbitRuntime, content: &str) {
    std::fs::write(runtime.global_root().join("config.toml"), content)
        .expect("write global config");
}

fn write_workspace_config(runtime: &OrbitRuntime, content: &str) {
    let path = runtime.shared_root().join("config.toml");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create workspace config parent");
    }
    std::fs::write(path, content).expect("write workspace config");
}

/// A workspace file that already names its default crew, as `orbit init`
/// writes it. A file that defines `[crews.*]` at all must resolve
/// `workflow.default_crew` within itself — single-file admission, same as
/// `orbit config set` — so a fixture without it would be testing that rule
/// rather than the crew endpoints.
const WORKSPACE_CONFIG_WITH_CREWS: &str = "[workflow]\ndefault_crew = \"opus\"\n\n[crews.opus]\nprovider = \"claude\"\nmodel = \"opus\"\n";

fn read_workspace_config(runtime: &OrbitRuntime) -> String {
    std::fs::read_to_string(runtime.shared_root().join("config.toml"))
        .expect("read workspace config")
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

/// Pin the process signals `CallerCapabilities::resolve` reads for the whole
/// request, exactly as the auto-task tests do: a sibling test setting the
/// override concurrently would otherwise turn an expected 403 into a 200.
#[allow(clippy::await_holding_lock)]
async fn with_caller_env<'a, T>(
    vars: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    fut: impl std::future::Future<Output = T>,
) -> T {
    let _env = orbit_common::test_env::scoped(vars);
    fut.await
}

async fn as_operator<T>(fut: impl std::future::Future<Output = T>) -> T {
    with_caller_env([(OPERATOR_OVERRIDE_ENV, Some("1"))], fut).await
}

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

fn section<'a>(view: &'a Value, token: &str) -> &'a Value {
    view["sections"]
        .as_array()
        .expect("sections")
        .iter()
        .find(|section| section["token"] == token)
        .unwrap_or_else(|| panic!("section '{token}' is rendered"))
}

fn row<'a>(view: &'a Value, token: &str, key: &str) -> &'a Value {
    section(view, token)["keys"]
        .as_array()
        .expect("section keys")
        .iter()
        .find(|row| row["key"] == key)
        .unwrap_or_else(|| panic!("row '{key}' is rendered"))
}

#[tokio::test]
async fn effective_view_groups_every_registry_key_into_its_section() {
    let (state, _) = state(runtime());
    let response = send(
        state,
        Method::GET,
        "/config/effective?workspace=default",
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let view = body_json(response).await;

    let tokens: Vec<&str> = view["sections"]
        .as_array()
        .expect("sections")
        .iter()
        .map(|section| section["token"].as_str().expect("token"))
        .collect();
    assert_eq!(
        tokens,
        vec![
            "delivery",
            "crews",
            "execution",
            "operation",
            "housekeeping"
        ],
        "{view}"
    );

    let registry_rows: usize = view["sections"]
        .as_array()
        .expect("sections")
        .iter()
        .map(|section| section["keys"].as_array().expect("keys").len())
        .sum();
    assert_eq!(
        registry_rows,
        orbit_config_key_count(),
        "every settable key has a home in exactly one section"
    );

    let delivery = section(&view, "delivery");
    let counts = &delivery["counts"];
    assert_eq!(
        counts["total"].as_u64().expect("total"),
        counts["set"].as_u64().expect("set")
            + counts["default"].as_u64().expect("default")
            + counts["unset"].as_u64().expect("unset"),
        "the count chip accounts for every row in the section"
    );
    assert!(!view["paths"].as_array().expect("paths").is_empty());
}

/// The registry is the authority for how many settable keys exist; counting it
/// here keeps the section assertion honest when a key is added or retired.
fn orbit_config_key_count() -> usize {
    let catalog = orbit_core::application::config::key_catalog();
    catalog["keys"].as_array().expect("keys").len()
}

#[tokio::test]
async fn effective_view_names_the_global_value_a_workspace_shadows() {
    let runtime = runtime();
    write_global_config(&runtime, "[workflow]\nbase_branch = \"trunk\"\n");
    write_workspace_config(&runtime, "[workflow]\nbase_branch = \"agent-main\"\n");
    let (state, _) = state(runtime);

    let view = body_json(
        send(
            state,
            Method::GET,
            "/config/effective?workspace=default",
            None,
        )
        .await,
    )
    .await;
    let base_branch = row(&view, "delivery", "workflow.base_branch");
    assert_eq!(base_branch["value"], "agent-main");
    assert_eq!(base_branch["state"], "set");
    assert_eq!(base_branch["source"]["layer"], "workspace");
    let shadow = &base_branch["shadowed_by"][0];
    assert_eq!(shadow["layer"], "global");
    assert_eq!(shadow["value"], "trunk");
    assert_eq!(shadow["reason"], "overridden");
    assert_eq!(shadow["note"], "overrides global: trunk");
}

#[tokio::test]
async fn effective_view_flags_a_global_execution_value_that_was_not_inherited() {
    let runtime = runtime();
    write_global_config(
        &runtime,
        "[execution.codex]\nsandbox = \"danger-full-access\"\n",
    );
    write_workspace_config(&runtime, "[workflow]\nbase_branch = \"agent-main\"\n");
    let (state, _) = state(runtime);

    let view = body_json(
        send(
            state,
            Method::GET,
            "/config/effective?workspace=default",
            None,
        )
        .await,
    )
    .await;
    assert_eq!(view["layers"]["execution_not_inherited"], true);
    assert_eq!(
        view["layers"]["not_inherited_keys"],
        serde_json::json!(["execution.codex.sandbox"])
    );

    let sandbox = row(&view, "execution", "execution.codex.sandbox");
    assert_eq!(sandbox["value"], "workspace-write");
    let shadow = &sandbox["shadowed_by"][0];
    assert_eq!(shadow["reason"], "not-inherited");
    assert_eq!(
        shadow["note"],
        "global sets danger-full-access — not inherited while a workspace file exists"
    );
    assert_eq!(section(&view, "execution")["not_inherited"], 1);
    // The editor's choices come from the registry, never from the dashboard.
    assert_eq!(
        sandbox["options"],
        serde_json::json!(["read-only", "workspace-write", "danger-full-access"])
    );
}

/// Without a workspace file the security exception is not in force, so the
/// warning must be absent rather than merely unrendered by the client.
#[tokio::test]
async fn effective_view_omits_the_inheritance_warning_without_a_workspace_file() {
    let runtime = runtime();
    write_global_config(
        &runtime,
        "[execution.codex]\nsandbox = \"danger-full-access\"\n",
    );
    let (state, _) = state(runtime);

    let view = body_json(
        send(
            state,
            Method::GET,
            "/config/effective?workspace=default",
            None,
        )
        .await,
    )
    .await;
    assert_eq!(view["layers"]["execution_not_inherited"], false);
    assert_eq!(view["layers"]["not_inherited_keys"], serde_json::json!([]));
    assert_eq!(
        row(&view, "execution", "execution.codex.sandbox")["value"],
        "danger-full-access"
    );
}

#[tokio::test]
async fn file_view_resolves_one_scope_in_isolation() {
    let runtime = runtime();
    write_global_config(&runtime, "[workflow]\nbase_branch = \"trunk\"\n");
    write_workspace_config(&runtime, "[workflow]\nbase_branch = \"agent-main\"\n");
    let (state, _) = state(runtime);

    let view = body_json(
        send(
            state,
            Method::GET,
            "/config/file?scope=global&workspace=default",
            None,
        )
        .await,
    )
    .await;
    assert_eq!(view["scope"], "global");
    let base_branch = row(&view, "delivery", "workflow.base_branch");
    assert_eq!(base_branch["value"], "trunk");
    assert_eq!(base_branch["source"]["layer"], "global");
    assert!(
        base_branch["shadowed_by"]
            .as_array()
            .expect("shadowed_by")
            .is_empty(),
        "one file in isolation has no layer to shadow"
    );
}

#[tokio::test]
async fn file_view_rejects_an_unknown_scope() {
    let (state, _) = state(runtime());
    let response = send(
        state,
        Method::GET,
        "/config/file?scope=effective&workspace=default",
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn key_catalog_publishes_types_sections_and_choices() {
    let (state, _) = state(runtime());
    let catalog =
        body_json(send(state, Method::GET, "/config/keys?workspace=default", None).await).await;
    let keys = catalog["keys"].as_array().expect("keys");
    let sandbox = keys
        .iter()
        .find(|key| key["key"] == "execution.codex.sandbox")
        .expect("sandbox key is published");
    assert_eq!(sandbox["section"], "execution");
    assert_eq!(sandbox["value_type"], "string");
    assert!(
        sandbox["options"]
            .as_array()
            .expect("options")
            .contains(&Value::from("workspace-write"))
    );
    assert!(
        catalog["sections"].as_array().expect("sections").len() >= 5,
        "{catalog}"
    );
}

#[tokio::test]
async fn writing_a_key_targets_the_workspace_file_and_re_resolves_the_row() {
    let runtime = runtime();
    write_global_config(&runtime, "[workflow]\nbase_branch = \"trunk\"\n");
    write_workspace_config(&runtime, "# operator notes\n");
    let (state, runtime) = state(runtime);

    let response = as_operator(send(
        state,
        Method::PUT,
        "/config/keys/workflow.base_branch?workspace=default",
        Some(r#"{"value":"agent-main"}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["scope"], "workspace");
    assert_eq!(body["old_value"], "trunk");
    assert_eq!(body["new_value"], "agent-main");
    assert_eq!(body["rows"][0]["source"]["layer"], "workspace");
    assert_eq!(body["rows"][0]["state"], "set");

    let written = read_workspace_config(&runtime);
    assert!(
        written.contains("base_branch = \"agent-main\""),
        "{written}"
    );
    assert!(
        written.contains("# operator notes"),
        "an edit preserves the rest of the file: {written}"
    );
}

/// A string value stays a string: inferring `"true"` as a boolean would write
/// a value the key's own admission never saw.
#[tokio::test]
async fn writing_a_typed_value_keeps_its_json_type() {
    let runtime = runtime();
    write_workspace_config(&runtime, "");
    let (state, runtime) = state(runtime);

    let response = as_operator(send(
        state,
        Method::PUT,
        "/config/keys/scoring.enabled?workspace=default",
        Some(r#"{"value":false}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        read_workspace_config(&runtime).contains("enabled = false"),
        "{}",
        read_workspace_config(&runtime)
    );
}

#[tokio::test]
async fn a_refused_value_returns_the_admission_error_verbatim() {
    let runtime = runtime();
    write_workspace_config(&runtime, "");
    let (state, _) = state(runtime);

    let response = as_operator(send(
        state,
        Method::PUT,
        "/config/keys/execution.codex.sandbox?workspace=default",
        Some(r#"{"value":"wide-open"}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    let message = body["error"].as_str().expect("error message");
    assert!(
        message.contains("execution.codex.sandbox has invalid value 'wide-open'")
            && message.contains("workspace-write"),
        "{message}"
    );
}

#[tokio::test]
async fn an_unknown_key_is_refused_before_any_write() {
    let runtime = runtime();
    write_workspace_config(&runtime, "");
    let (state, runtime) = state(runtime);

    let response = as_operator(send(
        state,
        Method::PUT,
        "/config/keys/workflow.base_brunch?workspace=default",
        Some(r#"{"value":"main"}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(read_workspace_config(&runtime), "");
}

/// The fail-closed first write: a bare edit never creates the workspace file,
/// because creating it silently moves the `execution.*` keys off global policy.
#[tokio::test]
async fn writing_without_a_workspace_file_refuses_until_initialization_is_chosen() {
    let (state, runtime) = state(runtime());

    let refused = as_operator(send(
        state.clone(),
        Method::PUT,
        "/config/keys/workflow.base_branch?workspace=default",
        Some(r#"{"value":"agent-main"}"#),
    ))
    .await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert!(!runtime.shared_root().join("config.toml").exists());

    let accepted = as_operator(send(
        state,
        Method::PUT,
        "/config/keys/workflow.base_branch?workspace=default",
        Some(r#"{"value":"agent-main","init":"fresh"}"#),
    ))
    .await;
    assert_eq!(accepted.status(), StatusCode::OK);
    assert!(read_workspace_config(&runtime).contains("agent-main"));
}

#[tokio::test]
async fn clearing_a_key_restores_the_layer_below() {
    let runtime = runtime();
    write_global_config(&runtime, "[workflow]\nbase_branch = \"trunk\"\n");
    write_workspace_config(&runtime, "[workflow]\nbase_branch = \"agent-main\"\n");
    let (state, _) = state(runtime);

    let response = as_operator(send(
        state,
        Method::DELETE,
        "/config/keys/workflow.base_branch?workspace=default",
        None,
    ))
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["old_value"], "agent-main");
    assert_eq!(body["new_value"], "trunk");
    assert_eq!(body["rows"][0]["source"]["layer"], "global");
}

#[tokio::test]
async fn a_write_denies_a_caller_without_operator_capability() {
    let runtime = runtime();
    write_workspace_config(&runtime, "");
    let (state, runtime) = state(runtime);

    let response = as_agent(send(
        state,
        Method::PUT,
        "/config/keys/workflow.base_branch?workspace=default",
        Some(r#"{"value":"agent-main"}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = body_json(response).await;
    assert_eq!(body["code"], "authorization_denied");
    assert_eq!(body["operation"], "config.set");
    assert_eq!(read_workspace_config(&runtime), "");
}

#[tokio::test]
async fn an_accepted_write_records_a_config_set_audit_event() {
    let runtime = runtime();
    write_global_config(&runtime, "[workflow]\nbase_branch = \"trunk\"\n");
    write_workspace_config(&runtime, "");
    let (state, runtime) = state(runtime);

    let response = as_operator(send(
        state,
        Method::PUT,
        "/config/keys/workflow.base_branch?workspace=default",
        Some(r#"{"value":"agent-main"}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let events = runtime
        .list_audit_events_with_kind(None, None, Some("config.set".to_string()), None, None, 20)
        .expect("list audit events");
    let event = events
        .iter()
        .find(|event| event.target_id.as_deref() == Some("workflow.base_branch"))
        .expect("config.set audit event is recorded");
    assert_eq!(event.status.to_string(), "success");
    let arguments: Value = serde_json::from_str(
        event
            .arguments_json
            .as_deref()
            .expect("audit arguments are recorded"),
    )
    .expect("audit arguments are json");
    assert_eq!(arguments["key"], "workflow.base_branch");
    assert_eq!(arguments["scope"], "workspace");
    assert_eq!(arguments["old_value"], "trunk");
    assert_eq!(arguments["new_value"], "agent-main");
}

#[tokio::test]
async fn a_crew_write_creates_the_table_and_annotates_references() {
    let runtime = runtime();
    write_workspace_config(&runtime, WORKSPACE_CONFIG_WITH_CREWS);
    let (state, _) = state(runtime);

    let response = as_operator(send(
        state.clone(),
        Method::PUT,
        "/config/crews/reviewer?workspace=default",
        Some(r#"{"fields":{"provider":"claude","model":"opus","tags":["review"]}}"#),
    ))
    .await;
    let status = response.status();
    let body = body_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let view = body_json(
        send(
            state,
            Method::GET,
            "/config/effective?workspace=default",
            None,
        )
        .await,
    )
    .await;
    let crew = view["crews"]
        .as_array()
        .expect("crews")
        .iter()
        .find(|crew| crew["name"] == "reviewer")
        .expect("the new crew is rendered");
    assert_eq!(crew["provider"], "claude");
    assert_eq!(crew["model"], "opus");
    assert_eq!(crew["tags"], serde_json::json!(["review"]));
    assert_eq!(crew["source"], "workspace");

    let default_crew = view["crews"]
        .as_array()
        .expect("crews")
        .iter()
        .find(|crew| {
            crew["referenced_by"]
                .as_array()
                .expect("referenced_by")
                .contains(&Value::from("workflow.default_crew"))
        })
        .expect("the default crew is annotated");
    assert_eq!(default_crew["name"], "opus");
}

#[tokio::test]
async fn deleting_a_referenced_crew_is_refused_with_the_key_that_names_it() {
    let runtime = runtime();
    write_workspace_config(
        &runtime,
        "[workflow]\ndefault_crew = \"reviewer\"\n\n[crews.reviewer]\nprovider = \"claude\"\nmodel = \"opus\"\n",
    );
    let (state, runtime) = state(runtime);

    let response = as_operator(send(
        state,
        Method::DELETE,
        "/config/crews/reviewer?workspace=default",
        None,
    ))
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let message = body_json(response).await["error"]
        .as_str()
        .expect("error message")
        .to_string();
    assert!(message.contains("workflow.default_crew"), "{message}");
    assert!(
        read_workspace_config(&runtime).contains("[crews.reviewer]"),
        "a refused delete leaves the table in place"
    );
}

#[tokio::test]
async fn deleting_an_unreferenced_crew_removes_its_table() {
    let runtime = runtime();
    write_workspace_config(
        &runtime,
        &format!(
            "{WORKSPACE_CONFIG_WITH_CREWS}\n[crews.reviewer]\nprovider = \"claude\"\nmodel = \"opus\"\n"
        ),
    );
    let (state, runtime) = state(runtime);

    let response = as_operator(send(
        state,
        Method::DELETE,
        "/config/crews/reviewer?workspace=default",
        None,
    ))
    .await;
    let status = response.status();
    let body = body_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!read_workspace_config(&runtime).contains("[crews.reviewer]"));
}

#[tokio::test]
async fn writing_an_ignored_optional_crew_property_is_refused_and_leaves_config_byte_identical() {
    let runtime = runtime();
    write_workspace_config(&runtime, WORKSPACE_CONFIG_WITH_CREWS);
    let (state, runtime) = state(runtime);

    // PUT /config/keys/crews.<name>.effort (set_key path)
    let response = as_operator(send(
        state.clone(),
        Method::PUT,
        "/config/keys/crews.opus.effort?workspace=default",
        Some(r#"{"value":"medium-low"}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    let message = body["error"].as_str().expect("error message");
    assert!(
        message.contains("expected one of low, medium, high, xhigh, max"),
        "{message}"
    );
    assert_eq!(read_workspace_config(&runtime), WORKSPACE_CONFIG_WITH_CREWS);

    // PUT /config/crews/<name> (set_crew path)
    let response = as_operator(send(
        state,
        Method::PUT,
        "/config/crews/opus?workspace=default",
        Some(r#"{"fields":{"effort":"medium-low"}}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    let message = body["error"].as_str().expect("error message");
    assert!(
        message.contains("expected one of low, medium, high, xhigh, max"),
        "{message}"
    );
    assert_eq!(read_workspace_config(&runtime), WORKSPACE_CONFIG_WITH_CREWS);
}

#[tokio::test]
async fn writing_unsupported_provider_crew_effort_is_refused_and_leaves_config_byte_identical() {
    let runtime = runtime();
    let original = "[workflow]\ndefault_crew = \"gemini\"\n\n[crews.gemini]\nmodel = \"gemini\"\nprovider = \"gemini\"\n";
    write_workspace_config(&runtime, original);
    let (state, runtime) = state(runtime);

    // PUT /config/keys/crews.<name>.effort
    let response = as_operator(send(
        state.clone(),
        Method::PUT,
        "/config/keys/crews.gemini.effort?workspace=default",
        Some(r#"{"value":"high"}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    let message = body["error"].as_str().expect("error message");
    assert!(
        message.contains("does not support configured reasoning effort"),
        "{message}"
    );
    assert_eq!(read_workspace_config(&runtime), original);

    // PUT /config/crews/<name>
    let response = as_operator(send(
        state,
        Method::PUT,
        "/config/crews/gemini?workspace=default",
        Some(r#"{"fields":{"effort":"high"}}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    let message = body["error"].as_str().expect("error message");
    assert!(
        message.contains("does not support configured reasoning effort"),
        "{message}"
    );
    assert_eq!(read_workspace_config(&runtime), original);
}
