//! Real CLI and MCP transports inside Bubblewrap, served by the host broker.
#![cfg(target_os = "linux")]
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use orbit_core::OrbitRuntime;
use orbit_engine::{PluginBrokerRun, RuntimeHost};
use orbit_tools::plugin::BrokeredCaller;
use orbit_types::policy::ResolvedFsProfile;
use serde_json::Value;

const CHILD: &str = "ORBIT_SANDBOX_BROKER_FIXTURE";

#[test]
fn cli_and_mcp_reach_host_backends_with_authoritative_identity() {
    let probe = orbit_exec::probe_bwrap();
    if !probe.available {
        let _ = writeln!(
            std::io::stderr(),
            "skipped broker sandbox integration: {}",
            probe.detail
        );
        return;
    }
    // Runtime bootstrap and CLI mutations are isolated from the test runner's
    // environment and managed workspace authority.
    let root = tempfile::Builder::new()
        .prefix("obe")
        .tempdir_in("/var/tmp")
        .expect("short sandbox-visible fixture root");
    let mut child = Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        child.env_remove(name);
    });
    let output = child
        .env(CHILD, root.path())
        .env("HOME", root.path().join("home"))
        .env("USERPROFILE", root.path().join("home"))
        .args(["--ignored", "--exact", "sandbox_fixture", "--nocapture"])
        .output()
        .expect("isolated fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cli(work: &Path, args: &[&str]) -> std::process::Output {
    let output = Command::new(env!("CARGO_BIN_EXE_orbit"))
        .current_dir(work)
        .args(args)
        .output()
        .expect("fixture CLI");
    assert!(
        output.status.success(),
        "orbit {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
#[ignore = "isolated child of the real-bwrap integration test"]
fn sandbox_fixture() {
    let Some(root) = std::env::var_os(CHILD) else {
        return;
    };
    let root = Path::new(&root);
    let home = root.join("home");
    let work = root.join("work");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&work).expect("work");
    let git = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&work)
        .status()
        .expect("git init");
    assert!(git.success());
    cli(
        &work,
        &[
            "init",
            "--non-interactive",
            "--machine-name",
            "broker-fixture",
            "--task-prefix",
            "TST",
        ],
    );
    cli(&work, &["workspace", "init", "--name", "broker-fixture"]);
    let routing: Value =
        serde_json::from_slice(&cli(&work, &["workspace", "show", "--format", "json"]).stdout)
            .expect("routing");
    assert_eq!(
        routing["checkout"]["repo_root"],
        work.to_str().expect("work")
    );
    for (name, kind) in [("execfixture", "exec"), ("mcpfixture", "mcp")] {
        let source = root.join(name);
        fs::create_dir_all(source.join("bin")).expect("plugin dirs");
        let backend = source.join("bin/backend");
        fs::write(
            &backend,
            if kind == "exec" {
                EXEC_BACKEND
            } else {
                MCP_BACKEND
            },
        )
        .expect("backend");
        fs::set_permissions(&backend, fs::Permissions::from_mode(0o755)).expect("executable");
        fs::write(source.join("plugin.yaml"), format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {name}\n  version: 1.0.0\n  description: Broker integration fixture.\nspec:\n  backend:\n    type: {kind}\n    command: bin/backend\n  tools:\n    - name: echo\n      description: Echo authenticated context.\n      execution_kind: read_only\n      mcp_scope: workspace\n      input_schema:\n        type: object\n"
        )).expect("manifest");
        cli(
            &work,
            &[
                "plugin",
                "add",
                source.to_str().expect("source"),
                "--enable",
            ],
        );
    }
    let global = home.join(".orbit");
    let runtime = OrbitRuntime::from_roots(&global, &work.join(".orbit")).expect("host runtime");
    let run = PluginBrokerRun {
        run_id: "broker-real-run".to_string(),
        job_run_id: Some("broker-real-run".to_string()),
        task_id: Some("TST-REAL".to_string()),
        activity_name: "sandbox_probe".to_string(),
        agent_name: Some("codex".to_string()),
        model_name: None,
        workspace: Some(runtime.workspace_id().expect("workspace identity")),
        allowed_tools: vec![
            "execfixture.echo".to_string(),
            "mcpfixture.echo".to_string(),
        ],
        tool_deny_policy: None,
        caller: BrokeredCaller {
            worktree: work.clone(),
            fs_profile: ResolvedFsProfile {
                name: "sandbox_probe".to_string(),
                read: vec!["/**".to_string()],
                modify: vec![format!("{}/**", root.display())],
            },
            proc_allowed_programs: Vec::new(),
            proc_disallowed_programs: None,
        },
    };
    let broker = runtime
        .start_plugin_broker(&run)
        .expect("start broker")
        .expect("Unix broker");
    let socket = broker.socket_path().to_path_buf();
    let provider = root.join("provider.py");
    fs::write(&provider, PROVIDER).expect("sandbox provider");
    let child = Command::new("/usr/bin/bwrap")
        .args([
            "--unshare-user",
            "--unshare-pid",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--bind",
        ])
        .arg(root)
        .arg(root)
        .args(["--chdir"])
        .arg(&work)
        .args(["--", "/usr/bin/python3"])
        .arg(&provider)
        .arg(env!("CARGO_BIN_EXE_orbit"))
        .env("ORBIT_PLUGIN_BROKER", &socket)
        .env("ORBIT_RUN_ID", "spoofed-run")
        .env("ORBIT_TASK_ID", "TST-SPOOFED")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("spawn real sandbox");
    broker
        .bind_sandbox(child.id())
        .expect("authenticate sandbox namespace");
    // supervise_child bounds the test and tears down the provider's descendants.
    let output = orbit_exec::supervise_child(child, Some(60_000), None)
        .expect("supervise sandbox")
        .result;
    assert!(
        output.success,
        "sandbox provider: {}\n{}",
        output.stdout, output.stderr
    );
    let rows: Vec<Value> = serde_json::from_str(&output.stdout).expect("provider results");
    assert_eq!(rows.len(), 4);
    for row in &rows {
        assert_eq!(
            row["parent"].as_u64(),
            Some(u64::from(std::process::id())),
            "backend is a child of the host worker, not of a nested CLI or sandbox"
        );
        assert_eq!(row["context"]["task_id"], "TST-REAL");
        assert_eq!(row["context"]["job_run_id"], "broker-real-run");
    }
    assert_eq!(
        rows[1]["pid"], rows[3]["pid"],
        "CLI and MCP clients in one invocation reuse its backend"
    );
    let pid = rows[1]["pid"].as_u64().expect("MCP pid") as i32;
    drop(broker);
    assert!(!socket.exists());
    // SAFETY: signal zero checks existence only. Teardown waits for reaping.
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "the run's MCP session was reclaimed"
    );
}

const EXEC_BACKEND: &str = r#"#!/usr/bin/python3
import json, os, sys
request = json.load(sys.stdin)
print(json.dumps({'ok': True, 'output': {'pid': os.getpid(), 'parent': os.getppid(), 'context': request['context']}}))
"#;

const MCP_BACKEND: &str = r#"#!/usr/bin/python3
import json, os, sys
for line in sys.stdin:
    request = json.loads(line)
    method = request.get('method')
    if method == 'initialize':
        result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'fixture', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': 'echo', 'inputSchema': {'type': 'object'}}]}
    elif method == 'tools/call':
        result = {'content': [], 'structuredContent': {'pid': os.getpid(), 'parent': os.getppid(), 'context': request['params']['_meta']['orbit']}}
    else:
        continue
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
"#;

const PROVIDER: &str = r#"import json, os, subprocess, sys
orbit = sys.argv[1]
rows = []
for ns in ['execfixture', 'mcpfixture']:
    result = subprocess.run([orbit, 'tool', 'run', ns + '.echo', '--input', '{}'], capture_output=True, text=True, timeout=20)
    assert result.returncode == 0, result.stderr
    rows.append(json.loads(result.stdout))
server = subprocess.Popen([orbit, 'mcp', 'serve'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=sys.stderr, text=True)
def send(message):
    server.stdin.write(json.dumps(message) + '\n')
    server.stdin.flush()
def request(id, method, params):
    send({'jsonrpc': '2.0', 'id': id, 'method': method, 'params': params})
    while True:
        line = server.stdout.readline()
        assert line, 'MCP server exited before replying'
        response = json.loads(line)
        if response.get('id') == id:
            assert 'error' not in response, response
            return response['result']
try:
    request(1, 'initialize', {'protocolVersion': '2025-06-18', 'capabilities': {}, 'clientInfo': {'name': 'probe', 'version': '1'}, '_meta': {'orbit': {'workspace': os.getcwd()}}})
    send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
    for id, ns in enumerate(['execfixture', 'mcpfixture'], 2):
        result = request(id, 'tools/call', {'name': ns + '_echo', 'arguments': {}})
        assert not result.get('isError'), result
        output = result.get('structuredContent')
        if output is None:
            output = json.loads(result['content'][0]['text'])
        rows.append(output)
finally:
    server.terminate()
    server.wait(timeout=5)
print(json.dumps(rows))
"#;
