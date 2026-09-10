//! End-to-end integration tests for the production MCP entry point.
//!
//! Each test initializes a real Orbit workspace in a temp dir, spawns the
//! actual `orbit mcp serve` binary with piped stdio — the exact transport MCP
//! clients use — and speaks raw newline-delimited JSON-RPC to it, crossing the
//! full serialize → server-side workspace resolution → `OrbitRuntime` → store →
//! serialize path.
//!
//! The `tools/list` snapshot is the breaking-change guard for the agent MCP
//! surface: per RELEASING.md, any tool input/output schema change is breaking.
#![allow(missing_docs)]
// tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use orbit_common::test_env;
use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(target_os = "linux")]
const EXEC_BUSY_RETRY_WINDOW: Duration = Duration::from_secs(2);
#[cfg(target_os = "linux")]
const REQUIRE_PROTECTED_SSH_REGRESSION_ENV: &str = "ORBIT_REQUIRE_PROTECTED_SSH_REGRESSION";

/// Path of the checked-in `tools/list` snapshot, relative to the crate root.
const SNAPSHOT_RELATIVE_PATH: &str = "tests/snapshots/mcp_tools_list.json";

// ---------------------------------------------------------------------------
// Fixture: an initialized Orbit workspace fully isolated from ~/.orbit.
// ---------------------------------------------------------------------------

struct McpWorkspace {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
}

/// Write an executable no-op named `name` into `bin`, standing in for an agent
/// CLI during detection. Tests that dispatch overwrite it with a response
/// fixture before starting the server.
fn plant_agent_cli_stub(bin: &Path, name: &str) {
    std::fs::create_dir_all(bin).expect("create stub CLI directory");
    let stub = bin.join(name);
    std::fs::write(&stub, "#!/bin/sh\nexit 0\n").expect("write stub agent CLI");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("mark the stub agent CLI executable");
    }
}

fn plant_agent_response_stub(bin: &Path, name: &str) {
    plant_agent_cli_stub(bin, name);
    let stub = bin.join(name);
    std::fs::write(
        &stub,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' \
         '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"summary\":\"remote probe complete\"},\"error\":null}'\n",
    )
    .expect("write response stub");
}

/// `PATH` with the fixture's stub directory first, so agent detection sees the
/// stubs regardless of what the host has installed.
fn stub_first_path(bin: &Path) -> std::ffi::OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![bin.to_path_buf()];
    entries.extend(std::env::split_paths(&inherited));
    std::env::join_paths(entries).expect("join PATH entries")
}

impl McpWorkspace {
    fn init() -> Self {
        Self::init_with_workspace_name("mcp-roundtrip")
    }

    /// A checkout registered as a replica of `owner_machine_id`, the shape a
    /// `git clone` on a second machine produces.
    fn init_replica_of(owner_machine_id: &str) -> Self {
        Self::init_with_workspace_args(
            "mcp-roundtrip",
            &["--role", "replica", "--owner", owner_machine_id],
        )
    }

    fn init_with_workspace_name(workspace_name: &str) -> Self {
        Self::init_with_workspace_args(workspace_name, &[])
    }

    fn init_with_workspace_args(workspace_name: &str, extra_workspace_args: &[&str]) -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&home).expect("create home");
        std::fs::create_dir_all(&work).expect("create work");

        // `orbit init` freezes crew seeding to the agent CLIs it finds on
        // `PATH` (ADR-0193), so the fixture plants a stub `codex` before it
        // runs. Without one, a host with no agent CLI installed — every CI
        // runner — seeds an empty `[crews]` table, and any task naming a crew
        // is rejected with `crew '<name>' is not defined in [crews.*]`.
        plant_agent_cli_stub(&Self::stub_bin_dir(&home), "codex");

        let output = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&work)
            .output()
            .expect("initialize Git checkout");
        assert!(output.status.success(), "git init failed: {output:?}");

        let init_args = vec![
            "init",
            "--non-interactive",
            "--host-name",
            "mcp-roundtrip-host",
            "--task-prefix",
            "TST",
        ];
        let output = Self::orbit_command(&work, &home)
            .args(init_args)
            .output()
            .expect("run global init");
        assert!(
            output.status.success(),
            "global init failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let mut workspace_init_args = vec!["workspace", "init", "--name", workspace_name];
        workspace_init_args.extend_from_slice(extra_workspace_args);
        let output = Self::orbit_command(&work, &home)
            .args(workspace_init_args)
            .output()
            .expect("run workspace init");
        assert!(
            output.status.success(),
            "workspace init failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        Self {
            _temp: temp,
            home,
            work,
        }
    }

    /// Directory holding the fixture's stub agent CLIs, derived from `home` so
    /// every `orbit` invocation resolves the same one.
    fn stub_bin_dir(home: &Path) -> PathBuf {
        home.join("stub-bin")
    }

    fn orbit_command(work: &Path, home: &Path) -> Command {
        Self::orbit_program_command(Path::new(env!("CARGO_BIN_EXE_orbit")), work, home)
    }

    fn orbit_program_command(program: &Path, work: &Path, home: &Path) -> Command {
        let mut command = Command::new(program);
        // ORB-11300: one shared list for every fixture that spawns `orbit`.
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(work)
            .env("PATH", stub_first_path(&Self::stub_bin_dir(home)))
            .env("HOME", home)
            .env("USERPROFILE", home);
        command
    }

    /// Spawn `orbit mcp serve`, run the MCP initialize handshake (announcing
    /// this workspace via `_meta.orbit.workspace`), and return the connected
    /// client.
    fn serve(&self) -> McpClient {
        self.serve_with_args(&[])
    }

    fn serve_with_args(&self, extra_args: &[&str]) -> McpClient {
        self.serve_with_args_and_env(extra_args, &[])
    }

    /// Spawn the server with extra argv and extra environment. `env` exists to
    /// pin what the MCP surface must *ignore*: the server's own process
    /// environment never contributes capabilities to a session.
    fn serve_with_args_and_env(&self, extra_args: &[&str], env: &[(&str, &str)]) -> McpClient {
        let mut args = vec!["mcp", "serve"];
        args.extend_from_slice(extra_args);
        let mut command = Self::orbit_command(&self.work, &self.home);
        for (key, value) in env {
            command.env(key, value);
        }
        let child = command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn orbit mcp serve");
        let mut client = McpClient::new(child);
        self.initialize(&mut client);
        client
    }

    /// Spawn the server with a complete argv generated by an operator-facing
    /// setup command, without reconstructing any part of that argv in the
    /// test.
    #[cfg(target_os = "linux")]
    fn serve_with_generated_command(&self, generated: &GeneratedForcedCommand) -> McpClient {
        let argv = &generated.argv;
        let mut command = Self::orbit_program_command(Path::new(&argv[0]), &self.work, &self.home);
        command
            .args(["-c", &generated.forced_command])
            .env(orbit_mcp::SSH_ACCEPTANCE_ENV, &generated.acceptance_token)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = retry_executable_busy(|| command.spawn())
            .expect("spawn generated orbit mcp serve command");
        let mut client = McpClient::new(child);
        self.initialize(&mut client);
        client
    }

    /// Spawn `orbit mcp listen` on `addr` and run the same handshake over the
    /// socket it accepts.
    fn listen(&self, addr: SocketAddr) -> McpClient {
        let child = Self::orbit_command(&self.work, &self.home)
            .args(["mcp", "listen", &addr.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn orbit mcp listen");
        let mut client = McpClient::over_tcp(child, connect_when_listening(addr));
        self.initialize(&mut client);
        client
    }

    /// The MCP initialize handshake, announcing this workspace via
    /// `_meta.orbit.workspace`.
    fn initialize(&self, client: &mut McpClient) {
        let workspace = self.work.to_str().expect("utf8 workspace path");
        let response = client.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "orbit-mcp-roundtrip-test", "version": "0" },
                "_meta": { "orbit": { "workspace": workspace } },
            }),
        );
        let result = &response["result"];
        assert_eq!(result["protocolVersion"], "2025-06-18");
        assert_eq!(result["serverInfo"]["name"], "orbit-mcp");
        assert!(
            result["capabilities"]["tools"].is_object(),
            "tools capability missing: {result}"
        );
        client.notify("notifications/initialized");
    }
}

/// Reserve a loopback port by binding it and letting it go again. This is the
/// practical way to hand a spawned process an unused port: the listener cannot
/// report its own bound port back to the test.
fn free_loopback_addr() -> SocketAddr {
    let probe = TcpListener::bind("127.0.0.1:0").expect("probe a free loopback port");
    probe.local_addr().expect("probe address")
}

/// Connect once the spawned server has bound, or fail loudly on the timeout the
/// rest of this suite uses.
fn connect_when_listening(addr: SocketAddr) -> TcpStream {
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    loop {
        match TcpStream::connect(addr) {
            Ok(stream) => return stream,
            Err(error) => {
                assert!(
                    Instant::now() < deadline,
                    "listener never accepted on {addr}: {error}"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Minimal JSON-RPC client over the server's newline-delimited byte stream —
// the child's stdio, or a socket when the server is a listener. Responses may
// arrive out of order (the server fans tool calls into blocking workers), so
// match strictly by id.
// ---------------------------------------------------------------------------

struct McpClient {
    child: Child,
    writer: Box<dyn Write + Send>,
    lines: Receiver<String>,
    next_id: i64,
}

impl McpClient {
    fn new(mut child: Child) -> Self {
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        Self::over_streams(child, Box::new(stdin), Box::new(stdout))
    }

    /// A session against `orbit mcp listen`, where the same protocol runs over
    /// an accepted socket instead of the child's stdio.
    fn over_tcp(child: Child, stream: TcpStream) -> Self {
        let reader = stream.try_clone().expect("clone the MCP socket for reads");
        Self::over_streams(child, Box::new(stream), Box::new(reader))
    }

    fn over_streams(
        child: Child,
        writer: Box<dyn Write + Send>,
        reader: Box<dyn Read + Send>,
    ) -> Self {
        let (sender, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            writer,
            lines,
            next_id: 0,
        }
    }

    fn send(&mut self, message: &Value) {
        let mut line = serde_json::to_string(message).expect("serialize message");
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .expect("write to the server");
        self.writer.flush().expect("flush the server stream");
    }

    fn notify(&mut self, method: &str) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method }));
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let mut message = json!({ "jsonrpc": "2.0", "id": id, "method": method });
        if !params.is_null() {
            message["params"] = params;
        }
        self.send(&message);
        loop {
            let line = self
                .lines
                .recv_timeout(RESPONSE_TIMEOUT)
                .unwrap_or_else(|err| {
                    panic!("no response to `{method}` (id {id}) within timeout: {err}")
                });
            if line.trim().is_empty() {
                continue;
            }
            let response: Value = serde_json::from_str(line.trim())
                .unwrap_or_else(|err| panic!("server emitted invalid JSON ({err}): {line}"));
            if response.get("id").and_then(Value::as_i64) == Some(id) {
                return response;
            }
        }
    }

    /// `tools/call` helper asserting a result envelope came back.
    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        response
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("tools/call `{name}` returned no result: {response}"))
    }

    /// `tools/call` that must succeed; returns the structured content.
    fn call_tool_ok(&mut self, name: &str, arguments: Value) -> Value {
        let result = self.call_tool(name, arguments);
        assert_eq!(result["isError"], false, "`{name}` failed: {result}");
        result
            .get("structuredContent")
            .cloned()
            .unwrap_or_else(|| panic!("`{name}` returned no structuredContent: {result}"))
    }

    /// `tools/call` that must fail as a structured tool error; returns it.
    fn call_tool_err(&mut self, name: &str, arguments: Value) -> Value {
        let result = self.call_tool(name, arguments);
        assert_eq!(
            result["isError"], true,
            "`{name}` unexpectedly succeeded: {result}"
        );
        result
            .get("structuredContent")
            .cloned()
            .unwrap_or_else(|| panic!("`{name}` returned no structuredContent: {result}"))
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // Closing stdin ends the stdio transport; give the server a moment to
        // exit cleanly, then make sure it is gone.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn mcp_serve_tools_list_matches_production_snapshot() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve();

    let response = client.request("tools/list", Value::Null);
    let tools = response["result"]["tools"].clone();

    let names: Vec<&str> = tools
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "tools/list must be name-sorted");
    for expected in [
        "orbit_friction_update",
        "orbit_workflow_ship",
        "orbit_workflow_run_show",
        "orbit_workflow_run_list",
        "orbit_workflow_run_resume",
    ] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
    }

    // Snapshot guard for the full production agent surface: names AND input
    // schemas. Any diff here is a breaking MCP schema change per RELEASING.md.
    //
    // `tools/list` is answered per session, and this fixture announces a
    // workspace at initialize, so the snapshot records the workspace-bound
    // selector documentation. The unbound wording is asserted in
    // `mcp_serve_lists_the_canonical_surface_outside_any_checkout`.
    let snapshot_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT_RELATIVE_PATH);
    if std::env::var("ORBIT_MCP_UPDATE_SNAPSHOT").as_deref() == Ok("1") {
        std::fs::create_dir_all(snapshot_path.parent().expect("snapshot dir"))
            .expect("create snapshot dir");
        let mut serialized = serde_json::to_string_pretty(&tools).expect("serialize snapshot");
        serialized.push('\n');
        std::fs::write(&snapshot_path, serialized).expect("write snapshot");
        return;
    }
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(&snapshot_path).unwrap_or_else(|err| {
            panic!(
                "cannot read {} ({err}); regenerate it with \
                 `ORBIT_MCP_UPDATE_SNAPSHOT=1 cargo test -p orbit-cli --test mcp_roundtrip`",
                snapshot_path.display()
            )
        }),
    )
    .expect("parse snapshot JSON");
    assert_eq!(
        tools, expected,
        "tools/list drifted from {SNAPSHOT_RELATIVE_PATH}. MCP tool schema changes are \
         BREAKING (see RELEASING.md). If the change is intentional, regenerate the snapshot \
         with `ORBIT_MCP_UPDATE_SNAPSHOT=1 cargo test -p orbit-cli --test mcp_roundtrip` and \
         call the release breaking."
    );
}

#[test]
fn mcp_server_advertises_governed_tools_but_denies_an_unprivileged_session() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve();
    let response = client.request("tools/list", Value::Null);
    let names = response["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<BTreeSet<_>>();

    for expected in [
        "orbit_workflow_ship",
        "orbit_workflow_run_show",
        "orbit_workflow_run_list",
        "orbit_workflow_run_resume",
        "orbit_command_exec",
    ] {
        assert!(names.contains(expected), "missing {expected}: {names:?}");
    }

    for (name, arguments) in [
        ("orbit_workflow_ship", json!({ "task_ids": ["ORB-00001"] })),
        ("orbit_workflow_run_show", json!({ "id": "jrun-missing" })),
        ("orbit_workflow_run_list", json!({})),
        ("orbit_workflow_run_resume", json!({ "id": "jrun-missing" })),
    ] {
        let denied = client.call_tool_err(name, arguments);
        assert_eq!(denied["code"], "capability_denied", "{name}: {denied}");
    }

    let marker = workspace.work.join("command-exec-must-not-run");
    let denied = client.call_tool_err(
        "orbit_command_exec",
        json!({
            "argv": ["touch", marker.to_str().expect("utf8 marker")],
            "working_directory": workspace.work,
        }),
    );
    assert_eq!(denied["code"], "capability_denied", "{denied}");
    assert!(!marker.exists(), "denied command reached domain execution");
}

/// A replica checkout is an execution binding, not the control plane. Over the
/// real v1 MCP transport, a coordination write is refused with the named
/// catalog-role code — not `invalid_input`, which would make a refusal
/// indistinguishable from a malformed call, and not `capability_denied`, which
/// is the separate operator-versus-agent axis [ORB-11012] [ORB-11021].
#[test]
fn a_replica_checkout_refuses_a_coordination_write_with_capability_refused() {
    let workspace = McpWorkspace::init_replica_of("hm_remote_owner");

    let mut client = workspace.serve();
    // A well-formed call, so the refusal is the catalog role and nothing else.
    let refused = client.call_tool_err(
        "orbit_task_add",
        json!({
            "title": "must not fork",
            "description": "A replica must not issue its own task ids",
            "complexity": "low",
            "model": "codex",
        }),
    );
    assert_eq!(refused["code"], "capability_refused", "{refused}");
    assert!(
        refused["message"]
            .as_str()
            .is_some_and(|message| message.contains("control_plane")),
        "MCP dispatch must refuse the capability class: {refused}"
    );

    // CLI task writes still fail closed on Core's coordination-write guard.
    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args([
            "task",
            "add",
            "--json",
            "--title",
            "must not fork",
            "--description",
            "A replica must not issue its own task ids",
            "--complexity",
            "low",
            "--model",
            "codex",
        ])
        .output()
        .expect("run task add on a replica");
    assert!(!output.status.success(), "CLI replica task add: {output:?}");
    let payload: Value =
        serde_json::from_slice(&output.stderr).expect("JSON error payload on stderr");
    assert_eq!(payload["code"], "capability_refused", "{payload}");
    assert!(
        payload["error"]
            .as_str()
            .is_some_and(|message| message.contains("hm_remote_owner")),
        "the CLI refusal names the declared owner: {payload}"
    );
}

/// The destination MCP host must enforce checkout capability classes on the
/// production dispatch path — including control-plane tools that never pass
/// through Core's task-write guard [ORB-11021].
#[test]
fn a_replica_mcp_session_enforces_checkout_capability_classes() {
    let workspace = McpWorkspace::init_replica_of("hm_remote_owner");
    let mut client = workspace.serve();

    for (name, arguments) in [
        (
            "orbit_friction_add",
            json!({
                "body": "must not fork a replica friction",
                "model": "codex",
            }),
        ),
        (
            "orbit_search",
            json!({
                "query": "must not search replica coordination",
                "model": "codex",
            }),
        ),
        ("orbit_auto_task_list", json!({})),
        ("orbit_friction_list", json!({})),
    ] {
        let refused = client.call_tool_err(name, arguments);
        assert_eq!(refused["code"], "capability_refused", "{name}: {refused}");
        assert!(
            refused["message"]
                .as_str()
                .is_some_and(|message| message.contains("control_plane")),
            "{name} must name the refused class: {refused}"
        );
    }

    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["friction", "list", "--json"])
        .output()
        .expect("list frictions on a replica via CLI");
    assert!(
        output.status.success(),
        "CLI friction list failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let listed: Value = serde_json::from_slice(&output.stdout).expect("friction list JSON");
    let items = listed.as_array().or_else(|| {
        listed
            .get("items")
            .and_then(Value::as_array)
            .or_else(|| listed.get("frictions").and_then(Value::as_array))
    });
    assert!(
        items.is_some_and(Vec::is_empty),
        "refused friction.add must not mutate local coordination state: {listed}"
    );

    let crews = client.call_tool_ok("orbit_crew_list", json!({}));
    assert!(
        crews.get("crews").and_then(Value::as_array).is_some(),
        "unclassified crew.list must remain permitted: {crews}"
    );

    let marker = workspace.work.join("replica-command-exec-must-not-run");
    for (name, arguments) in [
        ("orbit_workflow_run_list", json!({})),
        (
            "orbit_command_exec",
            json!({
                "argv": ["touch", marker.to_str().expect("utf8 marker")],
                "working_directory": workspace.work,
            }),
        ),
    ] {
        let denied = client.call_tool_err(name, arguments);
        assert_eq!(
            denied["code"], "capability_denied",
            "{name} must pass the catalog-role gate and hit its own auth: {denied}"
        );
    }
    assert!(
        !marker.exists(),
        "execute-class command.exec must not run after its independent denial"
    );
}

/// The operator MCP surface, over the real transport: a server an operator
/// started deliberately performs governed tools, and one an agent started does
/// not — however the launching environment was set up.
#[test]
fn an_operator_served_mcp_session_reaches_a_governed_tool() {
    let workspace = McpWorkspace::init();

    // Same governed tool, same process environment that authorizes it on the
    // CLI, but an ordinary session: still refused, and told the truth about it.
    let mut agent = workspace.serve_with_args_and_env(&[], &[("ORBIT_OPERATOR", "1")]);
    let denied = agent.call_tool_err("orbit_workflow_run_list", json!({}));
    assert_eq!(denied["code"], "capability_denied", "{denied}");
    let message = denied["message"]
        .as_str()
        .expect("denial carries a message");
    assert!(
        !message.contains("re-run it with ORBIT_OPERATOR=1"),
        "the MCP surface ignores the override, so it must not advise it: {message}"
    );
    assert!(message.contains("orbit mcp serve --operator"), "{message}");
    drop(agent);

    let mut operator = workspace.serve_with_args(&["--operator"]);
    let listed = operator.call_tool_ok("orbit_workflow_run_list", json!({}));
    assert_eq!(listed["items"], json!([]));
}

/// [ORB-11052] The destination decides what a remote-originated session may do.
///
/// This is the escalation the callers file closes: the caller writes the
/// remote argv, so it can always write `--operator`. Here it does, and the
/// destination — which is the machine that would run the command — refuses
/// anyway, because its own file grants that caller `agent`. `SSH_CONNECTION`
/// plus the pipe on stdin is what makes the server treat the session as
/// remote-originated, exactly as sshd would.
#[test]
fn a_remote_originated_session_is_capped_by_the_destinations_callers_file() {
    let workspace = McpWorkspace::init();
    write_callers(
        &workspace,
        r#"
default = "agent"

[[callers]]
machine_id = "hm_caller"
label = "the-calling-box"
capabilities = ["agent"]
"#,
    );

    let mut client = workspace.serve_with_args_and_env(
        &["--operator", "--remote-caller-machine-id", "hm_caller"],
        &[("SSH_CONNECTION", "192.0.2.8 43100 198.51.100.2 22")],
    );
    let denied = client.call_tool_err(
        "orbit_command_exec",
        json!({
            "argv": ["true"],
            "working_directory": workspace.work.to_str().expect("utf8 workspace path"),
            "workspace": "ws_mcp-roundtrip",
        }),
    );

    assert_eq!(denied["code"], "capability_denied", "{denied}");
    let message = denied["message"].as_str().expect("a denial message");
    assert!(message.contains("hm_caller"), "{message}");
    assert!(message.contains("mcp-callers.toml"), "{message}");
    assert!(message.contains("operator"), "{message}");
    assert!(
        !message.contains("orbit mcp serve --operator"),
        "advising the flag the caller already passed would send it in a circle: {message}"
    );
}

/// [ORB-11052] Origination is the destination's observation, not the caller's
/// claim. A caller that simply omits the audit label must not thereby present
/// itself as a local session.
#[test]
fn a_remote_session_without_a_caller_label_is_still_resolved_through_the_file() {
    let workspace = McpWorkspace::init();
    write_callers(&workspace, "default = \"deny\"\n");

    let mut client = workspace.serve_with_args_and_env(
        &["--operator"],
        &[("SSH_CONNECTION", "192.0.2.8 43100 198.51.100.2 22")],
    );
    for (name, arguments) in [
        ("orbit_task_list", json!({})),
        (
            "orbit_task_add",
            json!({
                "title": "must not be created",
                "description": "default deny must block ordinary MCP mutations",
                "complexity": "low",
                "model": "codex",
            }),
        ),
        ("orbit_workflow_run_list", json!({})),
    ] {
        let denied = client.call_tool_err(name, arguments);
        assert_eq!(
            denied["code"], "capability_denied",
            "an unlabelled remote caller falls to the file default, never to its own argv: {name}: {denied}"
        );
    }
}

/// [ORB-11056] A workspace-narrowed row falls back to the file default for a
/// call outside its allowed workspaces. With `default = "deny"`, both ordinary
/// reads and mutations must therefore fail before reaching their handlers.
#[test]
fn a_remote_caller_outside_its_workspace_narrowing_cannot_use_ordinary_tools() {
    let workspace = McpWorkspace::init();
    write_callers(
        &workspace,
        r#"
default = "deny"

[[callers]]
machine_id = "hm_caller"
capabilities = ["agent"]
workspaces = ["ws_somewhere-else"]
"#,
    );

    let mut client = workspace.serve_with_args_and_env(
        &["--remote-caller-machine-id", "hm_caller"],
        &[("SSH_CONNECTION", "192.0.2.8 43100 198.51.100.2 22")],
    );
    for (name, arguments) in [
        ("orbit_task_list", json!({})),
        (
            "orbit_task_add",
            json!({
                "title": "must not be created",
                "description": "workspace narrowing must block ordinary MCP mutations",
                "complexity": "low",
                "model": "codex",
            }),
        ),
    ] {
        let denied = client.call_tool_err(name, arguments);
        assert_eq!(denied["code"], "capability_denied", "{name}: {denied}");
    }
}

/// [ORB-11052] The file is a ceiling, not a grant: it can only lower a session
/// below what its argv asked for.
#[test]
fn the_callers_file_never_raises_a_session_above_its_request() {
    let workspace = McpWorkspace::init();
    write_callers(
        &workspace,
        r#"
[[callers]]
machine_id = "hm_caller"
capabilities = ["agent", "operator"]
"#,
    );

    let mut client = workspace.serve_with_args_and_env(
        &["--remote-caller-machine-id", "hm_caller"],
        &[("SSH_CONNECTION", "192.0.2.8 43100 198.51.100.2 22")],
    );
    let listed = client.call_tool_ok("orbit_task_list", json!({}));
    assert_eq!(listed["items"], json!([]));
    let created = client.call_tool_ok(
        "orbit_task_add",
        json!({
            "title": "remote agent task",
            "description": "an agent grant retains ordinary MCP mutations",
            "complexity": "low",
            "model": "codex",
        }),
    );
    assert_eq!(created["title"], "remote agent task");
    let denied = client.call_tool_err("orbit_workflow_run_list", json!({}));

    assert_eq!(
        denied["code"], "capability_denied",
        "a caller granted operator that did not ask for it still holds agent: {denied}"
    );
}

/// [ORB-11052] A granted caller reaches the governed tool, and the audit trail
/// separates what the destination granted from what the session ended up with.
#[test]
fn a_granted_remote_caller_reaches_a_governed_tool_and_is_audited_as_a_remote_grant() {
    let workspace = McpWorkspace::init();
    write_callers(
        &workspace,
        r#"
[[callers]]
machine_id = "hm_caller"
capabilities = ["agent", "operator"]
"#,
    );

    let mut client = workspace.serve_with_args_and_env(
        &["--operator", "--remote-caller-machine-id", "hm_caller"],
        &[("SSH_CONNECTION", "192.0.2.8 43100 198.51.100.2 22")],
    );
    let tasks = client.call_tool_ok("orbit_task_list", json!({}));
    assert_eq!(tasks["items"], json!([]));
    let listed = client.call_tool_ok("orbit_workflow_run_list", json!({}));
    assert_eq!(listed["items"], json!([]));

    // Now the same caller over-asks on a governed tool its narrowing excludes,
    // so a denial row lands and can be inspected.
    drop(client);
    write_callers(
        &workspace,
        r#"
[[callers]]
machine_id = "hm_caller"
capabilities = ["agent", "operator"]
workspaces = ["ws_somewhere-else"]
"#,
    );
    let mut narrowed = workspace.serve_with_args_and_env(
        &["--operator", "--remote-caller-machine-id", "hm_caller"],
        &[("SSH_CONNECTION", "192.0.2.8 43100 198.51.100.2 22")],
    );
    let denied = narrowed.call_tool_err("orbit_workflow_run_list", json!({}));
    assert_eq!(
        denied["code"], "capability_denied",
        "a workspaces narrowing is evaluated against the workspace the call lands in: {denied}"
    );
    drop(narrowed);

    let connection =
        Connection::open(workspace.home.join(".orbit/orbit.db")).expect("open server audit store");
    let (subcommand, effective, arguments) = connection
        .query_row(
            "SELECT subcommand, capabilities_json, arguments_json FROM audit_events \
             WHERE command = 'authorization' AND target_id = 'orbit.workflow.run.list' \
             ORDER BY id DESC LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .expect("an authorization row for the refused governed tool");

    assert_eq!(subcommand.as_deref(), Some("remote-grant"));
    let effective = effective.expect("the effective set is recorded");
    assert!(effective.contains("agent"), "{effective}");
    assert!(
        !effective.contains("operator"),
        "the narrowed call must not hold operator: {effective}"
    );
    let arguments = arguments.expect("the grant is recorded beside the effective set");
    assert!(arguments.contains("hm_caller"), "{arguments}");
    assert!(arguments.contains("granted_capabilities"), "{arguments}");
    assert!(arguments.contains("mcp-callers.toml"), "{arguments}");
}

/// [ORB-11052] A duplicate `machine_id` fails the whole file closed at load —
/// before any session is served, not per call.
#[test]
fn a_duplicate_caller_row_stops_the_server_before_it_serves() {
    let workspace = McpWorkspace::init();
    write_callers(
        &workspace,
        r#"
[[callers]]
machine_id = "hm_caller"
capabilities = ["agent"]

[[callers]]
machine_id = "hm_caller"
capabilities = ["agent", "operator"]
"#,
    );

    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["mcp", "serve", "--remote-caller-machine-id", "hm_caller"])
        .env("SSH_CONNECTION", "192.0.2.8 43100 198.51.100.2 22")
        .stdin(Stdio::null())
        .output()
        .expect("run orbit mcp serve");

    assert!(!output.status.success(), "a duplicate row must fail closed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("hm_caller"), "{stderr}");
}

/// [ORB-11052] A local session's authority resolution is unchanged: no
/// `SSH_CONNECTION`, so the callers file is not consulted at all and
/// `--operator` still means operator — even with a file that would deny.
#[test]
fn a_local_session_keeps_its_argv_authority() {
    let workspace = McpWorkspace::init();
    write_callers(&workspace, "default = \"deny\"\n");

    let mut operator = workspace.serve_with_args(&["--operator"]);
    let listed = operator.call_tool_ok("orbit_workflow_run_list", json!({}));

    assert_eq!(listed["items"], json!([]));
}

/// [ORB-11053] The `authorized_keys` fixture: a real key and the fingerprint
/// `ssh-keygen -l` prints for it.
const CALLER_KEY_FINGERPRINT: &str = "SHA256:5HTlLtSRdZg7lKPho8slfRr2Q1QTPuko05+KRX/8PQw";
#[cfg(target_os = "linux")]
const OTHER_KEY_FINGERPRINT: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
#[cfg(target_os = "linux")]
const CALLER_PUBLIC_KEY: &str = "ssh-ed25519 \
    AAAAC3NzaC1lZDI1NTE5AAAAINMX3zk7E9dEvV0tMWx6b+FKAWBcQiweXKgUOc0AqkKH caller@test";

#[cfg(target_os = "linux")]
struct GeneratedForcedCommand {
    argv: Vec<String>,
    forced_command: String,
    acceptance_token: String,
}

/// Retry a freshly copied test launcher while another parallel test's child
/// still has its writable descriptor inherited across `fork`.
///
/// The descriptor is close-on-exec, but Linux can reject a concurrent exec
/// with `ETXTBSY` during that short pre-exec window. This is the same bounded
/// transient the production updater handles when it launches a newly written
/// binary; all other errors remain immediate test failures.
#[cfg(target_os = "linux")]
fn retry_executable_busy<T>(
    mut operation: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let deadline = Instant::now() + EXEC_BUSY_RETRY_WINDOW;
    loop {
        match operation() {
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(25));
            }
            result => return result,
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_fresh_test_launcher_waits_for_a_writer_to_close() {
    let temp = tempdir().expect("tempdir");
    let launcher = temp.path().join("launcher");
    std::fs::copy("/bin/true", &launcher).expect("copy executable fixture");
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .open(&launcher)
        .expect("hold launcher open for writing");
    let error = Command::new(&launcher)
        .spawn()
        .expect_err("Linux must reject an executable that is open for writing");
    assert_eq!(error.kind(), std::io::ErrorKind::ExecutableFileBusy);
    let release_writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(75));
        drop(writer);
    });

    let mut command = Command::new(&launcher);
    let mut child = retry_executable_busy(|| command.spawn())
        .expect("spawn launcher after transient executable-file-busy errors");
    let status = child.wait().expect("wait for launcher");
    release_writer.join().expect("release launcher writer");

    assert!(status.success(), "copied launcher must eventually execute");
}

/// Install a metadata-valid copy of the tested binary whose setgid exec will
/// change this account's effective group. Sandboxes that set `NoNewPrivs` or
/// expose no mapped supplementary group cannot exercise the kernel boundary;
/// those hosts leave the Linux deployment test to unrestricted CI.
#[cfg(target_os = "linux")]
#[derive(Debug)]
enum ProtectedSshRegressionUnavailable {
    NoNewPrivileges,
    NoMappedSupplementaryGroup,
    GroupOwnershipDenied(std::io::Error),
}

#[cfg(target_os = "linux")]
impl std::fmt::Display for ProtectedSshRegressionUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoNewPrivileges => formatter
                .write_str("NoNewPrivs is enabled, so Linux will ignore the launcher's setgid bit"),
            Self::NoMappedSupplementaryGroup => formatter
                .write_str("no mapped supplementary group differs from the process's real group"),
            Self::GroupOwnershipDenied(error) => write!(
                formatter,
                "the runner denied changing the owned launcher to a supplementary group: {error}"
            ),
        }
    }
}

#[cfg(target_os = "linux")]
fn install_test_ssh_launcher(
    workspace: &McpWorkspace,
) -> Result<PathBuf, ProtectedSshRegressionUnavailable> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let status = std::fs::read_to_string("/proc/self/status")
        .expect("read Linux process status for the setgid capability probe");
    let no_new_privileges = status
        .lines()
        .find_map(|line| line.strip_prefix("NoNewPrivs:"))
        .and_then(|value| value.trim().parse::<u8>().ok())
        .expect("Linux process status must report NoNewPrivs as an integer");
    if no_new_privileges == 1 {
        return Err(ProtectedSshRegressionUnavailable::NoNewPrivileges);
    }
    // Safety: the first call queries the required length; the second writes
    // exactly that many gid_t entries into the allocated vector.
    let group_count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    assert!(
        group_count >= 0,
        "getgroups length query failed: {}",
        std::io::Error::last_os_error()
    );
    if group_count == 0 {
        return Err(ProtectedSshRegressionUnavailable::NoMappedSupplementaryGroup);
    }
    let mut groups = vec![0 as libc::gid_t; group_count as usize];
    let read = unsafe { libc::getgroups(group_count, groups.as_mut_ptr()) };
    assert!(
        read >= 0,
        "getgroups failed: {}",
        std::io::Error::last_os_error()
    );
    assert_eq!(
        read, group_count,
        "getgroups returned an unstable group count"
    );
    // Safety: getgid only reads the process's real group credential.
    let real_group = unsafe { libc::getgid() };
    let launcher_group = groups
        .into_iter()
        .find(|group| *group != real_group)
        .ok_or(ProtectedSshRegressionUnavailable::NoMappedSupplementaryGroup)?;
    let launcher = workspace.home.join("orbit-mcp-ssh-launcher");
    std::fs::copy(env!("CARGO_BIN_EXE_orbit"), &launcher).expect("copy tested Orbit launcher");
    let path = std::ffi::CString::new(launcher.as_os_str().as_encoded_bytes())
        .expect("launcher path has no NUL");
    // Safety: the path is a live NUL-terminated filesystem path; -1 preserves
    // the owner and the selected gid is one of this process's groups.
    if unsafe { libc::chown(path.as_ptr(), !0 as libc::uid_t, launcher_group) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EPERM) {
            return Err(ProtectedSshRegressionUnavailable::GroupOwnershipDenied(
                error,
            ));
        }
        panic!("change test launcher group failed: {error}");
    }
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o2555))
        .expect("protect test SSH launcher");
    let metadata = std::fs::metadata(&launcher).expect("inspect protected test SSH launcher");
    assert_eq!(
        metadata.gid(),
        launcher_group,
        "the protected test launcher must retain the selected supplementary group"
    );
    assert_eq!(
        metadata.mode() & 0o7777,
        0o2555,
        "the protected test launcher must retain its setgid mode"
    );
    Ok(launcher)
}

/// Use the production Tier 2 issuer and renderer to recover the complete
/// command sshd passes to the protected login shell. The operator-facing CLI
/// additionally verifies `/etc/passwd`, which an unprivileged test cannot
/// change; the launch and acceptance path below are otherwise the shipped one.
#[cfg(target_os = "linux")]
fn try_generated_forced_command(
    workspace: &McpWorkspace,
) -> Result<GeneratedForcedCommand, ProtectedSshRegressionUnavailable> {
    let launcher = install_test_ssh_launcher(workspace)?;
    let key = workspace.home.join("caller.pub");
    std::fs::write(&key, format!("{CALLER_PUBLIC_KEY}\n")).expect("write caller public key");
    let public_key = orbit_mcp::parse_public_key(CALLER_PUBLIC_KEY).expect("parse caller key");
    let fingerprint = public_key.fingerprint().expect("fingerprint caller key");
    let acceptance_token =
        orbit_mcp::issue_ssh_acceptance(&workspace.home.join(".orbit"), "hm_caller", &fingerprint)
            .expect("issue SSH acceptance");
    let line = public_key.authorized_keys_line(
        launcher.to_str().expect("utf8 launcher path"),
        "hm_caller",
        &acceptance_token,
    );
    let environment = line
        .strip_prefix("environment=\"")
        .and_then(|line| {
            line.split_once("\",command=\"")
                .map(|(environment, _)| environment)
        })
        .expect("an acceptance environment in the authorized_keys line");
    let rendered_acceptance_token = environment
        .strip_prefix(&format!("{}=", orbit_mcp::SSH_ACCEPTANCE_ENV))
        .expect("the named SSH acceptance environment")
        .to_string();
    assert_eq!(rendered_acceptance_token, acceptance_token);
    let forced_command = line
        .split_once("\",command=\"")
        .and_then(|(_, line)| line.split_once("\",").map(|(command, _)| command))
        .expect("a forced command in the authorized_keys line");
    Ok(GeneratedForcedCommand {
        argv: forced_command
            .split_ascii_whitespace()
            .map(ToOwned::to_owned)
            .collect(),
        forced_command: forced_command.to_string(),
        acceptance_token,
    })
}

#[cfg(target_os = "linux")]
fn generated_forced_command(workspace: &McpWorkspace) -> Option<GeneratedForcedCommand> {
    try_generated_forced_command(workspace).ok()
}

/// [ORB-11184] Setup must reject the ordinary binary before it rotates the
/// destination record. Otherwise the emitted line would carry the bearer into
/// an initially dumpable process even though serving later fails closed.
#[cfg(target_os = "linux")]
#[test]
fn authorize_refuses_an_unprotected_launcher_before_rotation() {
    let workspace = McpWorkspace::init();
    let key = workspace.home.join("caller.pub");
    std::fs::write(&key, format!("{CALLER_PUBLIC_KEY}\n")).expect("write caller public key");

    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args([
            "mcp",
            "callers",
            "authorize",
            "--machine-id",
            "hm_caller",
            "--key",
            key.to_str().expect("utf8 key path"),
            "--launcher",
            env!("CARGO_BIN_EXE_orbit"),
        ])
        .output()
        .expect("reject ordinary Orbit launcher");

    assert!(
        !output.status.success(),
        "ordinary launcher must fail setup"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("mode 2555"), "{stderr}");
    assert!(
        !workspace
            .home
            .join(".orbit/mcp-ssh-acceptance/hm_caller.toml")
            .exists(),
        "launcher validation must precede acceptance rotation"
    );
}

/// [ORB-11058] The supported Tier 2 setup requests the broad authority once,
/// and the destination's callers row remains the grant ceiling. This executes
/// the exact argv emitted into `authorized_keys`, through the real MCP stdio
/// transport, for both sides of that intersection.
// SSH MCP Tier 2 acceptance is a Linux-only deployment: the server refuses to
// serve without the generated isolated-account setup, so this asserts on a
// capability that does not exist on other platforms.
#[cfg(target_os = "linux")]
#[test]
fn the_generated_forced_command_obeys_the_matched_rows_operator_ceiling() {
    let workspace = McpWorkspace::init();
    write_callers(
        &workspace,
        &format!(
            r#"
[[callers]]
machine_id = "hm_caller"
capabilities = ["agent", "operator"]
ssh_key_fingerprint = "{CALLER_KEY_FINGERPRINT}"
"#
        ),
    );
    let Some(generated) = generated_forced_command(&workspace) else {
        return;
    };
    let argv = &generated.argv;
    assert_eq!(&argv[1..4], ["mcp", "serve", "--accept-ssh"]);
    assert!(
        generated.acceptance_token.starts_with(".orbit-ssh-"),
        "the setup must inject a destination-issued capability outside argv: {argv:?}"
    );
    assert_eq!(
        &argv[4..],
        ["--caller", "hm_caller", "--operator"],
        "the rendered destination-owned request must ask for operator"
    );
    assert!(
        !argv
            .iter()
            .any(|argument| argument == &generated.acceptance_token),
        "a reusable acceptance bearer must never enter server argv: {argv:?}"
    );

    let mut operator = workspace.serve_with_generated_command(&generated);
    let listed = operator.call_tool_ok("orbit_workflow_run_list", json!({}));
    assert_eq!(listed["items"], json!([]));
    drop(operator);

    write_callers(
        &workspace,
        &format!(
            r#"
[[callers]]
machine_id = "hm_caller"
capabilities = ["agent"]
ssh_key_fingerprint = "{CALLER_KEY_FINGERPRINT}"
"#
        ),
    );
    let mut capped = workspace.serve_with_generated_command(&generated);
    let denied = capped.call_tool_err("orbit_workflow_run_list", json!({}));
    assert_eq!(
        denied["code"], "capability_denied",
        "the same operator request must remain capped by an agent-only row: {denied}"
    );
    drop(capped);

    write_callers(&workspace, "default = \"deny\"\n");
    let mut denied_by_default = workspace.serve_with_generated_command(&generated);
    let denied = denied_by_default.call_tool_err("orbit_workflow_run_list", json!({}));
    assert_eq!(
        denied["code"], "capability_denied",
        "the generated request must not raise a deny grant: {denied}"
    );
}

/// A destination can delegate exactly the trusted-host operation without
/// turning every remote operator into an unsandboxed dispatcher.
/// This crosses the destination-issued forced command, real MCP transport,
/// workspace routing, durable run submission, worker, response projection,
/// and revocation-on-reconnect boundaries.
#[cfg(target_os = "linux")]
#[test]
fn a_key_bound_remote_operator_invokes_an_agent_only_in_the_granted_workspace() {
    let workspace = McpWorkspace::init();
    // `model` attributes the caller; the workspace's frozen default crew picks
    // the provider. Stub both possible selected providers, because an inherited
    // host `claude` binary makes `opus` the default crew during fixture init.
    plant_agent_response_stub(&McpWorkspace::stub_bin_dir(&workspace.home), "codex");
    plant_agent_response_stub(&McpWorkspace::stub_bin_dir(&workspace.home), "claude");
    let registry: Value = serde_json::from_str(
        &std::fs::read_to_string(workspace.home.join(".orbit/workspaces.json"))
            .expect("read workspace registry"),
    )
    .expect("parse workspace registry");
    let workspace_id = registry["workspaces"][0]["id"]
        .as_str()
        .expect("workspace id");

    let Some(generated) = generated_forced_command(&workspace) else {
        return;
    };

    write_callers(
        &workspace,
        &format!(
            r#"
default = "deny"

[[callers]]
machine_id = "hm_caller"
capabilities = ["agent", "operator"]
ssh_key_fingerprint = "{CALLER_KEY_FINGERPRINT}"
agent_invoke = true
agent_invoke_workspaces = ["ws_wrong"]
"#,
        ),
    );
    let mut wrong_workspace = workspace.serve_with_generated_command(&generated);
    assert_eq!(
        wrong_workspace.call_tool_ok("orbit_workflow_run_list", json!({}))["items"],
        json!([]),
        "the invocation-only scope must not reduce ordinary operator access"
    );
    let denied = wrong_workspace.call_tool_err(
        "orbit_agent_invoke",
        json!({
            "prompt": "read-only remote probe",
            "cwd": workspace.work,
            "timeout_seconds": 30,
        }),
    );
    assert_eq!(denied["code"], "capability_denied", "{denied}");
    drop(wrong_workspace);

    write_callers(
        &workspace,
        &format!(
            r#"
default = "deny"

[[callers]]
machine_id = "hm_caller"
capabilities = ["agent", "operator"]
ssh_key_fingerprint = "{CALLER_KEY_FINGERPRINT}"
agent_invoke = true
agent_invoke_workspaces = ["{workspace_id}"]
"#,
        ),
    );
    let mut granted = workspace.serve_with_generated_command(&generated);
    let submitted = granted.call_tool_ok(
        "orbit_agent_invoke",
        json!({
            "prompt": "read-only remote probe",
            "cwd": workspace.work,
            "timeout_seconds": 30,
            "model": "codex",
        }),
    );
    assert_eq!(submitted["authorized_by"], "hm_caller");
    assert_eq!(submitted["authorizer_provenance"], "remote-grant");
    assert_eq!(submitted["caller_machine_id"], "hm_caller");
    assert_eq!(submitted["caller_identity"], "key-bound");
    assert_eq!(
        submitted["workspace_path"],
        workspace.work.display().to_string()
    );
    assert_eq!(submitted["timeout_seconds"], 30);
    let run_id = submitted["run_id"].as_str().expect("run id").to_string();

    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    let terminal = loop {
        let shown = granted.call_tool_ok("orbit_workflow_run_show", json!({ "id": run_id }));
        if matches!(
            shown["state"].as_str(),
            Some("success" | "failed" | "timeout" | "cancelled" | "interrupted")
        ) {
            break shown;
        }
        assert!(
            Instant::now() < deadline,
            "agent invocation did not finish: {shown}"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(terminal["state"], "success", "{terminal}");
    assert_eq!(terminal["agent_invocation"]["completed_envelope"], true);
    assert_eq!(
        terminal["agent_invocation"]["summary"],
        "remote probe complete"
    );

    let connection =
        Connection::open(workspace.home.join(".orbit/orbit.db")).expect("open destination store");
    let input_json: String = connection
        .query_row(
            "SELECT input_json FROM job_runs WHERE run_id = ?1",
            [&run_id],
            |row| row.get(0),
        )
        .expect("persisted invocation input");
    let input: Value = serde_json::from_str(&input_json).expect("parse invocation input");
    let admission = &input["trusted_host_admission"];
    assert_eq!(admission["caller_machine_id"], "hm_caller");
    assert_eq!(admission["caller_identity"], "key-bound");
    assert_eq!(
        admission["workspace_path"],
        workspace.work.display().to_string()
    );
    drop(granted);

    write_callers(
        &workspace,
        &format!(
            r#"
default = "deny"

[[callers]]
machine_id = "hm_caller"
capabilities = ["agent", "operator"]
workspaces = ["{workspace_id}"]
ssh_key_fingerprint = "{CALLER_KEY_FINGERPRINT}"
agent_invoke = false
"#,
        ),
    );
    let mut revoked = workspace.serve_with_generated_command(&generated);
    let denied = revoked.call_tool_err(
        "orbit_agent_invoke",
        json!({
            "prompt": "must not run",
            "cwd": workspace.work,
            "timeout_seconds": 30,
        }),
    );
    assert_eq!(denied["code"], "capability_denied", "{denied}");
}

/// The destination may deliberately trust the existing SSH operator channel
/// when both ends cooperate through the same OS account. The invocation is
/// still workspace- and operation-scoped, while its output and durable
/// admission must say `cooperative` and `self-asserted`, never `key-bound`.
#[test]
fn a_cooperative_ssh_operator_invocation_records_its_actual_trust_boundary() {
    let workspace = McpWorkspace::init();
    plant_agent_response_stub(&McpWorkspace::stub_bin_dir(&workspace.home), "codex");
    let registry: Value = serde_json::from_str(
        &std::fs::read_to_string(workspace.home.join(".orbit/workspaces.json"))
            .expect("read workspace registry"),
    )
    .expect("parse workspace registry");
    let workspace_id = registry["workspaces"][0]["id"]
        .as_str()
        .expect("workspace id");

    write_callers(
        &workspace,
        &format!(
            r#"
default = "deny"

[[callers]]
machine_id = "hm_caller"
capabilities = ["agent", "operator"]
agent_invoke = true
agent_invoke_mode = "cooperative"
agent_invoke_workspaces = ["{workspace_id}"]
"#,
        ),
    );
    let mut cooperative = workspace.serve_with_args_and_env(
        &["--operator", "--remote-caller-machine-id", "hm_caller"],
        &[("SSH_CONNECTION", "192.0.2.8 43100 198.51.100.2 22")],
    );
    let submitted = cooperative.call_tool_ok(
        "orbit_agent_invoke",
        json!({
            "prompt": "read-only cooperative probe",
            "cwd": workspace.work,
            "crew": "system",
            "timeout_seconds": 30,
            "model": "codex",
        }),
    );

    assert_eq!(submitted["authorized_by"], "hm_caller");
    assert_eq!(submitted["authorizer_provenance"], "remote-grant");
    assert_eq!(submitted["caller_machine_id"], "hm_caller");
    assert_eq!(submitted["caller_identity"], "self-asserted");
    assert_eq!(submitted["agent_invoke_mode"], "cooperative");
    let run_id = submitted["run_id"].as_str().expect("run id").to_string();

    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    loop {
        let shown = cooperative.call_tool_ok("orbit_workflow_run_show", json!({ "id": run_id }));
        if matches!(
            shown["state"].as_str(),
            Some("success" | "failed" | "timeout" | "cancelled" | "interrupted")
        ) {
            assert_eq!(shown["state"], "success", "{shown}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "cooperative agent invocation did not finish: {shown}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    let connection =
        Connection::open(workspace.home.join(".orbit/orbit.db")).expect("destination store");
    let input_json: String = connection
        .query_row(
            "SELECT input_json FROM job_runs WHERE run_id = ?1",
            [&run_id],
            |row| row.get(0),
        )
        .expect("persisted invocation input");
    let input: Value = serde_json::from_str(&input_json).expect("parse invocation input");
    let admission = &input["trusted_host_admission"];
    assert_eq!(admission["caller_identity"], "self-asserted");
    assert_eq!(admission["agent_invoke_mode"], "cooperative");
    assert_eq!(
        admission["workspace_path"],
        workspace.work.display().to_string()
    );
}

/// [ORB-11184] A separate ordinary same-UID process scans every process's argv
/// and environment continuously while the generated protected launcher starts
/// repeated legitimate destinations. The kernel boundary exists at exec, so
/// there is no pre-main interval in which the scanner can recover the bearer;
/// copied public argv still cannot reconstruct a key-bound session.
#[cfg(target_os = "linux")]
#[test]
#[allow(clippy::print_stdout)]
fn process_metadata_does_not_supply_a_replayable_key_bound_identity() {
    let workspace = McpWorkspace::init();
    write_callers(
        &workspace,
        &format!(
            r#"
[[callers]]
machine_id = "hm_caller"
capabilities = ["agent", "operator"]
ssh_key_fingerprint = "{CALLER_KEY_FINGERPRINT}"
"#
        ),
    );
    let generated = match try_generated_forced_command(&workspace) {
        Ok(generated) => generated,
        Err(reason) => {
            assert!(
                std::env::var_os(REQUIRE_PROTECTED_SSH_REGRESSION_ENV).is_none(),
                "the protected SSH process-metadata regression is required on this runner but \
                 cannot execute: {reason}"
            );
            println!(
                "skipping protected SSH process-metadata regression: {reason}; \
                 unrestricted CI requires this test to execute"
            );
            return;
        }
    };
    let mut legitimate = workspace.serve_with_generated_command(&generated);

    let stop = workspace.home.join("stop-proc-scanner");
    let scanner_script = r#"
IFS= read -r needle
IFS= read -r public_caller
printf 'ready\n'
sweeps=0
saw_public_caller=0
while [ ! -e "$1" ] || [ "$sweeps" -lt 100 ]; do
  for process in /proc/[0-9]*; do
    for field in cmdline environ; do
      metadata="$process/$field"
      if grep -a -F -q -- "$needle" "$metadata" 2>/dev/null; then
        printf 'leak:%s\n' "$metadata"
        exit 0
      fi
    done
    if grep -a -F -q -- "$public_caller" "$process/cmdline" 2>/dev/null; then
      saw_public_caller=1
    fi
  done
  sweeps=$((sweeps + 1))
done
printf 'clear:%s:public=%s\n' "$sweeps" "$saw_public_caller"
"#;
    let mut scanner = Command::new("/bin/sh")
        .args([
            "-c",
            scanner_script,
            "orbit-proc-scanner",
            stop.to_str().expect("utf8 scanner stop path"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn separate same-UID proc scanner");
    let mut scanner_stdin = scanner.stdin.take().expect("scanner stdin");
    writeln!(scanner_stdin, "{}", generated.acceptance_token).expect("send scanner needle");
    writeln!(scanner_stdin, "hm_caller").expect("send observable caller label");
    drop(scanner_stdin);
    let mut scanner_stdout = BufReader::new(scanner.stdout.take().expect("scanner stdout"));
    let mut ready = String::new();
    scanner_stdout.read_line(&mut ready).expect("scanner ready");
    assert_eq!(ready, "ready\n");

    let mut repeated_launches = Vec::new();
    for _ in 0..48 {
        let argv = &generated.argv;
        let mut command = McpWorkspace::orbit_program_command(
            Path::new(&argv[0]),
            &workspace.work,
            &workspace.home,
        );
        command
            .args(["-c", generated.forced_command.as_str()])
            .env(orbit_mcp::SSH_ACCEPTANCE_ENV, &generated.acceptance_token)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = retry_executable_busy(|| command.spawn())
            .expect("start repeated legitimate protected destination");
        repeated_launches.push(child);
    }
    std::fs::write(&stop, "stop\n").expect("stop proc scanner");
    let mut scan_result = String::new();
    scanner_stdout
        .read_to_string(&mut scan_result)
        .expect("read proc scanner result");
    let scanner_output = scanner.wait_with_output().expect("wait for proc scanner");
    assert!(scanner_output.status.success(), "proc scanner failed");
    assert!(
        scan_result.starts_with("clear:") && scan_result.ends_with(":public=1\n"),
        "a separate same-UID scanner recovered the reusable acceptance value: {scan_result}"
    );
    for mut child in repeated_launches {
        let _ = child.kill();
        let _ = child.wait();
    }

    let replay = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(&generated.argv[1..])
        .env("SSH_CONNECTION", "192.0.2.8 43100 198.51.100.2 22")
        .env(
            "SSH_ORIGINAL_COMMAND",
            "orbit mcp serve --operator --remote-caller-machine-id hm_caller",
        )
        .stdin(Stdio::null())
        .output()
        .expect("replay observable process state");
    assert!(
        !replay.status.success(),
        "observable argv must not authenticate"
    );
    assert!(
        String::from_utf8_lossy(&replay.stderr).contains("did not enter through"),
        "the replay must fail before a KeyBound identity is stamped: {}",
        String::from_utf8_lossy(&replay.stderr)
    );

    let listed = legitimate.call_tool_ok("orbit_workflow_run_list", json!({}));
    assert_eq!(
        listed["items"],
        json!([]),
        "the legitimate protected session must retain its pinned caller and grant"
    );
}

/// [ORB-11057] The public flag name is not proof that a destination generated
/// the argv. Without the destination-issued value the old forged invocation is
/// rejected before a caller row can be selected.
// SSH MCP Tier 2 acceptance is a Linux-only deployment: the server refuses to
// serve without the generated isolated-account setup, so this asserts on a
// capability that does not exist on other platforms.
#[cfg(target_os = "linux")]
#[test]
fn caller_controlled_accept_ssh_flags_cannot_select_a_caller_row() {
    let workspace = McpWorkspace::init();
    write_callers(
        &workspace,
        r#"
[[callers]]
machine_id = "hm_caller"
capabilities = ["agent"]
"#,
    );

    for environment in [None, Some(("SSH_CONNECTION", "forged by caller"))] {
        let mut command = McpWorkspace::orbit_command(&workspace.work, &workspace.home);
        command
            .args([
                "mcp",
                "serve",
                "--accept-ssh",
                "--caller",
                "hm_caller",
                "--operator",
            ])
            .env(orbit_mcp::SSH_ACCEPTANCE_ENV, "caller-controlled-token");
        if let Some((name, value)) = environment {
            command.env(name, value).env(
                "SSH_ORIGINAL_COMMAND",
                "orbit mcp serve --accept-ssh --caller hm_caller",
            );
        }
        let output = command
            .stdin(Stdio::null())
            .output()
            .expect("run forged orbit mcp serve");
        assert!(!output.status.success(), "forged acceptance must fail");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("did not enter through"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// [ORB-11053] `SSH_ORIGINAL_COMMAND` is ignored entirely — not parsed, not
/// merged, not used to derive a requested authority.
///
/// The caller asks for agent in its original command while the destination's
/// generated forced command requests operator, and the callers file permits
/// both. The session still holds operator because the request comes from the
/// argv *this machine* composed, not from `SSH_ORIGINAL_COMMAND`.
// SSH MCP Tier 2 acceptance is a Linux-only deployment: the server refuses to
// serve without the generated isolated-account setup, so this asserts on a
// capability that does not exist on other platforms.
#[cfg(target_os = "linux")]
#[test]
fn a_forced_command_ignores_the_command_the_caller_asked_for() {
    let workspace = McpWorkspace::init();
    write_callers(
        &workspace,
        r#"
[[callers]]
machine_id = "hm_caller"
capabilities = ["agent", "operator"]
"#,
    );

    let Some(generated) = generated_forced_command(&workspace) else {
        return;
    };
    let argv = &generated.argv;
    let mut command =
        McpWorkspace::orbit_program_command(Path::new(&argv[0]), &workspace.work, &workspace.home);
    command
        .args(["-c", &generated.forced_command])
        .env(orbit_mcp::SSH_ACCEPTANCE_ENV, &generated.acceptance_token)
        .env("SSH_ORIGINAL_COMMAND", "orbit mcp serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child =
        retry_executable_busy(|| command.spawn()).expect("spawn destination-issued command");
    let mut client = McpClient::new(child);
    workspace.initialize(&mut client);
    let listed = client.call_tool_ok("orbit_workflow_run_list", json!({}));

    assert_eq!(
        listed["items"],
        json!([]),
        "the caller's own command must contribute nothing to the requested authority: {listed}"
    );
}

/// [ORB-11053] `--caller` is honored only under `--accept-ssh`. On an ordinary
/// `orbit mcp serve` it is a caller-supplied flag, and a caller-supplied
/// identity is the escalation this tier exists to close.
#[test]
fn an_ordinary_serve_refuses_to_take_a_caller_identity() {
    let workspace = McpWorkspace::init();

    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["mcp", "serve", "--caller", "hm_caller"])
        .stdin(Stdio::null())
        .output()
        .expect("run orbit mcp serve");

    assert!(
        !output.status.success(),
        "an identity nobody authenticated must not be accepted"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--accept-ssh"), "{stderr}");
}

/// [ORB-11057] The former fingerprint argv is not an authentication source.
#[test]
fn copied_fingerprint_flags_are_refused() {
    let workspace = McpWorkspace::init();
    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args([
            "mcp",
            "serve",
            "--accept-ssh",
            "--caller",
            "hm_caller",
            "--caller-key-fingerprint",
            CALLER_KEY_FINGERPRINT,
        ])
        .env("SSH_CONNECTION", "192.0.2.8 43100 198.51.100.2 22")
        .env("SSH_ORIGINAL_COMMAND", "caller controlled")
        .stdin(Stdio::null())
        .output()
        .expect("run legacy forged argv");

    assert!(
        !output.status.success(),
        "copied fingerprint must not authenticate"
    );
}

/// [ORB-11053] A pinned row is enforced where the key is observable, and a
/// mismatch stops the session at establishment rather than serving it at a
/// lower ceiling — which would be indistinguishable from a smaller grant.
// SSH MCP Tier 2 acceptance is a Linux-only deployment: the server refuses to
// serve without the generated isolated-account setup, so this asserts on a
// capability that does not exist on other platforms.
#[cfg(target_os = "linux")]
#[test]
fn a_key_mismatch_stops_the_server_before_it_serves() {
    let workspace = McpWorkspace::init();
    write_callers(
        &workspace,
        &format!(
            r#"
[[callers]]
machine_id = "hm_caller"
capabilities = ["agent", "operator"]
ssh_key_fingerprint = "{OTHER_KEY_FINGERPRINT}"
"#
        ),
    );
    let Some(generated) = generated_forced_command(&workspace) else {
        return;
    };
    let argv = &generated.argv;
    let mut command =
        McpWorkspace::orbit_program_command(Path::new(&argv[0]), &workspace.work, &workspace.home);
    command
        .args(["-c", generated.forced_command.as_str()])
        .env(orbit_mcp::SSH_ACCEPTANCE_ENV, &generated.acceptance_token)
        .stdin(Stdio::null());
    let output = retry_executable_busy(|| command.output()).expect("run orbit mcp serve");

    assert!(!output.status.success(), "a key mismatch must fail closed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("hm_caller"), "{stderr}");
}

/// [ORB-11053] The matching key is served, and the trail says the identity was
/// proved rather than claimed. Tier 2 is opt-in, so a reader who cannot tell
/// the tiers apart in the audit row would have to assume which one ran.
// SSH MCP Tier 2 acceptance is a Linux-only deployment: the server refuses to
// serve without the generated isolated-account setup, so this asserts on a
// capability that does not exist on other platforms.
#[cfg(target_os = "linux")]
#[test]
fn a_key_bound_caller_is_served_and_audited_as_key_bound() {
    let workspace = McpWorkspace::init();
    write_callers(
        &workspace,
        &format!(
            r#"
[[callers]]
machine_id = "hm_caller"
capabilities = ["agent", "operator"]
ssh_key_fingerprint = "{CALLER_KEY_FINGERPRINT}"
"#
        ),
    );

    let Some(generated) = generated_forced_command(&workspace) else {
        return;
    };
    let mut client = workspace.serve_with_generated_command(&generated);
    let listed = client.call_tool_ok("orbit_workflow_run_list", json!({}));
    assert_eq!(listed["items"], json!([]));
    drop(client);

    // Now the same key-bound caller over-asks, so a denial row lands and the
    // recorded grant can be inspected. Only a denial (or an override) writes
    // an authorization row; an ordinary success has nothing to explain.
    write_callers(
        &workspace,
        &format!(
            r#"
[[callers]]
machine_id = "hm_caller"
capabilities = ["agent"]
ssh_key_fingerprint = "{CALLER_KEY_FINGERPRINT}"
"#
        ),
    );
    let mut capped = workspace.serve_with_generated_command(&generated);
    let denied = capped.call_tool_err("orbit_workflow_run_list", json!({}));
    assert_eq!(denied["code"], "capability_denied", "{denied}");
    drop(capped);

    let arguments = last_authorization_arguments(&workspace);
    assert!(arguments.contains("hm_caller"), "{arguments}");
    assert!(
        arguments.contains("key-bound"),
        "the trail must separate a proved identity from a claimed one: {arguments}"
    );
}

/// [ORB-11053] A Tier 1 destination stays valid, and says so in the trail. The
/// same refusal, keyed on a label the caller forwarded, is recorded as
/// self-asserted rather than left for a reader to assume.
#[test]
fn a_tier_one_destination_is_audited_as_self_asserted() {
    let workspace = McpWorkspace::init();
    write_callers(
        &workspace,
        r#"
[[callers]]
machine_id = "hm_caller"
capabilities = ["agent"]
"#,
    );

    let mut client = workspace.serve_with_args_and_env(
        &["--operator", "--remote-caller-machine-id", "hm_caller"],
        &[("SSH_CONNECTION", "192.0.2.8 43100 198.51.100.2 22")],
    );
    let denied = client.call_tool_err("orbit_workflow_run_list", json!({}));
    assert_eq!(denied["code"], "capability_denied", "{denied}");
    drop(client);

    let arguments = last_authorization_arguments(&workspace);
    assert!(
        arguments.contains("self-asserted"),
        "a Tier 1 grant must be legible as one rather than assumed: {arguments}"
    );
}

/// The `arguments_json` of the most recent authorization row for the governed
/// workflow-list tool.
fn last_authorization_arguments(workspace: &McpWorkspace) -> String {
    let connection =
        Connection::open(workspace.home.join(".orbit/orbit.db")).expect("open server audit store");
    connection
        .query_row(
            "SELECT arguments_json FROM audit_events \
             WHERE command = 'authorization' AND target_id = 'orbit.workflow.run.list' \
             ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get::<_, Option<String>>(0),
        )
        .expect("an authorization row for the governed tool")
        .expect("the grant is recorded beside the effective set")
}

fn write_callers(workspace: &McpWorkspace, contents: &str) {
    let orbit_home = workspace.home.join(".orbit");
    std::fs::create_dir_all(&orbit_home).expect("global orbit root");
    std::fs::write(orbit_home.join("mcp-callers.toml"), contents).expect("write callers file");
}

/// ORB-10960: `orbit workspace init --mcp` is the operator-facing bootstrap
/// path. This proves it end to end — configuration output through server
/// startup — rather than only unit-testing the argv string builder: it runs
/// the real CLI to generate a Claude Code integration, extracts the exact
/// argv that integration launches, spawns `orbit` with that argv over the
/// real MCP stdio transport, and confirms a governed workflow tool is
/// authorized. Re-running the same reconciliation path must refresh the
/// entry to a single `--operator` argument rather than duplicating it.
#[test]
fn workspace_init_mcp_config_reaches_a_governed_tool_over_the_real_transport() {
    let workspace = McpWorkspace::init();
    std::fs::create_dir_all(workspace.work.join(".claude")).expect("create .claude marker");

    let reconcile = || {
        let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
            .args([
                "workspace",
                "init",
                "--name",
                "mcp-roundtrip",
                "--force",
                "--mcp",
            ])
            .output()
            .expect("run workspace init --force --mcp");
        assert!(
            output.status.success(),
            "workspace init --force --mcp failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };

    let read_generated_args = || -> Vec<String> {
        let claude_mcp: Value = serde_json::from_str(
            &std::fs::read_to_string(workspace.work.join(".claude.json"))
                .expect("read generated claude mcp config"),
        )
        .expect("parse generated claude mcp config");
        claude_mcp["mcpServers"]["orbit"]["args"]
            .as_array()
            .expect("generated args array")
            .iter()
            .map(|value| value.as_str().expect("arg is a string").to_string())
            .collect()
    };

    reconcile();
    let args = read_generated_args();
    assert_eq!(
        args,
        vec![
            "mcp".to_string(),
            "serve".to_string(),
            "--operator".to_string(),
            "--workspace".to_string(),
            "ws_mcp-roundtrip".to_string(),
        ],
        "orbit workspace init --mcp must write the authority it grants and the workspace it \
         registered"
    );

    // Re-running the same reconciliation path (`--force --mcp`, as a second
    // `orbit workspace init --mcp` bootstrap would do) must refresh the
    // managed entry rather than append a second `--operator` argument or a
    // second binding.
    reconcile();
    assert_eq!(
        read_generated_args(),
        args,
        "refreshing the operator-authorized entry must not duplicate its arguments"
    );

    // Spawn the exact argv the generated config launches, over the real MCP
    // stdio transport, and prove it reaches a governed workflow tool.
    let mut command = McpWorkspace::orbit_command(&workspace.work, &workspace.home);
    command
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .expect("spawn the server launched by the generated config");
    let mut client = McpClient::new(child);
    workspace.initialize(&mut client);

    let listed = client.call_tool_ok("orbit_workflow_run_list", json!({}));
    assert_eq!(listed["items"], json!([]));
}

#[test]
fn mcp_serve_lists_the_canonical_surface_outside_any_checkout() {
    let workspace = McpWorkspace::init();
    let scratch = workspace.home.join("scratch");
    std::fs::create_dir_all(&scratch).expect("create non-workspace launch dir");
    let child = McpWorkspace::orbit_command(&scratch, &workspace.home)
        .args(["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn checkout-independent MCP server");
    let mut client = McpClient::new(child);
    client.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "outside-checkout", "version": "0" }
        }),
    );
    client.notify("notifications/initialized");

    let listed = client.request("tools/list", Value::Null);
    let names = listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"orbit_task_show"));

    // `task show` needs no selector — it follows the ID — so a server outside
    // any checkout reports the ID as unknown rather than as unaddressable.
    let missing = client.call_tool_err("orbit_task_show", json!({ "id": "ORB-00001" }));
    assert!(
        missing["message"]
            .as_str()
            .is_some_and(|message| message.contains("ORB-00001")),
        "an unregistered id must be reported as not found: {missing}"
    );
    // Every other workspace-scoped tool still requires one. This session was
    // launched with no `--workspace` and announced none at initialize, so it
    // is unbound and must stay fail-closed [ORB-10967].
    let unscoped = client.call_tool_err("orbit_task_list", json!({}));
    assert!(unscoped["message"].as_str().is_some_and(|message| {
        message.contains("requires an explicit workspace selector")
            && message.contains("orbit_workspace_list")
            && message.contains("returned `ws_*` ID")
            && message.contains("orbit workspace init")
            && message.contains("never infers one from the server process cwd")
    }));
    let description = tool_workspace_description(&listed, "orbit_task_list");
    assert!(
        description.contains("Required in this session"),
        "an unbound session must advertise the selector as required: {description}"
    );

    // An explicit valid selector is the one way out, and it works.
    let scoped = client.call_tool_ok(
        "orbit_task_list",
        json!({ "workspace": "ws_mcp-roundtrip", "limit": 1 }),
    );
    assert!(
        scoped.get("items").is_some() || scoped.is_array(),
        "an explicit selector must route an unbound session: {scoped}"
    );
    drop(client);

    let connection =
        Connection::open(workspace.home.join(".orbit/orbit.db")).expect("open server audit store");
    let audited = connection
        .query_row(
            "SELECT COUNT(*), status, trace_id, caller_machine_id, process_machine_id
             FROM audit_events WHERE tool_name = 'orbit.task.show'",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .expect("missing-selector audit");
    assert_eq!(audited.0, 1, "recognized setup failure is audited once");
    assert_eq!(audited.1, "failure");
    assert!(
        audited
            .2
            .as_deref()
            .is_some_and(|id| id.starts_with("trace-"))
    );
    assert_eq!(audited.3, audited.4);
}

#[test]
fn uninitialized_unbound_mcp_launch_gives_setup_guidance_without_operator_authority() {
    let registry_metadata = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../server.json");
    let registry: Value = serde_json::from_str(
        &std::fs::read_to_string(&registry_metadata).expect("read checked-in registry metadata"),
    )
    .expect("parse checked-in registry metadata");
    assert_eq!(registry["name"], "io.github.constellation-works/orbit");
    assert_eq!(registry["packages"][0]["identifier"], "@orbit-tools/cli");
    assert_eq!(
        registry["packages"][0]["packageArguments"],
        json!([
            { "type": "positional", "value": "mcp" },
            { "type": "positional", "value": "serve" }
        ])
    );
    assert!(
        !serde_json::to_string(&registry)
            .expect("serialize registry metadata")
            .contains("--operator"),
        "the registry install path must not grant operator authority"
    );

    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let scratch = temp.path().join("scratch");
    std::fs::create_dir_all(&home).expect("create clean home");
    std::fs::create_dir_all(&scratch).expect("create non-workspace launch dir");

    // This is the registry-launch shape: no workspace binding, no operator
    // flag, and a cwd that cannot supply an ambient workspace.
    let child = McpWorkspace::orbit_command(&scratch, &home)
        .args(["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn clean registry-style MCP server");
    let mut client = McpClient::new(child);
    let initialized = client.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "registry-clean-launch", "version": "0" }
        }),
    );
    assert_eq!(initialized["result"]["serverInfo"]["name"], "orbit-mcp");
    client.notify("notifications/initialized");

    let listed = client.request("tools/list", Value::Null);
    assert!(
        listed["result"]["tools"]
            .as_array()
            .is_some_and(|tools| tools
                .iter()
                .any(|tool| tool["name"] == "orbit_workspace_list")),
        "the first routing tool must be available from a clean launch: {listed}"
    );

    let unscoped = client.call_tool_err("orbit_task_list", json!({}));
    assert!(unscoped["message"].as_str().is_some_and(|message| {
        message.contains("orbit_workspace_list")
            && message.contains("returned `ws_*` ID")
            && message.contains("orbit init")
            && message.contains("orbit workspace init")
            && message.contains("never infers one from the server process cwd")
    }));

    let workspaces = client.call_tool_ok("orbit_workspace_list", json!({}));
    assert_eq!(workspaces["workspaces"], json!([]));
}

#[test]
fn ssh_marked_mcp_server_audits_caller_and_server_identity_separately() {
    let workspace = McpWorkspace::init();
    let scratch = workspace.home.join("server-scratch");
    std::fs::create_dir_all(&scratch).expect("create server launch dir");
    let child = McpWorkspace::orbit_command(&scratch, &workspace.home)
        .args(["mcp", "serve", "--remote-caller-machine-id", "hm_caller"])
        .env("SSH_CONNECTION", "192.0.2.8 43100 198.51.100.2 22")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn SSH-marked MCP server");
    let mut client = McpClient::new(child);
    let initialized = client.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "remote-roundtrip", "version": "0" },
            "_meta": { "orbit": { "workspace": "ws_mcp-roundtrip" } },
        }),
    );
    assert_eq!(initialized["result"]["serverInfo"]["name"], "orbit-mcp");
    client.notify("notifications/initialized");

    let listed = client.request("tools/list", Value::Null);
    let names = listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect::<Vec<_>>();
    assert!(
        names.contains(&"orbit_task_add"),
        "missing task surface: {names:?}"
    );
    assert!(names.contains(&"orbit_friction_update"));

    let workspaces = client.call_tool_ok("orbit_workspace_list", json!({}));
    assert!(workspaces["machine_id"].as_str().is_some());
    assert!(
        workspaces["workspaces"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "server-local workspace discovery returned no workspaces: {workspaces}"
    );

    let created = client.call_tool_ok(
        "orbit_task_add",
        json!({
            "workspace": "ws_mcp-roundtrip",
            "title": "Remote server round trip",
            "description": "Created through the server-local runtime",
            "complexity": "low",
            "model": "codex"
        }),
    );
    assert_eq!(created["title"], "Remote server round trip");
    let wire_payload = serde_json::to_string(&(listed, created)).expect("serialize wire payload");
    assert!(!wire_payload.contains(workspace.work.to_string_lossy().as_ref()));
    assert!(!wire_payload.contains(workspace.home.to_string_lossy().as_ref()));
    drop(client);

    let connection =
        Connection::open(workspace.home.join(".orbit/orbit.db")).expect("open server audit store");
    let audit = connection
        .query_row(
            "SELECT COUNT(*), workspace_id, caller_machine_id, process_machine_id, transport, trace_id, caller_ip
             FROM audit_events WHERE tool_name = 'orbit.task.add'",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .expect("remote task audit");
    assert_eq!(audit.0, 1, "one audit row per accepted call");
    assert_eq!(audit.1.as_deref(), Some("ws_mcp-roundtrip"));
    assert_eq!(audit.2.as_deref(), Some("hm_caller"));
    assert!(audit.3.as_deref().is_some_and(|id| id.starts_with("hm_")));
    assert_ne!(audit.2, audit.3, "caller and server process stay distinct");
    assert_eq!(audit.4.as_deref(), Some("ssh-mcp"));
    assert!(
        audit
            .5
            .as_deref()
            .is_some_and(|id| id.starts_with("trace-"))
    );
    assert_eq!(audit.6.as_deref(), Some("192.0.2.8"));

    let workspace_audit = connection
        .query_row(
            "SELECT COUNT(*), workspace_id, caller_machine_id, process_machine_id, process_host_id, transport, trace_id, caller_ip
             FROM audit_events WHERE tool_name = 'orbit.workspace.list'",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .expect("remote workspace-list audit");
    assert_eq!(
        workspace_audit.0, 1,
        "workspace discovery records exactly one MCP audit row"
    );
    assert_eq!(workspace_audit.1, None, "global call has no workspace id");
    assert_eq!(workspace_audit.2.as_deref(), Some("hm_caller"));
    assert!(
        workspace_audit
            .3
            .as_deref()
            .is_some_and(|id| id.starts_with("hm_"))
    );
    assert!(workspace_audit.4.as_deref().is_some());
    assert_eq!(workspace_audit.5.as_deref(), Some("ssh-mcp"));
    assert!(
        workspace_audit
            .6
            .as_deref()
            .is_some_and(|id| id.starts_with("trace-"))
    );
    assert_eq!(workspace_audit.7.as_deref(), Some("192.0.2.8"));
}

/// The TCP listener through the production binary: same tool surface, same
/// single Core dispatch and audit boundary, plus the accepted peer's IP.
#[test]
fn mcp_listen_round_trips_over_a_loopback_socket_and_audits_the_peer_ip() {
    let workspace = McpWorkspace::init();
    let addr = free_loopback_addr();
    let mut client = workspace.listen(addr);

    let listed = client.request("tools/list", Value::Null);
    let names = listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<BTreeSet<_>>();
    assert!(
        names.contains("orbit_task_add"),
        "listener serves the same surface as stdio: {names:?}"
    );

    let created = client.call_tool_ok(
        "orbit_task_add",
        json!({
            "title": "Listener round trip",
            "description": "Created over the MCP TCP listener",
            "complexity": "low",
            "model": "codex",
        }),
    );
    assert_eq!(created["title"], "Listener round trip");
    let task_id = created["id"].as_str().expect("task id").to_string();
    assert_eq!(
        client.call_tool_ok("orbit_task_show", json!({ "id": task_id }))["title"],
        "Listener round trip"
    );

    // Dropping the client kills the server, which closes the listening socket.
    drop(client);
    assert!(
        TcpStream::connect(addr).is_err(),
        "the listening socket must be gone once the server exits"
    );

    let connection =
        Connection::open(workspace.home.join(".orbit/orbit.db")).expect("open server audit store");
    let audit = connection
        .query_row(
            "SELECT COUNT(*), status, transport, caller_ip, trace_id
             FROM audit_events WHERE tool_name = 'orbit.task.add'",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .expect("listener task audit");
    assert_eq!(audit.0, 1, "one audit row per accepted call");
    assert_eq!(audit.1, "success");
    assert_eq!(
        audit.2.as_deref(),
        Some("local"),
        "a listener session is served by the same local process as stdio"
    );
    assert_eq!(
        audit.3.as_deref(),
        Some("127.0.0.1"),
        "the accepted peer's IP is persisted through the audit context"
    );
    assert!(
        audit
            .4
            .as_deref()
            .is_some_and(|id| id.starts_with("trace-"))
    );
}

#[test]
fn mcp_serve_round_trips_records_against_a_temp_workspace() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve();

    // Task create → show → list. No explicit `workspace` argument anywhere:
    // the ambient session workspace announced via initialize `_meta` must be
    // applied.
    let created = client.call_tool_ok(
        "orbit_task_add",
        json!({
            "title": "MCP round-trip task",
            "description": "Created over the MCP stdio transport",
            "complexity": "medium",
            "type": "chore",
            "tags": ["mcp-roundtrip"],
            "crew": "sol",
            "orchestrator": "terra",
            "relations": [{"type": "related_to", "target": "DK-00042"}],
        }),
    );
    let task_id = created["id"].as_str().expect("task id").to_string();
    assert_eq!(created["title"], "MCP round-trip task");
    assert_eq!(created["status"], "proposed");
    client.call_tool_ok(
        "orbit_task_update",
        json!({"id": task_id, "job_run_id": "jrun-mcp-projection"}),
    );

    let shown = client.call_tool_ok("orbit_task_show", json!({ "id": task_id }));
    assert_eq!(shown["id"], json!(task_id));
    assert_eq!(shown["title"], "MCP round-trip task");
    assert_eq!(shown["description"], "Created over the MCP stdio transport");
    assert_eq!(shown["tags"], json!(["mcp-roundtrip"]));
    assert_eq!(shown["crew"], "sol");
    assert_eq!(shown["orchestrator"], "terra");
    assert_eq!(shown["job_run_id"], "jrun-mcp-projection");
    assert_eq!(
        client.call_tool_ok(
            "orbit_task_show",
            json!({ "id": task_id, "fields": ["crew", "orchestrator"] }),
        ),
        json!({"crew": "sol", "orchestrator": "terra"})
    );
    assert_eq!(
        client.call_tool_ok(
            "orbit_task_show",
            json!({ "id": task_id, "fields": ["status"] })
        ),
        json!({ "value": "proposed" })
    );
    assert_eq!(
        client.call_tool_ok(
            "orbit_task_show",
            json!({
                "id": task_id,
                "fields": ["status", "relations", "external_refs", "job_run_id"],
            }),
        ),
        json!({
            "status": "proposed",
            "relations": [{
                "type": "related_to",
                "target": "DK-00042",
                "verification": "not verifiable here",
            }],
            "external_refs": [],
            "job_run_id": "jrun-mcp-projection",
        })
    );
    assert_eq!(
        client.call_tool_ok(
            "orbit_task_show",
            json!({ "id": task_id, "field": "relations" }),
        ),
        json!({
            "items": [{
                "type": "related_to",
                "target": "DK-00042",
                "verification": "not verifiable here",
            }],
        })
    );

    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["task", "show", &task_id])
        .output()
        .expect("show task through the human CLI");
    assert!(
        output.status.success(),
        "human task show failed: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Execution Crew: sol"), "{stdout}");
    assert!(stdout.contains("Orchestrator: terra"), "{stdout}");

    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args([
            "task",
            "show",
            &task_id,
            "--json",
            "--fields",
            "orchestrator",
        ])
        .output()
        .expect("show orchestrator field through the CLI");
    assert!(
        output.status.success(),
        "field projection failed: {output:?}"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).expect("orchestrator JSON"),
        json!("terra")
    );

    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["task", "show", &task_id, "--json", "--fields", "status"])
        .output()
        .expect("show status field through the CLI");
    assert!(
        output.status.success(),
        "status field projection failed: {output:?}"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).expect("status JSON"),
        json!("proposed")
    );

    let tool_run = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args([
            "tool",
            "run",
            "orbit.task.show",
            "--input",
            &format!(r#"{{"id":"{task_id}","fields":["status"]}}"#),
        ])
        .output()
        .expect("show status through orbit tool run");
    assert!(
        tool_run.status.success(),
        "tool-run status projection failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&tool_run.stdout),
        String::from_utf8_lossy(&tool_run.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&tool_run.stdout).expect("tool-run status JSON"),
        json!("proposed")
    );

    let expected_projection = json!({
        "status": "proposed",
        "relations": [{
            "type": "related_to",
            "target": "DK-00042",
            "verification": "not verifiable here",
        }],
        "external_refs": [],
        "job_run_id": "jrun-mcp-projection",
    });
    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args([
            "task",
            "show",
            &task_id,
            "--json",
            "--fields",
            "status,relations,external_refs,job_run_id",
        ])
        .output()
        .expect("show mixed public DTO fields through the CLI");
    assert_command_succeeded("mixed CLI task-show projection", &output);
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).expect("mixed CLI projection JSON"),
        expected_projection
    );

    let tool_run = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args([
            "tool",
            "run",
            "orbit.task.show",
            "--input",
            &format!(
                r#"{{"id":"{task_id}","fields":["status","relations","external_refs","job_run_id"]}}"#
            ),
        ])
        .output()
        .expect("show mixed public DTO fields through orbit tool run");
    assert_command_succeeded("mixed tool-run task-show projection", &tool_run);
    assert_eq!(
        serde_json::from_slice::<Value>(&tool_run.stdout).expect("mixed tool-run projection JSON"),
        expected_projection
    );

    let listed = client.call_tool_ok("orbit_task_list", json!({}));
    let items = listed["items"].as_array().expect("task list items");
    assert!(
        items.iter().any(|task| task["id"] == json!(task_id)),
        "created task missing from list: {items:?}"
    );
    let listed_by_stable_id = client.call_tool_ok(
        "orbit_task_list",
        json!({ "workspace": "ws_mcp-roundtrip" }),
    );
    assert!(listed_by_stable_id["items"].as_array().is_some());

    // V1 exposes one complete surface. Domain validation and mutation still run
    // on the authoritative server-side runtime.
    let friction = client.call_tool_ok(
        "orbit_friction_add",
        json!({ "body": "MCP D2 exposure regression", "model": "codex" }),
    );
    let friction_id = friction["id"].as_str().expect("friction id").to_string();
    let updated = client.call_tool_ok(
        "orbit_friction_update",
        json!({ "id": friction_id, "status": "triaged", "model": "codex" }),
    );
    assert_eq!(updated["status"], "triaged");
}

#[test]
fn mcp_serve_error_paths_return_tool_errors_and_keep_serving() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve();

    // Bad params: missing required field is a structured `invalid_input`
    // tool error, not a JSON-RPC failure or a crash.
    let bad_params = client.call_tool_err("orbit_task_add", json!({}));
    assert_eq!(bad_params["code"], "invalid_input");
    assert!(
        bad_params["message"]
            .as_str()
            .is_some_and(|message| message.contains("title")),
        "error should name the missing field: {bad_params}"
    );

    // Inactive tools remain absent from the advertised MCP registry.
    let unexposed = client.call_tool_err("orbit_task_delete", json!({ "id": "ORB-00000" }));
    assert_eq!(unexposed["code"], "tool_not_found");

    // Entirely unknown tool name.
    let unknown = client.call_tool_err("orbit_definitely_not_a_tool", json!({}));
    assert_eq!(unknown["code"], "tool_not_found");

    // Unknown JSON-RPC method: proper protocol-level error.
    let response = client.request("orbit/no_such_method", Value::Null);
    assert_eq!(
        response["error"]["code"], -32601,
        "unknown methods must yield JSON-RPC method-not-found: {response}"
    );

    // The server must still answer after every error path above.
    let listed = client.call_tool_ok("orbit_task_list", json!({}));
    assert!(listed["items"].is_array(), "server wedged: {listed}");
}

#[test]
fn mcp_hybrid_search_without_companion_returns_lexical_results() {
    let workspace = McpWorkspace::init();
    let companion_state = workspace.home.join(".orbit").join("embed");
    assert!(
        !companion_state.exists(),
        "test must start without companion state"
    );
    let mut client = workspace.serve();

    let task = client.call_tool_ok(
        "orbit_task_add",
        json!({
            "title": "MCP lexical fallback regression",
            "description": "The optional companion is absent.",
            "tags": ["fallback"],
            "complexity": "low",
            "model": "codex"
        }),
    );
    let task_id = task["id"].as_str().expect("task id");
    let response = client.call_tool_ok(
        "orbit_search",
        json!({
            "query": "MCP lexical fallback regression",
            "hybrid": true,
            "kind": "task",
            "tag": ["fallback"],
            "limit": 1,
            "model": "codex"
        }),
    );

    assert_eq!(response["mode"], "lexical");
    assert_eq!(response["results"][0]["id"], task_id);
    assert_eq!(response["results"][0]["source"], "lexical");
    let notes = response["notes"].as_array().expect("fallback notes");
    assert!(notes.iter().any(|note| {
        note.as_str()
            .is_some_and(|note| note.contains("falling back to lexical task search"))
    }));
    assert!(notes.iter().all(|note| {
        note.as_str()
            .is_some_and(|note| !note.contains("orbit semantic install"))
    }));
    assert!(
        !companion_state.exists(),
        "MCP fallback must not install companion state"
    );
}

#[test]
fn mcp_calls_are_audited_once_including_unknown_raw_names() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve();

    client.call_tool_ok(
        "orbit_search",
        json!({ "query": "mcp-audit-marker", "model": "codex" }),
    );
    let error = client.call_tool_err(
        "orbit_task_add",
        json!({ "description": "missing title", "model": "codex" }),
    );
    assert_eq!(error["code"], "invalid_input");
    let removed = client.call_tool_err("orbit_removed_tool", json!({ "model": "codex" }));
    assert_eq!(removed["code"], "tool_not_found");
    let workflow_failure = client.call_tool_err(
        "orbit_workflow_ship",
        json!({ "task_ids": ["ORB-00001"], "model": "codex" }),
    );
    assert_eq!(workflow_failure["code"], "capability_denied");
    drop(client);

    for (tool_name, status) in [
        ("orbit.search", "success"),
        ("orbit.task.add", "failure"),
        ("orbit.workflow.ship", "denied"),
    ] {
        let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
            .args(["audit", "list", "--tool", tool_name, "--json"])
            .output()
            .expect("query MCP audit rows");
        assert!(
            output.status.success(),
            "audit list failed for {tool_name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rows: Value = serde_json::from_slice(&output.stdout).expect("parse audit rows");
        let rows = rows.as_array().expect("audit row array");
        assert_eq!(rows.len(), 1, "exactly one audit row for {tool_name}");
        let row = &rows[0];
        assert_eq!(row["tool_name"], tool_name);
        assert_eq!(row["subcommand"], "run-mcp");
        assert_eq!(row["status"], status);
        assert_eq!(row["role"], "unverified");
        assert_eq!(row["transport"], "local");
        assert_eq!(row["effective_capabilities"], json!(["agent"]));
        assert!(row["workspace_id"].as_str().is_some());
        assert!(row["caller_machine_id"].as_str().is_some());
        assert_eq!(row["caller_machine_id"], row["process_machine_id"]);
        assert!(row["trace_id"].as_str().is_some());
        assert!(row["origin_session_id"].as_str().is_some());
        assert!(row["mcp_call_id"].is_null());
        assert!(row["duration_ms"].as_i64().is_some_and(|value| value >= 1));
    }

    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["audit", "list", "--tool", "orbit_removed_tool", "--json"])
        .output()
        .expect("query unknown-tool audit rows");
    assert!(output.status.success());
    let rows: Value =
        serde_json::from_slice(&output.stdout).expect("parse unknown-tool audit rows");
    let rows = rows.as_array().expect("unknown-tool audit row array");
    assert_eq!(rows.len(), 1, "unknown call records exactly one row");
    let row = &rows[0];
    assert_eq!(row["tool_name"], "orbit_removed_tool");
    assert_eq!(row["subcommand"], "run-mcp");
    assert_eq!(row["status"], "denied");
    assert_eq!(row["role"], "unverified");
    assert_eq!(row["transport"], "local");
    assert_eq!(row["effective_capabilities"], json!(["agent"]));
    assert!(row["workspace_id"].is_null());
    assert!(row["caller_machine_id"].as_str().is_some());
    assert_eq!(row["caller_machine_id"], row["process_machine_id"]);
    assert!(row["process_host_id"].as_str().is_some());
    assert!(row["trace_id"].as_str().is_some());
    assert!(row["origin_session_id"].as_str().is_some());
}

/// ORB-10448 / F2026-07-099: the managed-executor shape.
///
/// An activity runs from a linked Git worktree and reaches Orbit through a
/// general-purpose MCP client, which cannot announce `_meta.orbit.workspace`
/// at initialize. Two things had to hold for that to work and neither did:
/// the workspace selector must be advertised so a schema-following caller can
/// supply it, and the selector must route coordination reads to the partition
/// the checkout-local surfaces use, even for a workspace whose logical
/// registry ID and checkout identity diverged before `orbit workspace init`
/// converged them (L-0098).
#[test]
fn worktree_backed_activity_routes_task_and_search_by_advertised_workspace_argument() {
    let workspace = McpWorkspace::init();

    // Diverge the logical registry ID from the checkout identity that keys the
    // coordination task registry. This is the production shape the friction was
    // filed from; a freshly initialized workspace writes both from one value.
    let registry_path = workspace.home.join(".orbit").join("workspaces.json");
    let registry = std::fs::read_to_string(&registry_path).expect("read workspace registry");
    std::fs::write(
        &registry_path,
        registry.replace("ws_mcp-roundtrip", "ws_legacy-logical"),
    )
    .expect("write diverged workspace registry");
    let identity =
        std::fs::read_to_string(workspace.work.join(".orbit").join("config.yaml")).expect("read");
    assert!(
        identity.contains("ws_mcp-roundtrip"),
        "checkout identity must keep the pre-divergence ID: {identity}"
    );

    // A task authored through the checkout-local CLI surface. Before the fix,
    // MCP looked for it under the logical ID and found an empty partition.
    let add_input = json!({
        "title": "Worktree routing regression",
        "description": "Authored via the CLI fallback",
        "workspace": workspace.work.to_str().expect("utf8 checkout path"),
        "complexity": "low",
        "model": "codex",
    })
    .to_string();
    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["tool", "run", "orbit.task.add", "--input", &add_input])
        .output()
        .expect("author task through the CLI fallback");
    assert!(
        output.status.success(),
        "CLI task add failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let created: Value = serde_json::from_slice(&output.stdout).expect("parse created task");
    let task_id = created["id"].as_str().expect("task id").to_string();

    let worktree = add_linked_worktree(&workspace.work);

    // The executor's client: no `_meta.orbit.workspace`, cwd inside the linked
    // worktree.
    let child = McpWorkspace::orbit_command(&worktree, &workspace.home)
        .args(["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn worktree-backed MCP server");
    let mut client = McpClient::new(child);
    client.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "managed-executor", "version": "0" }
        }),
    );
    client.notify("notifications/initialized");

    // Every workspace-scoped tool must advertise the selector it requires.
    let listed = client.request("tools/list", Value::Null);
    let unadvertised = listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter(|tool| tool["name"] != "orbit_workspace_list")
        .filter(|tool| tool["inputSchema"]["properties"]["workspace"].is_null())
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(
        unadvertised.is_empty(),
        "workspace-scoped tools require a selector they do not advertise: {unadvertised:?}"
    );

    let selector = worktree.to_str().expect("utf8 worktree path");
    for workspace_selector in [selector, "ws_legacy-logical"] {
        let shown = client.call_tool_ok(
            "orbit_task_show",
            json!({ "id": task_id, "workspace": workspace_selector }),
        );
        assert_eq!(
            shown["id"],
            json!(task_id),
            "task show must resolve through selector `{workspace_selector}`"
        );
        assert_eq!(shown["title"], "Worktree routing regression");
    }

    let updated = client.call_tool_ok(
        "orbit_task_update",
        json!({
            "id": task_id,
            "execution_summary": "Routed from a linked worktree",
            "workspace": selector,
            "model": "codex",
        }),
    );
    assert_eq!(
        updated["execution_summary"],
        "Routed from a linked worktree"
    );

    let found = client.call_tool_ok(
        "orbit_search",
        json!({ "query": "Worktree routing regression", "workspace": selector }),
    );
    assert!(
        found["results"]
            .as_array()
            .expect("search results")
            .iter()
            .any(|hit| hit["id"] == json!(task_id)),
        "search must reach the same workspace from the worktree: {found}"
    );
    drop(client);

    // The `orbit tool run` fallback stays functional from the worktree, and it
    // observes the write MCP just made — both surfaces address one partition.
    let output = McpWorkspace::orbit_command(&worktree, &workspace.home)
        .args([
            "tool",
            "run",
            "orbit.task.show",
            "--input",
            &format!(r#"{{"id":"{task_id}"}}"#),
            "--fields",
            "id,execution_summary",
        ])
        .output()
        .expect("run the CLI fallback from the worktree");
    assert!(
        output.status.success(),
        "CLI fallback failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let shown: Value = serde_json::from_slice(&output.stdout).expect("parse CLI task show");
    assert_eq!(shown["id"], json!(task_id));
    assert_eq!(shown["execution_summary"], "Routed from a linked worktree");
}

/// ORB-10967: the managed-executor shape, end to end through the real
/// generated integration.
///
/// ORB-10448 made the selector *advertised* so a schema-following caller could
/// pass it. That is not enough: a general-purpose MCP client cannot announce
/// `_meta.orbit.workspace` at initialize at all, so a session that carried no
/// binding refused every workspace-scoped call with "requires a workspace
/// selector". The generated integration knows its workspace, so it binds the
/// server to it at launch and a silent client still routes.
#[test]
fn managed_mcp_config_updates_a_task_without_a_workspace_argument() {
    let workspace = McpWorkspace::init();
    let task_id = author_task(&workspace, "Managed executor routing");
    let args = generate_managed_mcp_config(&workspace);

    let mut client = spawn_generated_server(&workspace, &workspace.work, &args);

    // The advertised schema must tell this session the truth: it is bound, so
    // the selector is optional here.
    let listed = client.request("tools/list", Value::Null);
    let description = tool_workspace_description(&listed, "orbit_task_update");
    assert!(
        description.contains("Optional in this session"),
        "a launch-bound session must advertise the selector as optional: {description}"
    );

    let updated = client.call_tool_ok(
        "orbit_task_update",
        json!({
            "id": task_id,
            "execution_summary": "Routed by the session's launch binding",
            "model": "codex",
        }),
    );
    assert_eq!(
        updated["execution_summary"], "Routed by the session's launch binding",
        "a managed session must update a task without naming a workspace"
    );

    // An explicit selector still overrides, and still validates: naming a
    // workspace that does not exist fails closed rather than falling back to
    // the binding.
    let explicit = client.call_tool_ok(
        "orbit_task_update",
        json!({
            "id": task_id,
            "execution_summary": "Routed by an explicit selector",
            "workspace": workspace.work.to_str().expect("utf8 checkout path"),
            "model": "codex",
        }),
    );
    assert_eq!(
        explicit["execution_summary"],
        "Routed by an explicit selector"
    );
    let rejected = client.call_tool_err(
        "orbit_task_update",
        json!({ "id": task_id, "workspace": "ws_not_registered", "model": "codex" }),
    );
    assert!(
        rejected["message"]
            .as_str()
            .is_some_and(|message| message.contains("ws_not_registered")),
        "an explicit selector must be validated, not silently replaced: {rejected}"
    );
    drop(client);

    // The checkout-local CLI surface observes the same partition MCP wrote to.
    assert_eq!(
        cli_task_execution_summary(&workspace, &workspace.work, &task_id),
        "Routed by an explicit selector"
    );
}

/// ORB-11017: federated `tools/list` must tell callers to copy `selector` from
/// the federated list. A bare `ws_*` — including a v1 session default — is
/// `unknown_selector` before forwarding. `orbit.task.show` does not inherit
/// the v1 id-only default in this namespace.
#[test]
fn federated_mcp_serve_requires_the_host_qualified_list_selector() {
    let workspace = McpWorkspace::init();
    std::fs::write(
        workspace.home.join(".orbit").join("mcp-destinations.toml"),
        "[[destinations]]\nssh = \"orbit-linux\"\nmachine_id = \"hm_alpha\"\n",
    )
    .expect("write federated destinations");

    let child = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["mcp", "serve", "--mode", "federated"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn federated MCP server");
    let mut client = McpClient::new(child);
    client.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "federated-roundtrip", "version": "0" },
        }),
    );
    client.notify("notifications/initialized");

    let listed = client.request("tools/list", Value::Null);
    for tool_name in ["orbit_task_list", "orbit_task_show", "orbit_crew_list"] {
        let description = tool_workspace_description(&listed, tool_name);
        assert!(
            description.contains("selector") && description.contains("orbit.workspace.list"),
            "{tool_name} must instruct copying from the federated list: {description}"
        );
        assert!(
            description.to_ascii_lowercase().contains("copy"),
            "{tool_name} must say to copy the list field: {description}"
        );
        assert!(
            !description.contains("registered workspace name")
                && !description.contains("ws_*")
                && !description.contains("absolute path")
                && !description.to_ascii_lowercase().contains("cwd"),
            "{tool_name} must not present a v1 local form as valid: {description}"
        );
    }

    let omitted = client.call_tool_err("orbit_task_show", json!({ "id": "ORB-00001" }));
    assert_ne!(
        omitted["code"], "unknown_selector",
        "omitting the selector is a missing-argument refusal, not a minted token: {omitted}"
    );
    assert!(
        omitted["message"]
            .as_str()
            .is_some_and(|message| message.contains("host-qualified")
                || message.contains("requires a workspace selector")),
        "federated task.show without a selector is refused: {omitted}"
    );

    let bare_show = client.call_tool_err(
        "orbit_task_show",
        json!({ "id": "ORB-00001", "workspace": "ws_orbit" }),
    );
    assert_eq!(
        bare_show["code"], "unknown_selector",
        "a bare ws_* on federated task.show is unknown_selector: {bare_show}"
    );

    let bare_list = client.call_tool_err("orbit_crew_list", json!({ "workspace": "ws_orbit" }));
    assert_eq!(
        bare_list["code"], "unknown_selector",
        "a bare ws_* is unknown_selector before forwarding: {bare_list}"
    );
}

fn federated_client(workspace: &McpWorkspace) -> McpClient {
    let child = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["mcp", "serve", "--mode", "federated"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn federated MCP server");
    let mut client = McpClient::new(child);
    client.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "federated-roundtrip", "version": "0" },
        }),
    );
    client.notify("notifications/initialized");
    client
}

fn host_identity(home: &Path) -> (String, String) {
    let parsed: toml::Value = toml::from_str(
        &std::fs::read_to_string(home.join(".orbit").join("host.toml")).expect("read host.toml"),
    )
    .expect("parse host.toml");
    (
        parsed["machine_id"]
            .as_str()
            .expect("machine_id")
            .to_string(),
        parsed["host_id"].as_str().expect("host_id").to_string(),
    )
}

fn plant_ssh_stub(bin: &Path, log: &Path) {
    plant_agent_cli_stub(bin, "ssh");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 1\n",
        log.display()
    );
    std::fs::write(bin.join("ssh"), script).expect("write ssh stub");
}

/// ORB-11044: federated mode includes local workspaces with no destinations file.
#[test]
fn federated_mcp_serve_lists_and_routes_local_workspaces_without_destinations() {
    let workspace = McpWorkspace::init();
    let ssh_log = workspace.home.join("ssh-invocations.log");
    plant_ssh_stub(&McpWorkspace::stub_bin_dir(&workspace.home), &ssh_log);

    let mut client = federated_client(&workspace);
    let (machine_id, host_id) = host_identity(&workspace.home);
    let listed = client.call_tool_ok("orbit_workspace_list", json!({}));
    let rows = listed["workspaces"].as_array().expect("workspace rows");
    assert_eq!(
        rows.len(),
        1,
        "local-only membership lists this machine: {listed}"
    );
    let row = &rows[0];
    assert_eq!(row["machine_id"], machine_id);
    assert_eq!(row["host"], host_id);
    assert_eq!(row["reachability"], "reachable");
    assert_eq!(row["checkout_health"], "active");
    assert!(
        row["capabilities"]
            .as_array()
            .is_some_and(|caps| caps.iter().any(|cap| cap == "control_plane")),
        "owner checkout advertises control_plane: {row}"
    );
    let selector = row["selector"].as_str().expect("selector");
    assert_eq!(
        selector,
        format!("{machine_id}/{}", row["id"].as_str().expect("id"))
    );

    let crews = client.call_tool_ok("orbit_crew_list", json!({ "workspace": selector }));
    assert_eq!(
        crews["workspace_id"], row["id"],
        "local selector must dispatch through the accepting machine: {crews}"
    );
    assert!(
        !ssh_log.exists()
            || std::fs::read_to_string(&ssh_log)
                .expect("read ssh log")
                .is_empty(),
        "local federated routing must not spawn SSH"
    );
}

/// ORB-11048: in-process federation must preserve the outer MCP call's audit
/// evidence while retaining the accepting server's trusted session fields.
#[test]
fn direct_and_federated_local_calls_record_equivalent_audit_contexts() {
    let workspace = McpWorkspace::init();
    let (machine_id, host_id) = host_identity(&workspace.home);

    let mut direct = workspace.serve();
    direct.call_tool_ok("orbit_crew_list", json!({}));
    drop(direct);

    let mut federated = federated_client(&workspace);
    let listed = federated.call_tool_ok("orbit_workspace_list", json!({}));
    let selector = listed["workspaces"][0]["selector"]
        .as_str()
        .expect("local selector");
    federated.call_tool_ok("orbit_crew_list", json!({ "workspace": selector }));
    drop(federated);

    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["audit", "list", "--tool", "orbit.crew.list", "--json"])
        .output()
        .expect("query direct and federated audit rows");
    assert!(
        output.status.success(),
        "audit list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Value = serde_json::from_slice(&output.stdout).expect("parse audit rows");
    let rows = rows.as_array().expect("audit row array");
    assert_eq!(rows.len(), 2, "one direct and one federated call: {rows:?}");

    let direct_row = rows
        .iter()
        .find(|row| row["self_reported_actor"] == "orbit-mcp-roundtrip-test")
        .expect("direct audit row preserves initialize actor");
    let federated_row = rows
        .iter()
        .find(|row| row["self_reported_actor"] == "federated-roundtrip")
        .expect("federated audit row preserves initialize actor");
    let direct_trace = direct_row["trace_id"].as_str().expect("direct trace");
    let federated_trace = federated_row["trace_id"].as_str().expect("federated trace");
    assert_ne!(direct_trace, federated_trace);

    for row in [direct_row, federated_row] {
        assert_eq!(row["caller_machine_id"], machine_id);
        assert_eq!(row["caller_host_id"], host_id);
        assert_eq!(row["process_machine_id"], machine_id);
        assert_eq!(row["process_host_id"], host_id);
        assert_eq!(row["transport"], "local");
        assert_eq!(row["effective_capabilities"], json!(["agent"]));
    }
}

/// ORB-11044: an explicit destination naming the local machine is one local route.
#[test]
fn federated_mcp_serve_collapses_an_explicit_local_destination_row() {
    let workspace = McpWorkspace::init();
    let (machine_id, host_id) = host_identity(&workspace.home);
    std::fs::write(
        workspace.home.join(".orbit").join("mcp-destinations.toml"),
        format!("[[destinations]]\nssh = \"localhost\"\nmachine_id = \"{machine_id}\"\n"),
    )
    .expect("write explicit local destination");
    let ssh_log = workspace.home.join("ssh-invocations.log");
    plant_ssh_stub(&McpWorkspace::stub_bin_dir(&workspace.home), &ssh_log);

    let mut client = federated_client(&workspace);
    let listed = client.call_tool_ok("orbit_workspace_list", json!({}));
    let rows = listed["workspaces"].as_array().expect("workspace rows");
    let local_rows: Vec<_> = rows
        .iter()
        .filter(|row| row["machine_id"] == machine_id)
        .collect();
    assert_eq!(
        local_rows.len(),
        1,
        "explicit local SSH row must not duplicate selectors: {listed}"
    );
    assert_eq!(local_rows[0]["host"], host_id);
    let selector = local_rows[0]["selector"].as_str().expect("selector");
    client.call_tool_ok("orbit_crew_list", json!({ "workspace": selector }));
    assert!(
        !ssh_log.exists()
            || std::fs::read_to_string(&ssh_log)
                .expect("read ssh log")
                .is_empty(),
        "collapsed local route must not spawn SSH"
    );
}

/// ORB-11044: mixed membership lists local workspaces beside configured remotes.
#[test]
fn federated_mcp_serve_lists_local_workspaces_beside_unreachable_remotes() {
    let workspace = McpWorkspace::init();
    std::fs::write(
        workspace.home.join(".orbit").join("mcp-destinations.toml"),
        "[[destinations]]\nssh = \"orbit-missing-host\"\nmachine_id = \"hm_remote\"\n",
    )
    .expect("write remote destination");
    let ssh_log = workspace.home.join("ssh-invocations.log");
    plant_ssh_stub(&McpWorkspace::stub_bin_dir(&workspace.home), &ssh_log);

    let mut client = federated_client(&workspace);
    let (machine_id, _) = host_identity(&workspace.home);
    let listed = client.call_tool_ok("orbit_workspace_list", json!({}));
    let rows = listed["workspaces"].as_array().expect("workspace rows");
    assert!(
        rows.iter()
            .any(|row| row["machine_id"] == machine_id && row["reachability"] == "reachable"),
        "local workspace must stay listed: {listed}"
    );
    let remote = rows
        .iter()
        .find(|row| row["machine_id"] == "hm_remote")
        .expect("configured remote is listed");
    assert_eq!(remote["reachability"], "unreachable");
    assert_eq!(remote["checkout_health"], "unknown");
    assert!(
        ssh_log.exists(),
        "the unreachable remote should have attempted SSH"
    );
}

/// ORB-11044: a machine-id-only destination row still fails closed.
#[test]
fn federated_mcp_serve_rejects_a_machine_id_only_destination_row() {
    let workspace = McpWorkspace::init();
    std::fs::write(
        workspace.home.join(".orbit").join("mcp-destinations.toml"),
        "[[destinations]]\nmachine_id = \"hm_alpha\"\n",
    )
    .expect("write invalid destination");

    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["mcp", "serve", "--mode", "federated"])
        .output()
        .expect("run federated serve");
    assert!(
        !output.status.success(),
        "machine-id-only rows must fail closed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ssh") || stderr.contains("invalid"),
        "the configuration error must be actionable: {stderr}"
    );
}

/// ORB-10967 / ORB-10961: the same contract from a linked job worktree whose
/// checkout identity diverged from the logical `ws_*` the registry knows.
///
/// The binding is that logical ID, so the ambient runtime identity of the
/// worktree never becomes the route, and the managed run lands in the one
/// partition the checkout-local surfaces use.
#[test]
fn managed_mcp_config_routes_a_linked_worktree_by_its_logical_workspace_id() {
    let workspace = McpWorkspace::init_with_workspace_name("orbit-5c61b3");

    // Diverge the logical registry ID from the checkout identity that keys the
    // coordination task registry, then generate the integration, so the config
    // is written for the diverged logical ID.
    let registry_path = workspace.home.join(".orbit").join("workspaces.json");
    let registry = std::fs::read_to_string(&registry_path).expect("read workspace registry");
    std::fs::write(
        &registry_path,
        registry.replace("ws_orbit-5c61b3", "ws_legacy-logical"),
    )
    .expect("write diverged workspace registry");
    let identity = std::fs::read_to_string(workspace.work.join(".orbit").join("config.yaml"))
        .expect("read checkout identity");
    assert!(
        identity.contains("ws_orbit-5c61b3"),
        "checkout identity must keep the pre-divergence ID: {identity}"
    );

    let task_id = author_task(&workspace, "Linked worktree routing");
    // `orbit mcp init` is the agent-authority generation path; it resolves the
    // binding from the registry rather than from the checkout's own identity.
    let args = generate_agent_mcp_config(&workspace);
    assert_eq!(
        args,
        vec![
            "mcp".to_string(),
            "serve".to_string(),
            "--workspace".to_string(),
            "ws_legacy-logical".to_string(),
        ],
        "the generated config must bind the logical registry ID, not the checkout identity"
    );

    let worktree = add_linked_worktree(&workspace.work);
    let mut client = spawn_generated_server(&workspace, &worktree, &args);

    let updated = client.call_tool_ok(
        "orbit_task_update",
        json!({
            "id": task_id,
            "execution_summary": "Routed from a linked worktree",
            "model": "codex",
        }),
    );
    assert_eq!(
        updated["execution_summary"],
        "Routed from a linked worktree"
    );
    drop(client);

    // The `orbit tool run` fallback from the worktree observes the same write:
    // both surfaces address one partition.
    assert_eq!(
        cli_task_execution_summary(&workspace, &worktree, &task_id),
        "Routed from a linked worktree"
    );
}

/// ORB-11117: a managed MCP child launched without `--workspace` still uses
/// the trusted envelope selector. A general-purpose client that omits
/// `_meta.orbit.workspace` must not see workspace-required.
#[test]
fn managed_mcp_env_binding_updates_without_a_workspace_argument() {
    let workspace = McpWorkspace::init();
    let task_id = author_task(&workspace, "Managed envelope MCP routing");
    let worktree = add_linked_worktree(&workspace.work);
    write_shadow_worktree_identity(&worktree);

    let mut client = spawn_managed_env_server(&workspace, &worktree, "ws_mcp-roundtrip");

    let listed = client.request("tools/list", Value::Null);
    let description = tool_workspace_description(&listed, "orbit_task_update");
    assert!(
        description.contains("Optional in this session"),
        "a managed envelope-bound session must advertise the selector as optional: {description}"
    );

    let updated = client.call_tool_ok(
        "orbit_task_update",
        json!({
            "id": task_id,
            "execution_summary": "Routed by the managed envelope",
            "model": "codex",
        }),
    );
    assert_eq!(
        updated["execution_summary"],
        "Routed by the managed envelope"
    );

    let friction = client.call_tool_ok(
        "orbit_friction_add",
        json!({
            "body": "Managed MCP envelope routed friction add.",
            "model": "codex",
        }),
    );
    assert!(
        friction.get("id").and_then(Value::as_str).is_some(),
        "friction add must persist through the envelope binding: {friction}"
    );

    let listed_tasks = client.call_tool_ok("orbit_task_list", json!({ "limit": 10 }));
    let ids = task_ids_from_list(&listed_tasks);
    assert!(
        ids.contains(&task_id),
        "read path must use the same canonical workspace: {listed_tasks}"
    );
    assert_eq!(
        latest_tool_audit_workspace(&workspace, "orbit.task.update").as_deref(),
        Some("ws_mcp-roundtrip")
    );
    assert_eq!(
        latest_tool_audit_workspace(&workspace, "orbit.friction.add").as_deref(),
        Some("ws_mcp-roundtrip")
    );
    assert_eq!(
        latest_tool_audit_workspace(&workspace, "orbit.task.list").as_deref(),
        Some("ws_mcp-roundtrip")
    );

    let rejected = client.call_tool_err(
        "orbit_task_update",
        json!({ "id": task_id, "workspace": "ws_not_registered", "model": "codex" }),
    );
    assert!(
        rejected["message"]
            .as_str()
            .is_some_and(|message| message.contains("ws_not_registered")),
        "an explicit selector must fail closed without cwd fallback: {rejected}"
    );
    drop(client);

    assert_eq!(
        cli_task_execution_summary(&workspace, &workspace.work, &task_id),
        "Routed by the managed envelope"
    );
    assert_no_shadow_task_store(&worktree);
}

/// ORB-11117: production-shaped managed CLI from a disposable linked worktree.
#[test]
fn managed_cli_from_linked_worktree_updates_canonical_workspace() {
    let workspace = McpWorkspace::init();
    let task_id = author_task(&workspace, "Managed envelope CLI routing");
    let worktree = add_linked_worktree(&workspace.work);
    write_shadow_worktree_identity(&worktree);

    let update_input = json!({
        "id": task_id,
        "execution_summary": "CLI routed by the managed envelope",
        "model": "codex",
    })
    .to_string();
    let update = managed_cli(&workspace, &worktree)
        .args(["tool", "run", "orbit.task.update", "--input", &update_input])
        .output()
        .expect("managed CLI task update");
    assert_command_succeeded("managed CLI orbit.task.update", &update);

    let friction_input = json!({
        "body": "Managed CLI envelope routed friction add.",
        "model": "codex",
    })
    .to_string();
    let friction = managed_cli(&workspace, &worktree)
        .args([
            "tool",
            "run",
            "orbit.friction.add",
            "--input",
            &friction_input,
        ])
        .output()
        .expect("managed CLI friction add");
    assert_command_succeeded("managed CLI orbit.friction.add", &friction);

    let list_input = json!({ "limit": 10, "model": "codex" }).to_string();
    let listed = managed_cli(&workspace, &worktree)
        .args(["tool", "run", "orbit.task.list", "--input", &list_input])
        .output()
        .expect("managed CLI task list");
    assert_command_succeeded("managed CLI orbit.task.list", &listed);
    let listed_json: Value = serde_json::from_slice(&listed.stdout).expect("parse list");
    assert!(
        task_ids_from_list(&listed_json).contains(&task_id),
        "managed CLI list must read the canonical workspace: {listed_json}"
    );

    assert_eq!(
        cli_task_execution_summary(&workspace, &workspace.work, &task_id),
        "CLI routed by the managed envelope"
    );
    assert_no_shadow_task_store(&worktree);
    assert_eq!(
        latest_tool_audit_workspace(&workspace, "orbit.task.update").as_deref(),
        Some("ws_mcp-roundtrip")
    );
    assert_eq!(
        latest_tool_audit_workspace(&workspace, "orbit.friction.add").as_deref(),
        Some("ws_mcp-roundtrip")
    );
    assert_eq!(
        latest_tool_audit_workspace(&workspace, "orbit.task.list").as_deref(),
        Some("ws_mcp-roundtrip")
    );

    let unknown = managed_cli(&workspace, &worktree)
        .args([
            "--workspace",
            "ws_not_registered",
            "tool",
            "run",
            "orbit.task.list",
            "--input",
            &list_input,
        ])
        .output()
        .expect("invalid explicit selector");
    assert!(
        !unknown.status.success(),
        "an invalid explicit selector must fail closed"
    );
    let stderr = String::from_utf8_lossy(&unknown.stderr);
    assert!(
        stderr.contains("ws_not_registered"),
        "the refusal must name the rejected selector: {stderr}"
    );
}

/// ORB-11117: an inherited `ORBIT_WORKSPACE` without managed provenance is not
/// a launch binding.
#[test]
fn unmanaged_orbit_workspace_env_does_not_bind_mcp() {
    let workspace = McpWorkspace::init();
    let child = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .env("ORBIT_WORKSPACE", "ws_mcp-roundtrip")
        .args(["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn unbound MCP server");
    let mut client = McpClient::new(child);
    client.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "unmanaged-env", "version": "0" },
        }),
    );
    client.notify("notifications/initialized");
    let unscoped = client.call_tool_err("orbit_task_list", json!({}));
    assert!(
        unscoped["message"]
            .as_str()
            .is_some_and(|message| { message.contains("requires an explicit workspace selector") })
    );
}

/// Author one task through the checkout-local CLI surface and return its ID.
fn author_task(workspace: &McpWorkspace, title: &str) -> String {
    let input = json!({
        "title": title,
        "description": "Authored via the CLI fallback",
        "complexity": "low",
        "workspace": workspace.work.to_str().expect("utf8 checkout path"),
        "model": "codex",
    })
    .to_string();
    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["tool", "run", "orbit.task.add", "--input", &input])
        .output()
        .expect("author task through the CLI fallback");
    assert_command_succeeded("orbit.task.add", &output);
    let created: Value = serde_json::from_slice(&output.stdout).expect("parse created task");
    created["id"].as_str().expect("task id").to_string()
}

/// Run the operator-facing bootstrap (`orbit workspace init --mcp`) and return
/// the exact argv the integration it generated launches Orbit with.
fn generate_managed_mcp_config(workspace: &McpWorkspace) -> Vec<String> {
    std::fs::create_dir_all(workspace.work.join(".claude")).expect("create .claude marker");
    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args([
            "workspace",
            "init",
            "--name",
            "mcp-roundtrip",
            "--force",
            "--mcp",
        ])
        .output()
        .expect("run workspace init --force --mcp");
    assert_command_succeeded("workspace init --force --mcp", &output);
    read_generated_claude_args(workspace)
}

/// Run the agent-authority registration (`orbit mcp init --claude`) and return
/// the argv the integration it generated launches Orbit with.
fn generate_agent_mcp_config(workspace: &McpWorkspace) -> Vec<String> {
    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["mcp", "init", "--claude"])
        .output()
        .expect("run orbit mcp init --claude");
    assert_command_succeeded("mcp init --claude", &output);
    read_generated_claude_args(workspace)
}

fn read_generated_claude_args(workspace: &McpWorkspace) -> Vec<String> {
    let claude_mcp: Value = serde_json::from_str(
        &std::fs::read_to_string(workspace.work.join(".claude.json"))
            .expect("read generated claude mcp config"),
    )
    .expect("parse generated claude mcp config");
    claude_mcp["mcpServers"]["orbit"]["args"]
        .as_array()
        .expect("generated args array")
        .iter()
        .map(|value| value.as_str().expect("arg is a string").to_string())
        .collect()
}

/// Spawn the generated argv from `cwd` and complete the handshake a real
/// managed client performs: `clientInfo` only, no `_meta.orbit.workspace`.
fn spawn_generated_server(workspace: &McpWorkspace, cwd: &Path, args: &[String]) -> McpClient {
    let child = McpWorkspace::orbit_command(cwd, &workspace.home)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the server launched by the generated config");
    let mut client = McpClient::new(child);
    client.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "managed-executor", "version": "0" },
        }),
    );
    client.notify("notifications/initialized");
    client
}

/// Read one task's execution summary back through the CLI surface at `cwd`.
fn cli_task_execution_summary(workspace: &McpWorkspace, cwd: &Path, task_id: &str) -> String {
    let output = McpWorkspace::orbit_command(cwd, &workspace.home)
        .args([
            "tool",
            "run",
            "orbit.task.show",
            "--input",
            &format!(r#"{{"id":"{task_id}"}}"#),
            "--fields",
            "id,execution_summary",
        ])
        .output()
        .expect("run the CLI fallback");
    assert_command_succeeded("orbit.task.show", &output);
    let shown: Value = serde_json::from_slice(&output.stdout).expect("parse CLI task show");
    assert_eq!(shown["id"], json!(task_id));
    shown["execution_summary"]
        .as_str()
        .expect("execution summary")
        .to_string()
}

fn spawn_managed_env_server(workspace: &McpWorkspace, cwd: &Path, selector: &str) -> McpClient {
    let child = McpWorkspace::orbit_command(cwd, &workspace.home)
        .env("ORBIT_MANAGED_RUN_CONTEXT", "1")
        .env("ORBIT_RUN_ID", "jrun-managed-mcp")
        .env("ORBIT_WORKSPACE", selector)
        .args(["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn managed-envelope MCP server");
    let mut client = McpClient::new(child);
    client.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "managed-executor", "version": "0" },
        }),
    );
    client.notify("notifications/initialized");
    client
}

fn managed_cli(workspace: &McpWorkspace, cwd: &Path) -> Command {
    let mut command = McpWorkspace::orbit_command(cwd, &workspace.home);
    command
        .env("ORBIT_MANAGED_RUN_CONTEXT", "1")
        .env("ORBIT_RUN_ID", "jrun-managed-cli")
        .env("ORBIT_WORKSPACE", "ws_mcp-roundtrip");
    command
}

fn write_shadow_worktree_identity(worktree: &Path) {
    let orbit_dir = worktree.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).expect("worktree local orbit");
    std::fs::write(
        orbit_dir.join("config.yaml"),
        "schema_version: 1\nworkspace_id: daniel-e9c542\n",
    )
    .expect("shadow worktree identity");
}

fn assert_no_shadow_task_store(worktree: &Path) {
    assert!(
        !worktree.join(".orbit/tasks").exists(),
        "managed routing must not create a worktree-local shadow task store"
    );
}

fn latest_tool_audit_workspace(workspace: &McpWorkspace, tool_name: &str) -> Option<String> {
    let connection =
        Connection::open(workspace.home.join(".orbit/orbit.db")).expect("open audit store");
    connection
        .query_row(
            "SELECT workspace_id FROM audit_events
             WHERE tool_name = ?1 AND workspace_id IS NOT NULL
             ORDER BY rowid DESC LIMIT 1",
            [tool_name],
            |row| row.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
}

fn task_ids_from_list(value: &Value) -> Vec<String> {
    let items = value
        .as_array()
        .cloned()
        .or_else(|| value.get("items").and_then(Value::as_array).cloned())
        .or_else(|| value.get("tasks").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    items
        .iter()
        .filter_map(|task| {
            task.get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect()
}

/// The advertised `workspace` selector documentation for one listed tool.
fn tool_workspace_description(listed: &Value, tool_name: &str) -> String {
    listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .find(|tool| tool["name"] == json!(tool_name))
        .expect("tool listed")["inputSchema"]["properties"]["workspace"]["description"]
        .as_str()
        .expect("selector carries routing guidance")
        .to_string()
}

/// ORB-10797: the constellation-orchestrator shape.
///
/// An agent sits in one workspace's MCP session and holds a task ID from
/// another. The session's announced workspace is ambient, like cwd, so a
/// `{id}`-only `orbit.task.show` follows the ID to its owner; an explicit
/// per-call `workspace` stays a filter and 404s.
#[test]
fn mcp_task_show_follows_the_global_id_and_explicit_workspace_stays_a_filter() {
    let workspace = McpWorkspace::init();

    let elsewhere = workspace.home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("create the second checkout");
    let output = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&elsewhere)
        .output()
        .expect("initialize the second Git checkout");
    assert!(output.status.success(), "git init failed: {output:?}");
    let output = McpWorkspace::orbit_command(&elsewhere, &workspace.home)
        .args(["workspace", "init", "--name", "mcp-elsewhere"])
        .output()
        .expect("register the second workspace");
    assert!(
        output.status.success(),
        "second workspace init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let add_input = json!({
        "title": "Owned by the other workspace",
        "description": "Addressed by ID from a session bound elsewhere",
        "workspace": elsewhere.to_str().expect("utf8 checkout path"),
        "complexity": "low",
        "model": "codex",
    })
    .to_string();
    let output = McpWorkspace::orbit_command(&elsewhere, &workspace.home)
        .args(["tool", "run", "orbit.task.add", "--input", &add_input])
        .output()
        .expect("author a task in the second workspace");
    assert!(
        output.status.success(),
        "task add failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let created: Value = serde_json::from_slice(&output.stdout).expect("parse created task");
    let task_id = created["id"].as_str().expect("task id").to_string();

    // The session announces the *first* workspace at initialize.
    let mut client = workspace.serve();

    let shown = client.call_tool_ok("orbit_task_show", json!({ "id": task_id }));
    assert_eq!(
        shown["id"],
        json!(task_id),
        "an id-only show must follow the id past the session workspace: {shown}"
    );
    assert_eq!(shown["title"], "Owned by the other workspace");

    let session_workspace = workspace.work.to_str().expect("utf8 session workspace");
    let missed = client.call_tool_err(
        "orbit_task_show",
        json!({ "id": task_id, "workspace": session_workspace }),
    );
    assert!(
        missed["message"]
            .as_str()
            .is_some_and(|message| message.contains(&task_id)),
        "an explicit workspace must filter rather than follow the id: {missed}"
    );
}

/// ORB-10961: the ORB-10952 managed-worktree shape.
///
/// A linked worktree whose checkout identity (`ws_orbit-5c61b3`) diverged from
/// the logical registry ID must not turn that runtime identity into an
/// `orbit.task.show` filter. Id-only tool-run and MCP calls follow the task
/// ID; other workspace-scoped tools still require a registered selector.
#[test]
fn task_show_is_global_by_default_across_tool_run_and_mcp() {
    let workspace = McpWorkspace::init_with_workspace_name("orbit-5c61b3");

    let registry_path = workspace.home.join(".orbit").join("workspaces.json");
    let registry = std::fs::read_to_string(&registry_path).expect("read workspace registry");
    std::fs::write(
        &registry_path,
        registry.replace("ws_orbit-5c61b3", "ws_legacy-logical"),
    )
    .expect("diverge the logical registry id");

    let elsewhere = workspace.home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("create the second checkout");
    let output = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&elsewhere)
        .output()
        .expect("initialize the second Git checkout");
    assert!(output.status.success(), "git init failed: {output:?}");
    let output = McpWorkspace::orbit_command(&elsewhere, &workspace.home)
        .args(["workspace", "init", "--name", "mcp-elsewhere"])
        .output()
        .expect("register the second workspace");
    assert!(
        output.status.success(),
        "second workspace init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let add_input = json!({
        "title": "Owned despite runtime identity ws_orbit-5c61b3",
        "description": "Addressed by ID from a linked worktree and a foreign cwd",
        "workspace": workspace.work.to_str().expect("utf8 checkout path"),
        "complexity": "low",
        "model": "codex",
    })
    .to_string();
    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["tool", "run", "orbit.task.add", "--input", &add_input])
        .output()
        .expect("author a task in the diverged workspace");
    assert!(
        output.status.success(),
        "task add failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let created: Value = serde_json::from_slice(&output.stdout).expect("parse created task");
    let task_id = created["id"].as_str().expect("task id").to_string();
    let show_input = json!({ "id": task_id, "model": "codex" }).to_string();

    let worktree = add_linked_worktree(&workspace.work);
    let scratch = workspace.home.join("scratch");
    std::fs::create_dir_all(&scratch).expect("create a non-workspace directory");

    for cwd in [&worktree, &elsewhere, &scratch] {
        let output = McpWorkspace::orbit_command(cwd, &workspace.home)
            .args(["tool", "run", "orbit.task.show", "--input", &show_input])
            .output()
            .expect("run id-only task show");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "id-only tool run from {} must follow the task id\nstdout:\n{}\nstderr:\n{stderr}",
            cwd.display(),
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            !stderr.contains("ws_orbit-5c61b3"),
            "tool run must not promote the runtime identity into a selector from {}: {stderr}",
            cwd.display()
        );
        let shown: Value = serde_json::from_slice(&output.stdout).expect("parse tool run show");
        assert_eq!(shown["id"], json!(task_id));
        assert_eq!(shown["workspace"]["id"], "ws_legacy-logical");
        assert_eq!(shown["workspace"]["name"], "orbit-5c61b3");
    }

    let invalid = McpWorkspace::orbit_command(&scratch, &workspace.home)
        .args([
            "tool",
            "run",
            "orbit.task.show",
            "--input",
            &json!({
                "id": task_id,
                "workspace": "no-such-workspace",
                "model": "codex"
            })
            .to_string(),
        ])
        .output()
        .expect("run invalid explicit workspace filter");
    assert!(!invalid.status.success());
    let invalid_stderr = String::from_utf8_lossy(&invalid.stderr);
    assert!(
        invalid_stderr.contains("no-such-workspace"),
        "an invalid explicit selector must be named: {invalid_stderr}"
    );

    let runtime_identity = McpWorkspace::orbit_command(&scratch, &workspace.home)
        .args([
            "tool",
            "run",
            "orbit.task.show",
            "--input",
            &json!({
                "id": task_id,
                "workspace": "ws_orbit-5c61b3",
                "model": "codex"
            })
            .to_string(),
        ])
        .output()
        .expect("run runtime-identity explicit filter");
    assert!(!runtime_identity.status.success());
    let runtime_stderr = String::from_utf8_lossy(&runtime_identity.stderr);
    assert!(
        runtime_stderr.contains("ws_orbit-5c61b3"),
        "an explicit runtime identity remains fail-closed: {runtime_stderr}"
    );

    let mut client = serve_mcp_from(
        &worktree,
        &workspace.home,
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "managed-executor", "version": "0" },
            "_meta": { "orbit": { "workspace": "ws_orbit-5c61b3" } },
        }),
    );
    let listed = client.request("tools/list", Value::Null);
    let task_show = listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .find(|tool| tool["name"] == "orbit_task_show")
        .expect("orbit_task_show advertised");
    let description = task_show["description"].as_str().expect("description");
    assert!(
        description.contains("globally unique"),
        "advertised help must say id is globally resolved: {description}"
    );
    let workspace_help = task_show["inputSchema"]["properties"]["workspace"]["description"]
        .as_str()
        .expect("optional workspace filter advertised");
    assert!(
        workspace_help.contains("resolved globally by default"),
        "generic workspace-selection copy must not replace task.show help: {workspace_help}"
    );
    assert!(
        !workspace_help.contains("_meta.orbit.workspace"),
        "session-default copy would make clients inject ambient identity: {workspace_help}"
    );

    let shown = client.call_tool_ok("orbit_task_show", json!({ "id": task_id }));
    assert_eq!(shown["id"], json!(task_id));
    assert_eq!(shown["workspace"]["id"], "ws_legacy-logical");
    assert_eq!(shown["workspace"]["name"], "orbit-5c61b3");

    let listed_err = client.call_tool_err("orbit_task_list", json!({}));
    assert!(
        listed_err["message"]
            .as_str()
            .is_some_and(|message| message.contains("ws_orbit-5c61b3")),
        "task list must still fail-closed on the runtime identity: {listed_err}"
    );
    drop(client);

    let mut client = serve_mcp_from(
        &scratch,
        &workspace.home,
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "no-initialize-workspace", "version": "0" },
        }),
    );
    let shown = client.call_tool_ok("orbit_task_show", json!({ "id": task_id }));
    assert_eq!(shown["id"], json!(task_id));
    let unscoped = client.call_tool_err("orbit_task_list", json!({}));
    assert!(unscoped["message"].as_str().is_some_and(|message| {
        message.contains("requires an explicit workspace selector")
            && message.contains("orbit_workspace_list")
            && message.contains("orbit workspace init")
    }));
    drop(client);

    let mut client = serve_mcp_from(
        &scratch,
        &workspace.home,
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "other-session", "version": "0" },
            "_meta": {
                "orbit": { "workspace": elsewhere.to_str().expect("utf8 elsewhere") }
            },
        }),
    );
    let shown = client.call_tool_ok("orbit_task_show", json!({ "id": task_id }));
    assert_eq!(
        shown["id"],
        json!(task_id),
        "id-only show must ignore a session bound to another workspace: {shown}"
    );
    let missed = client.call_tool_err(
        "orbit_task_show",
        json!({
            "id": task_id,
            "workspace": elsewhere.to_str().expect("utf8 elsewhere")
        }),
    );
    assert!(
        missed["message"]
            .as_str()
            .is_some_and(|message| message.contains(&task_id)),
        "an explicit foreign workspace must filter: {missed}"
    );
}

/// ORB-10968: a stored crew this host has no `[crews.*]` entry for must not
/// make the task unreadable.
///
/// Crew configuration is host-local, so the same task is routinely served by a
/// machine that never defined its crew — over SSH-marked MCP most of all. The
/// read surfaces render the raw `crew` and mark the projection unresolved; only
/// paths that must actually dispatch (`orbit.task.start`) still fail closed.
#[test]
fn task_read_surfaces_tolerate_a_crew_this_host_does_not_define() {
    let workspace = McpWorkspace::init();
    let config_path = workspace.home.join(".orbit").join("config.toml");
    let baseline = std::fs::read_to_string(&config_path).expect("read the seeded config");

    // Author the task while the crew exists, the way its owning machine would.
    std::fs::write(
        &config_path,
        format!("{baseline}\n[crews.remote-only]\nprovider = \"codex\"\nmodel = \"gpt-5.6-sol\"\n"),
    )
    .expect("define the authoring host's crew");
    let add_input = json!({
        "title": "Authored against a crew only the other host defines",
        "description": "Read back from a host whose [crews.*] table lacks it",
        "workspace": workspace.work.to_str().expect("utf8 checkout path"),
        "complexity": "low",
        "crew": "remote-only",
        "model": "codex",
    })
    .to_string();
    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["tool", "run", "orbit.task.add", "--input", &add_input])
        .output()
        .expect("author a task naming the remote crew");
    assert!(
        output.status.success(),
        "task add failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let created: Value = serde_json::from_slice(&output.stdout).expect("parse created task");
    let task_id = created["id"].as_str().expect("task id").to_string();

    // Serve it from a host that never had that crew.
    std::fs::write(&config_path, &baseline).expect("drop the crew from this host");
    let show_input = json!({ "id": task_id, "model": "codex" }).to_string();

    // (1) CLI tool-run, from a directory outside any workspace, so the read
    // also goes through the global id lookup.
    let scratch = workspace.home.join("scratch");
    std::fs::create_dir_all(&scratch).expect("create a non-workspace directory");
    // `--full` because tool-run projects a minimal task shape by default, and
    // the crew fields are what this asserts.
    let output = McpWorkspace::orbit_command(&scratch, &workspace.home)
        .args([
            "tool",
            "run",
            "orbit.task.show",
            "--full",
            "--input",
            &show_input,
        ])
        .output()
        .expect("run id-only task show");
    assert!(
        output.status.success(),
        "tool-run show must stay readable\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_unresolved_crew_projection(
        &serde_json::from_slice(&output.stdout).expect("parse tool run show"),
    );

    // (2) A local MCP session bound to the workspace.
    let mut client = workspace.serve();
    assert_unresolved_crew_projection(
        &client.call_tool_ok("orbit_task_show", json!({ "id": task_id })),
    );
    let listed = client.call_tool_ok(
        "orbit_task_list",
        json!({ "workspace": workspace.work.to_str().expect("utf8 checkout path") }),
    );
    assert!(
        listed["items"]
            .as_array()
            .expect("task list items")
            .iter()
            .any(|task| task["id"] == json!(task_id)),
        "listing applies the same tolerant read contract: {listed}"
    );
    drop(client);

    // (3) The SSH-marked remote session that first hit this (four failed
    // `orbit.task.show` calls in one session).
    let server_scratch = workspace.home.join("server-scratch");
    std::fs::create_dir_all(&server_scratch).expect("create server launch dir");
    let child = McpWorkspace::orbit_command(&server_scratch, &workspace.home)
        .args(["mcp", "serve", "--remote-caller-machine-id", "hm_caller"])
        .env("SSH_CONNECTION", "192.0.2.8 43100 198.51.100.2 22")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn SSH-marked MCP server");
    let mut client = McpClient::new(child);
    client.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "remote-crew-roundtrip", "version": "0" },
            "_meta": { "orbit": { "workspace": "ws_mcp-roundtrip" } },
        }),
    );
    client.notify("notifications/initialized");
    assert_unresolved_crew_projection(
        &client.call_tool_ok("orbit_task_show", json!({ "id": task_id })),
    );
    drop(client);

    // Execution still fails closed: starting the task needs a crew this host
    // can actually dispatch to.
    let started = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["tool", "run", "orbit.task.start", "--input", &show_input])
        .output()
        .expect("run task start");
    assert!(
        !started.status.success(),
        "start must not tolerate the crew"
    );
    let stderr = String::from_utf8_lossy(&started.stderr);
    assert!(
        stderr.contains("remote-only") && stderr.contains("not defined"),
        "start must fail with the actionable crew-validation error: {stderr}"
    );
}

/// The tolerant read contract: the stored crew stays verbatim, the resolved
/// projection is withheld rather than guessed, and the reason is reported as a
/// non-fatal field.
fn assert_unresolved_crew_projection(shown: &Value) {
    assert_eq!(
        shown["crew"],
        json!("remote-only"),
        "the raw stored crew stays visible: {shown}"
    );
    assert!(
        shown.get("resolved_crew").is_none() && shown.get("crew_model").is_none(),
        "an unresolvable crew must not be projected: {shown}"
    );
    assert!(
        shown["crew_unresolved"]
            .as_str()
            .is_some_and(|reason| reason.contains("remote-only")),
        "the unresolved marker must name the crew: {shown}"
    );
}

fn serve_mcp_from(cwd: &Path, home: &Path, initialize: Value) -> McpClient {
    let child = McpWorkspace::orbit_command(cwd, home)
        .args(["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn orbit mcp serve");
    let mut client = McpClient::new(child);
    client.request("initialize", initialize);
    client.notify("notifications/initialized");
    client
}

/// ORB-10963: managed executors may see the primary Orbit state through a
/// read-only mount while their linked checkout remains writable. Exercise the
/// production CLI and MCP entry points inside that mount namespace rather than
/// approximating the boundary with permission bits.
#[cfg(target_os = "linux")]
#[test]
fn readonly_state_mount_keeps_cli_and_mcp_reads_observational() {
    if !bubblewrap_mount_namespaces_available() {
        // Some nested container runners deny user/mount namespaces. The
        // production behavior remains covered by the focused store and
        // dispatch tests; an unrestricted Linux CI runner executes this path.
        return;
    }

    let workspace = McpWorkspace::init();
    let add_input = json!({
        "title": "Read-only managed fixture",
        "description": "State is warmed before it is remounted read-only",
        "workspace": workspace.work,
        "complexity": "low",
        "model": "codex",
    })
    .to_string();
    let created = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["tool", "run", "orbit.task.add", "--input", &add_input])
        .output()
        .expect("create the fixture task");
    assert_command_succeeded("fixture task add", &created);
    let created: Value = serde_json::from_slice(&created.stdout).expect("parse fixture task");
    let task_id = created["id"].as_str().expect("fixture task id");

    // Materialize every optional cache/import/index before the mount changes.
    // The assertions below still prove that opening and reading those stores
    // does not require a new sidecar, access-time, or audit write.
    let warm_search = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args([
            "tool",
            "run",
            "orbit.search",
            "--input",
            &json!({
                "query": "read-only managed fixture",
                "workspace": workspace.work,
                "model": "codex"
            })
            .to_string(),
        ])
        .output()
        .expect("warm fixture search state");
    assert_command_succeeded("fixture search warmup", &warm_search);

    // Leave a committed registry write behind in the WAL, the state the mount
    // has to read through (ORB-12090). The holder pins the snapshot taken
    // before the next task is created, so nothing checkpoints that task back
    // into the main database file.
    let canonical_root = workspace.home.join(".orbit");
    let registry = canonical_root.join("tasks").join("index.sqlite");
    let registry_holder = hold_uncheckpointed_registry_snapshot(&registry);
    let wal_only = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args([
            "tool",
            "run",
            "orbit.task.add",
            "--input",
            &json!({
                "title": "Committed into the registry WAL",
                "description": "Only a WAL-aware read-only open reports this task",
                "workspace": workspace.work,
                "complexity": "low",
                "model": "codex",
            })
            .to_string(),
        ])
        .output()
        .expect("create the WAL-only fixture task");
    assert_command_succeeded("WAL-only task add", &wal_only);
    let wal_only: Value = serde_json::from_slice(&wal_only.stdout).expect("parse WAL-only task");
    let wal_only_task_id = wal_only["id"].as_str().expect("WAL-only task id");
    assert!(
        !main_registry_file_has_task(&registry, wal_only_task_id),
        "the fixture must keep {wal_only_task_id} out of the main registry file"
    );

    let worktree = add_linked_worktree(&workspace.work);
    let workspace_state_root = workspace.work.join(".orbit");
    let protected_state = snapshot_fixture_state(&[&canonical_root, &workspace_state_root]);

    let tool_list = readonly_orbit_command(
        &worktree,
        &workspace.home,
        &canonical_root,
        &workspace_state_root,
    )
    .args(["tool", "list", "--json"])
    .output()
    .expect("list tools through the read-only mount");
    assert_command_succeeded("read-only orbit.tool.list", &tool_list);

    for (name, input) in [
        (
            "orbit.task.show",
            json!({ "id": task_id, "model": "codex" }),
        ),
        (
            "orbit.task.show",
            json!({ "id": wal_only_task_id, "model": "codex" }),
        ),
        (
            "orbit.task.list",
            json!({ "workspace": workspace.work, "model": "codex" }),
        ),
        (
            "orbit.search",
            json!({
                "query": "read-only managed fixture",
                "workspace": workspace.work,
                "model": "codex"
            }),
        ),
    ] {
        let output = readonly_orbit_command(
            &worktree,
            &workspace.home,
            &canonical_root,
            &workspace_state_root,
        )
        .args([
            "tool",
            "run",
            name,
            "--root",
            canonical_root.to_str().expect("utf8 Orbit root"),
            "--input",
            &input.to_string(),
        ])
        .output()
        .unwrap_or_else(|error| panic!("run {name} through the read-only mount: {error}"));
        assert_command_succeeded(name, &output);
    }

    let mutation = readonly_orbit_command(
        &worktree,
        &workspace.home,
        &canonical_root,
        &workspace_state_root,
    )
    .args([
        "tool",
        "run",
        "orbit.task.update",
        "--root",
        canonical_root.to_str().expect("utf8 Orbit root"),
        "--input",
        &json!({
            "id": task_id,
            "execution_summary": "must not persist",
            "model": "codex"
        })
        .to_string(),
    ])
    .output()
    .expect("attempt task mutation through the read-only mount");
    assert_readonly_mutation_failed("CLI", &mutation);

    let mut child = readonly_orbit_command(
        &worktree,
        &workspace.home,
        &canonical_root,
        &workspace_state_root,
    );
    child
        .args([
            "mcp",
            "serve",
            "--root",
            canonical_root.to_str().expect("utf8 Orbit root"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut client = McpClient::new(child.spawn().expect("spawn read-only MCP server"));
    let initialize = json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": { "name": "readonly-managed-executor", "version": "0" },
        "_meta": { "orbit": { "workspace": worktree } },
    });
    let initialized = client.request("initialize", initialize);
    assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
    client.notify("notifications/initialized");

    let listed = client.request("tools/list", Value::Null);
    assert!(listed["result"]["tools"].is_array(), "{listed}");
    assert_eq!(
        client.call_tool_ok("orbit_task_show", json!({ "id": task_id }))["id"],
        task_id
    );
    assert_eq!(
        client.call_tool_ok("orbit_task_show", json!({ "id": wal_only_task_id }))["id"],
        wal_only_task_id
    );
    let listed = client.call_tool_ok("orbit_task_list", json!({}));
    assert!(
        listed["items"]
            .as_array()
            .expect("task list items")
            .iter()
            .any(|task| task["id"] == json!(wal_only_task_id)),
        "the registry index must report its uncheckpointed WAL state: {listed}"
    );
    assert_eq!(
        client.call_tool_ok(
            "orbit_search",
            json!({ "query": "read-only managed fixture", "model": "codex" })
        )["mode"],
        "lexical"
    );
    let mutation = client.call_tool_err(
        "orbit_task_update",
        json!({
            "id": task_id,
            "execution_summary": "must not persist",
            "model": "codex"
        }),
    );
    assert_readonly_diagnostic("MCP", mutation["message"].as_str().unwrap_or_default());
    drop(client);

    assert_eq!(
        snapshot_fixture_state(&[&canonical_root, &workspace_state_root]),
        protected_state,
        "read-only CLI and MCP calls must leave canonical and workspace state unchanged"
    );
    drop(registry_holder);
}

/// The mount test above needs user and mount namespaces, which nested
/// container runners deny. This carries the same WAL-aware read contract on
/// every Unix runner by making the registry file set itself unwritable: the
/// CLI reaches the identical read-only open, one process removed from its
/// state, and must still report the task that only the WAL holds.
///
/// It does not replace the mount test — a `--ro-bind` also refuses the sidecar
/// creation and directory writes that permission bits here still allow.
#[cfg(unix)]
#[test]
fn read_only_registry_files_keep_uncheckpointed_wal_reads_observational() {
    use std::os::unix::fs::PermissionsExt;

    let workspace = McpWorkspace::init();
    let registry = workspace
        .home
        .join(".orbit")
        .join("tasks")
        .join("index.sqlite");
    let created = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args([
            "tool",
            "run",
            "orbit.task.add",
            "--input",
            &json!({
                "title": "Warmed before the registry files turn read-only",
                "description": "Every commit here is checkpointed into the main database file",
                "workspace": workspace.work,
                "complexity": "low",
                "model": "codex",
            })
            .to_string(),
        ])
        .output()
        .expect("create the warmup task");
    assert_command_succeeded("warmup task add", &created);

    let registry_holder = hold_uncheckpointed_registry_snapshot(&registry);
    let wal_only = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args([
            "tool",
            "run",
            "orbit.task.add",
            "--input",
            &json!({
                "title": "Committed into the registry WAL",
                "description": "Only a WAL-aware read-only open reports this task",
                "workspace": workspace.work,
                "complexity": "low",
                "model": "codex",
            })
            .to_string(),
        ])
        .output()
        .expect("create the WAL-only task");
    assert_command_succeeded("WAL-only task add", &wal_only);
    let wal_only: Value = serde_json::from_slice(&wal_only.stdout).expect("parse WAL-only task");
    let wal_only_task_id = wal_only["id"].as_str().expect("WAL-only task id");
    assert!(
        !main_registry_file_has_task(&registry, wal_only_task_id),
        "the fixture must keep {wal_only_task_id} out of the main registry file"
    );

    let registry_files: Vec<PathBuf> = ["", "-wal", "-shm"]
        .iter()
        .map(|suffix| PathBuf::from(format!("{}{suffix}", registry.display())))
        .collect();
    for file in &registry_files {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o400))
            .unwrap_or_else(|error| panic!("make {} read-only: {error}", file.display()));
    }
    let before: Vec<Vec<u8>> = registry_files
        .iter()
        .map(|file| std::fs::read(file).expect("snapshot registry file"))
        .collect();

    let shown = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args([
            "tool",
            "run",
            "orbit.task.show",
            "--input",
            &json!({ "id": wal_only_task_id, "model": "codex" }).to_string(),
        ])
        .output()
        .expect("show the WAL-only task through the read-only registry");
    assert_command_succeeded("read-only orbit.task.show", &shown);
    let shown: Value = serde_json::from_slice(&shown.stdout).expect("parse shown task");
    assert_eq!(shown["id"], json!(wal_only_task_id));

    let after: Vec<Vec<u8>> = registry_files
        .iter()
        .map(|file| std::fs::read(file).expect("re-read registry file"))
        .collect();
    assert_eq!(
        after, before,
        "observing must not change the registry database, WAL, or SHM"
    );
    drop(registry_holder);
}

/// The mount test's other read-only surface, carried on every Unix runner: the
/// canonical state directory refuses writes while the optional semantic index
/// it never carried is absent. The CLI reaches the identical creation denial
/// one process removed from a `--ro-bind`, and must still start and answer the
/// reads — such as tool listing — that never needed that index.
///
/// It does not replace the mount test: permission bits still allow directory
/// and sidecar writes a `--ro-bind` refuses outright.
#[cfg(unix)]
#[test]
fn absent_global_semantic_index_keeps_read_only_cli_reads_observational() {
    use std::os::unix::fs::PermissionsExt;

    let workspace = McpWorkspace::init();
    let created = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args([
            "tool",
            "run",
            "orbit.task.add",
            "--input",
            &json!({
                "title": "Readable without a global semantic index",
                "description": "The canonical root never built one",
                "workspace": workspace.work,
                "complexity": "low",
                "model": "codex",
            })
            .to_string(),
        ])
        .output()
        .expect("create the fixture task");
    assert_command_succeeded("fixture task add", &created);
    let created: Value = serde_json::from_slice(&created.stdout).expect("parse fixture task");
    let task_id = created["id"].as_str().expect("fixture task id");

    // The canonical root is not the checkout: nothing has run `orbit semantic
    // index` against it, so its optional index is absent.
    let canonical_root = workspace.home.join(".orbit");
    let state_dir = canonical_root.join("state");
    std::fs::create_dir_all(&state_dir).expect("canonical state directory");
    let semantic_db = state_dir.join("semantic.db");
    for suffix in ["", "-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", semantic_db.display()));
        if sidecar.exists() {
            std::fs::remove_file(&sidecar).expect("clear the unbuilt semantic index");
        }
    }

    let worktree = add_linked_worktree(&workspace.work);
    let original = std::fs::metadata(&state_dir)
        .expect("canonical state metadata")
        .permissions();
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o500))
        .expect("make the canonical state directory read-only");
    if std::fs::File::create(state_dir.join("probe")).is_ok() {
        // A user that ignores the mode bits (typically root) cannot reproduce
        // the creation denial this fixture is about.
        std::fs::set_permissions(&state_dir, original).expect("restore state permissions");
        return;
    }

    let listed = McpWorkspace::orbit_command(&worktree, &workspace.home)
        .env("ORBIT_ROOT", &canonical_root)
        .args(["tool", "list", "--json"])
        .output()
        .expect("list tools with an unavailable semantic index");
    let shown = McpWorkspace::orbit_command(&worktree, &workspace.home)
        .env("ORBIT_ROOT", &canonical_root)
        .args([
            "tool",
            "run",
            "orbit.task.show",
            "--root",
            canonical_root.to_str().expect("utf8 Orbit root"),
            "--input",
            &json!({ "id": task_id, "model": "codex" }).to_string(),
        ])
        .output()
        .expect("show a task with an unavailable semantic index");
    std::fs::set_permissions(&state_dir, original).expect("restore state permissions");

    assert_command_succeeded("read-only orbit.tool.list", &listed);
    let listed: Value = serde_json::from_slice(&listed.stdout).expect("parse listed tools");
    assert!(
        listed
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "orbit.task.show")),
        "tool listing needs no semantic index and must still answer: {listed}"
    );
    assert_command_succeeded("read-only orbit.task.show", &shown);
    let shown: Value = serde_json::from_slice(&shown.stdout).expect("parse shown task");
    assert_eq!(shown["id"], json!(task_id));
    assert!(
        !semantic_db.exists(),
        "an unavailable index must not be created by a read"
    );
}

/// Pin the registry's current snapshot so every write that follows stays in the
/// WAL: SQLite checkpoints when the last connection closes, and otherwise
/// copies frames back only as far as the oldest reader allows.
#[cfg(unix)]
fn hold_uncheckpointed_registry_snapshot(registry: &Path) -> Connection {
    assert!(
        registry.exists(),
        "warmed fixture registry: {}",
        registry.display()
    );
    let holder = Connection::open(registry).expect("open registry snapshot holder");
    let journal_mode: String = holder
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .expect("read registry journal mode");
    assert_eq!(
        journal_mode.to_lowercase(),
        "wal",
        "this fixture needs the registry's WAL"
    );
    holder
        .pragma_update(None, "wal_autocheckpoint", 0)
        .expect("keep new frames in the WAL");
    holder
        .execute_batch("BEGIN")
        .expect("start the pinning read transaction");
    holder
        .query_row("SELECT count(*) FROM task_bundle_bindings", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("pin the current registry snapshot");
    holder
}

/// Whether the main registry file alone — the view `immutable=1` gives, which
/// ignores the WAL entirely — knows about `task_id`.
#[cfg(unix)]
fn main_registry_file_has_task(registry: &Path, task_id: &str) -> bool {
    let main_file_only = Connection::open_with_flags(
        format!("file:{}?immutable=1", registry.display()),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open the main registry file alone");
    let bindings: i64 = main_file_only
        .query_row(
            "SELECT count(*) FROM task_bundle_bindings WHERE task_id = ?1",
            [task_id],
            |row| row.get(0),
        )
        .expect("count bindings in the main registry file");
    bindings > 0
}

#[cfg(target_os = "linux")]
fn bubblewrap_mount_namespaces_available() -> bool {
    Command::new("bwrap")
        .args([
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--",
            "/bin/true",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "linux")]
fn readonly_orbit_command(
    worktree: &Path,
    home: &Path,
    canonical_root: &Path,
    workspace_state_root: &Path,
) -> Command {
    let mut command = Command::new("bwrap");
    command
        .args(["--die-with-parent", "--bind", "/", "/"])
        .arg("--ro-bind")
        .arg(canonical_root)
        .arg(canonical_root)
        .arg("--ro-bind")
        .arg(workspace_state_root)
        .arg(workspace_state_root)
        .arg("--chdir")
        .arg(worktree)
        .arg("--setenv")
        .arg("HOME")
        .arg(home)
        .arg("--setenv")
        .arg("USERPROFILE")
        .arg(home)
        .arg("--setenv")
        .arg("PATH")
        .arg(stub_first_path(&McpWorkspace::stub_bin_dir(home)))
        .arg("--setenv")
        .arg("ORBIT_ROOT")
        .arg(canonical_root)
        .arg("--")
        .arg(env!("CARGO_BIN_EXE_orbit"));
    command
}

#[cfg(target_os = "linux")]
fn snapshot_fixture_state(roots: &[&Path]) -> BTreeSet<(PathBuf, Vec<u8>)> {
    let mut state = BTreeSet::new();

    for root in roots {
        snapshot_fixture_state_at(root, &mut state);
    }

    state
}

#[cfg(target_os = "linux")]
fn snapshot_fixture_state_at(root: &Path, state: &mut BTreeSet<(PathBuf, Vec<u8>)>) {
    for entry in std::fs::read_dir(root).expect("read protected fixture state directory") {
        let entry = entry.expect("read protected fixture state entry");
        let path = entry.path();
        let file_type = entry
            .file_type()
            .expect("read protected fixture state type");

        if file_type.is_dir() {
            snapshot_fixture_state_at(&path, state);
        } else if file_type.is_file() {
            state.insert((
                path.clone(),
                std::fs::read(&path).expect("read protected fixture state"),
            ));
        }
    }
}

// Not Linux-specific: this only asserts on exit status, and 14 call sites
// outside any target gate depend on it. It picked up the gate from the
// bubblewrap helpers it sits next to, which left this whole integration test
// uncompilable anywhere but Linux.
fn assert_command_succeeded(label: &str, output: &std::process::Output) {
    assert!(
        output.status.success(),
        "{label} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "linux")]
fn assert_readonly_mutation_failed(label: &str, output: &std::process::Output) {
    assert!(
        !output.status.success(),
        "{label} mutation unexpectedly succeeded"
    );
    assert_readonly_diagnostic(label, &String::from_utf8_lossy(&output.stderr));
}

#[cfg(target_os = "linux")]
fn assert_readonly_diagnostic(label: &str, diagnostic: &str) {
    assert!(
        diagnostic.contains(".task.yaml.lock"),
        "{label} diagnostic must attribute the task path: {diagnostic}"
    );
    let normalized = diagnostic.to_ascii_lowercase();
    assert!(
        normalized.contains("read-only file system")
            || normalized.contains("permission denied")
            || normalized.contains("not writable"),
        "{label} diagnostic must identify EROFS/EACCES: {diagnostic}"
    );
}

/// Commit the checkout and attach a linked worktree, mirroring how the engine
/// stages a managed run.
fn add_linked_worktree(work: &Path) -> PathBuf {
    let commit = Command::new("git")
        .args([
            "-c",
            "user.email=mcp-roundtrip@orbit.test",
            "-c",
            "user.name=mcp-roundtrip",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "worktree fixture base",
        ])
        .current_dir(work)
        .output()
        .expect("commit worktree base");
    assert!(commit.status.success(), "git commit failed: {commit:?}");

    let worktree = work
        .parent()
        .expect("checkout parent")
        .join("linked-worktree");
    let added = Command::new("git")
        .args([
            "worktree",
            "add",
            "--quiet",
            "-b",
            "orbit/worktree-fixture",
            worktree.to_str().expect("utf8 worktree path"),
        ])
        .current_dir(work)
        .output()
        .expect("attach linked worktree");
    assert!(added.status.success(), "git worktree add failed: {added:?}");
    worktree
}
