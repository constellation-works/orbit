//! `/api/plugins` and `/api/plugins/<ns>/panels/<id>` [§4.7].
//!
//! The fixture installs a real plugin through the ordinary lifecycle and
//! then reopens the runtime, so the listing and the panel read answer from
//! the same load pass the tool surface was built from. Each test runs in an
//! isolated child of this test binary, so a managed run's inherited worker
//! binding, tool allowlist or plugin broker never decides the outcome.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use orbit_common::governance::authorization::OPERATOR_OVERRIDE_ENV;
use orbit_core::adapter::command::{PluginAddOptions, PluginEnableOptions, PluginSecretValue};
use orbit_core::runtime::WorkspaceRuntimeBinding;
use orbit_core::{OrbitRuntime, ShipMode};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

use super::super::router;
use super::test_support::{assert_isolated_child, body_json, enter_isolated_child};
use crate::state::{DashboardState, WsEntry};

struct PluginFixture {
    _temp: TempDir,
    global_root: PathBuf,
    workspace_root: PathBuf,
}

impl PluginFixture {
    fn new() -> Self {
        assert_isolated_child();
        let temp = tempfile::tempdir().expect("tempdir");
        let global_root = temp.path().join("global");
        let workspace_root = temp.path().join("repo/.orbit");
        for dir in [&global_root, &workspace_root] {
            std::fs::create_dir_all(dir).expect("create fixture dir");
        }
        Self {
            _temp: temp,
            global_root,
            workspace_root,
        }
    }

    fn runtime(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.global_root, &self.workspace_root).expect("build runtime")
    }

    fn source(&self) -> PathBuf {
        self._temp.path().join("sources/panels/.orbit-plugin")
    }

    fn dashboard_state(&self) -> DashboardState {
        let binding_id = self.runtime().workspace_id().expect("workspace id");
        DashboardState::global(
            self.global_root.clone(),
            vec![WsEntry {
                id: "ws_plugins".to_string(),
                name: "plugins".to_string(),
                repo_root: self._temp.path().join("repo"),
                orbit_dir: self.workspace_root.clone(),
                binding: Some(WorkspaceRuntimeBinding {
                    logical_workspace_id: binding_id.clone(),
                    task_partition_id: binding_id,
                    owner_machine_id: None,
                    repo_root: self._temp.path().join("repo"),
                    ship_mode: ShipMode::Local,
                    base_branch: None,
                }),
                active: true,
            }],
            Some("ws_plugins".to_string()),
        )
    }
}

/// A plugin with one `read_only` tool and one `kv` panel over it.
fn write_plugin(root: &Path) {
    std::fs::create_dir_all(root.join("bin")).expect("create plugin dirs");
    let backend = root.join("bin/backend.sh");
    std::fs::write(
        &backend,
        "#!/bin/sh\ncat > /dev/null\nprintf '{\"ok\":true,\"output\":{\"indexed\":7,\"state\":\"ready\"}}\\n'\n",
    )
    .expect("write backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755))
            .expect("chmod backend");
    }
    std::fs::write(
        root.join("plugin.yaml"),
        r#"schemaVersion: 2
kind: Plugin
metadata:
  name: panels
  version: 0.1.0
  description: Panel fixture.
spec:
  backend:
    type: exec
    command: bin/backend.sh
  tools:
    - name: status
      description: Report status.
      execution_kind: read_only
      mcp_scope: workspace
  web:
    panels:
      - id: status
        title: Index
        source: tool:status
        render: kv
        refresh_ms: 1000
    links:
      - title: Explorer
        url: http://127.0.0.1:7890/
"#,
    )
    .expect("write manifest");
}

async fn get(state: DashboardState, uri: &str) -> axum::response::Response {
    router()
        .with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .header("host", "localhost:7878")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

async fn post(state: DashboardState, uri: &str, scope: &str) -> axum::response::Response {
    router()
        .with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header("host", "localhost:7878")
                .header("origin", "http://localhost:7878")
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"scope":"{scope}"}}"#)))
                .expect("request"),
        )
        .await
        .expect("response")
}

#[allow(clippy::await_holding_lock)]
async fn as_operator<T>(fut: impl std::future::Future<Output = T>) -> T {
    let _env = orbit_common::test_env::scoped([(OPERATOR_OVERRIDE_ENV, Some("1"))]);
    fut.await
}

#[allow(clippy::await_holding_lock)]
async fn as_agent<T>(fut: impl std::future::Future<Output = T>) -> T {
    let _env = orbit_common::test_env::scoped([
        (OPERATOR_OVERRIDE_ENV, None),
        ("ORBIT_AGENT_NAME", Some("plugin-api-test")),
        ("ORBIT_AGENT_MODEL", Some("plugin-api-test")),
    ]);
    fut.await
}

fn audit(runtime: &OrbitRuntime, operation: &str) -> Vec<orbit_types::telemetry::AuditEvent> {
    runtime
        .list_audit_events_with_kind(None, None, Some(operation.to_string()), None, None, 20)
        .expect("plugin audit events")
}

fn state(runtime: OrbitRuntime) -> DashboardState {
    DashboardState::single(Arc::new(runtime))
}

#[cfg(unix)]
#[tokio::test]
async fn operator_can_toggle_both_scopes_and_each_write_is_audited() {
    if !enter_isolated_child(
        module_path!(),
        "operator_can_toggle_both_scopes_and_each_write_is_audited",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    write_plugin(&fixture.source());
    let runtime = fixture.runtime();
    runtime
        .add_plugin(
            fixture.source().to_str().expect("source"),
            &PluginAddOptions::default(),
        )
        .expect("install");
    runtime
        .enable_plugin("panels", &PluginEnableOptions::default())
        .expect("record empty-grant consent");
    let state = fixture.dashboard_state();
    state.set_operator_session(true);

    for (path, scope, expected_host, expected_toggle) in [
        ("/plugins/panels/disable", "workspace", true, false),
        ("/plugins/panels/enable", "workspace", true, true),
        ("/plugins/panels/disable", "host", false, true),
        ("/plugins/panels/enable", "host", true, true),
    ] {
        let response = as_operator(post(state.clone(), path, scope)).await;
        let status = response.status();
        let payload = body_json(response).await;
        assert_eq!(status, StatusCode::OK, "{payload}");
        assert_eq!(payload["plugin"]["host_enabled"], expected_host);
        assert_eq!(payload["plugin"]["workspace_toggle"], expected_toggle);
        let listed = body_json(get(state.clone(), "/plugins").await).await;
        assert_eq!(
            listed[0]["host_enabled"], expected_host,
            "same dashboard state observes the host write without restart"
        );
        assert_eq!(listed[0]["workspace_toggle"], expected_toggle);
    }
    assert_eq!(
        audit(&runtime, "plugin.disable")
            .iter()
            .filter(|event| event.status.to_string() == "success")
            .count(),
        2
    );
    assert_eq!(
        audit(&runtime, "plugin.enable")
            .iter()
            .filter(|event| event.status.to_string() == "success")
            .count(),
        2
    );
    let listed = body_json(get(state, "/plugins").await).await;
    assert_eq!(
        listed[0]["tools"][0]["active"], true,
        "the tool surface refreshes after re-enable"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn non_operator_is_refused_and_audited_before_any_plugin_change() {
    if !enter_isolated_child(
        module_path!(),
        "non_operator_is_refused_and_audited_before_any_plugin_change",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    write_plugin(&fixture.source());
    let runtime = fixture.runtime();
    runtime
        .add_plugin(
            fixture.source().to_str().expect("source"),
            &PluginAddOptions::default(),
        )
        .expect("install");
    let state = fixture.dashboard_state();
    let listed = as_agent(get(state.clone(), "/plugins")).await;
    let listed = body_json(listed).await;
    for action in ["enable", "disable"] {
        assert_eq!(listed[0]["capabilities"][action]["authorized"], false);
        let response = as_agent(post(
            state.clone(),
            &format!("/plugins/panels/{action}"),
            "host",
        ))
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let events = audit(&runtime, &format!("plugin.{action}"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status.to_string(), "denied");
    }
    assert!(!runtime.list_plugins().expect("list")[0].host_enabled);
}

#[cfg(unix)]
#[tokio::test]
async fn host_enable_requires_recorded_matching_grants_and_program_paths() {
    if !enter_isolated_child(
        module_path!(),
        "host_enable_requires_recorded_matching_grants_and_program_paths",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    write_plugin(&fixture.source());
    let manifest = fixture.source().join("plugin.yaml");
    let mut document = std::fs::read_to_string(&manifest).expect("manifest");
    document.push_str("  permissions:\n    network: loopback\n  requires:\n    programs: [sh]\n");
    std::fs::write(&manifest, document).expect("request grant and program");
    let runtime = fixture.runtime();
    runtime
        .add_plugin(
            fixture.source().to_str().expect("source"),
            &PluginAddOptions::default(),
        )
        .expect("install");
    let state = fixture.dashboard_state();
    state.set_operator_session(true);

    let response = as_operator(post(state.clone(), "/plugins/panels/enable", "host")).await;
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "unrecorded consent is refused"
    );
    let payload = body_json(response).await;
    assert!(
        payload["error"]
            .as_str()
            .is_some_and(|message| message.contains("network")
                && message.contains("orbit plugin enable panels --grant network")),
        "{payload}"
    );

    runtime
        .enable_plugin("panels", &PluginEnableOptions::default())
        .expect("record an empty grant set");
    runtime.disable_plugin("panels").expect("disable");
    let response = as_operator(post(state.clone(), "/plugins/panels/enable", "host")).await;
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "changed requested grants are refused"
    );
    assert_eq!(body_json(response).await["code"], "plugin_refused");

    runtime
        .enable_plugin(
            "panels",
            &PluginEnableOptions {
                grants: vec!["network".to_string()],
                force: false,
            },
        )
        .expect("record requested grant and program");
    runtime.disable_plugin("panels").expect("disable");
    let witness_path = fixture.global_root.join("plugins/.grants/panels.json");
    let mut witness: Value =
        serde_json::from_str(&std::fs::read_to_string(&witness_path).expect("witness"))
            .expect("witness JSON");
    witness["programs"]["sh"] = serde_json::json!("/changed/sh");
    std::fs::write(
        &witness_path,
        serde_json::to_vec(&witness).expect("serialize witness"),
    )
    .expect("change recorded path");
    let response = as_operator(post(state.clone(), "/plugins/panels/enable", "host")).await;
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "changed program path is refused"
    );
    assert!(
        body_json(response).await["error"]
            .as_str()
            .is_some_and(|message| message.contains("orbit plugin enable panels"))
    );
    assert!(!runtime.list_plugins().expect("list")[0].host_enabled);

    runtime
        .enable_plugin("panels", &PluginEnableOptions::default())
        .expect("CLI reviews paths");
    runtime.disable_plugin("panels").expect("disable");
    let response = as_operator(post(state, "/plugins/panels/enable", "host")).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "matching consent permits re-enable"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_enable_under_host_off_and_unknown_plugin_have_structured_responses() {
    if !enter_isolated_child(
        module_path!(),
        "workspace_enable_under_host_off_and_unknown_plugin_have_structured_responses",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    write_plugin(&fixture.source());
    let runtime = fixture.runtime();
    runtime
        .add_plugin(
            fixture.source().to_str().expect("source"),
            &PluginAddOptions::default(),
        )
        .expect("install");
    let state = fixture.dashboard_state();
    state.set_operator_session(true);
    let response = as_operator(post(state.clone(), "/plugins/panels/enable", "workspace")).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(response).await["code"], "PluginDisabledOnHost");
    let response = as_operator(post(state, "/plugins/unknown/disable", "host")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[cfg(unix)]
#[tokio::test]
async fn pinned_but_missing_install_has_a_structured_enable_refusal() {
    if !enter_isolated_child(
        module_path!(),
        "pinned_but_missing_install_has_a_structured_enable_refusal",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    std::fs::write(
        fixture.workspace_root.join("plugins.yaml"),
        "schemaVersion: 1\nplugins:\n  - name: panels\n    source: /missing/plugin\n    enabled: true\n",
    )
    .expect("pin missing plugin");
    let state = fixture.dashboard_state();
    state.set_operator_session(true);
    let response = as_operator(post(state, "/plugins/panels/enable", "host")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = body_json(response).await;
    assert_eq!(payload["code"], "plugin_refused");
    assert!(
        payload["error"]
            .as_str()
            .is_some_and(|message| message.contains("orbit plugin sync")),
        "{payload}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn plugins_list_reports_enable_state_and_a_panel_serves_its_read_only_tool() {
    if !enter_isolated_child(
        module_path!(),
        "plugins_list_reports_enable_state_and_a_panel_serves_its_read_only_tool",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let source = fixture.source();
    write_plugin(&source);

    let runtime = fixture.runtime();
    runtime
        .add_plugin(
            source.to_str().expect("utf8 source"),
            &PluginAddOptions::default(),
        )
        .expect("install the fixture plugin");
    let state = fixture.dashboard_state();

    // This request builds and caches the web runtime while the plugin is
    // disabled. The same DashboardState must observe the lifecycle write
    // below without a server restart.
    let payload = body_json(get(state.clone(), "/plugins").await).await;
    let plugin = &payload
        .as_array()
        .unwrap_or_else(|| panic!("an array of plugins, got {payload}"))[0];
    assert_eq!(plugin["name"], "panels");
    assert_eq!(plugin["status"], "disabled");
    assert_eq!(plugin["enabled"], false);
    assert!(
        plugin["diagnostic"].is_null() || plugin["diagnostic"].as_str().is_some(),
        "the listing carries the plugin's diagnostic slot"
    );
    assert_eq!(
        plugin["panels"].as_array().map(Vec::len),
        Some(0),
        "a plugin that is not serving its tools advertises no panel to read"
    );

    runtime
        .enable_plugin("panels", &PluginEnableOptions::default())
        .expect("enable the fixture plugin");

    let payload = body_json(get(state.clone(), "/plugins").await).await;
    let plugin = &payload.as_array().expect("an array of plugins")[0];
    assert_eq!(plugin["status"], "active");
    assert_eq!(plugin["enabled"], true);
    assert_eq!(plugin["panels"][0]["id"], "status");
    assert_eq!(plugin["panels"][0]["render"], "kv");
    assert_eq!(plugin["panels"][0]["tool"], "panels.status");
    assert_eq!(plugin["links"][0]["url"], "http://127.0.0.1:7890/");
    assert_eq!(plugin["tools"][0]["execution_kind"], "read_only");

    let response = get(state, "/plugins/panels/panels/status").await;
    let status = response.status();
    let payload = body_json(response).await;
    assert_eq!(status, StatusCode::OK, "{payload}");
    assert_eq!(
        payload["output"],
        serde_json::json!({ "indexed": 7, "state": "ready" }),
        "the panel serves the source tool's output"
    );
}

/// A plugin's secret value never reaches the dashboard: `/api/plugins`
/// answers from the same records `orbit plugin show` does, and neither holds
/// a value.
#[cfg(unix)]
#[tokio::test]
async fn plugins_list_never_carries_a_secret_value() {
    if !enter_isolated_child(module_path!(), "plugins_list_never_carries_a_secret_value") {
        return;
    }
    const SECRET: &str = "orbit-web-secret-4d8e2a";
    let fixture = PluginFixture::new();
    let source = fixture.source();
    write_plugin(&source);
    let manifest = source.join("plugin.yaml");
    let mut document = std::fs::read_to_string(&manifest).expect("read manifest");
    document.push_str("  secrets:\n    - name: token\n      description: API token.\n");
    std::fs::write(&manifest, document).expect("declare a secret");

    let runtime = fixture.runtime();
    runtime
        .add_plugin(
            source.to_str().expect("utf8 source"),
            &PluginAddOptions {
                enable: true,
                ..PluginAddOptions::default()
            },
        )
        .expect("install and enable the fixture plugin");
    runtime
        .set_plugin_secret(
            "panels",
            "token",
            &PluginSecretValue::new(SECRET.to_string()).expect("secret value"),
        )
        .expect("set the declared secret");

    let response = get(fixture.dashboard_state(), "/plugins").await;
    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    assert_eq!(payload[0]["name"], "panels", "{payload}");
    assert!(
        !payload.to_string().contains(SECRET),
        "/api/plugins must never carry a secret value: {payload}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn concurrent_panel_reads_write_one_audit_row_per_ttl_window() {
    if !enter_isolated_child(
        module_path!(),
        "concurrent_panel_reads_write_one_audit_row_per_ttl_window",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let source = fixture.source();
    write_plugin(&source);
    let runtime = fixture.runtime();
    runtime
        .add_plugin(
            source.to_str().expect("utf8 source"),
            &PluginAddOptions::default(),
        )
        .expect("install plugin");
    runtime
        .enable_plugin("panels", &PluginEnableOptions::default())
        .expect("enable plugin");
    let state = fixture.dashboard_state();

    let (first, second) = tokio::join!(
        get(state.clone(), "/plugins/panels/panels/status"),
        get(state.clone(), "/plugins/panels/panels/status"),
    );
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        runtime
            .list_audit_events(None, Some("panels.status".to_string()), None, None, 10)
            .expect("audit rows")
            .len(),
        1,
        "overlapping tabs must share one audited panel execution"
    );

    tokio::time::sleep(std::time::Duration::from_millis(1_050)).await;
    let third = get(state, "/plugins/panels/panels/status").await;
    assert_eq!(third.status(), StatusCode::OK);
    assert_eq!(
        runtime
            .list_audit_events(None, Some("panels.status".to_string()), None, None, 10)
            .expect("audit rows")
            .len(),
        2,
        "the next TTL window admits exactly one new execution"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oversized_panel_output_is_replaced_by_a_bounded_diagnostic_payload() {
    if !enter_isolated_child(
        module_path!(),
        "oversized_panel_output_is_replaced_by_a_bounded_diagnostic_payload",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let source = fixture.source();
    write_plugin(&source);
    std::fs::write(
        source.join("bin/backend.sh"),
        "#!/bin/sh\ncat > /dev/null\nprintf '{\"ok\":true,\"output\":\"'\nhead -c 300000 /dev/zero | tr '\\000' x\nprintf '\"}\\n'\n",
    )
    .expect("large-output backend");
    let runtime = fixture.runtime();
    runtime
        .add_plugin(
            source.to_str().expect("utf8 source"),
            &PluginAddOptions::default(),
        )
        .expect("install plugin");
    runtime
        .enable_plugin("panels", &PluginEnableOptions::default())
        .expect("enable plugin");

    let response = get(fixture.dashboard_state(), "/plugins/panels/panels/status").await;
    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    assert_eq!(payload["truncated"], true);
    assert!(
        payload["diagnostic"]
            .as_str()
            .is_some_and(|message| message.contains("above the 262144-byte limit")),
        "a truncated response explains its ceiling: {payload}"
    );
    assert!(
        payload["output"]
            .as_str()
            .is_some_and(|output| output.len() < 262_144),
        "the retained prefix stays below the ceiling"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_undeclared_panel_is_not_found() {
    if !enter_isolated_child(module_path!(), "an_undeclared_panel_is_not_found") {
        return;
    }
    let fixture = PluginFixture::new();
    let source = fixture.source();
    write_plugin(&source);
    let runtime = fixture.runtime();
    runtime
        .add_plugin(
            source.to_str().expect("utf8 source"),
            &PluginAddOptions::default(),
        )
        .expect("install the fixture plugin");
    runtime
        .enable_plugin("panels", &PluginEnableOptions::default())
        .expect("enable the fixture plugin");

    // Only a declared panel is reachable: the endpoint never takes a tool
    // name from the caller, so there is nothing to point at a mutating tool.
    let response = get(state(fixture.runtime()), "/plugins/panels/panels/status2").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = get(state(fixture.runtime()), "/plugins/absent/panels/status").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_path_segment_that_is_not_an_identifier_is_refused() {
    if !enter_isolated_child(
        module_path!(),
        "a_path_segment_that_is_not_an_identifier_is_refused",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let response = get(state(fixture.runtime()), "/plugins/..%2Fx/panels/status").await;
    assert!(
        response.status() == StatusCode::BAD_REQUEST || response.status() == StatusCode::NOT_FOUND,
        "a traversal-shaped segment never reaches the lookup: {}",
        response.status()
    );
}
