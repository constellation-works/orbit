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

/// Answer with a structured backend error.
const FAILING_BACKEND: &str = "#!/bin/sh\ncat > /dev/null\nprintf '{\"ok\":false,\"error\":{\"code\":\"upstream_down\",\"message\":\"the service is down\",\"retryable\":true}}\\n'\n";

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
        let root = tempfile::tempdir().expect("tempdir");
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
        let root = self.sources.join(name);
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
fn an_allowed_plugin_call_runs_under_the_run_record_and_is_audited_once_as_brokered() {
    let fixture = Fixture::new();
    fixture.plugin("demo", ECHO_BACKEND, None);
    let serving = serve(&fixture, fixture.run(&["demo.hello"]));

    let response = serving.call(request(
        "demo.hello",
        json!({"subject": "world", "task_id": DECOY_TASK, "job_run_id": DECOY_RUN}),
        &fixture.worktree,
    ));

    assert_eq!(response["schema_version"], 1);
    assert_eq!(response["ok"], true, "{response}");
    let envelope = &response["output"]["envelope"];
    assert_eq!(envelope["input"]["subject"], "world", "{envelope}");
    assert_eq!(
        envelope["context"]["task_id"], RUN_TASK,
        "the backend's task comes from the run record: {envelope}"
    );
    assert_eq!(
        envelope["context"]["job_run_id"], RUN_ID,
        "the backend's job run comes from the run record: {envelope}"
    );

    let rows = serving.rows("demo.hello");
    assert_eq!(rows.len(), 1, "exactly one audit row per brokered call");
    let row = &rows[0];
    assert_eq!(row.status, AuditEventStatus::Success);
    assert!(row.brokered);
    assert_eq!(row.peer_pid, Some(std::process::id()));
    assert_eq!(row.task_id.as_deref(), Some(RUN_TASK));
    assert_eq!(row.job_run_id.as_deref(), Some(RUN_ID));
    assert_eq!(row.activity_id.as_deref(), Some("agent_implement"));
    assert_eq!(
        row.working_directory,
        fixture.worktree.display().to_string()
    );
    assert_eq!(
        row.plugin.as_ref().map(|plugin| plugin.name.as_str()),
        Some("demo")
    );
}

#[test]
fn a_tool_outside_the_run_policy_is_refused_and_audited_as_denied() {
    let fixture = Fixture::new();
    fixture.plugin("demo", ECHO_BACKEND, None);
    fixture.plugin("other", ECHO_BACKEND, None);
    let serving = serve(&fixture, fixture.run(&["demo.hello"]));

    let outside = serving.call(request("other.hello", json!({}), &fixture.worktree));

    assert_eq!(outside["ok"], false, "{outside}");
    assert_eq!(outside["error"]["code"], "plugin_broker_refused");
    assert_eq!(outside["error"]["retryable"], false);
    let rows = serving.rows("other.hello");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, AuditEventStatus::Denied);
    assert!(rows[0].brokered);

    let built_in = serving.call(request("orbit.task.list", json!({}), &fixture.worktree));
    assert_eq!(
        built_in["error"]["code"], "plugin_broker_refused",
        "the broker runs plugin tools only: {built_in}"
    );
}

#[test]
fn an_activity_that_allows_no_tool_admits_no_brokered_call() {
    let fixture = Fixture::new();
    fixture.plugin("demo", ECHO_BACKEND, None);
    let serving = serve(&fixture, fixture.run(&[]));

    let response = serving.call(request("demo.hello", json!({}), &fixture.worktree));

    assert_eq!(
        response["error"]["code"], "plugin_broker_refused",
        "{response}"
    );
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
fn a_backend_error_reaches_the_caller_with_its_own_code() {
    let fixture = Fixture::new();
    fixture.plugin("flaky", FAILING_BACKEND, None);
    let serving = serve(&fixture, fixture.run(&["flaky.hello"]));

    let response = serving.call(request("flaky.hello", json!({}), &fixture.worktree));

    assert_eq!(response["ok"], false, "{response}");
    assert_eq!(response["error"]["code"], "upstream_down");
    assert_eq!(response["error"]["message"], "the service is down");
    assert_eq!(response["error"]["retryable"], true);
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

/// A blocked exec backend leaves a descendant holding its pipes. Disconnect
/// and broker teardown must kill both, without waiting for the tool timeout.
#[test]
fn disconnect_and_run_teardown_kill_the_backend_process_group() {
    const CHILD: &str = "ORBIT_BROKER_LIFECYCLE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"));
        orbit_common::test_env::clear_inherited_authority(|name| {
            child.env_remove(name);
        });
        let output = child.env(CHILD, "1").args(["--exact", "adapter::command::dispatch::tests::brokered::disconnect_and_run_teardown_kill_the_backend_process_group", "--nocapture"]).output().expect("isolated lifecycle fixture");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let fixture = Fixture::new();
    fixture.plugin("waiting", "#!/bin/sh\ncat >/dev/null\nsleep 120 &\nprintf '%s %s' \"$$\" \"$!\" > \"$ORBIT_PLUGIN_STATE/pids\"\nwait\n", None);
    // The manifest needs a write grant for the readiness file.
    // Use the existing source with an explicitly scoped plugin-state write.
    let manifest = fixture.sources.join("waiting/plugin.yaml");
    let text = std::fs::read_to_string(&manifest).expect("manifest");
    std::fs::write(
        &manifest,
        format!("{text}  permissions:\n    fs:\n      write: ['{{{{plugin_state}}}}']\n"),
    )
    .expect("permissions");
    install_plugin(
        &fixture.runtime(),
        fixture.sources.join("waiting").to_str().expect("source"),
        &PluginAddOptions {
            force: true,
            ..Default::default()
        },
    )
    .expect("update fixture");
    let options = PluginEnableOptions {
        grants: vec!["fs".to_string()],
        ..Default::default()
    };
    enable_plugin(&fixture.runtime(), "waiting", &options).expect("grant state write");
    for disconnect in [true, false] {
        let serving = serve(&fixture, fixture.run(&["waiting.hello"]));
        let path = fixture.global_root.join("state/plugins/waiting/pids");
        if path.exists() {
            std::fs::remove_file(&path).expect("remove prior readiness");
        }
        let mut stream = UnixStream::connect(serving.broker.socket_path()).expect("connect");
        let bytes = request("waiting.hello", json!({}), &fixture.worktree)
            .to_string()
            .into_bytes();
        stream
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .expect("header");
        stream.write_all(&bytes).expect("body");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let pids = loop {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let pids: Vec<i32> = text
                    .split_whitespace()
                    .filter_map(|v| v.parse().ok())
                    .collect();
                if pids.len() == 2 {
                    break pids;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "backend did not publish readiness"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let serving = if disconnect {
            drop(stream);
            Some(serving)
        } else {
            drop(serving);
            drop(stream);
            None
        };
        for pid in pids {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            loop {
                // Linux orphan zombies are no longer executing, even if init
                // has not yet reaped them. On other hosts kill(0) reports exit.
                #[cfg(target_os = "linux")]
                let zombie =
                    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                        stat.rsplit_once(") ")
                            .is_some_and(|(_, rest)| rest.starts_with('Z'))
                    });
                #[cfg(not(target_os = "linux"))]
                let zombie = false;
                // SAFETY: signal zero checks existence only.
                if zombie || unsafe { libc::kill(pid, 0) } != 0 {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "backend group member {pid} survived teardown"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        drop(serving);
    }
}

#[test]
fn broker_mcp_sessions_are_reused_and_reclaimed_without_dropping_the_runtime() {
    const CHILD: &str = "ORBIT_BROKER_MCP_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"));
        orbit_common::test_env::clear_inherited_authority(|name| {
            child.env_remove(name);
        });
        let output = child.env(CHILD, "1").args(["--exact", "adapter::command::dispatch::tests::brokered::broker_mcp_sessions_are_reused_and_reclaimed_without_dropping_the_runtime", "--nocapture"]).output().expect("isolated MCP fixture");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let fixture = Fixture::new();
    fixture.plugin_kind("mcpecho", MCP_BACKEND, None, "mcp");
    let first = serve(&fixture, fixture.run(&["mcpecho.hello"]));
    let mut other_run = fixture.run(&["mcpecho.hello"]);
    other_run.activity_name = "another_invocation".to_string();
    let second = serve(&fixture, other_run);
    let call = || request("mcpecho.hello", json!({}), &fixture.worktree);
    let a = first.call(call());
    let b = second.call(call());
    assert_eq!(a["ok"], true, "{a}");
    assert_eq!(b["ok"], true, "{b}");
    assert_ne!(a["output"]["pid"], b["output"]["pid"]);
    assert_eq!(first.call(call())["output"]["pid"], a["output"]["pid"]);
    assert_eq!(a["output"]["context"]["task_id"], RUN_TASK);
    assert_eq!(a["output"]["context"]["job_run_id"], RUN_ID);
    assert_eq!(a["output"]["parent"], std::process::id());
    let runtime = first.runtime.clone();
    let pid = a["output"]["pid"].as_u64().expect("pid") as i32;
    drop(first);
    // SAFETY: signal zero checks existence only.
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "broker drop reaped its session while host runtime remains alive"
    );
    assert_eq!(second.call(call())["output"]["pid"], b["output"]["pid"]);
    let pid = b["output"]["pid"].as_u64().expect("pid") as i32;
    drop(second);
    // SAFETY: signal zero checks existence only.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    drop(runtime);
}

const MCP_BACKEND: &str = r#"#!/usr/bin/python3
import json, os, sys
for line in sys.stdin:
    request = json.loads(line)
    method = request.get('method')
    if method == 'initialize':
        result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'fixture', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': 'hello', 'inputSchema': {'type': 'object', 'properties': {'subject': {'type': 'string', 'description': 'Who to greet.'}}}}]}
    elif method == 'tools/call':
        result = {'content': [], 'structuredContent': {'pid': os.getpid(), 'parent': os.getppid(), 'context': request['params']['_meta']['orbit']}}
    else:
        continue
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
"#;

/// Inside a masked agent sandbox with no broker exported, a plugin call is
/// refused before the local audit boundary and before any backend spawns;
/// the same call on an unmasked host still runs in-process.
#[test]
fn a_masked_process_without_a_broker_refuses_plugin_calls() {
    let fixture = Fixture::new();
    fixture.plugin("demo", ECHO_BACKEND, None);
    let runtime = fixture.runtime();
    let call = |runtime: &OrbitRuntime| {
        runtime.execute_tool_command_dispatch_with_session_context(
            "demo.hello",
            json!({"subject": "host"}),
            None,
            None,
            super::super::execute::ToolEntryPoint::Cli,
            orbit_types::tool::ToolSessionContext {
                transport: Some(orbit_types::tool::McpTransport::Local),
                ..orbit_types::tool::ToolSessionContext::default()
            },
        )
    };

    let unmasked = call(&runtime).expect("an unmasked host runs the call in-process");
    assert!(unmasked.audit_recorded, "{:?}", unmasked.value);
    let rows_before = runtime
        .list_audit_events(None, Some("demo.hello".to_string()), None, None, 100)
        .expect("audit rows")
        .len();

    // What the nested orbit sees through the Linux mask.
    let state = fixture.global_root.join("state/plugins");
    std::fs::create_dir_all(&state).expect("state tree");
    std::fs::write(
        state.join(crate::runtime::plugin::sandbox_mask::PLUGIN_MASK_SENTINEL_FILE),
        b"masked",
    )
    .expect("sentinel");
    let error = call(&runtime).expect_err("a masked call without a broker is refused");

    let orbit_common::OrbitError::RemoteTool { code, payload, .. } = &error else {
        panic!("the refusal must be structured: {error:?}");
    };
    assert_eq!(code, "plugin_broker_unavailable");
    assert_eq!(payload["retryable"], false);
    let rows_after = runtime
        .list_audit_events(None, Some("demo.hello".to_string()), None, None, 100)
        .expect("audit rows")
        .len();
    assert_eq!(rows_after, rows_before, "nothing was dispatched locally");
}
