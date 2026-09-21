//! `/api/plugins` and `/api/plugins/<ns>/panels/<id>` [§4.7].
//!
//! The fixture installs a real plugin through the ordinary lifecycle and
//! then reopens the runtime, so the listing and the panel read answer from
//! the same load pass the tool surface was built from.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::{PluginAddOptions, PluginEnableOptions};
use tempfile::TempDir;
use tower::ServiceExt;

use super::super::router;
use super::test_support::body_json;
use crate::state::DashboardState;

struct PluginFixture {
    _temp: TempDir,
    global_root: PathBuf,
    workspace_root: PathBuf,
}

impl PluginFixture {
    fn new() -> Self {
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
        self._temp.path().join("sources/panels")
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
        "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: panels\n  version: 0.1.0\n  \
         description: Panel fixture.\nspec:\n  backend:\n    type: exec\n    command: \
         bin/backend.sh\n  tools:\n    - name: status\n      description: Report status.\n      \
         execution_kind: read_only\n      mcp_scope: workspace\n  web:\n    panels:\n      - id: \
         status\n        title: Index\n        source: tool:status\n        render: kv\n    \
         links:\n      - title: Explorer\n        url: http://127.0.0.1:7890/\n",
    )
    .expect("write manifest");
}

/// A panel read goes through the same tool dispatch an activity's calls do,
/// so an inherited managed-run allowlist would decide the outcome. Pin the
/// two variables that carry one, the way the config tests pin the caller's.
#[allow(clippy::await_holding_lock)]
async fn without_inherited_activity_scope<T>(fut: impl std::future::Future<Output = T>) -> T {
    let _env = orbit_common::test_env::scoped([
        ("ORBIT_TASK_ACTOR_KIND", None),
        ("ORBIT_ACTIVITY_TOOLS", None),
    ]);
    fut.await
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

fn state(runtime: OrbitRuntime) -> DashboardState {
    DashboardState::single(Arc::new(runtime))
}

#[cfg(unix)]
#[tokio::test]
async fn plugins_list_reports_enable_state_and_a_panel_serves_its_read_only_tool() {
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

    // Installed but not enabled: listed, with the step that would activate it.
    let payload = body_json(get(state(fixture.runtime()), "/plugins").await).await;
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

    let payload = body_json(get(state(fixture.runtime()), "/plugins").await).await;
    let plugin = &payload.as_array().expect("an array of plugins")[0];
    assert_eq!(plugin["status"], "active");
    assert_eq!(plugin["enabled"], true);
    assert_eq!(plugin["panels"][0]["id"], "status");
    assert_eq!(plugin["panels"][0]["render"], "kv");
    assert_eq!(plugin["panels"][0]["tool"], "panels.status");
    assert_eq!(plugin["links"][0]["url"], "http://127.0.0.1:7890/");
    assert_eq!(plugin["tools"][0]["execution_kind"], "read_only");

    let response = without_inherited_activity_scope(get(
        state(fixture.runtime()),
        "/plugins/panels/panels/status",
    ))
    .await;
    let status = response.status();
    let payload = body_json(response).await;
    assert_eq!(status, StatusCode::OK, "{payload}");
    assert_eq!(
        payload["output"],
        serde_json::json!({ "indexed": 7, "state": "ready" }),
        "the panel serves the source tool's output"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_undeclared_panel_is_not_found() {
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
    let fixture = PluginFixture::new();
    let response = get(state(fixture.runtime()), "/plugins/..%2Fx/panels/status").await;
    assert!(
        response.status() == StatusCode::BAD_REQUEST || response.status() == StatusCode::NOT_FOUND,
        "a traversal-shaped segment never reaches the lookup: {}",
        response.status()
    );
}
