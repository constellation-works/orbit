//! A run's plugin broker end to end: requests over its real socket, executed
//! by [`RunDispatch`] against fixture exec plugins, with the audit rows and
//! secret store read back. The client is this test process, anchored by
//! ancestry so no agent sandbox is needed.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use orbit_common::process::ancestry::process_start_key;
use orbit_common::test_env::ScopedEnv;
use orbit_engine::{PluginBrokerHandle, PluginBrokerRun};
use orbit_tools::plugin::BrokeredCaller;
use orbit_types::plugin::PluginSecretUpdateStatus;
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::telemetry::{AuditEvent, AuditEventStatus};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::super::brokered::RunDispatch;
use crate::OrbitRuntime;
use crate::application::plugin::{
    PluginAddOptions, PluginEnableOptions, enable_plugin, install_plugin, set_plugin_secret,
};
use crate::runtime::plugin::broker::{PeerAnchor, PluginBroker};
use crate::runtime::plugin::secrets::{PluginSecretStore, PluginSecretValue};

const RUN_ID: &str = "jrun-broker-run";
const RUN_TASK: &str = "ORB-RUN-1";
/// Correlation the request and this process's environment claim. The run
/// record, not either of these, decides what the backend and audit row see.
const DECOY_TASK: &str = "ORB-DECOY-9";
const DECOY_RUN: &str = "jrun-decoy";

/// Echo the envelope back, so a test reads the call context the backend got.
const ECHO_BACKEND: &str =
    "#!/bin/sh\ninput=$(cat)\nprintf '{\"ok\":true,\"output\":{\"envelope\":%s}}\\n' \"$input\"\n";

/// Rotate `refresh_token` from the version the call was delivered.
const ROTATING_BACKEND: &str = r##"#!/bin/sh
input=$(cat)
version=$(printf '%s' "$input" | sed -n 's/.*"refresh_token":{"value":"[^"]*","version":"\([^"]*\)".*/\1/p')
printf '{"ok":true,"output":{"rotated":true},"secret_updates":{"refresh_token":{"value":"rotated-token-7d1","expected_version":"%s"}}}\n' "$version"
"##;

struct Fixture {
    // Dropped first, so HOME and the decoys are restored before the tree goes.
    _env: ScopedEnv,
    _root: TempDir,
    global_root: PathBuf,
    workspace_root: PathBuf,
    worktree: PathBuf,
    sources: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        // The broker refuses a socket path longer than `sun_path` rather than
        // moving it, and the default macOS temp directory is too deep for the
        // run directory beneath it. Keep the root short, as the broker's own
        // fixtures do.
        let root = tempfile::Builder::new()
            .prefix("obk")
            .tempdir_in("/tmp")
            .expect("short broker test root");
        let home = root.path().join("home");
        let global_root = root.path().join("global");
        let worktree = root.path().join("repo");
        let workspace_root = worktree.join(".orbit");
        let sources = root.path().join("sources");
        for dir in [&home, &global_root, &workspace_root, &sources] {
            std::fs::create_dir_all(dir).expect("create fixture dir");
        }
        let home = home.to_str().expect("utf8 HOME").to_string();
        let env = orbit_common::test_env::scoped([
            ("HOME", Some(home.as_str())),
            ("USERPROFILE", Some(home.as_str())),
            ("ORBIT_TASK_ID", Some(DECOY_TASK)),
            ("ORBIT_RUN_ID", Some(DECOY_RUN)),
            ("ORBIT_AGENT_NAME", None),
            ("ORBIT_AGENT_MODEL", None),
            ("ORBIT_OPERATOR", None),
            ("ORBIT_TASK_ACTOR_KIND", None),
            (
                crate::runtime::run_input::ORBIT_MANAGED_RUN_CONTEXT_ENV,
                None,
            ),
            (orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_ENV, None),
            (orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_FD_ENV, None),
            ("ORBIT_PLUGIN_BROKER", None),
            ("ORBIT_ACTIVITY_TOOLS", None),
        ]);
        let worktree = worktree.canonicalize().expect("canonical worktree");
        Self {
            _env: env,
            _root: root,
            global_root,
            workspace_root,
            worktree,
            sources,
        }
    }

    fn runtime(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.global_root, &self.workspace_root).expect("runtime")
    }

    /// Install and enable a one-tool exec plugin `<name>.hello`.
    fn plugin(&self, name: &str, backend: &str, secrets: Option<&str>) {
        self.plugin_kind(name, backend, secrets, "exec");
    }

    fn plugin_kind(&self, name: &str, backend: &str, secrets: Option<&str>, kind: &str) {
        let root = self
            .sources
            .join(name)
            .join(orbit_types::plugin::PLUGIN_DIR_NAME);
        std::fs::create_dir_all(root.join("bin")).expect("plugin bin dir");
        let script = root.join("bin/backend.sh");
        std::fs::write(&script, backend).expect("write backend");
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod backend");
        let secrets = secrets
            .map(|entries| format!("  secrets:\n{entries}"))
            .unwrap_or_default();
        std::fs::write(
            root.join("plugin.yaml"),
            format!(
                "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {name}\n  version: 1.0.0\n  \
                 description: Fixture plugin.\nspec:\n  backend:\n    type: {kind}\n    command: \
                 bin/backend.sh\n  tools:\n    - name: hello\n      description: Say hello.\n      \
                 execution_kind: read_only\n      mcp_scope: workspace\n      input_schema:\n        \
                 type: object\n        properties:\n          subject: {{ type: string, \
                 description: Who to greet. }}\n{secrets}"
            ),
        )
        .expect("write manifest");
        let runtime = self.runtime();
        install_plugin(
            &runtime,
            root.to_str().expect("utf8 source"),
            &PluginAddOptions::default(),
        )
        .expect("install plugin");
        enable_plugin(&runtime, name, &PluginEnableOptions::default()).expect("enable plugin");
    }

    /// The record the step runner builds for a sandboxed run whose activity
    /// allows `allowed`.
    fn run(&self, allowed: &[&str]) -> PluginBrokerRun {
        PluginBrokerRun {
            run_id: RUN_ID.to_string(),
            job_run_id: Some(RUN_ID.to_string()),
            task_id: Some(RUN_TASK.to_string()),
            activity_name: "agent_implement".to_string(),
            agent_name: Some("codex".to_string()),
            model_name: None,
            workspace: None,
            allowed_tools: allowed.iter().map(|tool| tool.to_string()).collect(),
            tool_deny_policy: None,
            caller: BrokeredCaller {
                worktree: self.worktree.clone(),
                fs_profile: ResolvedFsProfile {
                    name: "broker-e2e".to_string(),
                    read: vec!["/**".to_string()],
                    modify: vec![format!("{}/**", self.worktree.display())],
                },
                proc_allowed_programs: Vec::new(),
                proc_disallowed_programs: None,
            },
        }
    }
}

/// A broker serving `run` for this process, and the runtime its calls audit to.
struct Serving {
    broker: PluginBroker,
    runtime: OrbitRuntime,
}

fn serve(fixture: &Fixture, run: PluginBrokerRun) -> Serving {
    let runtime = fixture.runtime();
    let dispatch = Arc::new(RunDispatch::new(runtime.clone(), run));
    let broker = PluginBroker::start(&fixture.global_root, RUN_ID, dispatch).expect("start broker");
    broker.bind_anchor(PeerAnchor::Ancestor(
        process_start_key(std::process::id()).expect("own start key"),
    ));
    Serving { broker, runtime }
}

impl Serving {
    /// Send one §4.3 request and return the broker's response, framed by
    /// hand so the wire format itself is under test.
    fn call(&self, request: Value) -> Value {
        let mut stream = UnixStream::connect(self.broker.socket_path()).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .expect("read timeout");
        let body = request.to_string().into_bytes();
        stream
            .write_all(&u32::try_from(body.len()).expect("len").to_be_bytes())
            .expect("send length");
        stream.write_all(&body).expect("send body");
        let mut header = [0u8; 4];
        stream.read_exact(&mut header).expect("response length");
        let mut response = vec![0u8; u32::from_be_bytes(header) as usize];
        stream.read_exact(&mut response).expect("response body");
        serde_json::from_slice(&response).expect("response is JSON")
    }

    fn rows(&self, tool: &str) -> Vec<AuditEvent> {
        self.runtime
            .list_audit_events(None, Some(tool.to_string()), None, None, 10)
            .expect("audit events")
    }
}

fn request(tool: &str, input: Value, cwd: &Path) -> Value {
    json!({
        "schema_version": 1,
        "tool": tool,
        "input": input,
        "cwd": cwd,
        "workspace": null,
        "entry_point": "cli",
        "dry_run": false,
    })
}

#[test]
fn a_request_outside_the_run_is_refused_before_the_backend_runs() {
    let fixture = Fixture::new();
    fixture.plugin("demo", ECHO_BACKEND, None);
    let serving = serve(&fixture, fixture.run(&["demo.hello"]));
    let outside = fixture.global_root.clone();

    for (case, mut request) in [
        ("cwd", request("demo.hello", json!({}), &outside)),
        (
            "workspace",
            request("demo.hello", json!({}), &fixture.worktree),
        ),
        (
            "dry_run",
            request("demo.hello", json!({}), &fixture.worktree),
        ),
    ] {
        match case {
            "workspace" => request["workspace"] = json!("another-workspace"),
            "dry_run" => request["dry_run"] = json!(true),
            _ => {}
        }

        let response = serving.call(request);

        assert_eq!(
            response["error"]["code"], "plugin_broker_refused",
            "{case}: {response}"
        );
    }
    let rows = serving.rows("demo.hello");
    assert_eq!(rows.len(), 3, "each refusal is audited");
    assert!(
        rows.iter()
            .all(|row| row.status == AuditEventStatus::Denied && row.brokered)
    );
}

#[test]
fn a_brokered_rotation_is_stored_by_compare_and_swap_and_never_returned() {
    let fixture = Fixture::new();
    fixture.plugin(
        "demo",
        ROTATING_BACKEND,
        Some("    - name: refresh_token\n      rotatable: true\n"),
    );
    set_plugin_secret(
        &fixture.runtime(),
        "demo",
        "refresh_token",
        &PluginSecretValue::new("first-token-2a9".to_string()).expect("secret value"),
    )
    .expect("set refresh token");
    let store = PluginSecretStore::new(&fixture.global_root);
    let first_version = store
        .get("demo", "refresh_token")
        .expect("read")
        .expect("set")
        .version;
    let serving = serve(&fixture, fixture.run(&["demo.hello"]));

    let response = serving.call(request("demo.hello", json!({}), &fixture.worktree));

    assert_eq!(
        response,
        json!({"schema_version": 1, "ok": true, "output": {"rotated": true}}),
        "the response carries the output only"
    );
    let rotated = store
        .get("demo", "refresh_token")
        .expect("read")
        .expect("set");
    assert_eq!(rotated.value.expose(), "rotated-token-7d1");
    assert_ne!(rotated.version, first_version);
    let text = response.to_string();
    assert!(
        !text.contains("secret_updates")
            && !text.contains("rotated-token-7d1")
            && !text.contains("first-token-2a9"),
        "no secret leaves the host: {text}"
    );
    let rows = serving.rows("demo.hello");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].plugin_secret_updates.get("refresh_token"),
        Some(&PluginSecretUpdateStatus::Applied)
    );
}
