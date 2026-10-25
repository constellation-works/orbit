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
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::bound_server::{Announcement, BoundServer};
use crate::child_guard::ChildGuard;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use orbit_common::test_env;
use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);
/// Synthetic Claude worker credential for stub providers on macOS [ORB-15154].
/// Not a real token. The guard refuses `claude` unless this name is both in
/// `[execution.env] pass` and present on the server that launches the worker.
const FIXTURE_CLAUDE_WORKER_TOKEN: &str = "fixture-claude-worker-token";
const FIXTURE_CLAUDE_WORKER_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";
/// Path of the checked-in `tools/list` snapshot, relative to the crate root.
const SNAPSHOT_RELATIVE_PATH: &str = "tests/snapshots/mcp_tools_list.json";
/// The SSH session marker every remote-originated scenario presents.
const SSH_ENV: (&str, &str) = ("SSH_CONNECTION", "192.0.2.8 43100 198.51.100.2 22");

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

/// [ORB-13899] An answer using every part of the envelope `result` an
/// operator reads: the named fields and one the prompt asked for.
const AGENT_ANSWER_ENVELOPE: &str = r##"{"schemaVersion":1,"status":"success","result":{"summary":"remote probe complete","findings":["the clock restarts on reload"],"next_steps":["pin the reload interval"],"report_markdown":"# Probe\n\nAll clear."},"error":null}"##;

/// Admit the synthetic credential through the pass list the guard reads.
///
/// A distinct workspace `config.toml` makes `execution.env.pass` replace-only,
/// so the name has to be restated there. Otherwise the global file written by
/// `orbit init` is the list that reaches the provider.
fn admit_fixture_claude_worker_token(workspace: &McpWorkspace) {
    let workspace_config = workspace.work.join(".orbit").join("config.toml");
    let global_config = workspace.home.join(".orbit").join("config.toml");
    let path = if workspace_config.is_file() {
        workspace_config
    } else {
        global_config
    };
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = ensure_pass_contains(&raw, FIXTURE_CLAUDE_WORKER_TOKEN_ENV);
    if updated != raw {
        std::fs::write(&path, updated).expect("write fixture claude worker pass list");
    }
}

fn ensure_pass_contains(config: &str, name: &str) -> String {
    let mut document: toml_edit::DocumentMut = config.parse().expect("fixture config TOML");
    let mut pass = document
        .get("execution")
        .and_then(|execution| execution.get("env"))
        .and_then(|env| env.get("pass"))
        .and_then(toml_edit::Item::as_array)
        .cloned()
        .unwrap_or_else(|| {
            [
                "HOME",
                "PATH",
                "CODEX_HOME",
                "TMPDIR",
                "USER",
                "__CF_USER_TEXT_ENCODING",
            ]
            .into_iter()
            .collect()
        });
    if pass.iter().any(|entry| entry.as_str() == Some(name)) {
        return config.to_string();
    }
    // Init may seed a compact array without a trailing comma; edit TOML rather
    // than splicing text that only works for the global multiline template.
    pass.push(name);
    document["execution"]["env"]["pass"] = toml_edit::value(pass);
    document.to_string()
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
        Self::init_with_task_prefix(workspace_name, extra_workspace_args, "TST")
    }

    fn init_with_task_prefix(
        workspace_name: &str,
        extra_workspace_args: &[&str],
        task_prefix: &str,
    ) -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&home).expect("create home");
        std::fs::create_dir_all(&work).expect("create work");
        // Orbit registers and reports the checkout at its resolved path, and
        // tests compare what it reports with `work`, so spell it that way.
        // The default macOS temp directory sits behind `/var`, a symlink.
        let work = work.canonicalize().expect("resolve work");

        // `orbit init` freezes crew seeding to the agent CLIs it finds on
        // `PATH` (ADR-0193), so the fixture plants a stub `codex` before it
        // runs. Without one, a host with no agent CLI installed — every CI
        // runner — seeds an empty `[crews]` table, and any task naming a crew
        // is rejected with `crew '<name>' is not defined in [crews.*]`.
        plant_agent_cli_stub(&Self::stub_bin_dir(&home), "codex");

        let output = crate::git_repo::command()
            .args(["init", "--quiet"])
            .current_dir(&work)
            .output()
            .expect("initialize Git checkout");
        assert!(output.status.success(), "git init failed: {output:?}");
        assert!(
            work.join(".git").is_dir(),
            "fixture needs its own repository"
        );

        let init_args = vec![
            "init",
            "--non-interactive",
            // This disposable transport fixture does not dispatch sandboxed
            // work. Host prerequisite probes would prevent its reads from
            // being exercised inside a managed executor's outer sandbox.
            "--skip-host-prerequisites",
            "--machine-name",
            "mcp-roundtrip-host",
            "--task-prefix",
            task_prefix,
        ];
        orbit_ok(Self::orbit_command(&work, &home).args(init_args));

        let mut workspace_init_args = vec!["workspace", "init", "--name", workspace_name];
        workspace_init_args.extend_from_slice(extra_workspace_args);
        orbit_ok(Self::orbit_command(&work, &home).args(workspace_init_args));

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

    /// An `orbit` command rooted at this workspace's checkout and home.
    fn cmd(&self) -> Command {
        Self::orbit_command(&self.work, &self.home)
    }

    /// Run the fixture CLI in this workspace; the run must succeed.
    fn run_ok(&self, args: &[&str]) -> std::process::Output {
        cli_ok(&self.work, &self.home, args)
    }

    /// Run the fixture CLI in this workspace without asserting success.
    fn run(&self, args: &[&str]) -> std::process::Output {
        cli(&self.work, &self.home, args)
    }

    /// `orbit tool run <tool> --input <input>` in this workspace; must succeed.
    fn tool_ok(&self, tool: &str, input: Value) -> std::process::Output {
        tool_ok_at(&self.work, &self.home, tool, input)
    }

    /// The fixture's global store, for scenarios that seed or audit rows.
    fn store(&self) -> Connection {
        Connection::open(self.home.join(".orbit/orbit.db")).expect("open fixture store")
    }

    /// A fresh workspace and its connected `serve` client, in one step.
    fn served() -> (Self, McpClient) {
        let workspace = Self::init();
        let client = workspace.serve();
        (workspace, client)
    }

    /// A session marked as SSH-originated from machine `hm_caller`-style
    /// labels: the caller passes the full extra argv, this adds `SSH_ENV`.
    fn serve_remote(&self, extra_args: &[&str]) -> McpClient {
        self.serve_with_args_and_env(extra_args, &[SSH_ENV])
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
        // A real credential inherited from the test process must not satisfy
        // the macOS guard. Callers that dispatch a stub set a synthetic token.
        command.env_remove("CLAUDE_CODE_OAUTH_TOKEN");
        command.env_remove("ANTHROPIC_API_KEY");
        for (key, value) in env {
            command.env(key, value);
        }
        let child = command.args(args).spawn_piped();
        let mut client = McpClient::new(child);
        self.initialize(&mut client);
        client
    }

    /// Spawn the server with the workspace-runtime open log enabled, written to
    /// `log_path` instead of a pipe nothing drains. The log is the server's own
    /// account of how many runtimes a session opened, and the fmt layer writes
    /// it to stderr synchronously, so it is complete for every call already
    /// answered.
    fn serve_logging_runtime_opens(&self, log_path: &Path) -> McpClient {
        let log = std::fs::File::create(log_path).expect("create the server log");
        let child = Self::orbit_command(&self.work, &self.home)
            .args(["mcp", "serve"])
            .env("RUST_LOG", "orbit.mcp.runtime=debug")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .map(ChildGuard::new)
            .expect("spawn orbit mcp serve");
        let mut client = McpClient::new(child);
        self.initialize(&mut client);
        client
    }

    /// Spawn `orbit mcp listen` on an ephemeral port announced by the server
    /// and run the same handshake over the socket it accepts.
    fn listen(&self) -> (McpClient<BoundServer>, SocketAddr) {
        let (mut client, addr) = self.listen_client(&[]);
        self.initialize(&mut client);
        (client, addr)
    }

    /// Spawn `orbit mcp listen --workspace <selector>` and connect without
    /// announcing any workspace at initialize, so the only binding the session
    /// can have is the one the listener was launched with.
    fn listen_bound_to(&self, selector: &str) -> McpClient<BoundServer> {
        let (mut client, _) = self.listen_client(&["--workspace", selector]);
        let response = client.initialize(McpClient::initialize_params("listen-bound", None));
        assert_eq!(response["result"]["serverInfo"]["name"], "orbit-mcp");
        client
    }

    fn listen_client(&self, extra_args: &[&str]) -> (McpClient<BoundServer>, SocketAddr) {
        let mut child = BoundServer::spawn(
            Self::orbit_command(&self.work, &self.home)
                .args(["mcp", "listen"])
                .args(extra_args)
                .arg("127.0.0.1:0")
                .env("RUST_LOG", "info")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped()),
            Announcement::Mcp,
        )
        .expect("spawn orbit mcp listen");
        let addr = child
            .wait_for_address(RESPONSE_TIMEOUT)
            .expect("MCP listener bind announcement");
        let stream = TcpStream::connect(addr).expect("connect to announced MCP listener");
        (McpClient::over_tcp(child, stream), addr)
    }

    /// The MCP initialize handshake, announcing this workspace via
    /// `_meta.orbit.workspace`.
    fn initialize<C>(&self, client: &mut McpClient<C>) {
        let workspace = utf8(&self.work);
        let response = client.initialize(McpClient::initialize_params(
            "orbit-mcp-roundtrip-test",
            Some(workspace),
        ));
        let result = &response["result"];
        assert_eq!(result["protocolVersion"], "2025-06-18");
        assert_eq!(result["serverInfo"]["name"], "orbit-mcp");
        assert!(
            result["capabilities"]["tools"].is_object(),
            "tools capability missing: {result}"
        );
    }
}

/// Run a fixture CLI command in its already-isolated child process and keep
/// both streams in the failure report. The caller can inspect stdout/stderr.
fn orbit_ok(command: &mut Command) -> std::process::Output {
    let output = command.output().expect("run fixture orbit CLI");
    assert!(
        output.status.success(),
        "orbit CLI failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    output
}

/// `orbit <args>` at an explicit checkout/home pair; the run must succeed.
fn cli_ok(work: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    orbit_ok(McpWorkspace::orbit_command(work, home).args(args))
}

/// `orbit <args>` at an explicit checkout/home pair, success not asserted.
fn cli(work: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    McpWorkspace::orbit_command(work, home)
        .args(args)
        .output()
        .expect("run fixture orbit CLI")
}

/// `orbit tool run <tool> --input <input>` at a checkout; must succeed.
fn tool_ok_at(work: &Path, home: &Path, tool: &str, input: Value) -> std::process::Output {
    cli_ok(
        work,
        home,
        &["tool", "run", tool, "--input", &input.to_string()],
    )
}

/// A command's stdout as text.
fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A command's stderr as text.
fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Spawn with all three streams piped, guarded so the child dies with the test.
trait SpawnPiped {
    fn spawn_piped(&mut self) -> ChildGuard;
}

impl SpawnPiped for Command {
    fn spawn_piped(&mut self) -> ChildGuard {
        self.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map(ChildGuard::new)
            .expect("spawn orbit mcp serve")
    }
}

/// The tool objects inside a `tools/list` response envelope.
fn tools_array(listed: &Value) -> &Vec<Value> {
    listed["result"]["tools"].as_array().expect("tools array")
}

/// One advertised tool object inside a listed tools array, by name.
fn find_tool<'a>(tools: &'a [Value], name: &str) -> &'a Value {
    tools
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("{name} is advertised"))
}

/// The advertised tool names inside a `tools/list` response envelope.
fn tool_names(listed: &Value) -> Vec<&str> {
    tools_array(listed)
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect()
}

/// `orbit audit list <args> --json` in this workspace, parsed.
fn audit_rows(workspace: &McpWorkspace, args: &[&str]) -> Value {
    let mut full = vec!["audit", "list"];
    full.extend_from_slice(args);
    full.push("--json");
    stdout_json(&workspace.run_ok(&full))
}

/// One audit aggregate row: `COUNT(*)` plus `cols` text columns, by position.
fn audit_tuple(conn: &Connection, sql: &str, cols: usize) -> (i64, Vec<Option<String>>) {
    conn.query_row(sql, [], |row| {
        let mut values = Vec::with_capacity(cols);
        for i in 1..=cols {
            values.push(row.get::<_, Option<String>>(i)?);
        }
        Ok((row.get::<_, i64>(0)?, values))
    })
    .expect("audit aggregate row")
}

/// Create a task through the CLI `tool run` surface; returns its id.
fn add_task_cli(workspace: &McpWorkspace, title: &str, description: &str) -> String {
    let output = workspace.tool_ok(
        "orbit.task.add",
        json!({
            "title": title, "description": description, "workspace": workspace.work,
            "complexity": "low", "model": "codex",
        }),
    );
    id_of(&stdout_json(&output))
}

/// Plant the same agent script under both provider names the fixture's
/// default crew might have frozen at init.
fn plant_both_providers(workspace: &McpWorkspace, script: &str) {
    for provider in ["codex", "claude"] {
        plant_agent_cli_stub(&McpWorkspace::stub_bin_dir(&workspace.home), provider);
        std::fs::write(
            McpWorkspace::stub_bin_dir(&workspace.home).join(provider),
            script,
        )
        .unwrap();
    }
}

/// Assert a bound session advertises `tool`'s workspace selector as optional.
fn assert_selector_optional<C>(client: &mut McpClient<C>, tool: &str) {
    let listed = client.tools_list();
    let description = tool_workspace_description(&listed, tool);
    assert!(
        description.contains("Optional in this session"),
        "a bound session must advertise {tool}'s selector as optional: {description}"
    );
}

/// A command's stdout parsed as JSON.
fn stdout_json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("parse CLI JSON output")
}

/// The `id` of a created record.
fn id_of(value: &Value) -> String {
    value["id"].as_str().expect("task id").to_string()
}

/// Whether a structured tool error's `message` contains `needle`.
fn message_contains(value: &Value, needle: &str) -> bool {
    value["message"]
        .as_str()
        .is_some_and(|message| message.contains(needle))
}

/// The array at `key` of a JSON object.
fn array_of<'a>(value: &'a Value, key: &str) -> &'a Vec<Value> {
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} array: {value}"))
}

/// Write a fixture text file.
fn write_text(path: &Path, contents: &str) {
    std::fs::write(path, contents)
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

/// Write the fixture's federated `mcp-destinations.toml`.
fn write_destinations(workspace: &McpWorkspace, contents: &str) {
    write_text(
        &workspace.home.join(".orbit").join("mcp-destinations.toml"),
        contents,
    );
}

/// Assert a scenario spawned no SSH: the stub's invocation log is absent/empty.
fn assert_no_ssh(ssh_log: &Path, why: &str) {
    assert!(
        !ssh_log.exists()
            || std::fs::read_to_string(ssh_log)
                .expect("read ssh log")
                .is_empty(),
        "{why}"
    );
}

/// A fixture path as UTF-8 text.
fn utf8(path: &Path) -> &str {
    path.to_str().expect("utf8 fixture path")
}

/// One named scenario row: its recorded label and its check.
type Row = (&'static str, fn());
/// One named scenario row whose coverage can be unavailable on this host.
#[cfg(unix)]
type BoolRow = (&'static str, fn() -> bool);

/// Run named scenario rows, recording each pass under its own label so a
/// failure still names the case and QA rows keep their pinned labels.
fn run_rows(rows: &[Row]) {
    for (name, check) in rows {
        tracing::info!(case = name, "MCP contract row");
        check();
        orbit_common::test_env::record_scenario_pass(name);
    }
}

/// Rows whose coverage can be unavailable on this host (`false`): the pass
/// is recorded only when the row actually ran.
#[cfg(unix)]
fn run_bool_rows(rows: &[BoolRow]) {
    for (name, check) in rows {
        tracing::info!(case = name, "MCP contract row");
        if check() {
            orbit_common::test_env::record_scenario_pass(name);
        } else {
            let _ = writeln!(
                std::io::stderr(),
                "SKIP: {name}: host permits writes to the read-only fixture"
            );
        }
    }
}

/// Scenario-row table: each entry is a scenario function whose own name is
/// its recorded row label.
macro_rules! rows {
    ($($f:ident),+ $(,)?) => { run_rows(&[$((stringify!($f), $f as fn())),+]) };
}
#[cfg(unix)]
macro_rules! bool_rows {
    ($($f:ident),+ $(,)?) => { run_bool_rows(&[$((stringify!($f), $f as fn() -> bool)),+]) };
}

// ---------------------------------------------------------------------------
// Minimal JSON-RPC client over the server's newline-delimited byte stream —
// the child's stdio, or a socket when the server is a listener. Responses may
// arrive out of order (the server fans tool calls into blocking workers), so
// match strictly by id.
// ---------------------------------------------------------------------------

struct McpClient<C = ChildGuard> {
    child: C,
    writer: Box<dyn Write + Send>,
    lines: Receiver<String>,
    next_id: i64,
}

impl McpClient {
    fn initialize_params(name: &str, workspace: Option<&str>) -> Value {
        let mut params = json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": name, "version": "0" },
        });
        if let Some(workspace) = workspace {
            params["_meta"] = json!({ "orbit": { "workspace": workspace } });
        }
        params
    }

    /// Complete the standard MCP handshake for a custom server launch.
    fn initialized(child: ChildGuard, params: Value) -> (Self, Value) {
        let mut client = Self::new(child);
        let response = client.initialize(params);
        (client, response)
    }

    fn new(mut child: ChildGuard) -> Self {
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        Self::over_streams(child, Box::new(stdin), Box::new(stdout))
    }
}

impl<C> McpClient<C> {
    fn initialize(&mut self, params: Value) -> Value {
        let response = self.request("initialize", params);
        self.notify("notifications/initialized");
        response
    }

    /// A session against `orbit mcp listen`, where the same protocol runs over
    /// an accepted socket instead of the child's stdio.
    fn over_tcp(child: C, stream: TcpStream) -> Self {
        let reader = stream.try_clone().expect("clone the MCP socket for reads");
        Self::over_streams(child, Box::new(stream), Box::new(reader))
    }

    fn over_streams(child: C, writer: Box<dyn Write + Send>, reader: Box<dyn Read + Send>) -> Self {
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

    /// The raw `tools/list` response envelope.
    fn tools_list(&mut self) -> Value {
        self.request("tools/list", Value::Null)
    }

    /// The advertised tool objects from `tools/list`.
    fn listed_tools(&mut self) -> Vec<Value> {
        self.tools_list()["result"]["tools"]
            .as_array()
            .expect("tools array")
            .clone()
    }

    /// One advertised tool object, by name.
    fn listed_tool(&mut self, name: &str) -> Value {
        self.listed_tools()
            .into_iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} is advertised"))
    }

    /// `orbit_task_show` for one task, which must succeed.
    fn show_task(&mut self, id: &str) -> Value {
        self.call_tool_ok("orbit_task_show", json!({ "id": id }))
    }

    /// `orbit_task_add` with the fixture's standard fields; returns the id.
    fn add_task(&mut self, title: &str, description: &str) -> String {
        id_of(&self.add_task_full(title, description))
    }

    /// `orbit_task_add` with the fixture's standard fields; returns the record.
    fn add_task_full(&mut self, title: &str, description: &str) -> Value {
        self.call_tool_ok(
            "orbit_task_add",
            json!({ "title": title, "description": description, "complexity": "low", "model": "codex" }),
        )
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

/// Create a second Git checkout at `dir` and register it as workspace `name`
/// under the fixture home `home`.
fn init_checkout(home: &Path, dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).expect("create the second checkout");
    let output = crate::git_repo::command()
        .args(["init", "--quiet"])
        .current_dir(dir)
        .output()
        .expect("initialize the second Git checkout");
    assert!(output.status.success(), "git init failed: {output:?}");
    cli_ok(dir, home, &["workspace", "init", "--name", name]);
}

/// Rewrite the registry's logical workspace ID to `ws_legacy-logical`,
/// diverging it from the checkout identity `from` that keys the task store.
fn diverge_registry_id(workspace: &McpWorkspace, from: &str) {
    let registry_path = workspace.home.join(".orbit").join("workspaces.json");
    let registry = std::fs::read_to_string(&registry_path).expect("read workspace registry");
    std::fs::write(&registry_path, registry.replace(from, "ws_legacy-logical"))
        .expect("write diverged workspace registry");
}

/// Assert a governed session's run list is reachable and empty.
fn assert_run_list_empty(client: &mut McpClient) {
    assert_eq!(
        client.call_tool_ok("orbit_workflow_run_list", json!({}))["items"],
        json!([])
    );
}

/// Assert an unprivileged session is denied the governed run list.
fn assert_run_list_denied(client: &mut McpClient) {
    let denied = client.call_tool_err("orbit_workflow_run_list", json!({}));
    assert_eq!(denied["code"], "capability_denied", "{denied}");
}

/// Poll a workflow run over MCP until it reaches a terminal state.
fn wait_for_run(client: &mut McpClient, run_id: &str) -> Value {
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    loop {
        let shown = client.call_tool_ok("orbit_workflow_run_show", json!({ "id": run_id }));
        if matches!(
            shown["state"].as_str(),
            Some("success" | "failed" | "timeout" | "cancelled" | "interrupted")
        ) {
            return shown;
        }
        assert!(
            Instant::now() < deadline,
            "agent invocation did not finish: {shown}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn task_update_refusal_names_a_reachable_status() {
    let workspace = McpWorkspace::init();
    let task_id = author_task(&workspace, "Fallback lifecycle guidance");
    let mut client = workspace.serve();

    let refused = client.call_tool_err(
        "orbit_task_update",
        json!({ "id": task_id, "status": "review", "model": "codex" }),
    );
    assert_eq!(
        refused["code"], "invalid_input",
        "wrong error category: {refused}"
    );
    assert!(
        message_contains(&refused, "backlog"),
        "the refusal should name a status reachable from proposed: {refused}"
    );

    let unchanged = client.show_task(&task_id);
    assert_eq!(
        unchanged["status"], "proposed",
        "a refused status was written"
    );
}

#[test]
fn agent_task_deletion_is_denied_through_every_dispatch_path() {
    if crate::isolated_test::run_in_child_with_env(
        "mcp_roundtrip::agent_task_deletion_is_denied_through_every_dispatch_path",
        &[("ORBIT_AGENT_MODEL", "codex")],
    ) {
        return;
    }

    use orbit_core::OrbitRuntime;
    use orbit_core::adapter::command::ToolEntryPoint;
    use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};

    let runtime = OrbitRuntime::in_memory().expect("isolated runtime");
    let task = runtime
        .run_tool(
            "orbit.task.add",
            json!({
                "workspace": "fixture", "title": "Deletion guard", "description": "Keep history",
                "complexity": "low", "model": "codex",
            }),
        )
        .expect("seed a disposable task");
    let task_id = task["id"].as_str().expect("created task id");
    let input = json!({"id": task_id, "force": true});
    for (path, result) in [
        (
            "runtime",
            runtime.run_tool("orbit.task.delete", input.clone()),
        ),
        (
            "human attribution",
            runtime.run_tool_as_human("orbit.task.delete", input.clone()),
        ),
    ] {
        assert!(
            matches!(result, Err(orbit_common::OrbitError::CapabilityDenied(_))),
            "agent deletion must be capability-denied through {path}: {result:?}"
        );
        assert!(runtime.get_task(task_id).is_ok(), "{path} deleted the task");
    }
    for entry in [ToolEntryPoint::Cli, ToolEntryPoint::Mcp] {
        for transport in [McpTransport::Local, McpTransport::SshMcp] {
            let result = runtime.execute_tool_command_dispatch_with_session_context(
                "orbit.task.delete",
                input.clone(),
                None,
                Some("codex".into()),
                entry,
                ToolSessionContext {
                    effective_capabilities: BTreeSet::from([McpCapability::Agent]),
                    transport: Some(transport),
                    caller_machine_id: Some("agent-origin".into()),
                    ..ToolSessionContext::default()
                },
            );
            assert!(
                result.is_err(),
                "agent deletion reached {entry:?}/{transport:?}: {result:?}"
            );
            assert!(
                runtime.get_task(task_id).is_ok(),
                "{entry:?}/{transport:?} deleted the task"
            );
        }
    }
    // Positive control: the same handler really deletes when an operator authorizes it.
    {
        let _operator = test_env::scoped([("ORBIT_OPERATOR", Some("1"))]);
        runtime
            .run_tool("orbit.task.delete", input)
            .expect("operator reaches task deletion");
        assert!(
            runtime.get_task(task_id).is_err(),
            "operator deletion must persist"
        );
    }
    // Ordinary agent MCP discovery hides deletion, but guessed names must also fail.
    let workspace = McpWorkspace::init();
    let real_task_id = author_task(&workspace, "MCP deletion guard");
    for args in [vec![], vec!["--remote-caller-machine-id", "hm_agent"]] {
        let mut client = workspace.serve_with_args(&args);
        for name in ["orbit_task_delete", "orbit.task.delete"] {
            let refused = client.call_tool_err(name, json!({"id": real_task_id, "force": true}));
            assert_eq!(refused["code"], "tool_not_found", "{name}: {refused}");
            let shown = client.show_task(&real_task_id);
            assert_eq!(shown["id"], real_task_id, "MCP deletion destroyed the task");
        }
    }
}

#[test]
fn mcp_serve_tools_list_matches_production_snapshot() {
    let (_workspace, mut client) = McpWorkspace::served();

    let response = client.tools_list();
    let tools = response["result"]["tools"].clone();

    let names = tool_names(&response);
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
                 `ORBIT_MCP_UPDATE_SNAPSHOT=1 cargo test -p orbit-cli --test mcp mcp_roundtrip::`",
                snapshot_path.display()
            )
        }),
    )
    .expect("parse snapshot JSON");
    assert_eq!(
        tools, expected,
        "tools/list drifted from {SNAPSHOT_RELATIVE_PATH}. MCP tool schema changes are \
         BREAKING (see RELEASING.md). If the change is intentional, regenerate the snapshot \
         with `ORBIT_MCP_UPDATE_SNAPSHOT=1 cargo test -p orbit-cli --test mcp mcp_roundtrip::` and \
         call the release breaking."
    );
}

fn mcp_server_advertises_governed_tools_but_denies_an_unprivileged_session() {
    let (workspace, mut client) = McpWorkspace::served();
    let response = client.tools_list();
    let names: BTreeSet<_> = tool_names(&response).into_iter().collect();

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
        json!({ "argv": ["touch", utf8(&marker)], "working_directory": workspace.work, }),
    );
    assert_eq!(denied["code"], "capability_denied", "{denied}");
    assert!(!marker.exists(), "denied command reached domain execution");
}

/// Index of `step_id` in a shipped job, read from the job asset itself so the
/// fixture checkpoints the step the host would.
fn shipped_step_index(job_id: &str, step_id: &str) -> u32 {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../orbit-core/assets/jobs")
        .join(format!("{job_id}.yaml"));
    let job: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(&path).expect("read shipped job"))
            .expect("parse shipped job");
    job["spec"]["steps"]
        .as_sequence()
        .expect("job steps")
        .iter()
        .position(|step| step["id"].as_str() == Some(step_id))
        .and_then(|index| u32::try_from(index).ok())
        .unwrap_or_else(|| panic!("{job_id} has step {step_id}"))
}

/// [ORB-13744] An ordinary, non-operator MCP session reads the bounded delivery
/// observation of a PR run — host commit and merge checkpoints only — while
/// the full run view stays operator-only. The call is audited, and nothing the
/// run was submitted with reaches the wire.
#[test]
fn an_unprivileged_session_reads_bounded_delivery_evidence_but_not_the_run() {
    if crate::isolated_test::run_in_child(
        "mcp_roundtrip::an_unprivileged_session_reads_bounded_delivery_evidence_but_not_the_run",
    ) {
        return;
    }
    const SECRET: &str = "sk-live-delivery-roundtrip-must-not-echo";
    const BASE: &str = "1111111111111111111111111111111111111111";
    const HEAD: &str = "2222222222222222222222222222222222222222";
    const LANDED: &str = "3333333333333333333333333333333333333333";
    const RUN_ID: &str = "jrun-20261003-0000-dl";

    let (workspace, mut client) = McpWorkspace::served();
    let task_id = client.add_task(
        "Deliver over MCP",
        "Observed through the public delivery read.",
    );
    let foreign_task_id = client.add_task(
        "Task outside delivery run",
        "Not submitted with the delivery run.",
    );
    assert_ne!(foreign_task_id, task_id);

    // The run row and its checkpoints, written against the fixture's own
    // disposable roots exactly as the host's pipeline worker leaves them.
    let runtime = orbit_core::OrbitRuntime::from_roots(
        &workspace.home.join(".orbit"),
        &workspace.work.join(".orbit"),
    )
    .expect("fixture runtime");
    let workspace_id = runtime.workspace_id().expect("workspace identity");
    workspace
        .store()
        .execute(
            "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, input_json,
                 scheduled_at, started_at, finished_at, duration_ms, created_at)
             VALUES (?1, ?2, 'task_pr_pipeline', 1, 'success', ?3,
                 '2026-10-03T01:00:00+00:00', '2026-10-03T01:00:01+00:00',
                 '2026-10-03T01:30:00+00:00', 1799000, '2026-10-03T01:00:00+00:00')",
            rusqlite::params![
                RUN_ID,
                workspace_id,
                json!({ "task_ids": [task_id], "prompt": SECRET }).to_string(),
            ],
        )
        .expect("seed delivery run");
    let mut state = orbit_types::workflow::PipelineState::new(
        RUN_ID.to_string(),
        "task_pr_pipeline".to_string(),
        json!({}),
    );
    for (step_id, output) in [
        (
            "commit",
            json!({
                "phase": "commit", "decision": "performed", "committed": true,
                "commit_sha": HEAD, "base_sha": BASE, "job_run_id": RUN_ID,
                "task_id": task_id,
            }),
        ),
        (
            "complete_pr",
            json!({
                "phase": "complete",
                "merge": {
                    "merged": true, "pr_number": "4242", "landed_commit": LANDED,
                    "delivery_evidence": { "token": SECRET },
                },
            }),
        ),
    ] {
        let index = shipped_step_index("task_pr_pipeline", step_id);
        state.record_step(
            index,
            orbit_types::workflow::JobRunState::Success,
            Some(output.clone()),
            None,
        );
        state.record_pipeline_output(step_id, output);
    }
    runtime
        .write_run_state(RUN_ID, &state)
        .expect("write run checkpoints");

    let delivery = |client: &mut McpClient, run_id: Option<&str>| {
        let mut input = json!({ "id": task_id, "field": "delivery" });
        if let Some(run_id) = run_id {
            input["run_id"] = json!(run_id);
        }
        client.call_tool_ok("orbit_task_show", input)
    };
    let observed = delivery(&mut client, Some(RUN_ID));
    // Without a run ID the read answers from the newest delivery run.
    let newest = delivery(&mut client, None);
    assert_eq!(newest, observed);
    assert_eq!(observed["delivery_status"], "landed", "{observed}");
    assert_eq!(observed["workspace_id"], json!(workspace_id));
    assert_eq!(observed["commit"]["status"], "committed");
    assert_eq!(observed["commit"]["base_sha"], BASE);
    assert_eq!(observed["commit"]["head_sha"], HEAD);
    assert_eq!(observed["commit"]["provenance"]["activity"], "git_commit");
    assert_eq!(observed["landing"]["status"], "merged");
    assert_eq!(observed["landing"]["method"], "pull_request");
    assert_eq!(observed["landing"]["landed_commit"], LANDED);
    assert_eq!(observed["landing"]["pr_number"], 4242);
    const DELIVERY_FIELDS: [&str; 11] = [
        "schema_version",
        "workspace_id",
        "repository",
        "task_id",
        "run_id",
        "job_id",
        "run_state",
        "run_finished_at",
        "delivery_status",
        "commit",
        "landing",
    ];
    assert_eq!(
        observed
            .as_object()
            .expect("observation object")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(DELIVERY_FIELDS),
        "the observation is a closed, bounded shape"
    );
    assert!(
        !observed.to_string().contains(SECRET),
        "run input or step output leaked: {observed}"
    );

    // The real foreign task is absent from the run input above, so this call
    // must reach the run-membership refusal instead of failing task lookup.
    let foreign = client.call_tool_err(
        "orbit_task_show",
        json!({ "id": foreign_task_id, "field": "delivery", "run_id": RUN_ID }),
    );
    assert_eq!(
        foreign["code"], "invalid_input",
        "a real task omitted from the run must reach the membership refusal: {foreign}"
    );
    for field in DELIVERY_FIELDS {
        assert!(
            foreign.get(field).is_none(),
            "refused delivery response included observation field {field}: {foreign}"
        );
    }

    // The full run view is still the operator's.
    let denied = client.call_tool_err("orbit_workflow_run_show", json!({ "id": RUN_ID }));
    assert_eq!(denied["code"], "capability_denied", "{denied}");
    drop(client);

    let rows = audit_rows(&workspace, &["--tool", "orbit.task.show"]);
    let statuses = rows
        .as_array()
        .expect("audit row array")
        .iter()
        .map(|row| row["status"].as_str().expect("audit status"))
        .collect::<Vec<_>>();
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == "success")
            .count(),
        2,
        "each answered read is audited once: {rows}"
    );
    assert_eq!(statuses.len(), 3, "the refused read is audited too: {rows}");
    assert!(!rows.to_string().contains(SECRET), "audit leaked run input");

    // Reproduce the public-resume reporting gap on the same public MCP/CLI
    // boundary. The host fixture's failed source retains its actual commit;
    // public resume admission copies that checkpoint without rewriting it.
    use orbit_types::workflow::{JobRunState, JobRunStep, JobTargetType};
    let mut client = workspace.serve();
    client.call_tool_ok(
        "orbit_task_update",
        json!({"id":task_id, "plan":"Read deterministic delivery checkpoints.", "model":"codex"}),
    );
    client.call_tool_ok(
        "orbit_task_update",
        json!({"id":task_id, "status":"backlog", "model":"codex"}),
    );
    drop(client);
    let mut source = runtime.show_job_run_observed(RUN_ID).unwrap();
    source.state = JobRunState::Failed;
    runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&workspace_id, &source, None)
        .unwrap();
    let landing_index = shipped_step_index("task_pr_pipeline", "complete_pr");
    state.step_states.remove(&landing_index);
    state.step_outputs.remove(&landing_index);
    state
        .pipeline
        .as_object_mut()
        .unwrap()
        .remove("complete_pr");
    runtime.write_run_state(RUN_ID, &state).unwrap();
    let record = |run_id: &str, index: u32, id: &str, at| {
        runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_step_for_workspace(
                &workspace_id,
                run_id,
                &JobRunStep {
                    lock_wait_ms: None,
                    step_index: index,
                    target_type: JobTargetType::Activity,
                    target_id: id.into(),
                    started_at: Some(at),
                    finished_at: Some(at),
                    duration_ms: Some(0),
                    exit_code: Some(0),
                    agent_response_json: None,
                    state: JobRunState::Success,
                    error_code: None,
                    error_message: None,
                },
            )
            .unwrap();
    };
    record(
        RUN_ID,
        shipped_step_index("task_pr_pipeline", "commit"),
        "commit",
        source.created_at,
    );
    orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);
    let submitted = runtime
        .submit_resume_run(RUN_ID, Some("fixture"), None)
        .unwrap();
    runtime
        .wait_pipeline_runs(
            std::slice::from_ref(&submitted.run_id),
            10,
            1,
            Some("fixture"),
        )
        .unwrap();
    let mut resumed = runtime.show_job_run_observed(&submitted.run_id).unwrap();
    let mut inherited = runtime.read_run_state(&resumed.run_id).unwrap().unwrap();
    let completed = json!({"phase":"complete", "merge":{"merged":true,
        "pr_number":"4242", "landed_commit":LANDED}});
    inherited.record_step(
        landing_index,
        JobRunState::Success,
        Some(completed.clone()),
        None,
    );
    inherited.record_pipeline_output("complete_pr", completed);
    inherited.record_pipeline_output(
        "sync_base",
        json!({"head_before":HEAD, "head_after":"4".repeat(40)}),
    );
    inherited.compact_for_terminal(JobRunState::Success);
    runtime
        .write_run_state(&resumed.run_id, &inherited)
        .unwrap();
    // This is the first executed step of the resumed tail, not logical step 0.
    record(&resumed.run_id, 0, "complete_pr", resumed.created_at);
    resumed.state = JobRunState::Success;
    runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&workspace_id, &resumed, None)
        .unwrap();

    let mut client = workspace.serve();
    let original = delivery(&mut client, Some(RUN_ID));
    assert_eq!(original["delivery_status"], "committed");
    assert_eq!(original["landing"]["status"], "not_reached");
    let observed = delivery(&mut client, Some(&resumed.run_id));
    let default = delivery(&mut client, None);
    assert_eq!(default, observed);
    assert_eq!(observed["delivery_status"], "landed", "{observed}");
    assert_eq!(observed["commit"]["head_sha"], HEAD);
    assert_eq!(observed["commit"]["observed_at"], json!(source.created_at));
    assert_eq!(observed["commit"]["provenance"]["source_run_id"], RUN_ID);
    assert_eq!(
        observed["landing"]["provenance"]["source_run_id"],
        resumed.run_id
    );
    assert_eq!(
        observed["landing"]["provenance"]["step_index"],
        landing_index
    );
    assert_eq!(observed["landing"]["landed_commit"], LANDED);
    assert_eq!(runtime.read_run_state(RUN_ID).unwrap().unwrap(), state);
    assert!(
        !observed.to_string().contains(SECRET),
        "inherited observation leaked run input"
    );
    drop(client);
    let output = workspace.tool_ok(
        "orbit.task.show",
        json!({"id":task_id,"field":"delivery","run_id":resumed.run_id}),
    );
    let cli: Value = stdout_json(&output);
    assert_eq!(
        cli, observed,
        "CLI and MCP report the same authenticated resume"
    );
}

fn a_replica_mcp_session_enforces_checkout_capability_classes() {
    let workspace = McpWorkspace::init_replica_of("hm_remote_owner");
    let mut client = workspace.serve();

    for (name, arguments) in [
        (
            "orbit_friction_add",
            json!({ "body": "must not fork a replica friction", "model": "codex", }),
        ),
        (
            "orbit_search",
            json!({ "query": "must not search replica coordination", "model": "codex", }),
        ),
        ("orbit_auto_task_list", json!({})),
        (
            "orbit_friction_update",
            json!({ "id": "F2099-01-001", "status": "triaged" }),
        ),
    ] {
        let refused = client.call_tool_err(name, arguments);
        assert_eq!(refused["code"], "capability_refused", "{name}: {refused}");
        assert!(
            message_contains(&refused, "control_plane"),
            "{name} must name the refused class: {refused}"
        );
    }

    let listed: Value =
        stdout_json(&workspace.run_ok(&["friction", "list", "--status", "all", "--json"]));
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

    let listed = client.call_tool_ok("orbit_workspace_list", json!({ "include": ["crews"] }));
    let rows = array_of(&listed, "workspaces");
    assert!(
        !rows.is_empty()
            && rows
                .iter()
                .all(|row| row["crews"]["crews"].is_array() || row["crews_error"].is_string()),
        "unclassified discovery with crews must remain permitted: {listed}"
    );

    let marker = workspace.work.join("replica-command-exec-must-not-run");
    for (name, arguments) in [
        ("orbit_workflow_run_list", json!({})),
        (
            "orbit_command_exec",
            json!({ "argv": ["touch", utf8(&marker)], "working_directory": workspace.work, }),
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
    assert_run_list_empty(&mut operator);
}

/// [ORB-12564] A destination honors the authority in the argv it was started
/// with, whether the session arrived over SSH or locally.
///
/// This is the whole change: Orbit is a single-user tool, and anyone who can
/// run `ssh box "orbit mcp serve --operator"` can equally run
/// `ssh box "ORBIT_OPERATOR=1 orbit tool run …"`. A destination-side ceiling
/// stored in a file the caller can rewrite bought nothing and cost every
/// destination a per-caller setup.
fn a_remote_originated_operator_session_reaches_a_governed_tool() {
    let workspace = McpWorkspace::init();

    let mut client =
        workspace.serve_remote(&["--operator", "--remote-caller-machine-id", "hm_caller"]);

    assert_run_list_empty(&mut client);
}

/// The remote session's authority is exactly what its argv asked for, so a
/// caller whose own client held nothing is refused here too.
fn a_remote_originated_agent_session_is_refused_a_governed_tool() {
    let workspace = McpWorkspace::init();

    let mut client = workspace.serve_remote(&["--remote-caller-machine-id", "hm_caller"]);

    assert_run_list_denied(&mut client);
}

/// [ORB-12564] `--remote-caller-machine-id` survives as attribution: it names
/// the machine that reached in, in the destination's own authorization rows,
/// while contributing nothing to the decision.
fn the_forwarded_caller_label_reaches_the_destinations_authorization_audit_row() {
    let workspace = McpWorkspace::init();

    let mut client = workspace.serve_remote(&["--remote-caller-machine-id", "hm_caller"]);
    assert_run_list_denied(&mut client);
    drop(client);

    let arguments = last_authorization_arguments(&workspace);
    assert!(
        arguments.contains("hm_caller"),
        "the forwarded label must be recorded as attribution: {arguments}"
    );
}

/// [ORB-12564] A destination upgraded from the retired model keeps its old
/// files. They must be ignored — named once, never enforced, and never a
/// reason to refuse a session.
fn leftover_caller_authorization_files_are_ignored_rather_than_enforced() {
    let workspace = McpWorkspace::init();
    let orbit_home = workspace.home.join(".orbit");
    std::fs::create_dir_all(&orbit_home).expect("global orbit root");
    // Deliberately unparseable, and deliberately a `deny` shape: neither the
    // defect nor the denial may reach a session.
    std::fs::write(
        orbit_home.join("mcp-callers.toml"),
        "default = \"deny\"\nthis is not valid toml = [",
    )
    .expect("leftover callers file");
    std::fs::create_dir_all(orbit_home.join("mcp-ssh-acceptance"))
        .expect("leftover acceptance dir");

    let mut client =
        workspace.serve_remote(&["--operator", "--remote-caller-machine-id", "hm_caller"]);

    assert_run_list_empty(&mut client);
    drop(client);

    // And `orbit doctor` is where the operator is told to delete them.
    let output = workspace.run_ok(&["doctor", "--format", "json"]);
    let rendered = format!("{}{}", stdout(&output), stderr(&output));
    assert!(
        rendered.contains("mcp-callers.toml") && rendered.contains("ignored"),
        "doctor must suggest deleting the leftover: {rendered}"
    );
}

/// [ORB-12564] The one remote flow with live users: `orbit_agent_invoke` from a
/// federated client on another machine. It crosses the SSH-originated session,
/// real MCP transport, workspace routing, durable run submission, worker, and
/// response projection — with argv-propagated operator and no destination-side
/// file anywhere in the path.
fn a_remote_operator_session_invokes_an_agent_end_to_end() {
    let workspace = McpWorkspace::init();
    std::fs::write(workspace.work.join(".gitignore"), ".orbit/\n").unwrap();
    crate::git_repo::run(&workspace.work, &["add", ".gitignore"]);
    crate::git_repo::run(
        &workspace.work,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "seed",
        ],
    );
    let linked = workspace.work.join(".orbit/state/worktrees/invoke");
    crate::git_repo::run(
        &workspace.work,
        &["worktree", "add", "--detach", linked.to_str().unwrap()],
    );
    let linked = linked.canonicalize().unwrap();
    let observed_cwd = workspace.home.join("provider-cwd");
    // `model` attributes the caller; the workspace's frozen default crew picks
    // the provider. Stub both possible selected providers, because an inherited
    // host `claude` binary makes `opus` the default crew during fixture init.
    plant_both_providers(
        &workspace,
        &format!(
            "#!/bin/sh\ncat > /dev/null\n/bin/pwd -P > {}\nprintf '%s\\n' '{AGENT_ANSWER_ENVELOPE}'\n",
            orbit_common::process::shell::quote_posix_arg(observed_cwd.to_str().unwrap())
        ),
    );
    admit_fixture_claude_worker_token(&workspace);

    let mut client = workspace.serve_with_args_and_env(
        &["--operator", "--remote-caller-machine-id", "hm_caller"],
        &[
            SSH_ENV,
            (FIXTURE_CLAUDE_WORKER_TOKEN_ENV, FIXTURE_CLAUDE_WORKER_TOKEN),
        ],
    );

    let submitted = client.call_tool_ok(
        "orbit_agent_invoke",
        json!({
            "prompt": "read-only remote probe",
            "cwd": linked,
            "timeout_seconds": 30,
            "wait_seconds": 30,
            "model": "codex",
        }),
    );
    assert_eq!(submitted["authorized_by"], "hm_caller");
    assert_eq!(submitted["authorizer_provenance"], "session");
    assert_eq!(submitted["caller_machine_id"], "hm_caller");
    assert_eq!(submitted["workspace_path"], linked.display().to_string());
    let run_id = submitted["run_id"].as_str().expect("run id").to_string();

    let terminal = wait_for_run(&mut client, &run_id);
    assert_eq!(terminal["state"], "success", "{terminal}");
    assert_eq!(submitted["state"], "success", "{submitted}");
    assert_eq!(submitted["waited"], true);
    assert_eq!(submitted["agent_invocation"]["completed_envelope"], true);
    assert_eq!(terminal["agent_invocation"]["completed_envelope"], true);
    assert_eq!(
        std::fs::read_to_string(&observed_cwd).unwrap().trim(),
        linked.to_str().unwrap()
    );
    assert_eq!(
        crate::git_repo::run(&workspace.work, &["status", "--porcelain"]),
        ""
    );

    // The full run response keeps the parsed result fields and blob refs while
    // dropping the duplicate raw final message and captured-stream preview.
    let answer = &terminal["agent_invocation"]["answer"];
    assert_eq!(answer["summary"], "remote probe complete", "{terminal}");
    assert_eq!(answer["findings"], json!(["the clock restarts on reload"]));
    assert_eq!(answer["next_steps"], json!(["pin the reload interval"]));
    assert_eq!(
        answer["extra"],
        json!({ "report_markdown": "# Probe\n\nAll clear." }),
        "only the agent's own extra fields, never Orbit's metadata"
    );
    assert!(answer.get("final_message").is_none(), "{terminal}");
    assert!(
        terminal["agent_invocation"].get("preview").is_none(),
        "{terminal}"
    );
    assert!(terminal["agent_invocation"]["stdout_blob_ref"].is_string());

    let bounded = client.call_tool_ok(
        "orbit_workflow_run_show",
        json!({ "workspace": "ws_mcp-roundtrip", "view": "bounded", "id": run_id }),
    );
    let bounded_invocation = &bounded["run"]["agent_invocation"];
    assert_eq!(bounded_invocation["outcome"], "success", "{bounded}");
    assert_eq!(
        bounded_invocation["failure_reason"],
        Value::Null,
        "{bounded}"
    );
    assert_eq!(
        bounded_invocation["answer"]["summary"],
        "remote probe complete"
    );
    assert_eq!(bounded_invocation["answer"]["findings"], answer["findings"]);
    assert_eq!(
        bounded_invocation["answer"]["next_steps"],
        answer["next_steps"]
    );
    assert_eq!(bounded_invocation["answer"]["extra"], answer["extra"]);
    assert_eq!(bounded_invocation["answer"], *answer);
    assert_eq!(bounded_invocation["answer_truncated"], false);
    assert_eq!(bounded_invocation["answer_omitted_fields"], json!([]));

    // The CLI reads the same answer, as JSON and as text.
    let shown = workspace.run_ok(&["run", "show", &run_id, "--json"]);
    let shown: Value = stdout_json(&shown);
    assert_eq!(
        shown["run"]["agent_invocation"]["answer"]["summary"],
        answer["summary"]
    );
    assert_eq!(
        shown["run"]["agent_invocation"]["answer"]["final_message"],
        AGENT_ANSWER_ENVELOPE
    );
    let text = workspace.run_ok(&["run", "show", &run_id]);
    let text = stdout(&text);
    for expected in [
        "summary: remote probe complete",
        "findings:\n    - the clock restarts on reload",
        "next_steps:\n    - pin the reload interval",
        "report_markdown:\n    # Probe\n    \n    All clear.",
        "final message: {\"schemaVersion\":1",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }

    let connection = workspace.store();
    let input_json: String = connection
        .query_row(
            "SELECT input_json FROM job_runs WHERE run_id = ?1",
            [&run_id],
            |row| row.get(0),
        )
        .expect("persisted invocation input");
    let input: Value = serde_json::from_str(&input_json).expect("parse invocation input");
    assert_eq!(
        input["trusted_host_admission"]["caller_machine_id"],
        "hm_caller"
    );
    assert_eq!(input["repo_root"], linked.to_str().unwrap());
    assert_eq!(input["workspace_path"], linked.to_str().unwrap());
    assert_eq!(
        input["trusted_host_admission"]["workspace_path"],
        linked.to_str().unwrap()
    );
}

/// Compact reads bound the serialized answer even when the provider capture
/// admits much more, while the full MCP read retains every structured value.
#[test]
fn compact_agent_answers_have_a_serialized_budget() {
    let workspace = McpWorkspace::init();
    let response_file = workspace.home.join("oversized-answer.json");
    plant_both_providers(
        &workspace,
        &format!(
            "#!/bin/sh\ncat > /dev/null\ncat {}\n",
            orbit_common::process::shell::quote_posix_arg(response_file.to_str().unwrap())
        ),
    );
    admit_fixture_claude_worker_token(&workspace);
    let mut client = workspace.serve_with_args_and_env(
        &["--operator"],
        &[
            (FIXTURE_CLAUDE_WORKER_TOKEN_ENV, FIXTURE_CLAUDE_WORKER_TOKEN),
            ("ORBIT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES", "2097152"),
        ],
    );
    let large_text = "界🙂\n\"\\".repeat(24_000);
    for result in [
        json!({"summary": "large report", "report_markdown": large_text}),
        json!({"summary": "nested values", "nested": {"reports": [{"text": large_text}]}}),
        json!({
            "summary": "large collections",
            "findings": vec![json!({"detail": "界🙂\n\"\\".repeat(20)}); 600],
            "next_steps": vec![json!({"action": "界🙂\n\"\\".repeat(20)}); 600],
        }),
        json!({"summary": "界🙂\n\"\\\u{0001}".repeat(2000)}),
    ] {
        let envelope =
            json!({"schemaVersion": 1, "status": "success", "result": result, "error": null});
        std::fs::write(&response_file, serde_json::to_vec(&envelope).unwrap()).unwrap();
        let submitted = client.call_tool_ok(
            "orbit_agent_invoke",
            json!({"prompt": "oversized answer probe", "cwd": workspace.work,
                "timeout_seconds": 30, "wait_seconds": 30, "model": "codex"}),
        );
        assert_eq!(submitted["state"], "success", "{submitted}");
        let run_id = submitted["run_id"].as_str().unwrap();
        let mut previous = None;
        for limit in [1, 50] {
            let bounded = client.call_tool_ok(
                "orbit_workflow_run_show",
                json!({"workspace": "ws_mcp-roundtrip", "view": "bounded", "id": run_id, "limit": limit}),
            );
            let invocation = &bounded["run"]["agent_invocation"];
            let answer = &invocation["answer"];
            let bytes = serde_json::to_vec(answer).unwrap();
            assert!(
                bytes.len() <= 4096,
                "compact answer returned {} bytes",
                bytes.len()
            );
            assert_eq!(invocation["answer_max_bytes"], 4096);
            assert_eq!(invocation["answer_truncated"], true);
            assert!(invocation.get("preview").is_none());
            assert!(answer.get("final_message").is_none());
            assert_eq!(invocation["summary"], answer["summary"]);
            let omitted = invocation["answer_omitted_fields"].as_array().unwrap();
            if result.get("report_markdown").is_some() || result.get("nested").is_some() {
                assert!(omitted.contains(&json!("extra")));
                assert!(answer.get("extra").is_none());
            } else if result.get("findings").is_some() {
                for field in ["findings", "next_steps"] {
                    assert!(omitted.contains(&json!(field)));
                    assert!(answer.get(field).is_none());
                }
            } else {
                assert_ne!(answer["summary"], result["summary"]);
            }
            let text = std::str::from_utf8(&bytes).expect("UTF-8 compact answer");
            assert_eq!(serde_json::from_str::<Value>(text).unwrap(), *answer);
            if let Some(summary) = answer["summary"].as_str() {
                assert!(result["summary"].as_str().unwrap().starts_with(summary));
                assert!(!summary.is_empty(), "keep a useful summary excerpt");
            }
            if let Some(previous) = &previous {
                assert_eq!(
                    answer, previous,
                    "log page length must not alter the answer budget"
                );
            }
            previous = Some(answer.clone());
            let retrieval = &invocation["answer_retrieval"];
            assert_eq!(retrieval["tool"], "orbit.workflow.run.show");
            assert!(retrieval["arguments"].get("view").is_none());
            let full =
                client.call_tool_ok("orbit_workflow_run_show", retrieval["arguments"].clone());
            let full_answer = &full["agent_invocation"]["answer"];
            assert_eq!(full_answer["summary"], result["summary"]);
            for field in ["findings", "next_steps"] {
                assert_eq!(
                    full_answer[field],
                    result.get(field).cloned().unwrap_or(json!([]))
                );
            }
            let mut extra = result.as_object().unwrap().clone();
            for field in ["summary", "findings", "next_steps"] {
                extra.remove(field);
            }
            assert_eq!(full_answer["extra"], Value::Object(extra));
        }
    }
}

/// [ORB-13899] An invocation that exits 0 without a response envelope did not
/// answer. It fails with a reason naming the envelope, and its raw output
/// stays reachable although a failed step checkpoints nothing.
fn an_invocation_without_a_response_envelope_fails_with_its_reason() {
    let workspace = McpWorkspace::init();
    plant_both_providers(
        &workspace,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' 'I looked around and everything seems fine.'\n",
    );
    admit_fixture_claude_worker_token(&workspace);
    let mut client = workspace.serve_with_args_and_env(
        &["--operator"],
        &[(FIXTURE_CLAUDE_WORKER_TOKEN_ENV, FIXTURE_CLAUDE_WORKER_TOKEN)],
    );
    let submitted = client.call_tool_ok(
        "orbit_agent_invoke",
        json!({
            "prompt": "probe without answering",
            "cwd": workspace.work,
            "timeout_seconds": 30,
            "model": "codex",
        }),
    );
    let run_id = submitted["run_id"].as_str().expect("run id").to_string();

    let terminal = wait_for_run(&mut client, &run_id);
    let invocation = &terminal["agent_invocation"];
    assert_eq!(terminal["state"], "failed", "{terminal}");
    let reason = invocation["failure_reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("response envelope"),
        "the reason must name the missing envelope: {terminal}"
    );
    assert!(
        invocation["stdout_blob_ref"].as_str().is_some(),
        "the raw output must stay reachable: {terminal}"
    );
    assert_eq!(invocation["answer"], Value::Null, "{terminal}");

    let bounded = client.call_tool_ok(
        "orbit_workflow_run_show",
        json!({ "workspace": "ws_mcp-roundtrip", "view": "bounded", "id": run_id }),
    );
    let bounded_invocation = &bounded["run"]["agent_invocation"];
    assert_eq!(bounded_invocation["outcome"], "failed", "{bounded}");
    assert!(
        bounded_invocation["failure_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("response envelope")),
        "failed bounded invocation keeps its reason: {bounded}"
    );
    assert!(
        bounded_invocation["stdout_blob_ref"].as_str().is_some(),
        "failed bounded invocation keeps the output reference: {bounded}"
    );

    let logs = workspace.run_ok(&["run", "logs", &run_id]);
    assert!(
        stdout(&logs).contains("everything seems fine"),
        "`orbit run logs` must print the raw output"
    );
}

/// A remote session the caller's own client served as an agent is refused the
/// unsandboxed mode, exactly as a local agent is.
fn a_remote_agent_session_cannot_invoke_an_agent() {
    let workspace = McpWorkspace::init();

    let mut client = workspace.serve_remote(&["--remote-caller-machine-id", "hm_caller"]);

    let denied = client.call_tool_err(
        "orbit_agent_invoke",
        json!({ "prompt": "must not run", "cwd": workspace.work, "timeout_seconds": 30, }),
    );
    assert_eq!(denied["code"], "capability_denied", "{denied}");
}

/// [ORB-12564] A local session's authority resolution is unchanged.
fn a_local_session_keeps_its_argv_authority() {
    let workspace = McpWorkspace::init();

    let mut client = workspace.serve_with_args(&["--operator"]);

    assert_run_list_empty(&mut client);
}

/// The `arguments_json` of the most recent authorization row for the governed
/// workflow-list tool.
fn last_authorization_arguments(workspace: &McpWorkspace) -> String {
    let connection = workspace.store();
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

/// Initialization's tracked edits still block local setup, while its unrelated
/// untracked MCP files are omitted from the refusal. PR setup accepts both.
#[test]
fn fresh_workspace_init_mcp_explains_local_ship_and_allows_pr_worktree_setup() {
    for mode in ["local", "pr"] {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        let remote = temp.path().join("remote.git");
        std::fs::create_dir_all(&home).expect("create home");
        std::fs::create_dir_all(&work).expect("create work");
        plant_agent_cli_stub(&McpWorkspace::stub_bin_dir(&home), "codex");

        let git = |cwd: &Path, args: &[&str]| {
            let output = crate::git_repo::command()
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("run git fixture command");
            assert_command_succeeded(&format!("git {}", args.join(" ")), &output);
        };
        git(&work, &["init", "--quiet", "-b", "main"]);
        git(&work, &["config", "user.name", "Orbit Test"]);
        git(
            &work,
            &["config", "user.email", "orbit-test@example.invalid"],
        );
        std::fs::write(work.join("README.md"), "first commit\n").expect("write first commit");
        std::fs::write(work.join(".gitignore"), "").expect("track ignore file before init");
        git(&work, &["add", "README.md", ".gitignore"]);
        git(&work, &["commit", "--quiet", "-m", "first commit"]);
        git(
            temp.path(),
            &["init", "--quiet", "--bare", remote.to_str().unwrap()],
        );
        git(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&work, &["push", "--quiet", "-u", "origin", "main"]);
        if mode == "pr" {
            // PR admission needs a remote on a forge host. Delivery still
            // fetches and pushes through the local `origin`; this one is
            // never contacted.
            git(
                &work,
                &[
                    "remote",
                    "add",
                    "upstream",
                    "https://github.com/orbit-test/fresh-host.git",
                ],
            );
        }

        cli_ok(
            &work,
            &home,
            &[
                "init",
                "--non-interactive",
                "--machine-name",
                "fresh-host",
                "--task-prefix",
                "FRH",
            ],
        );
        std::fs::create_dir_all(work.join(".claude")).expect("mark Claude installed");
        let initialized = cli_ok(
            &work,
            &home,
            &["workspace", "init", "--mcp", "--ship-mode", mode],
        );
        let human_report = String::from_utf8(initialized.stdout).expect("init report is UTF-8");
        for path in [".claude/settings.json", ".gitignore", ".mcp.json"] {
            assert!(
                human_report.contains(path),
                "init omitted {path}: {human_report}"
            );
        }
        assert!(human_report.contains("commit"), "{human_report}");
        let status = crate::git_repo::command()
            .args(["status", "--porcelain", "--untracked-files=all"])
            .current_dir(&work)
            .output()
            .expect("read fresh checkout status");
        assert_command_succeeded("git status", &status);
        let status = String::from_utf8(status.stdout).expect("Git status is UTF-8");
        for entry in status.lines() {
            let path = &entry[3..];
            assert!(
                human_report.contains(path),
                "init omitted {path}: {human_report}"
            );
        }
        for path in [".claude/settings.json", ".gitignore", ".mcp.json"] {
            assert!(work.join(path).is_file(), "init did not write {path}");
        }

        let added = cli_ok(
            &work,
            &home,
            &[
                "task",
                "add",
                "--title",
                "First task",
                "--complexity",
                "low",
                "--context",
                "file:README.md",
                "--crew",
                "sol",
                "--status",
                "backlog",
                "--json",
            ],
        );
        let task: Value = stdout_json(&added);
        let task_id = task["id"].as_str().expect("task id");
        let shipped = cli_ok(&work, &home, &["run", "ship", task_id, "--json"]);
        let submission: Value = stdout_json(&shipped);
        let parent_run_id = submission["run_id"].as_str().expect("run id");

        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        let leaf_run = loop {
            let history = cli_ok(
                &work,
                &home,
                &[
                    "run",
                    "history",
                    "-j",
                    &format!("task_{mode}_pipeline"),
                    "--no-reconcile",
                    "--json",
                ],
            );
            let history: Value = stdout_json(&history);
            let Some(run_id) = history["runs"][0]["run_id"].as_str() else {
                assert!(
                    Instant::now() < deadline,
                    "ship {parent_run_id} did not dispatch: {history}"
                );
                std::thread::sleep(Duration::from_millis(100));
                continue;
            };
            let shown = cli_ok(
                &work,
                &home,
                &["run", "show", run_id, "--no-reconcile", "--json"],
            );
            let run: Value = stdout_json(&shown);
            if matches!(
                run["run"]["state"].as_str(),
                Some("success" | "failed" | "timeout")
            ) {
                break run;
            }
            assert!(
                Instant::now() < deadline,
                "worktree setup did not finish: {run}"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        if mode == "local" {
            assert_eq!(leaf_run["run"]["state"], "failed", "{leaf_run}");
            let error = leaf_run["run"]["error_message"]
                .as_str()
                .expect("local refusal");
            assert!(
                error.contains("worktree_setup")
                    && error.contains(".gitignore")
                    && error.contains("Commit or stash"),
                "{error}"
            );
            assert!(
                !error.contains(".claude/settings.json") && !error.contains(".mcp.json"),
                "unrelated untracked init files do not block local delivery: {error}"
            );
        } else {
            assert!(
                leaf_run["pipeline_state"]["step_states"]["0"] == "success",
                "PR mode must complete worktree setup despite init files: {leaf_run}"
            );
        }
    }
}

fn mcp_serve_lists_the_canonical_surface_outside_any_checkout() {
    let workspace = McpWorkspace::init();
    let scratch = workspace.home.join("scratch");
    std::fs::create_dir_all(&scratch).expect("create non-workspace launch dir");
    let mut client = serve_mcp_from(
        &scratch,
        &workspace.home,
        McpClient::initialize_params("outside-checkout", None),
    );

    let listed = client.tools_list();
    let names = tool_names(&listed);
    assert!(names.contains(&"orbit_task_show"));

    // `task show` needs no selector — it follows the ID — so a server outside
    // any checkout reports the ID as unknown rather than as unaddressable.
    let missing = client.call_tool_err("orbit_task_show", json!({ "id": "ORB-00001" }));
    assert!(
        message_contains(&missing, "ORB-00001"),
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
        scoped.get("tasks").is_some(),
        "an explicit selector must route an unbound session: {scoped}"
    );
    drop(client);

    let connection = workspace.store();
    let (count, audited) = audit_tuple(
        &connection,
        "SELECT COUNT(*), status, trace_id, caller_machine_id, process_machine_id
         FROM audit_events WHERE tool_name = 'orbit.task.show'",
        4,
    );
    assert_eq!(count, 1, "recognized setup failure is audited once");
    assert_eq!(audited[0].as_deref(), Some("failure"));
    assert!(
        audited[1]
            .as_deref()
            .is_some_and(|id| id.starts_with("trace-"))
    );
    assert_eq!(audited[2], audited[3]);
}

/// [ORB-12263] `orbit.task.artifact.get`'s schema advertises the identical
/// "resolved globally by default" wording `orbit.task.show` does, and
/// ORB-12254 made the MCP surface honor it, but left the CLI surfaces
/// (`orbit tool run` and `orbit task artifact get`) resolving `id`
/// cwd-locally and reporting the mismatch as `task_not_found`. Modelled on
/// `task_show_is_global_by_default_across_tool_run_and_mcp`: a task owned by
/// a sibling checkout B must still be readable by ID alone from checkout A on
/// every surface, and an explicit foreign `workspace` selector must stay a
/// fail-closed filter on all three. This must fail if the CLI routing change
/// in `command/operation/registry.rs` is reverted.
fn task_artifact_get_is_global_by_default_across_tool_run_task_cli_and_mcp() {
    let workspace = McpWorkspace::init();

    let checkout_b = workspace.home.join("checkout-b");
    init_checkout(&workspace.home, &checkout_b, "checkout-b");

    let checkout_b_selector = utf8(&checkout_b);
    let created = stdout_json(&tool_ok_at(
        &checkout_b,
        &workspace.home,
        "orbit.task.add",
        json!({
            "title": "Owned by checkout B, read from checkout A",
            "description": "The artifact payload is addressed by task ID alone",
            "workspace": checkout_b_selector,
            "complexity": "low",
            "model": "codex",
        }),
    ));
    let task_id = id_of(&created);

    std::fs::write(
        checkout_b.join("qa-note.md"),
        "globally addressable payload",
    )
    .expect("write the artifact source");
    tool_ok_at(
        &checkout_b,
        &workspace.home,
        "orbit.task.artifact.put",
        json!({
            "id": task_id,
            "source_path": "qa-note.md",
            "path": "qa/note.md",
            "workspace": checkout_b_selector,
            "model": "codex",
        }),
    );

    // Checkout A (`workspace.work`) does not own the task. An id-only read
    // must follow the id past cwd on every surface.
    let checkout_a_selector = utf8(&workspace.work);
    let via_tool_run: Value = stdout_json(&workspace.tool_ok(
        "orbit.task.artifact.get",
        json!({ "id": task_id, "path": "qa/note.md" }),
    ));
    assert_eq!(via_tool_run["content"], "globally addressable payload");

    let output = workspace.run_ok(&["task", "artifact", "get", &task_id, "qa/note.md"]);
    assert!(
        stdout(&output).contains("globally addressable payload"),
        "`orbit task artifact get` must print the artifact content: {}",
        stdout(&output)
    );

    // The MCP session announces checkout A at initialize and must still
    // follow the task id, matching ORB-12254.
    let mut client = workspace.serve();
    let read = client.call_tool_ok(
        "orbit_task_artifact_get",
        json!({ "id": task_id, "path": "qa/note.md" }),
    );
    assert_eq!(read["content"], "globally addressable payload");
    drop(client);

    // An explicit foreign `workspace` selector stays a fail-closed filter on
    // every surface: checkout A does not own the task.
    let foreign_input =
        json!({ "id": task_id, "path": "qa/note.md", "workspace": checkout_a_selector })
            .to_string();
    for (args, why) in [
        (
            &[
                "tool",
                "run",
                "orbit.task.artifact.get",
                "--input",
                &foreign_input,
            ] as &[&str],
            "an explicit foreign `workspace` input must filter rather than follow the id",
        ),
        (
            &[
                "--workspace",
                checkout_a_selector,
                "task",
                "artifact",
                "get",
                &task_id,
                "qa/note.md",
            ],
            "an explicit foreign `--workspace` must filter rather than follow the id",
        ),
    ] {
        let missed = workspace.run(args);
        assert!(!missed.status.success(), "{why}");
        let missed_stderr = stderr(&missed);
        assert!(
            missed_stderr.contains(&task_id),
            "the not-found error must name the task id: {missed_stderr}"
        );
    }
}

/// [ORB-17648] A missing artifact path refuses with the listing remedy in the
/// caller's own surface syntax: the CLI names `orbit task show --fields
/// artifacts`, while MCP keeps the `orbit.task.show` tool-call spelling.
#[test]
fn missing_artifact_names_the_listing_in_each_surface_syntax() {
    let workspace = McpWorkspace::init();
    let task_id = author_task(&workspace, "Surface-specific artifact hint");

    let missed = workspace.run(&["task", "artifact", "get", &task_id, "nope.txt"]);
    assert!(
        !missed.status.success(),
        "a missing artifact must refuse on the CLI"
    );
    let stderr = stderr(&missed);
    assert!(
        stderr.contains("orbit task show") && stderr.contains("--fields artifacts"),
        "the CLI refusal must name the CLI listing command: {stderr}"
    );
    assert!(
        !stderr.contains("orbit.task.show"),
        "the CLI refusal must not name the MCP tool: {stderr}"
    );

    let mut client = workspace.serve();
    let refused = client.call_tool_err(
        "orbit_task_artifact_get",
        json!({ "id": task_id, "path": "nope.txt" }),
    );
    let message = refused["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("orbit.task.show") && message.contains("field: \"artifacts\""),
        "the MCP refusal must keep the MCP listing spelling: {refused}"
    );

    // [ORB-17784] MCP resolves the task before the artifact, so a missing task
    // and a missing artifact stay distinct refusals.
    let prefix = task_id.split_once('-').map_or("ORB", |(prefix, _)| prefix);
    let unknown = format!("{prefix}-99999");
    let missing_task = client.call_tool_err(
        "orbit_task_artifact_get",
        json!({ "id": unknown, "path": "nope.txt" }),
    );
    assert_eq!(
        missing_task,
        json!({ "code": "not_found", "message": format!("task not found: {unknown}") }),
        "a missing task is not reported as a missing artifact"
    );
    assert_eq!(refused["code"], "not_found", "{refused}");
    assert!(
        message.starts_with(&format!("task artifact not found: {task_id}/nope.txt")),
        "a missing artifact names the artifact: {refused}"
    );
}

/// [ORB-12254] Regression guard for the class of bug this task fixed: a tool
/// whose advertised schema leaves `workspace` optional in an unbound session
/// must actually resolve without one, and a tool that requires it must still
/// fail closed. Walking the full canonical MCP surface keeps a future
/// addition from drifting the same way `orbit.task.artifact.get` did. The
/// id-routed task tools resolve an id-only call through the host task
/// registry [ORB-14449], so their selector is optional here.
fn every_workspace_scoped_tool_behavior_matches_its_advertised_selector_requirement() {
    let workspace = McpWorkspace::init();
    let scratch = workspace.home.join("scratch");
    std::fs::create_dir_all(&scratch).expect("create non-workspace launch dir");
    let mut client = serve_mcp_from(
        &scratch,
        &workspace.home,
        McpClient::initialize_params("selector-wording-audit", None),
    );
    let listed = client.tools_list();
    let advertised = tools_array(&listed);

    let definitions = orbit_mcp::canonical_mcp_tool_definitions()
        .expect("canonical MCP tool definitions must build");

    // Global-scope tools never go through the session-selector gate at all;
    // only `WorkspaceRequired` tools carry the selector contract this test
    // checks.
    for definition in definitions
        .iter()
        .filter(|definition| definition.scope == orbit_types::tool::McpToolScope::WorkspaceRequired)
    {
        let name = definition.schema.name.as_str();
        let advertised_name = orbit_types::tool::mcp_advertised_tool_name(name);
        let requires_selector = advertised
            .iter()
            .find(|tool| tool["name"] == json!(advertised_name))
            .and_then(|tool| tool["inputSchema"]["required"].as_array())
            .is_some_and(|required| required.contains(&json!("workspace")));

        let result = client.call_tool(&advertised_name, json!({}));
        let message = result
            .get("structuredContent")
            .and_then(|content| content.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let refused_for_missing_selector = result["isError"] == true
            && message.contains("requires an explicit workspace selector");

        assert_eq!(
            refused_for_missing_selector, requires_selector,
            "{name}: the unbound schema requires workspace={requires_selector} but the \
             session's behavior disagrees (refused_for_missing_selector={refused_for_missing_selector}): {message}"
        );
    }
}

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
        .spawn_piped();
    let (mut client, initialized) = McpClient::initialized(
        child,
        McpClient::initialize_params("registry-clean-launch", None),
    );
    assert_eq!(initialized["result"]["serverInfo"]["name"], "orbit-mcp");

    let listed = client.tools_list();
    assert!(
        tool_names(&listed).contains(&"orbit_workspace_list"),
        "the first routing tool must be available from a clean launch: {listed}"
    );
    let tools = tools_array(&listed);
    let task_list = find_tool(tools, "orbit_task_list");
    assert!(
        task_list["inputSchema"]["required"]
            .as_array()
            .is_some_and(|required| required.contains(&json!("workspace")))
    );
    let task_show = find_tool(tools, "orbit_task_show");
    assert!(
        !task_show["inputSchema"]["required"]
            .as_array()
            .is_some_and(|required| required.contains(&json!("workspace")))
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

fn ssh_marked_mcp_server_audits_caller_and_server_identity_separately() {
    let workspace = McpWorkspace::init();
    let scratch = workspace.home.join("server-scratch");
    std::fs::create_dir_all(&scratch).expect("create server launch dir");
    let child = McpWorkspace::orbit_command(&scratch, &workspace.home)
        .args(["mcp", "serve", "--remote-caller-machine-id", "hm_caller"])
        .env(SSH_ENV.0, SSH_ENV.1)
        .spawn_piped();
    let (mut client, initialized) = McpClient::initialized(
        child,
        McpClient::initialize_params("remote-roundtrip", Some("ws_mcp-roundtrip")),
    );
    assert_eq!(initialized["result"]["serverInfo"]["name"], "orbit-mcp");

    let listed = client.tools_list();
    let names = tool_names(&listed);
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

    let connection = workspace.store();
    let (count, audit) = audit_tuple(
        &connection,
        "SELECT COUNT(*), workspace_id, caller_machine_id, process_machine_id, transport, trace_id, caller_ip
         FROM audit_events WHERE tool_name = 'orbit.task.add'",
        6,
    );
    assert_eq!(count, 1, "one audit row per accepted call");
    assert_eq!(audit[0].as_deref(), Some("ws_mcp-roundtrip"));
    assert_eq!(audit[1].as_deref(), Some("hm_caller"));
    assert!(audit[2].as_deref().is_some_and(|id| id.starts_with("hm_")));
    assert_ne!(
        audit[1], audit[2],
        "caller and server process stay distinct"
    );
    assert_eq!(audit[3].as_deref(), Some("ssh-mcp"));
    assert!(
        audit[4]
            .as_deref()
            .is_some_and(|id| id.starts_with("trace-"))
    );
    assert_eq!(audit[5].as_deref(), Some("192.0.2.8"));

    let (ws_count, workspace_audit) = audit_tuple(
        &connection,
        "SELECT COUNT(*), workspace_id, caller_machine_id, process_machine_id, process_machine_name, transport, trace_id, caller_ip
         FROM audit_events WHERE tool_name = 'orbit.workspace.list'",
        7,
    );
    assert_eq!(
        ws_count, 1,
        "workspace discovery records exactly one MCP audit row"
    );
    assert_eq!(workspace_audit[0], None, "global call has no workspace id");
    assert_eq!(workspace_audit[1].as_deref(), Some("hm_caller"));
    assert!(
        workspace_audit[2]
            .as_deref()
            .is_some_and(|id| id.starts_with("hm_"))
    );
    assert!(workspace_audit[3].as_deref().is_some());
    assert_eq!(workspace_audit[4].as_deref(), Some("ssh-mcp"));
    assert!(
        workspace_audit[5]
            .as_deref()
            .is_some_and(|id| id.starts_with("trace-"))
    );
    assert_eq!(workspace_audit[6].as_deref(), Some("192.0.2.8"));
}

/// The TCP listener through the production binary: same tool surface, same
/// single Core dispatch and audit boundary, plus the accepted peer's IP.
fn mcp_listen_round_trips_over_a_loopback_socket_and_audits_the_peer_ip() {
    let workspace = McpWorkspace::init();
    let (mut client, addr) = workspace.listen();

    let listed = client.tools_list();
    let names: BTreeSet<_> = tool_names(&listed).into_iter().collect();
    assert!(
        names.contains("orbit_task_add"),
        "listener serves the same surface as stdio: {names:?}"
    );

    let created = client.add_task_full("Listener round trip", "Created over the MCP TCP listener");
    assert_eq!(created["title"], "Listener round trip");
    let task_id = id_of(&created);
    assert_eq!(client.show_task(&task_id)["title"], "Listener round trip");

    // Dropping the client kills the server, which closes the listening socket.
    drop(client);
    assert!(
        TcpStream::connect(addr).is_err(),
        "the listening socket must be gone once the server exits"
    );

    let connection = workspace.store();
    let (count, audit) = audit_tuple(
        &connection,
        "SELECT COUNT(*), status, transport, caller_ip, trace_id
         FROM audit_events WHERE tool_name = 'orbit.task.add'",
        4,
    );
    assert_eq!(count, 1, "one audit row per accepted call");
    assert_eq!(audit[0].as_deref(), Some("success"));
    assert_eq!(
        audit[1].as_deref(),
        Some("local"),
        "a listener session is served by the same local process as stdio"
    );
    assert_eq!(
        audit[2].as_deref(),
        Some("127.0.0.1"),
        "the accepted peer's IP is persisted through the audit context"
    );
    assert!(
        audit[3]
            .as_deref()
            .is_some_and(|id| id.starts_with("trace-"))
    );
}

/// `orbit mcp listen --workspace <selector>` binds the sessions it accepts the
/// way `orbit mcp serve --workspace` does, instead of accepting the flag and
/// leaving every session unbound.
fn mcp_listen_workspace_flag_binds_the_sessions_it_accepts() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.listen_bound_to("ws_mcp-roundtrip");

    assert_selector_optional(&mut client, "orbit_task_list");

    let created = client.add_task_full("Bound listener task", "Created with no per-call workspace");
    assert_eq!(created["title"], "Bound listener task");
    let tasks = client.call_tool_ok("orbit_task_list", json!({ "limit": 5 }));
    assert!(
        array_of(&tasks, "tasks")
            .iter()
            .any(|task| task["title"] == "Bound listener task"),
        "a call with no selector must land in the launch workspace: {tasks}"
    );
}

fn mcp_serve_round_trips_records_against_a_temp_workspace() {
    let (workspace, mut client) = McpWorkspace::served();

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
            "orchestrator": "sol",
            "relations": [{"type": "related_to", "target": "DK-00042"}],
        }),
    );
    let task_id = id_of(&created);
    assert_eq!(created["title"], "MCP round-trip task");
    assert_eq!(created["status"], "proposed");
    client.call_tool_ok(
        "orbit_task_update",
        json!({"id": task_id, "job_run_id": "jrun-mcp-projection"}),
    );

    let shown = client.show_task(&task_id);
    assert_eq!(shown["id"], json!(task_id));
    assert_eq!(shown["title"], "MCP round-trip task");
    assert_eq!(shown["description"], "Created over the MCP stdio transport");
    assert_eq!(shown["tags"], json!(["mcp-roundtrip"]));
    assert_eq!(shown["crew"], "sol");
    assert_eq!(shown["orchestrator"], "sol");
    assert_eq!(shown["job_run_id"], "jrun-mcp-projection");
    for (input, expected) in [
        (
            json!({ "id": task_id, "fields": ["crew", "orchestrator"] }),
            json!({"crew": "sol", "orchestrator": "sol"}),
        ),
        (
            json!({ "id": task_id, "fields": ["status"] }),
            json!({ "value": "proposed" }),
        ),
        (
            json!({
                "id": task_id,
                "fields": ["status", "relations", "external_refs", "job_run_id"],
            }),
            json!({
                "status": "proposed",
                "relations": [{
                    "type": "related_to",
                    "target": "DK-00042",
                    "verification": "not verifiable here",
                }],
                "external_refs": [],
                "job_run_id": "jrun-mcp-projection",
            }),
        ),
        (
            json!({ "id": task_id, "field": "relations" }),
            json!({
                "items": [{
                    "type": "related_to",
                    "target": "DK-00042",
                    "verification": "not verifiable here",
                }],
            }),
        ),
    ] {
        assert_eq!(
            client.call_tool_ok("orbit_task_show", input.clone()),
            expected,
            "{input}"
        );
    }

    let output = workspace.run_ok(&["task", "show", &task_id]);
    let stdout = stdout(&output);
    assert!(stdout.contains("Execution Crew: sol"), "{stdout}");
    assert!(stdout.contains("Orchestrator: sol"), "{stdout}");

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
    let status_input = format!(r#"{{"id":"{task_id}","fields":["status"]}}"#);
    let mixed_input = format!(
        r#"{{"id":"{task_id}","fields":["status","relations","external_refs","job_run_id"]}}"#
    );
    for (args, expected) in [
        (
            &[
                "task",
                "show",
                &task_id,
                "--json",
                "--fields",
                "orchestrator",
            ] as &[&str],
            json!("sol"),
        ),
        (
            &["task", "show", &task_id, "--json", "--fields", "status"],
            json!("proposed"),
        ),
        (
            &["tool", "run", "orbit.task.show", "--input", &status_input],
            json!("proposed"),
        ),
        (
            &[
                "task",
                "show",
                &task_id,
                "--json",
                "--fields",
                "status,relations,external_refs,job_run_id",
            ],
            expected_projection.clone(),
        ),
        (
            &["tool", "run", "orbit.task.show", "--input", &mixed_input],
            expected_projection,
        ),
    ] {
        assert_eq!(stdout_json(&workspace.run_ok(args)), expected, "{args:?}");
    }

    let listed = client.call_tool_ok("orbit_task_list", json!({}));
    let items = array_of(&listed, "tasks");
    assert!(
        items.iter().any(|task| task["id"] == json!(task_id)),
        "created task missing from list: {items:?}"
    );
    let listed_by_stable_id = client.call_tool_ok(
        "orbit_task_list",
        json!({ "workspace": "ws_mcp-roundtrip" }),
    );
    assert!(listed_by_stable_id["tasks"].as_array().is_some());

    // V1 exposes one complete surface. Domain validation and mutation still run
    // on the authoritative server-side runtime.
    let friction = client.call_tool_ok(
        "orbit_friction_add",
        json!({ "body": "MCP D2 exposure regression", "model": "codex" }),
    );
    let friction_id = id_of(&friction);
    let updated = client.call_tool_ok(
        "orbit_friction_update",
        json!({ "id": friction_id, "status": "triaged", "model": "codex" }),
    );
    assert_eq!(updated["status"], "triaged");
}

fn mcp_serve_error_paths_return_tool_errors_and_keep_serving() {
    let (_workspace, mut client) = McpWorkspace::served();

    // Bad params: missing required field is a structured `invalid_input`
    // tool error, not a JSON-RPC failure or a crash.
    let bad_params = client.call_tool_err("orbit_task_add", json!({}));
    assert_eq!(bad_params["code"], "invalid_input");
    assert!(
        message_contains(&bad_params, "title"),
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
    assert!(listed["tasks"].is_array(), "server wedged: {listed}");
}

fn mcp_workspace_argument_must_be_a_string_and_a_blank_one_defers_to_the_session() {
    let (workspace, mut client) = McpWorkspace::served();
    let task_id = author_task(&workspace, "selector probe");

    // A selector of the wrong type used to be read as absent, so the call was
    // silently served from the session's workspace instead of the one named.
    for wrong_type in [json!(5), json!(["other"]), json!({ "name": "other" })] {
        let refused = client.call_tool_err("orbit_task_list", json!({ "workspace": wrong_type }));
        assert_eq!(refused["code"], "invalid_input", "{refused}");
        assert!(
            message_contains(&refused, "`workspace` must be a string"),
            "the refusal must name the argument and its type: {refused}"
        );
    }

    // A blank selector names nothing. It must not override the bound session
    // and then fail as if no workspace were bound.
    for blank in ["", "   "] {
        let listed = client.call_tool_ok("orbit_task_list", json!({ "workspace": blank }));
        assert!(
            listed["tasks"]
                .as_array()
                .is_some_and(|tasks| tasks.iter().any(|task| task["id"] == json!(task_id))),
            "a blank selector must fall back to the session workspace: {listed}"
        );
        let shown = client.call_tool_ok(
            "orbit_task_show",
            json!({ "id": task_id, "workspace": blank, "fields": ["title", "status"] }),
        );
        assert_eq!(shown["title"], "selector probe", "{shown}");
    }
}

fn mcp_task_list_fields_projects_each_task_to_an_object_of_just_those_fields() {
    let (workspace, mut client) = McpWorkspace::served();
    let task_id = author_task(&workspace, "projection probe");

    // Full records are the default and carry the bulky fields.
    let full = client.call_tool_ok("orbit_task_list", json!({}));
    assert!(
        full["tasks"][0]["description"].is_string(),
        "an unprojected list keeps full records: {full}"
    );

    // A single field must still be an object per task; `orbit_task_show`'s
    // bare-value shape for one field would make the element shape depend on
    // how many fields were asked for.
    for fields in [json!(["id"]), json!("id"), json!(" id ")] {
        let listed = client.call_tool_ok("orbit_task_list", json!({ "fields": fields }));
        assert_eq!(
            listed["tasks"],
            json!([{ "id": task_id }]),
            "one field: {listed}"
        );
        assert_eq!(listed["total"], 1);
        assert_eq!(listed["truncated"], false);
    }

    let listed = client.call_tool_ok(
        "orbit_task_list",
        json!({ "fields": "id, title,status", "status": ["proposed"] }),
    );
    assert_eq!(
        listed["tasks"],
        json!([{ "id": task_id, "title": "projection probe", "status": "proposed" }]),
        "comma-separated fields: {listed}"
    );

    // An unknown field is refused even when the filter matches no task, so a
    // typo is not mistaken for "nothing to project".
    let refused = client.call_tool_err(
        "orbit_task_list",
        json!({ "fields": ["id", "titel"], "status": ["done"] }),
    );
    assert_eq!(refused["code"], "invalid_input", "{refused}");
    assert!(
        message_contains(&refused, "titel"),
        "the refusal must name the unknown field: {refused}"
    );
}

fn task_mutations_are_immediately_searchable_from_the_cli_and_mcp() {
    let (workspace, mut client) = McpWorkspace::served();
    let task = client.call_tool_ok(
        "orbit_task_add",
        json!({
            "title": "Cobalt routing observatory regression",
            "description": "Synchronous lexical indexing",
            "complexity": "low", "model": "codex"
        }),
    );
    let id = task["id"].as_str().expect("id");
    let search = |query: &str| {
        stdout_json(&workspace.run_ok(&["search", query, "--kind", "task", "--json"]))
    };
    assert_eq!(search("Cobalt observatory")["results"][0]["id"], id);
    client.call_tool_ok(
        "orbit_task_update",
        json!({"id": id, "title": "Quartz routing telescope regression", "model": "codex"}),
    );
    assert_eq!(search("Quartz telescope")["results"][0]["id"], id);
    assert!(
        search("Cobalt observatory")["results"]
            .as_array()
            .expect("results")
            .is_empty()
    );
    let rebuilt = workspace.run_ok(&["search", "reindex", "--json"]);
    let report: Value = stdout_json(&rebuilt);
    assert!(report["chunks"].as_u64().expect("chunk count") > 0);
    assert_eq!(search("Quartz telescope")["results"][0]["id"], id);
}

fn mcp_calls_are_audited_once_including_unknown_raw_names() {
    let (workspace, mut client) = McpWorkspace::served();

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

    let only_row = |tool_name: &str| {
        let rows = audit_rows(&workspace, &["--tool", tool_name]);
        let rows = rows.as_array().expect("audit row array");
        assert_eq!(rows.len(), 1, "exactly one audit row for {tool_name}");
        rows[0].clone()
    };
    let assert_common = |row: &Value, tool_name: &str, status: &str| {
        assert_eq!(row["tool_name"], tool_name);
        assert_eq!(row["subcommand"], "run-mcp");
        assert_eq!(row["status"], status);
        assert_eq!(row["role"], "unverified");
        assert_eq!(row["transport"], "local");
        assert_eq!(row["effective_capabilities"], json!(["agent"]));
        assert!(row["caller_machine_id"].as_str().is_some());
        assert_eq!(row["caller_machine_id"], row["process_machine_id"]);
        assert!(row["trace_id"].as_str().is_some());
        assert!(row["origin_session_id"].as_str().is_some());
    };
    for (tool_name, status) in [
        ("orbit.search", "success"),
        ("orbit.task.add", "failure"),
        ("orbit.workflow.ship", "denied"),
    ] {
        let row = only_row(tool_name);
        assert_common(&row, tool_name, status);
        assert!(row["workspace_id"].as_str().is_some());
        assert!(row["mcp_call_id"].is_null());
        assert!(row["duration_ms"].as_i64().is_some_and(|value| value >= 1));
    }

    let row = only_row("orbit_removed_tool");
    assert_common(&row, "orbit_removed_tool", "denied");
    assert!(row["workspace_id"].is_null());
    assert!(row["process_machine_name"].as_str().is_some());

    // The audit log is one host-global table, so a bare `audit list` must stay
    // unscoped. A workspace-scoped default is a `workspace_id = ?` predicate,
    // which cannot match the NULL the global seam writes before any workspace
    // resolves — the unknown-name denial above would vanish from the log while
    // the attributed rows stayed visible.
    let rows = audit_rows(&workspace, &["--limit", "500"]);
    let rows = rows.as_array().expect("unscoped audit row array");
    assert!(
        rows.iter().any(|row| {
            row["tool_name"] == "orbit_removed_tool" && row["workspace_id"].is_null()
        }),
        "unscoped audit list keeps the unattributed global-seam row: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| {
            row["tool_name"] == "orbit.search" && row["workspace_id"].as_str().is_some()
        }),
        "unscoped audit list keeps workspace-attributed rows: {rows:?}"
    );
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
fn worktree_backed_activity_routes_task_and_search_by_advertised_workspace_argument() {
    let workspace = McpWorkspace::init();

    // Diverge the logical registry ID from the checkout identity that keys the
    // coordination task registry. This is the production shape the friction was
    // filed from; a freshly initialized workspace writes both from one value.
    diverge_registry_id(&workspace, "ws_mcp-roundtrip");
    let identity =
        std::fs::read_to_string(workspace.work.join(".orbit").join("config.yaml")).expect("read");
    assert!(
        identity.contains("ws_mcp-roundtrip"),
        "checkout identity must keep the pre-divergence ID: {identity}"
    );

    // A task authored through the checkout-local CLI surface. Before the fix,
    // MCP looked for it under the logical ID and found an empty partition.
    let (task_id, worktree) = worktree_task_fixture(&workspace, "Worktree routing regression");

    // The executor's client: no `_meta.orbit.workspace`, cwd inside the linked
    // worktree.
    let mut client = spawn_unbound_worktree_server(&workspace, &worktree, None);

    // Every workspace-scoped tool must advertise the selector it requires.
    let listed = client.tools_list();
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

    let selector = utf8(&worktree);
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
        array_of(&found, "results")
            .iter()
            .any(|hit| hit["id"] == json!(task_id)),
        "search must reach the same workspace from the worktree: {found}"
    );
    drop(client);

    // The `orbit tool run` fallback stays functional from the worktree, and it
    // observes the write MCP just made — both surfaces address one partition.
    let output = cli_ok(
        &worktree,
        &workspace.home,
        &[
            "tool",
            "run",
            "orbit.task.show",
            "--input",
            &format!(r#"{{"id":"{task_id}"}}"#),
            "--fields",
            "id,execution_summary",
        ],
    );
    let shown: Value = stdout_json(&output);
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
fn managed_mcp_config_updates_a_task_without_a_workspace_argument() {
    let workspace = McpWorkspace::init();
    let task_id = author_task(&workspace, "Managed executor routing");
    let args = generate_managed_mcp_config(&workspace);
    let expected_args = [
        "mcp",
        "serve",
        "--operator",
        "--workspace",
        "ws_mcp-roundtrip",
    ];
    assert_eq!(
        args, expected_args,
        "bootstrap must generate a bound operator launch"
    );
    assert_eq!(
        generate_managed_mcp_config(&workspace),
        expected_args,
        "re-running bootstrap must preserve one operator flag and workspace binding"
    );

    // Launch outside any checkout, without initialize metadata or payload
    // selectors: only the generated argv can supply workspace authority.
    let mut client = spawn_generated_server(&workspace, &workspace.home, &args);
    let runs = client.call_tool_ok("orbit_workflow_run_list", json!({}));
    assert_eq!(
        runs["items"],
        json!([]),
        "generated operator authority must reach a governed tool"
    );

    // The advertised schema must tell this session the truth: it is bound, so
    // the selector is optional here.
    assert_selector_optional(&mut client, "orbit_task_update");

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

    // An explicit selector for the bound workspace is accepted. Naming a
    // workspace that does not exist fails closed rather than falling back to
    // the binding.
    let explicit = client.call_tool_ok(
        "orbit_task_update",
        json!({
            "id": task_id,
            "execution_summary": "Routed by an explicit selector",
            "workspace": utf8(&workspace.work),
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
        message_contains(&rejected, "ws_not_registered"),
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
fn federated_mcp_serve_requires_the_machine_qualified_list_selector() {
    let workspace = McpWorkspace::init();
    write_destinations(
        &workspace,
        "[[destinations]]\nssh = \"orbit-linux\"\nmachine_id = \"hm_alpha\"\n",
    );

    let mut client = federated_client(&workspace);

    let listed = client.tools_list();
    for tool_name in ["orbit_task_list", "orbit_task_show", "orbit_auto_task_list"] {
        let description = tool_workspace_description(&listed, tool_name);
        assert!(
            description.contains("selector") && description.contains("orbit_workspace_list"),
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

    // An id-only call routes by the id's prefix [ORB-14449]; it is never
    // read as a minted selector.
    let omitted = client.call_tool_err("orbit_task_show", json!({ "id": "ORB-00001" }));
    assert_eq!(
        omitted["code"], "unknown_task_prefix",
        "an id-only call is routed by prefix, not refused as a selector: {omitted}"
    );

    let bare_show = client.call_tool_err(
        "orbit_task_show",
        json!({ "id": "ORB-00001", "workspace": "ws_orbit" }),
    );
    assert_eq!(
        bare_show["code"], "unknown_selector",
        "a bare ws_* on federated task.show is unknown_selector: {bare_show}"
    );

    let bare_list = client.call_tool_err("orbit_task_list", json!({ "workspace": "ws_orbit" }));
    assert_eq!(
        bare_list["code"], "unknown_selector",
        "a bare ws_* is unknown_selector before forwarding: {bare_list}"
    );
}

fn federated_client(workspace: &McpWorkspace) -> McpClient {
    let child = workspace
        .cmd()
        .args(["mcp", "serve", "--mode", "federated"])
        .spawn_piped();
    let (client, _) = McpClient::initialized(
        child,
        McpClient::initialize_params("federated-roundtrip", None),
    );
    client
}

fn machine_identity(home: &Path) -> (String, String) {
    let parsed: toml::Value = toml::from_str(
        &std::fs::read_to_string(home.join(".orbit").join("config.toml"))
            .expect("read config.toml"),
    )
    .expect("parse config.toml");
    let machine = &parsed["machine"];
    (
        machine["id"].as_str().expect("machine.id").to_string(),
        machine["name"].as_str().expect("machine.name").to_string(),
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
fn federated_mcp_serve_lists_and_routes_local_workspaces_without_destinations() {
    let workspace = McpWorkspace::init();
    let ssh_log = workspace.home.join("ssh-invocations.log");
    plant_ssh_stub(&McpWorkspace::stub_bin_dir(&workspace.home), &ssh_log);

    let mut client = federated_client(&workspace);
    let (machine_id, machine_name) = machine_identity(&workspace.home);
    let listed = client.call_tool_ok("orbit_workspace_list", json!({}));
    let rows = array_of(&listed, "workspaces");
    assert_eq!(
        rows.len(),
        1,
        "local-only membership lists this machine: {listed}"
    );
    let row = &rows[0];
    assert_eq!(row["machine_id"], machine_id);
    assert_eq!(row["machine_name"], machine_name);
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

    client.call_tool_ok("orbit_task_list", json!({ "workspace": selector }));
    let with_crews = client.call_tool_ok("orbit_workspace_list", json!({ "include": ["crews"] }));
    let crews = &with_crews["workspaces"][0]["crews"];
    assert_eq!(
        crews["workspace_id"], row["id"],
        "the local row carries crews its own machine resolved: {with_crews}"
    );
    assert!(crews["crews"].is_array(), "{with_crews}");
    assert_no_ssh(&ssh_log, "local federated routing must not spawn SSH");
}

/// ORB-11048: in-process federation must preserve the outer MCP call's audit
/// evidence while retaining the accepting server's trusted session fields.
fn direct_and_federated_local_calls_record_equivalent_audit_contexts() {
    let workspace = McpWorkspace::init();
    let (machine_id, machine_name) = machine_identity(&workspace.home);

    let mut direct = workspace.serve();
    direct.call_tool_ok("orbit_task_list", json!({}));
    drop(direct);

    let mut federated = federated_client(&workspace);
    let listed = federated.call_tool_ok("orbit_workspace_list", json!({}));
    let selector = listed["workspaces"][0]["selector"]
        .as_str()
        .expect("local selector");
    federated.call_tool_ok("orbit_task_list", json!({ "workspace": selector }));
    drop(federated);

    let rows = audit_rows(&workspace, &["--tool", "orbit.task.list"]);
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
        assert_eq!(row["caller_machine_name"], machine_name);
        assert_eq!(row["process_machine_id"], machine_id);
        assert_eq!(row["process_machine_name"], machine_name);
        assert_eq!(row["transport"], "local");
        assert_eq!(row["effective_capabilities"], json!(["agent"]));
    }
}

/// ORB-11044: an explicit destination naming the local machine is one local route.
fn federated_mcp_serve_collapses_an_explicit_local_destination_row() {
    let workspace = McpWorkspace::init();
    let (machine_id, machine_name) = machine_identity(&workspace.home);
    write_destinations(
        &workspace,
        &format!("[[destinations]]\nssh = \"localhost\"\nmachine_id = \"{machine_id}\"\n"),
    );
    let ssh_log = workspace.home.join("ssh-invocations.log");
    plant_ssh_stub(&McpWorkspace::stub_bin_dir(&workspace.home), &ssh_log);

    let mut client = federated_client(&workspace);
    let listed = client.call_tool_ok("orbit_workspace_list", json!({}));
    let rows = array_of(&listed, "workspaces");
    let local_rows: Vec<_> = rows
        .iter()
        .filter(|row| row["machine_id"] == machine_id)
        .collect();
    assert_eq!(
        local_rows.len(),
        1,
        "explicit local SSH row must not duplicate selectors: {listed}"
    );
    assert_eq!(local_rows[0]["machine_name"], machine_name);
    let selector = local_rows[0]["selector"].as_str().expect("selector");
    client.call_tool_ok("orbit_task_list", json!({ "workspace": selector }));
    assert_no_ssh(&ssh_log, "collapsed local route must not spawn SSH");
}

/// ORB-11044: mixed membership lists local workspaces beside configured remotes.
fn federated_mcp_serve_lists_local_workspaces_beside_unreachable_remotes() {
    let workspace = McpWorkspace::init();
    write_destinations(
        &workspace,
        "[[destinations]]\nssh = \"orbit-missing-host\"\nmachine_id = \"hm_remote\"\n",
    );
    let ssh_log = workspace.home.join("ssh-invocations.log");
    plant_ssh_stub(&McpWorkspace::stub_bin_dir(&workspace.home), &ssh_log);

    let mut client = federated_client(&workspace);
    let (machine_id, _) = machine_identity(&workspace.home);
    let listed = client.call_tool_ok("orbit_workspace_list", json!({}));
    let rows = array_of(&listed, "workspaces");
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

/// ORB-14448: federated serve reads its remotes from the host file `orbit
/// host` writes, and refuses to choose while the legacy file also exists.
fn federated_mcp_serve_reads_the_host_file_and_refuses_both_host_files() {
    let workspace = McpWorkspace::init();
    let orbit_root = workspace.home.join(".orbit");
    std::fs::write(
        orbit_root.join("hosts.toml"),
        "schema_version = 1\n\n[[hosts]]\nname = \"remote\"\nmachine_id = \"hm_remote\"\n\
         ssh = \"orbit-missing-host\"\ntask_prefix = \"RM\"\n",
    )
    .expect("write host file");
    let ssh_log = workspace.home.join("ssh-invocations.log");
    plant_ssh_stub(&McpWorkspace::stub_bin_dir(&workspace.home), &ssh_log);

    let mut client = federated_client(&workspace);
    let listed = client.call_tool_ok("orbit_workspace_list", json!({}));
    let remote = array_of(&listed, "workspaces")
        .iter()
        .find(|row| row["machine_id"] == "hm_remote")
        .unwrap_or_else(|| panic!("registered host is listed: {listed}"));
    assert_eq!(remote["reachability"], "unreachable");
    assert!(
        std::fs::read_to_string(&ssh_log).is_ok_and(|log| log.contains("orbit-missing-host")),
        "the registered host's SSH target is the one dialed"
    );
    drop(client);

    std::fs::write(
        orbit_root.join("mcp-destinations.toml"),
        "[[destinations]]\nssh = \"orbit-missing-host\"\nmachine_id = \"hm_remote\"\n",
    )
    .expect("write legacy destinations");
    let output = workspace.run(&["mcp", "serve", "--mode", "federated"]);
    assert!(!output.status.success(), "both host files must fail closed");
    let stderr = stderr(&output);
    assert!(
        stderr.contains("host_file_conflict")
            && stderr.contains("hosts.toml")
            && stderr.contains("mcp-destinations.toml"),
        "the refusal names both files: {stderr}"
    );
}

/// ORB-11044: a machine-id-only destination row still fails closed.
fn federated_mcp_serve_rejects_a_machine_id_only_destination_row() {
    let workspace = McpWorkspace::init();
    write_destinations(&workspace, "[[destinations]]\nmachine_id = \"hm_alpha\"\n");

    let output = workspace.run(&["mcp", "serve", "--mode", "federated"]);
    assert!(
        !output.status.success(),
        "machine-id-only rows must fail closed"
    );
    let stderr = stderr(&output);
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
    diverge_registry_id(&workspace, "ws_orbit-5c61b3");
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
fn managed_mcp_env_binding_updates_without_a_workspace_argument() {
    let workspace = McpWorkspace::init();
    let (task_id, worktree) = worktree_task_fixture(&workspace, "Managed envelope MCP routing");
    write_shadow_worktree_identity(&worktree);

    let mut client = spawn_unbound_worktree_server(&workspace, &worktree, Some("ws_mcp-roundtrip"));

    assert_selector_optional(&mut client, "orbit_task_update");

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
        json!({ "body": "Managed MCP envelope routed friction add.", "model": "codex", }),
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
        message_contains(&rejected, "ws_not_registered"),
        "an explicit selector must fail closed without cwd fallback: {rejected}"
    );
    drop(client);

    assert_eq!(
        cli_task_execution_summary(&workspace, &workspace.work, &task_id),
        "Routed by the managed envelope"
    );
    assert_no_shadow_task_store(&worktree);
}

/// A nested Codex server receives these names via its configured `env_vars`.
/// Exercise the real MCP entry point with the resulting managed environment:
/// an allowed task write succeeds, while an omitted tool is refused before
/// dispatch, and the write is audited as an agent action.
fn managed_codex_mcp_env_enforces_activity_tools_and_agent_actor_kind() {
    let workspace = McpWorkspace::init();
    let task_id = author_task(&workspace, "Managed Codex activity policy");
    let mut client = workspace.serve_with_args_and_env(
        &[],
        &[
            ("ORBIT_MANAGED_RUN_CONTEXT", "1"),
            ("ORBIT_RUN_ID", "jrun-codex-policy-test"),
            ("ORBIT_AGENT_NAME", "codex"),
            ("ORBIT_AGENT_MODEL", "gpt-6-sol"),
            ("ORBIT_TASK_ACTOR_KIND", "agent"),
            ("ORBIT_ACTIVITY_TOOLS", "orbit.task.show,orbit.task.update"),
        ],
    );

    let shown = client.show_task(&task_id);
    assert_eq!(shown["id"], task_id);
    let updated = client.call_tool_ok(
        "orbit_task_update",
        json!({ "id": task_id, "comment": "Allowed managed Codex write", "model": "codex", }),
    );
    assert_eq!(updated["id"], task_id);

    let denied = client.call_tool_err(
        "orbit_search",
        json!({ "query": "policy test", "model": "codex" }),
    );
    assert_eq!(denied["code"], "policy_denied", "{denied}");
    assert!(
        message_contains(&denied, "not in the activity allowlist"),
        "an omitted tool must be refused by the managed activity policy: {denied}"
    );
    drop(client);

    let connection = workspace.store();
    let actor_kind: String = connection
        .query_row(
            "SELECT actor_kind FROM audit_events WHERE tool_name = 'orbit.task.update'
             ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("read managed task-write actor kind");
    assert_eq!(actor_kind, "agent");
}

/// ORB-11117: an inherited `ORBIT_WORKSPACE` without managed provenance is not
/// a launch binding.
fn unmanaged_orbit_workspace_env_does_not_bind_mcp() {
    let workspace = McpWorkspace::init();
    let child = workspace
        .cmd()
        .env("ORBIT_WORKSPACE", "ws_mcp-roundtrip")
        .env("ORBIT_SESSION_ID", "inherited-inspection-id")
        .args(["mcp", "serve"])
        .spawn_piped();
    let (mut client, _) =
        McpClient::initialized(child, McpClient::initialize_params("unmanaged-env", None));
    let unscoped = client.call_tool_err("orbit_task_list", json!({}));
    assert!(
        unscoped["message"]
            .as_str()
            .is_some_and(|message| { message.contains("requires an explicit workspace selector") })
    );
}

/// A source-inspection provider has an Orbit invocation identity but no job
/// run. The MCP server it starts must bind before its first tool call.
fn managed_source_inspection_mcp_search_uses_the_dispatching_workspace() {
    let workspace = McpWorkspace::init();
    let task_id = author_task(&workspace, "Inspection binding search fixture");
    let child = workspace
        .cmd()
        .env("ORBIT_MANAGED_RUN_CONTEXT", "1")
        .env_remove("ORBIT_RUN_ID")
        .env("ORBIT_SESSION_ID", "inspection-invocation")
        .env("ORBIT_WORKSPACE", "ws_mcp-roundtrip")
        .env("PATH", McpWorkspace::stub_bin_dir(&workspace.home))
        .args(["mcp", "serve"])
        .spawn_piped();
    let (mut client, _) = McpClient::initialized(
        child,
        McpClient::initialize_params("inspection-provider", None),
    );

    let listed = client.tools_list();
    assert!(
        tool_workspace_description(&listed, "orbit_search").contains("Optional in this session")
    );
    let search_schema = find_tool(tools_array(&listed), "orbit_search");
    assert!(
        search_schema["inputSchema"]["required"]
            .as_array()
            .is_none_or(|required| !required.iter().any(|field| field == "workspace")),
        "the inspection session must advertise workspace as optional: {search_schema}"
    );
    let found = client.call_tool_ok(
        "orbit_search",
        json!({ "query": "Inspection binding search fixture", "model": "codex", }),
    );
    assert!(found.to_string().contains(&task_id), "{found}");
    assert_eq!(
        latest_tool_audit_workspace(&workspace, "orbit.search").as_deref(),
        Some("ws_mcp-roundtrip")
    );
    let conflicting = client.call_tool_err(
        "orbit_search",
        json!({
            "workspace": "ws_not_registered",
            "query": "Inspection binding search fixture",
            "model": "codex",
        }),
    );
    assert!(
        message_contains(&conflicting, "ws_not_registered"),
        "an explicit payload selector must fail closed: {conflicting}"
    );
}

/// Author one task through the checkout-local CLI surface and return its ID.
fn author_task(workspace: &McpWorkspace, title: &str) -> String {
    add_task_cli(workspace, title, "Authored via the CLI fallback")
}

/// The two binding modes share a task authored in the checkout and a linked
/// worktree, but configure their registry and runtime identity independently.
fn worktree_task_fixture(workspace: &McpWorkspace, title: &str) -> (String, PathBuf) {
    let task_id = author_task(workspace, title);
    let worktree = add_linked_worktree(&workspace.work);
    (task_id, worktree)
}

/// Run the operator-facing bootstrap (`orbit workspace init --mcp`) and return
/// the exact argv the integration it generated launches Orbit with.
fn generate_managed_mcp_config(workspace: &McpWorkspace) -> Vec<String> {
    std::fs::create_dir_all(workspace.work.join(".claude")).expect("create .claude marker");
    let output = workspace.run_ok(&[
        "workspace",
        "init",
        "--name",
        "mcp-roundtrip",
        "--force",
        "--mcp",
    ]);
    assert_command_succeeded("workspace init --force --mcp", &output);
    read_generated_claude_args(workspace)
}

/// Run the agent-authority registration (`orbit mcp init --claude`) and return
/// the argv the integration it generated launches Orbit with.
fn generate_agent_mcp_config(workspace: &McpWorkspace) -> Vec<String> {
    let output = workspace.run_ok(&["mcp", "init", "--claude"]);
    assert_command_succeeded("mcp init --claude", &output);
    read_generated_claude_args(workspace)
}

fn read_generated_claude_args(workspace: &McpWorkspace) -> Vec<String> {
    let claude_mcp: Value = serde_json::from_str(
        &std::fs::read_to_string(workspace.work.join(".mcp.json"))
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
        .spawn_piped();
    let (client, _) = McpClient::initialized(
        child,
        McpClient::initialize_params("managed-executor", None),
    );
    client
}

/// Read one task's execution summary back through the CLI surface at `cwd`.
fn cli_task_execution_summary(workspace: &McpWorkspace, cwd: &Path, task_id: &str) -> String {
    let output = cli_ok(
        cwd,
        &workspace.home,
        &[
            "tool",
            "run",
            "orbit.task.show",
            "--input",
            &format!(r#"{{"id":"{task_id}"}}"#),
            "--fields",
            "id,execution_summary",
        ],
    );
    assert_command_succeeded("orbit.task.show", &output);
    let shown: Value = stdout_json(&output);
    assert_eq!(shown["id"], json!(task_id));
    shown["execution_summary"]
        .as_str()
        .expect("execution summary")
        .to_string()
}

/// Launch from a linked worktree with a client that sends no initialize
/// workspace metadata. `managed_selector` supplies the trusted run envelope
/// only for the managed-binding case.
fn spawn_unbound_worktree_server(
    workspace: &McpWorkspace,
    cwd: &Path,
    managed_selector: Option<&str>,
) -> McpClient {
    let mut command = McpWorkspace::orbit_command(cwd, &workspace.home);
    if let Some(selector) = managed_selector {
        command
            .env("ORBIT_MANAGED_RUN_CONTEXT", "1")
            .env("ORBIT_RUN_ID", "jrun-managed-mcp")
            .env("ORBIT_WORKSPACE", selector);
    }
    let child = command.args(["mcp", "serve"]).spawn_piped();
    let (client, _) = McpClient::initialized(
        child,
        McpClient::initialize_params("managed-executor", None),
    );
    client
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
    let connection = workspace.store();
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

/// ORB-10961: the ORB-10952 managed-worktree shape.
///
/// A linked worktree whose checkout identity (`ws_orbit-5c61b3`) diverged from
/// the logical registry ID must not turn that runtime identity into an
/// `orbit.task.show` filter. Id-only tool-run and MCP calls follow the task
/// ID; other workspace-scoped tools still require a registered selector.
fn task_show_is_global_by_default_across_tool_run_and_mcp() {
    let workspace = McpWorkspace::init_with_workspace_name("orbit-5c61b3");

    diverge_registry_id(&workspace, "ws_orbit-5c61b3");

    let elsewhere = workspace.home.join("elsewhere");
    init_checkout(&workspace.home, &elsewhere, "mcp-elsewhere");

    let task_id = add_task_cli(
        &workspace,
        "Owned despite runtime identity ws_orbit-5c61b3",
        "Addressed by ID from a linked worktree and a foreign cwd",
    );
    let show_input = json!({ "id": task_id, "model": "codex" }).to_string();

    let worktree = add_linked_worktree(&workspace.work);
    let scratch = workspace.home.join("scratch");
    std::fs::create_dir_all(&scratch).expect("create a non-workspace directory");

    for cwd in [&worktree, &elsewhere, &scratch] {
        let output = cli_ok(
            cwd,
            &workspace.home,
            &["tool", "run", "orbit.task.show", "--input", &show_input],
        );
        let stderr = stderr(&output);
        assert!(output.status.success());
        assert!(
            !stderr.contains("ws_orbit-5c61b3"),
            "tool run must not promote the runtime identity into a selector from {}: {stderr}",
            cwd.display()
        );
        let shown: Value = stdout_json(&output);
        assert_eq!(shown["id"], json!(task_id));
        assert_eq!(shown["workspace"]["id"], "ws_legacy-logical");
        assert_eq!(shown["workspace"]["name"], "orbit-5c61b3");
    }

    for (selector, why) in [
        (
            "no-such-workspace",
            "an invalid explicit selector must be named",
        ),
        (
            "ws_orbit-5c61b3",
            "an explicit runtime identity remains fail-closed",
        ),
    ] {
        let refused = cli(
            &scratch,
            &workspace.home,
            &[
                "tool",
                "run",
                "orbit.task.show",
                "--input",
                &json!({ "id": task_id, "workspace": selector, "model": "codex" }).to_string(),
            ],
        );
        assert!(!refused.status.success(), "{why}");
        let refused_stderr = stderr(&refused);
        assert!(refused_stderr.contains(selector), "{why}: {refused_stderr}");
    }

    let mut client = serve_mcp_from(
        &worktree,
        &workspace.home,
        McpClient::initialize_params("managed-executor", Some("ws_orbit-5c61b3")),
    );
    let listed = client.tools_list();
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

    let shown = client.show_task(&task_id);
    assert_eq!(shown["id"], json!(task_id));
    assert_eq!(shown["workspace"]["id"], "ws_legacy-logical");
    assert_eq!(shown["workspace"]["name"], "orbit-5c61b3");

    let listed_err = client.call_tool_err("orbit_task_list", json!({}));
    assert!(
        message_contains(&listed_err, "ws_orbit-5c61b3"),
        "task list must still fail-closed on the runtime identity: {listed_err}"
    );
    drop(client);

    let mut client = serve_mcp_from(
        &scratch,
        &workspace.home,
        McpClient::initialize_params("no-initialize-workspace", None),
    );
    let shown = client.show_task(&task_id);
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
        McpClient::initialize_params("other-session", Some(utf8(&elsewhere))),
    );
    let shown = client.show_task(&task_id);
    assert_eq!(
        shown["id"],
        json!(task_id),
        "id-only show must ignore a session bound to another workspace: {shown}"
    );
    let missed = client.call_tool_err(
        "orbit_task_show",
        json!({ "id": task_id, "workspace": utf8(&elsewhere) }),
    );
    assert!(
        message_contains(&missed, &task_id),
        "an explicit foreign workspace must filter: {missed}"
    );
}

/// ORB-10968: a stored crew this host has no `[crews.*]` entry for must not
/// make the task unreadable.
///
/// Crew configuration is host-local, so the same task is routinely served by a
/// machine that never defined its crew — over SSH-marked MCP most of all. The
/// read surfaces render the raw `crew` and mark the projection unresolved; only
/// paths that must actually dispatch (`orbit.task.update` to `in_progress`) still fail closed.
fn task_read_surfaces_tolerate_a_crew_this_host_does_not_define() {
    let workspace = McpWorkspace::init();
    let config_path = workspace.home.join(".orbit").join("config.toml");
    let baseline = std::fs::read_to_string(&config_path).expect("read the seeded config");

    // Author the task while the crew exists, the way its owning machine would.
    std::fs::write(
        &config_path,
        format!("{baseline}\n[crews.remote-only]\nprovider = \"codex\"\nmodel = \"gpt-6-sol\"\n"),
    )
    .expect("define the authoring host's crew");
    let add_input = json!({
        "title": "Authored against a crew only the other host defines",
        "description": "Read back from a host whose [crews.*] table lacks it",
        "workspace": utf8(&workspace.work),
        "complexity": "low",
        "crew": "remote-only",
        "model": "codex",
    })
    .to_string();
    let created: Value =
        stdout_json(&workspace.run_ok(&["tool", "run", "orbit.task.add", "--input", &add_input]));
    let task_id = id_of(&created);

    // Serve it from a host that never had that crew.
    std::fs::write(&config_path, &baseline).expect("drop the crew from this host");
    let show_input = json!({ "id": task_id, "model": "codex" }).to_string();

    // (1) CLI tool-run, from a directory outside any workspace, so the read
    // also goes through the global id lookup.
    let scratch = workspace.home.join("scratch");
    std::fs::create_dir_all(&scratch).expect("create a non-workspace directory");
    // `--full` because tool-run projects a minimal task shape by default, and
    // the crew fields are what this asserts.
    let output = cli_ok(
        &scratch,
        &workspace.home,
        &[
            "tool",
            "run",
            "orbit.task.show",
            "--full",
            "--input",
            &show_input,
        ],
    );
    assert_unresolved_crew_projection(&stdout_json(&output));

    // (2) A local MCP session bound to the workspace.
    let mut client = workspace.serve();
    assert_unresolved_crew_projection(&client.show_task(&task_id));
    let listed = client.call_tool_ok(
        "orbit_task_list",
        json!({ "workspace": utf8(&workspace.work) }),
    );
    assert!(
        array_of(&listed, "tasks")
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
        .env(SSH_ENV.0, SSH_ENV.1)
        .spawn_piped();
    let (mut client, _) = McpClient::initialized(
        child,
        McpClient::initialize_params("remote-crew-roundtrip", Some("ws_mcp-roundtrip")),
    );
    assert_unresolved_crew_projection(&client.show_task(&task_id));
    drop(client);

    // Execution still fails closed: starting the task needs a crew this host
    // can actually dispatch to.
    let start_input = format!(r#"{{"id":"{}","status":"in_progress"}}"#, task_id);
    let started = workspace.run(&["tool", "run", "orbit.task.update", "--input", &start_input]);
    assert!(
        !started.status.success(),
        "start must not tolerate the crew"
    );
    let stderr = stderr(&started);
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
        .spawn_piped();
    McpClient::initialized(child, initialize).0
}

/// ORB-10963: managed executors may see the primary Orbit state through a
/// read-only mount while their linked checkout remains writable. Exercise the
/// production CLI and MCP entry points inside that mount namespace rather than
/// approximating the boundary with permission bits.
// Linux: Bubblewrap read-only mounts prove CLI/MCP reads preserve real EROFS state and WAL data.
#[cfg(target_os = "linux")]
#[test]
fn readonly_state_mount_keeps_cli_and_mcp_reads_observational() {
    if let Some(detail) = bubblewrap_mount_namespaces_unavailable() {
        // Some nested container runners and agent lanes deny user/mount
        // namespaces. The production behavior remains covered by the focused
        // store and dispatch tests; an unrestricted Linux host executes this
        // path.
        orbit_exec::report_bwrap_deferral(
            "readonly_state_mount_keeps_cli_and_mcp_reads_observational",
            &detail,
        );
        return;
    }

    let workspace = McpWorkspace::init();
    let task_id = add_task_cli(
        &workspace,
        "Read-only managed fixture",
        "State is warmed before it is remounted read-only",
    );

    // Materialize every optional cache/import/index before the mount changes.
    // The assertions below still prove that opening and reading those stores
    // does not require a new sidecar, access-time, or audit write.
    let warm_search = workspace.run_ok(&["tool", "run", "orbit.search", "--input", &json!({ "query": "read-only managed fixture", "workspace": workspace.work, "model": "codex" }) .to_string(),]);
    assert_command_succeeded("fixture search warmup", &warm_search);

    // Leave a committed registry write behind in the WAL, the state the mount
    // has to read through (ORB-12090). The holder pins the snapshot taken
    // before the next task is created, so nothing checkpoints that task back
    // into the main database file.
    let canonical_root = workspace.home.join(".orbit");
    let registry = canonical_root.join("tasks").join("index.sqlite");
    let registry_holder = hold_uncheckpointed_registry_snapshot(&registry);
    let wal_only_task_id = add_task_cli(
        &workspace,
        "Committed into the registry WAL",
        "Only a WAL-aware read-only open reports this task",
    );
    assert!(
        !main_registry_file_has_task(&registry, &wal_only_task_id),
        "the fixture must keep {wal_only_task_id} out of the main registry file"
    );

    let worktree = add_linked_worktree(&workspace.work);
    let workspace_state_root = workspace.work.join(".orbit");
    let protected_state = snapshot_fixture_state(&[&canonical_root, &workspace_state_root]);

    let ro = || {
        readonly_orbit_command(
            &worktree,
            &workspace.home,
            &canonical_root,
            &workspace_state_root,
        )
    };
    let tool_list = orbit_ok(ro().args(["tool", "list", "--json"]));
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
        let output = orbit_ok(ro().args([
            "tool",
            "run",
            name,
            "--root",
            utf8(&canonical_root),
            "--input",
            &input.to_string(),
        ]));
        assert_command_succeeded(name, &output);
    }

    // `TaskUpdateParams` has no workspace selector, so `--root canonical_root`
    // is the only routing this mutation gets: it always lands in the unbound
    // canonical partition (`UNBOUND_DATA_DIR_PARTITION_ID`), not the checkout's
    // bound workspace. The denial below must therefore name that partition.
    let mutation = ro()
        .args([
            "tool",
            "run",
            "orbit.task.update",
            "--root",
            utf8(&canonical_root),
            "--input",
            &json!({ "id": task_id, "execution_summary": "must not persist", "model": "codex" })
                .to_string(),
        ])
        .output()
        .expect("attempt task mutation through the read-only mount");
    assert_readonly_mutation_failed("CLI", &canonical_root, &mutation);

    let mut child = ro();
    child.args(["mcp", "serve"]);
    // `ORBIT_ROOT` locates the canonical data root and the session selects
    // the registered checkout, while the server itself still runs from the
    // linked worktree like a managed executor.
    let initialize =
        McpClient::initialize_params("readonly-managed-executor", Some(utf8(&workspace.work)));
    let (mut client, initialized) = McpClient::initialized(child.spawn_piped(), initialize);
    assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");

    let listed = client.tools_list();
    assert!(listed["result"]["tools"].is_array(), "{listed}");
    assert_eq!(client.show_task(&task_id)["id"], task_id);
    assert_eq!(client.show_task(&wal_only_task_id)["id"], wal_only_task_id);
    let listed = client.call_tool_ok("orbit_task_list", json!({}));
    assert!(
        array_of(&listed, "tasks")
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
    // Unlike the CLI mutation, this one lands in the session's workspace. Its
    // partition exists, so the runtime opens it observationally and the
    // storage itself refuses the write.
    let mutation = client.call_tool_err(
        "orbit_task_update",
        json!({ "id": task_id, "execution_summary": "must not persist", "model": "codex" }),
    );
    assert_readonly_diagnostic(
        "MCP",
        &canonical_root,
        "ws_mcp-roundtrip",
        mutation["message"].as_str().unwrap_or_default(),
    );
    drop(client);

    assert_eq!(
        snapshot_fixture_state(&[&canonical_root, &workspace_state_root]),
        protected_state,
        "read-only CLI and MCP calls must leave canonical and workspace state unchanged"
    );
    drop(registry_holder);
}

/// CLI tool dispatch may request a writable runtime before it knows the tool
/// is a read. A denied, absent canonical partition must not refuse that read,
/// nor may the fallback silently allow a subsequent mutation.
#[cfg(unix)]
fn absent_unwritable_partition_keeps_cli_tool_reads_observational() -> bool {
    let workspace = McpWorkspace::init();
    let canonical_root = workspace.home.join(".orbit");
    let partitions = canonical_root.join("tasks/workspaces");
    let partition = partitions.join("ws_unbound-data-dir");
    assert!(
        !partition.exists(),
        "fixture must leave the unbound partition absent"
    );
    // A writable probe cannot exercise the creation-denial path.
    let Some(original) = deny_dir_writes(&partitions, || {
        std::fs::create_dir(partitions.join("probe"))
    }) else {
        return false;
    };
    let before = snapshot_fixture_state(&[&partitions]);
    let read = workspace.run(&[
        "tool",
        "run",
        "orbit.task.list",
        "--root",
        utf8(&canonical_root),
        "--input",
        &json!({ "model": "codex" }).to_string(),
    ]);
    let mutation = workspace.run(&["tool", "run", "orbit.task.add", "--root", utf8(&canonical_root), "--input", &json!({ "title": "Must not persist", "description": "An observational fallback cannot authorize writes", "complexity": "low", "model": "codex", }) .to_string(),]);
    let after = snapshot_fixture_state(&[&partitions]);
    std::fs::set_permissions(&partitions, original).expect("restore partition permissions");

    assert_command_succeeded("read through an absent denied partition", &read);
    let read: Value = stdout_json(&read);
    assert_eq!(read["tasks"], json!([]));
    assert_readonly_mutation_failed("CLI add", &canonical_root, &mutation);
    assert_eq!(
        after, before,
        "reads and refused writes must create no partition files or directories"
    );
    true
}

/// ORB-12097: `readonly_orbit_command` must scrub the managed-run authority a
/// job worker exports into every process it launches before it ever launches
/// `bwrap` — otherwise bwrap forwards its own (inherited) environment into the
/// sandboxed `orbit` child, and `ORBIT_REGISTRY_ROOT` there outranks the
/// `--root canonical_root` argument the mutation below passes explicitly.
///
/// The sentinel registry stands in for that live authority. It is never bound
/// into the sandbox, so only the scrub — not the mount — can keep the child
/// off it: `--bind / /` leaves everything outside `canonical_root` and
/// `workspace_state_root` fully writable.
// Linux: Bubblewrap mount isolation proves inherited worker authority cannot redirect a read-only child.
#[cfg(target_os = "linux")]
#[test]
fn readonly_orbit_command_scrubs_inherited_managed_run_authority() {
    if crate::isolated_test::run_in_child(
        "mcp_roundtrip::readonly_orbit_command_scrubs_inherited_managed_run_authority",
    ) {
        return;
    }
    if let Some(detail) = bubblewrap_mount_namespaces_unavailable() {
        orbit_exec::report_bwrap_deferral(
            "readonly_orbit_command_scrubs_inherited_managed_run_authority",
            &detail,
        );
        return;
    }

    let sentinel = McpWorkspace::init_with_workspace_name("orb-12097-sentinel");
    let sentinel_registry_root = sentinel.home.join(".orbit");
    let sentinel_before = snapshot_fixture_state(&[&sentinel_registry_root]);

    let workspace = McpWorkspace::init();
    let task_id = add_task_cli(&workspace, "Scrub regression fixture", "Guards ORB-12097");

    let worktree = add_linked_worktree(&workspace.work);
    let canonical_root = workspace.home.join(".orbit");
    let workspace_state_root = workspace.work.join(".orbit");

    // Present the sentinel exactly the way a managed Orbit run presents live
    // authority to every process it launches, including the test binary
    // itself. This complete body runs alone in an isolated child, so the
    // deliberate process-wide changes cannot reach sibling parent tests.
    let _ambient_authority = test_env::scoped([
        ("ORBIT_MANAGED_RUN_CONTEXT", Some("1")),
        ("ORBIT_RUN_ID", Some("jrun-orb-12097-regression")),
        (
            "ORBIT_REGISTRY_ROOT",
            Some(
                sentinel_registry_root
                    .to_str()
                    .expect("utf8 sentinel registry root"),
            ),
        ),
    ]);

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
        utf8(&canonical_root),
        "--input",
        &json!({ "id": task_id, "execution_summary": "must not persist", "model": "codex" })
            .to_string(),
    ])
    .output()
    .expect("attempt task mutation with ambient managed-run authority present");

    assert_readonly_mutation_failed("CLI", &canonical_root, &mutation);
    assert_eq!(
        snapshot_fixture_state(&[&sentinel_registry_root]),
        sentinel_before,
        "the ambient ORBIT_REGISTRY_ROOT must never reach the sandboxed orbit child"
    );
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
fn read_only_registry_files_keep_uncheckpointed_wal_reads_observational() -> bool {
    use std::os::unix::fs::PermissionsExt;

    let workspace = McpWorkspace::init();
    let registry = workspace
        .home
        .join(".orbit")
        .join("tasks")
        .join("index.sqlite");
    add_task_cli(
        &workspace,
        "Warmed before the registry files turn read-only",
        "Every commit here is checkpointed into the main database file",
    );

    let registry_holder = hold_uncheckpointed_registry_snapshot(&registry);
    let wal_only_task_id = add_task_cli(
        &workspace,
        "Committed into the registry WAL",
        "Only a WAL-aware read-only open reports this task",
    );
    assert!(
        !main_registry_file_has_task(&registry, &wal_only_task_id),
        "the fixture must keep {wal_only_task_id} out of the main registry file"
    );

    let registry_files: Vec<PathBuf> = ["", "-wal", "-shm"]
        .iter()
        .map(|suffix| PathBuf::from(format!("{}{suffix}", registry.display())))
        .collect();
    for file in &registry_files {
        std::fs::set_permissions(
            file,
            std::fs::Permissions::from_mode(read_only_fixture_mode(0o400)),
        )
        .unwrap_or_else(|error| panic!("make {} read-only: {error}", file.display()));
    }
    for file in &registry_files {
        match std::fs::OpenOptions::new().write(true).open(file) {
            Ok(_) => return false,
            Err(error) => assert_eq!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied,
                "verify write denial for {}: {error}",
                file.display()
            ),
        }
    }
    let before: Vec<Vec<u8>> = registry_files
        .iter()
        .map(|file| std::fs::read(file).expect("snapshot registry file"))
        .collect();

    let shown = workspace.tool_ok(
        "orbit.task.show",
        json!({ "id": wal_only_task_id, "model": "codex" }),
    );
    assert_command_succeeded("read-only orbit.task.show", &shown);
    let shown: Value = stdout_json(&shown);
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
    true
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
fn absent_global_semantic_index_keeps_read_only_cli_reads_observational() -> bool {
    let workspace = McpWorkspace::init();
    let task_id = add_task_cli(
        &workspace,
        "Readable without a global semantic index",
        "The canonical root never built one",
    );

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
    // A user that ignores the mode bits (typically root) cannot reproduce
    // the creation denial this fixture is about.
    let Some(original) = deny_dir_writes(&state_dir, || {
        std::fs::File::create(state_dir.join("probe")).map(|_| ())
    }) else {
        return false;
    };

    let listed = orbit_ok(
        McpWorkspace::orbit_command(&worktree, &workspace.home)
            .env("ORBIT_ROOT", &canonical_root)
            .args(["tool", "list", "--json"]),
    );
    let shown = orbit_ok(
        McpWorkspace::orbit_command(&worktree, &workspace.home)
            .env("ORBIT_ROOT", &canonical_root)
            .args([
                "tool",
                "run",
                "orbit.task.show",
                "--root",
                utf8(&canonical_root),
                "--input",
                &json!({ "id": task_id, "model": "codex" }).to_string(),
            ]),
    );
    std::fs::set_permissions(&state_dir, original).expect("restore state permissions");

    assert_command_succeeded("read-only orbit.tool.list", &listed);
    let listed: Value = stdout_json(&listed);
    assert!(
        listed
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "orbit.task.show")),
        "tool listing needs no semantic index and must still answer: {listed}"
    );
    assert_command_succeeded("read-only orbit.task.show", &shown);
    let shown: Value = stdout_json(&shown);
    assert_eq!(shown["id"], json!(task_id));
    assert!(
        !semantic_db.exists(),
        "an unavailable index must not be created by a read"
    );
    true
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

/// Why Bubblewrap cannot create a mount namespace here, or `None` when it can.
#[cfg(target_os = "linux")]
fn bubblewrap_mount_namespaces_unavailable() -> Option<String> {
    match Command::new("bwrap")
        .args([
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--",
            "/bin/true",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(output) if output.status.success() => None,
        Ok(output) => Some(format!(
            "bwrap exited {}: {}",
            output.status,
            stderr(&output).trim()
        )),
        Err(error) => Some(format!("bwrap could not start: {error}")),
    }
}

#[cfg(target_os = "linux")]
fn readonly_orbit_command(
    worktree: &Path,
    home: &Path,
    canonical_root: &Path,
    workspace_state_root: &Path,
) -> Command {
    let mut command = Command::new("bwrap");
    // ORB-12097: bwrap forwards its own process environment into the sandbox
    // unless told otherwise, so a managed run's ORBIT_RUN_ID/REGISTRY_ROOT/
    // WORKSPACE would otherwise reach the `orbit` child through bwrap rather
    // than through this fixture's deliberate --setenv values below.
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
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

#[cfg(unix)]
fn snapshot_fixture_state(roots: &[&Path]) -> BTreeSet<(PathBuf, Option<Vec<u8>>)> {
    let mut state = BTreeSet::new();

    for root in roots {
        snapshot_fixture_state_at(root, &mut state);
    }

    state
}

#[cfg(unix)]
fn snapshot_fixture_state_at(root: &Path, state: &mut BTreeSet<(PathBuf, Option<Vec<u8>>)>) {
    for entry in std::fs::read_dir(root).expect("read protected fixture state directory") {
        let entry = entry.expect("read protected fixture state entry");
        let path = entry.path();
        let file_type = entry
            .file_type()
            .expect("read protected fixture state type");

        if file_type.is_dir() {
            state.insert((path.clone(), None));
            snapshot_fixture_state_at(&path, state);
        } else if file_type.is_file() {
            state.insert((
                path.clone(),
                Some(std::fs::read(&path).expect("read protected fixture state")),
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
        stdout(output),
        stderr(output)
    );
}

#[cfg(unix)]
fn assert_readonly_mutation_failed(
    label: &str,
    canonical_root: &Path,
    output: &std::process::Output,
) {
    assert!(
        !output.status.success(),
        "{label} mutation unexpectedly succeeded"
    );
    // A CLI mutation routes through `--root canonical_root` with no workspace
    // selector, so it lands in the unbound canonical partition
    // (`UNBOUND_DATA_DIR_PARTITION_ID` in `orbit-core`'s runtime builder).
    assert_readonly_diagnostic(
        label,
        canonical_root,
        "ws_unbound-data-dir",
        &stderr(output),
    );
}

/// The denial must name the task partition the write actually targeted, not
/// a stale pre-layout-v3 lock path.
#[cfg(unix)]
fn assert_readonly_diagnostic(
    label: &str,
    canonical_root: &Path,
    partition_id: &str,
    diagnostic: &str,
) {
    let partition = canonical_root
        .join("tasks")
        .join("workspaces")
        .join(partition_id);
    let partition = utf8(&partition);
    assert!(
        diagnostic.contains(partition),
        "{label} diagnostic must attribute the task partition {partition}: {diagnostic}"
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
    let commit = crate::git_repo::command()
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
    let added = crate::git_repo::command()
        .args([
            "worktree",
            "add",
            "--quiet",
            "-b",
            "orbit/worktree-fixture",
            utf8(&worktree),
        ])
        .current_dir(work)
        .output()
        .expect("attach linked worktree");
    assert!(added.status.success(), "git worktree add failed: {added:?}");
    assert!(
        worktree.join(".git").is_file(),
        "fixture needs a linked worktree"
    );
    worktree
}

/// The runtimes a session opens are the largest per-call cost on the MCP path
/// (registry records, host identity, and every workspace store), and
/// `HostLifetime::LongLived` promises the server keeps what it opens. Repeated
/// workspace-scoped calls must therefore open exactly one runtime — and must
/// stop reusing it the moment this machine's registry changes underneath the
/// session.
fn workspace_tool_calls_reuse_one_runtime_until_the_registry_changes() {
    let workspace = McpWorkspace::init();
    let log = workspace._temp.path().join("serve-runtime-opens.log");
    let mut client = workspace.serve_logging_runtime_opens(&log);

    assert_eq!(
        client.call_tool_ok("orbit_task_list", json!({}))["tasks"],
        json!([])
    );
    let created = client.call_tool_ok(
        "orbit_task_add",
        json!({
            "title": "runtime reuse",
            "description": "a write and the reads around it share one runtime",
            "complexity": "low",
            "model": "codex",
        }),
    );
    let task_id = id_of(&created);
    client.show_task(&task_id);
    assert_runtime_opens(
        &log,
        1,
        "three workspace-scoped calls must share one runtime",
    );

    // An ordinary registry write from another process: registering a second
    // workspace rewrites `workspaces.json`, and the live session must compose
    // against the file as it now stands rather than what it cached.
    let second = workspace._temp.path().join("second");
    init_checkout(&workspace.home, &second, "second");

    client.show_task(&task_id);
    assert_runtime_opens(
        &log,
        2,
        "a registry edit must invalidate the cached runtime",
    );

    // The rebuilt runtime is reused in turn, and still serves the same state.
    let listed = client.call_tool_ok("orbit_task_list", json!({}));
    assert_eq!(listed["tasks"][0]["id"], Value::String(task_id));
    assert_eq!(runtime_opens(&log), 2);
}

/// A cached runtime carries the crews, default crew, and execution policy it
/// resolved from the layered `config.toml` at open. Before runtimes were kept
/// for the session, every call re-read that configuration; keeping them must
/// not turn an edit to either layer into a setting the session serves stale
/// until restart.
fn workspace_tool_calls_observe_a_config_toml_edit_made_during_the_session() {
    let workspace = McpWorkspace::init();
    let log = workspace._temp.path().join("serve-runtime-opens.log");
    let mut client = workspace.serve_logging_runtime_opens(&log);

    let created = client.call_tool_ok(
        "orbit_task_add",
        json!({
            "title": "config.toml edits reach a live session",
            "description": "the crew projection follows the configuration on disk",
            "complexity": "low",
            "crew": "sol",
        }),
    );
    let task_id = id_of(&created);
    let shown = client.show_task(&task_id);
    assert_eq!(shown["resolved_crew"], json!("sol"));
    let seeded_model = shown["crew_model"]
        .as_str()
        .expect("the seeded crew projects its model")
        .to_string();
    assert_eq!(runtime_opens(&log), 1);

    // Workspace layer: `<checkout>/.orbit/config.toml` did not exist when the
    // runtime was built. Its appearance overrides one crew field.
    let workspace_config = workspace.work.join(".orbit").join("config.toml");
    std::fs::write(
        &workspace_config,
        "[crews.sol]\nmodel = \"gpt-session-override\"\n",
    )
    .expect("write the workspace config layer");
    assert_ne!(seeded_model, "gpt-session-override");
    let shown = client.show_task(&task_id);
    assert_eq!(
        shown["crew_model"],
        json!("gpt-session-override"),
        "a workspace config.toml written after the runtime opened must be served: {shown}"
    );
    assert_eq!(shown["resolved_crew"], json!("sol"));
    assert_runtime_opens(
        &log,
        2,
        "the workspace config edit must invalidate the cached runtime",
    );

    // Global layer: a crew defined in `~/.orbit/config.toml` after the rebuild.
    // Assigning it goes through strict crew validation, which fails closed
    // against a crews table that does not know the name.
    let global_config = workspace.home.join(".orbit").join("config.toml");
    let baseline = std::fs::read_to_string(&global_config).expect("read the seeded config");
    std::fs::write(
        &global_config,
        format!(
            "{baseline}\n[crews.late-arrival]\nprovider = \"codex\"\nmodel = \"gpt-late-arrival\"\n"
        ),
    )
    .expect("define a crew in the global config layer");
    client.call_tool_ok(
        "orbit_task_update",
        json!({ "id": task_id, "crew": "late-arrival" }),
    );
    let shown = client.show_task(&task_id);
    assert_eq!(shown["resolved_crew"], json!("late-arrival"));
    assert_eq!(shown["crew_model"], json!("gpt-late-arrival"));
    assert_runtime_opens(
        &log,
        3,
        "the global config edit must invalidate the cached runtime",
    );

    // With both layers at rest the rebuilt runtime is reused again.
    client.show_task(&task_id);
    assert_eq!(runtime_opens(&log), 3);
}

/// Assert the server has opened exactly `expected` workspace runtimes so far.
fn assert_runtime_opens(log: &Path, expected: usize, why: &str) {
    assert_eq!(
        runtime_opens(log),
        expected,
        "{why}:\n{}",
        std::fs::read_to_string(log).unwrap_or_default()
    );
}

/// How many workspace runtimes the server reported opening, from the tracing
/// target `crates/orbit-cli/src/command/mcp/server/host.rs` emits once per open.
fn runtime_opens(log_path: &Path) -> usize {
    let log = std::fs::read_to_string(log_path).expect("read the server log");
    log.lines()
        .filter(|line| line.contains("opened a workspace runtime"))
        .count()
}

mod upgrade;

mod plugins;
mod serving_hosts;

/// Read-only presentation uses the production host's task filter and audit path.
fn mcp_apps_presentation_reads_explicit_tasks_without_authority_or_workspace_fallback() {
    let workspace = McpWorkspace::init();
    let task_id = author_task(&workspace, "<script>unsafe task title</script>");
    let selector = workspace.work.to_str().expect("workspace path");
    let mut client = workspace.serve();
    let listed = client.tools_list();
    let tools = tools_array(&listed);
    for (name, entrypoint) in [("orbit_ui_open", "global"), ("orbit_ui_inspect", "thread")] {
        let tool = find_tool(tools, name);
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        assert_eq!(
            tool["_meta"]["ui"]["resourceUri"],
            "ui://orbit/control-center/v1/index.html"
        );
        assert_eq!(
            tool["_meta"]["openai/ui"]["entrypoints"][0]["type"],
            entrypoint
        );
    }
    let ordinary = find_tool(tools, "orbit_task_show");
    assert!(
        ordinary.get("_meta").is_none(),
        "ordinary reads remain widget-free"
    );
    let empty = client.call_tool_ok("orbit_ui_open", json!({}));
    assert!(empty["task"].is_null());
    let panel = client.call_tool_ok(
        "orbit_ui_inspect",
        json!({ "workspace": selector, "id": task_id }),
    );
    assert_eq!(panel["schema_version"], 1);
    assert_eq!(panel["workspace"], selector);
    assert_eq!(panel["task"]["id"], task_id);
    assert_eq!(panel["task"]["title"], "<script>unsafe task title</script>");
    assert!(panel["task"]["updated_at"].is_string());
    let ordinary = client.show_task(&task_id);
    assert_eq!(ordinary["title"], panel["task"]["title"]);
    for arguments in [
        json!({"id":task_id}),
        json!({"workspace":selector}),
        json!({"workspace":"", "id":task_id}),
        json!({"workspace":selector,"id":task_id,"actor":"human"}),
        json!({"workspace":selector,"id":task_id,"status":"done"}),
    ] {
        assert_eq!(
            client.call_tool_err("orbit_ui_inspect", arguments)["code"],
            "invalid_input"
        );
    }
    let wrong = client.call_tool_err(
        "orbit_ui_inspect",
        json!({"workspace":"missing-workspace", "id":task_id}),
    );
    assert!(wrong["code"].is_string());
    let elsewhere = workspace.home.join("another-workspace");
    init_checkout(&workspace.home, &elsewhere, "another-workspace");
    client.call_tool_err(
        "orbit_ui_inspect",
        json!({"workspace":elsewhere, "id":task_id}),
    );
    client.call_tool_err(
        "orbit_ui_inspect",
        json!({"workspace":selector, "id":"TST-999999"}),
    );
    let denied_before = client.call_tool_err("orbit_workflow_ship", json!({"task_ids":[task_id]}));
    client.call_tool_ok("orbit_ui_open", json!({"workspace":selector,"id":task_id}));
    let denied_after = client.call_tool_err("orbit_workflow_ship", json!({"task_ids":[task_id]}));
    assert_eq!(denied_before["code"], "capability_denied");
    assert_eq!(denied_after["code"], denied_before["code"]);
    let unchanged = client.show_task(&task_id);
    assert_eq!(unchanged["updated_at"], ordinary["updated_at"]);
    assert_eq!(unchanged["status"], ordinary["status"]);

    let mut federated = federated_client(&workspace);
    let discovered = federated.call_tool_ok("orbit_workspace_list", json!({}));
    let descriptors = array_of(&discovered, "workspaces");
    let qualified = descriptors
        .iter()
        .find(|row| row["repo_root"] == selector)
        .or_else(|| {
            descriptors
                .iter()
                .find(|row| row["name"] == "mcp-roundtrip")
        })
        .expect("local descriptor")["selector"]
        .clone();
    assert!(
        qualified.is_string(),
        "copy a returned selector: {discovered}"
    );
    let panel = federated.call_tool_ok(
        "orbit_ui_inspect",
        json!({"workspace":qualified,"id":task_id}),
    );
    assert_eq!(panel["workspace"], qualified);
    assert_eq!(panel["task"]["id"], task_id);
    let bare = federated.call_tool_err(
        "orbit_ui_inspect",
        json!({"workspace":selector,"id":task_id}),
    );
    assert_eq!(bare["code"], "unknown_selector");
    let static_resource = federated.request(
        "resources/read",
        json!({"uri":"ui://orbit/control-center/v1/index.html"}),
    );
    assert_eq!(
        static_resource["result"]["contents"][0]["mimeType"],
        "text/html;profile=mcp-app"
    );
}

mod desktop;

mod root_selection;
mod transport_operations;

mod internal_drain;
mod tool_input_hints;

#[cfg(unix)]
#[test]
fn agent_invoke_wait_deadline_is_bounded_and_leaves_the_run_observable() {
    let workspace = McpWorkspace::init();
    plant_both_providers(
        &workspace,
        &format!("#!/bin/sh\ncat > /dev/null\nsleep 2\nprintf '%s\\n' '{AGENT_ANSWER_ENVELOPE}'\n"),
    );
    admit_fixture_claude_worker_token(&workspace);
    let mut client = workspace.serve_with_args_and_env(
        &["--operator"],
        &[(FIXTURE_CLAUDE_WORKER_TOKEN_ENV, FIXTURE_CLAUDE_WORKER_TOKEN)],
    );
    let tool = client.listed_tool("orbit_agent_invoke");
    let properties = &tool["inputSchema"]["properties"];
    assert_eq!(properties["wait_seconds"]["minimum"], 0);
    assert_eq!(properties["wait_seconds"]["maximum"], 600);
    assert_eq!(
        properties["timeout_seconds"]["maximum"],
        orbit_core::application::job::MAX_AGENT_INVOKE_TIMEOUT_SECONDS
    );
    assert!(
        properties["timeout_seconds"]["default"].is_null(),
        "timeout_seconds has no schema default because operator and explorer ceilings differ"
    );
    for invalid in [json!(601), json!(-1), json!(0.5), json!("1")] {
        let error = client.call_tool_err(
            "orbit_agent_invoke",
            json!({"prompt":"invalid wait", "cwd":workspace.work,"wait_seconds":invalid}),
        );
        assert_eq!(error["code"], "invalid_input", "{error}");
    }
    let listed = client.call_tool_ok("orbit_workflow_run_list", json!({}));
    assert_eq!(
        listed["items"],
        json!([]),
        "invalid wait must not submit a run"
    );
    let request = json!({"prompt":"bounded wait", "cwd":workspace.work,"wait_seconds":0,"timeout_seconds":30,"idempotency_key":"bounded-wait"});
    let pending = client.call_tool_ok("orbit_agent_invoke", request.clone());
    assert_eq!(pending["answer"], Value::Null, "{pending}");
    let run_id = pending["run_id"].as_str().unwrap();
    let observed = client.call_tool_ok("orbit_workflow_run_show", json!({"id":run_id}));
    let (machine_id, machine_name) = machine_identity(&workspace.home);
    assert_eq!(observed["executed_on"]["machine_id"], machine_id);
    assert_eq!(observed["executed_on"]["machine_name"], machine_name);
    assert!(
        matches!(observed["state"].as_str(), Some("pending" | "running")),
        "a client deadline leaves the run active: {observed}"
    );
    let mut retry = request;
    retry["wait_seconds"] = json!(30);
    let answer = client.call_tool_ok("orbit_agent_invoke", retry);
    assert_eq!(answer["run_id"], run_id);
    assert_eq!(answer["deduplicated"], true);
    assert_eq!(answer["state"], "success", "{answer}");
    assert_eq!(answer["answer"]["summary"], "remote probe complete");
}

mod search;
#[cfg(unix)]
mod worker_routing;

#[test]
fn governed_session_admission() {
    rows!(
        mcp_server_advertises_governed_tools_but_denies_an_unprivileged_session,
        a_replica_mcp_session_enforces_checkout_capability_classes,
        an_operator_served_mcp_session_reaches_a_governed_tool
    );
}

#[test]
fn forwarded_caller_authorization() {
    rows!(
        a_remote_originated_operator_session_reaches_a_governed_tool,
        a_remote_originated_agent_session_is_refused_a_governed_tool,
        the_forwarded_caller_label_reaches_the_destinations_authorization_audit_row
    );
}

#[test]
fn agent_invocation_envelopes() {
    rows!(
        leftover_caller_authorization_files_are_ignored_rather_than_enforced,
        a_remote_operator_session_invokes_an_agent_end_to_end,
        an_invocation_without_a_response_envelope_fails_with_its_reason
    );
}

#[test]
fn session_authority_and_unbound_discovery() {
    rows!(
        a_remote_agent_session_cannot_invoke_an_agent,
        a_local_session_keeps_its_argv_authority,
        mcp_serve_lists_the_canonical_surface_outside_any_checkout
    );
}

#[test]
fn global_artifacts_and_workspace_selectors() {
    rows!(
        task_artifact_get_is_global_by_default_across_tool_run_task_cli_and_mcp,
        every_workspace_scoped_tool_behavior_matches_its_advertised_selector_requirement,
        uninitialized_unbound_mcp_launch_gives_setup_guidance_without_operator_authority
    );
}

#[test]
fn transport_peer_identity() {
    rows!(
        ssh_marked_mcp_server_audits_caller_and_server_identity_separately,
        mcp_listen_round_trips_over_a_loopback_socket_and_audits_the_peer_ip,
        mcp_listen_workspace_flag_binds_the_sessions_it_accepts
    );
}

/// A `regression_from` link carries its typed nature and escape through
/// `orbit_task_add`, `orbit_task_update` and `orbit_task_show`; an unclassified
/// link reads back without the payload, which an explicit `unclassified` keeps.
fn mcp_regression_from_classification_round_trips_and_refuses_unknown_values() {
    let (workspace, mut client) = McpWorkspace::served();
    let culprit = author_task(&workspace, "culprit delivery");
    let relations = |id: &str, client: &mut McpClient| {
        client.call_tool_ok(
            "orbit_task_show",
            json!({ "id": id, "fields": ["relations"] }),
        )["items"]
            .clone()
    };

    let classified = client.call_tool_ok(
        "orbit_task_add",
        json!({
            "title": "Classified regression",
            "description": "Filed by a review finding",
            "complexity": "low",
            "relations": [{
                "type": "regression_from",
                "target": culprit,
                "regression": {"nature": "defect_in_new", "escape": "test_not_selected"},
            }],
        }),
    )["id"]
        .as_str()
        .expect("task id")
        .to_string();
    assert_eq!(
        relations(&classified, &mut client),
        json!([{
            "type": "regression_from",
            "target": culprit,
            "regression": {"nature": "defect_in_new", "escape": "test_not_selected"},
        }])
    );
    let output = workspace.run_ok(&["task", "show", &classified]);
    let stdout = stdout(&output);
    assert!(
        stdout.contains("(nature: defect_in_new, escape: test_not_selected)"),
        "the human show names the classification: {stdout}"
    );

    // An unclassified link — the shape every link stored before the payload
    // existed has — reads back without one; an explicit `unclassified`
    // payload stays distinct from that absent payload.
    for expected in [
        json!([{"type": "regression_from", "target": culprit}]),
        json!([{
            "type": "regression_from",
            "target": culprit,
            "regression": {"nature": "unclassified", "escape": "unclassified"},
        }]),
    ] {
        client.call_tool_ok(
            "orbit_task_update",
            json!({ "id": classified, "relations": expected }),
        );
        assert_eq!(relations(&classified, &mut client), expected);
    }

    let unknown = client.call_tool_err(
        "orbit_task_update",
        json!({
            "id": classified,
            "relations": [{
                "type": "regression_from",
                "target": culprit,
                "regression": {"nature": "flaky", "escape": "no_test"},
            }],
        }),
    );
    let message = unknown.to_string();
    for allowed in [
        "broke_existing",
        "merge_reintroduced",
        "contract_drift",
        "unclassified",
    ] {
        assert!(
            message.contains(allowed),
            "the refusal lists `{allowed}`: {message}"
        );
    }
    let misplaced = client.call_tool_err(
        "orbit_task_add",
        json!({
            "title": "Misplaced classification",
            "description": "Only regression_from is classified",
            "complexity": "low",
            "relations": [{
                "type": "related_to",
                "target": culprit,
                "regression": {"nature": "incomplete", "escape": "review_missed"},
            }],
        }),
    );
    assert!(
        misplaced
            .to_string()
            .contains("only regression_from accepts one"),
        "{misplaced}"
    );
    assert_eq!(
        relations(&classified, &mut client)[0]["regression"]["nature"],
        "unclassified",
        "a refused write leaves the stored classification untouched"
    );
}

#[test]
fn record_mutation_and_error_protocol() {
    rows!(
        mcp_serve_round_trips_records_against_a_temp_workspace,
        mcp_regression_from_classification_round_trips_and_refuses_unknown_values,
        mcp_serve_error_paths_return_tool_errors_and_keep_serving,
        mcp_workspace_argument_must_be_a_string_and_a_blank_one_defers_to_the_session
    );
}

/// `orbit_task_eligible` over stdio is advertised read-only, answers a task
/// that holds nothing, and refuses a holder status or an unknown filter.
/// Lock overlap stays on the CLI eligibility test.
fn mcp_task_eligible_is_advertised_read_only_and_answers_over_stdio() {
    let (workspace, mut client) = McpWorkspace::served();
    let task_id = author_task(&workspace, "eligibility probe");

    let tool = client.listed_tool("orbit_task_eligible");
    assert_eq!(tool["annotations"]["readOnlyHint"], true, "{tool}");
    let properties = tool["inputSchema"]["properties"]
        .as_object()
        .expect("input properties");
    for option in ["status", "path", "limit", "explain"] {
        assert!(properties.contains_key(option), "{option}: {tool}");
    }

    // A proposed task declaring no context holds nothing, so nothing in flight
    // can collide with it.
    let eligible = client.call_tool_ok("orbit_task_eligible", json!({ "explain": true }));
    assert_eq!(eligible["tasks"][0]["id"], task_id, "{eligible}");
    assert_eq!(eligible["total"], 1);
    assert_eq!(eligible["truncated"], false);
    assert_eq!(eligible["conflicting"], json!([]));

    // Holders are not candidates, and the tool takes no filters beyond its own.
    for input in [json!({ "status": "review" }), json!({ "tag": "cli" })] {
        let refused = client.call_tool_err("orbit_task_eligible", input.clone());
        assert_eq!(refused["code"], "invalid_input", "{input}: {refused}");
    }
}

#[test]
fn task_projection_search_and_audit() {
    rows!(
        mcp_task_list_fields_projects_each_task_to_an_object_of_just_those_fields,
        task_mutations_are_immediately_searchable_from_the_cli_and_mcp,
        mcp_calls_are_audited_once_including_unknown_raw_names,
        mcp_task_eligible_is_advertised_read_only_and_answers_over_stdio
    );
}

#[test]
fn worktree_and_federated_routing() {
    rows!(
        worktree_backed_activity_routes_task_and_search_by_advertised_workspace_argument,
        managed_mcp_config_updates_a_task_without_a_workspace_argument,
        federated_mcp_serve_requires_the_machine_qualified_list_selector
    );
}

#[test]
fn federated_local_destinations() {
    rows!(
        federated_mcp_serve_lists_and_routes_local_workspaces_without_destinations,
        direct_and_federated_local_calls_record_equivalent_audit_contexts,
        federated_mcp_serve_collapses_an_explicit_local_destination_row
    );
}

#[test]
fn federated_host_configuration() {
    rows!(
        federated_mcp_serve_lists_local_workspaces_beside_unreachable_remotes,
        federated_mcp_serve_reads_the_host_file_and_refuses_both_host_files,
        federated_mcp_serve_rejects_a_machine_id_only_destination_row
    );
}

#[test]
fn managed_environment_binding() {
    rows!(
        managed_mcp_env_binding_updates_without_a_workspace_argument,
        managed_codex_mcp_env_enforces_activity_tools_and_agent_actor_kind,
        unmanaged_orbit_workspace_env_does_not_bind_mcp
    );
}

#[test]
fn global_task_reads_and_source_inspection() {
    rows!(
        managed_source_inspection_mcp_search_uses_the_dispatching_workspace,
        task_show_is_global_by_default_across_tool_run_and_mcp,
        task_read_surfaces_tolerate_a_crew_this_host_does_not_define
    );
}

#[test]
fn live_runtime_and_app_presentation() {
    rows!(
        workspace_tool_calls_reuse_one_runtime_until_the_registry_changes,
        workspace_tool_calls_observe_a_config_toml_edit_made_during_the_session,
        mcp_apps_presentation_reads_explicit_tasks_without_authority_or_workspace_fallback
    );
}

/// Make `dir` unwritable and verify with `probe` that this host honors the
/// mode bits. Returns the original permissions to restore, or `None` when a
/// writable probe means this host cannot exercise the creation denial.
#[cfg(unix)]
fn deny_dir_writes(
    dir: &Path,
    probe: impl FnOnce() -> std::io::Result<()>,
) -> Option<std::fs::Permissions> {
    use std::os::unix::fs::PermissionsExt;
    let original = std::fs::metadata(dir)
        .expect("fixture dir metadata")
        .permissions();
    std::fs::set_permissions(
        dir,
        std::fs::Permissions::from_mode(read_only_fixture_mode(0o500)),
    )
    .expect("deny fixture dir writes");
    match probe() {
        Ok(()) => {
            std::fs::set_permissions(dir, original).expect("restore fixture dir permissions");
            None
        }
        Err(error) => {
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied,
                "the fixture must refuse writes through permissions"
            );
            Some(original)
        }
    }
}

#[cfg(unix)]
fn read_only_fixture_mode(mode: u32) -> u32 {
    // The subprocess regression leaves owner writes enabled to deterministically
    // reproduce a host (such as root) that can write despite read-only mode bits.
    if std::env::var("ORBIT_TEST_WRITABLE_READ_ONLY_FIXTURE").as_deref() == Ok("1") {
        mode | 0o200
    } else {
        mode
    }
}

#[cfg(unix)]
#[test]
fn observational_read_only_stores() {
    bool_rows!(
        absent_unwritable_partition_keeps_cli_tool_reads_observational,
        read_only_registry_files_keep_uncheckpointed_wal_reads_observational,
        absent_global_semantic_index_keeps_read_only_cli_reads_observational
    );
}

#[cfg(unix)]
#[test]
fn observational_read_only_stores_report_unavailable_coverage() {
    const TEST: &str = "mcp_roundtrip::observational_read_only_stores";
    let root = tempdir().expect("read-only coverage regression scratch");
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .args(["--exact", TEST, "--nocapture"])
        .env("ORBIT_TEST_WRITABLE_READ_ONLY_FIXTURE", "1");
    let output = test_env::run_child_test(&mut command, TEST, root.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut remaining: Vec<&str> = stderr.lines().collect();
    for name in [
        "absent_unwritable_partition_keeps_cli_tool_reads_observational",
        "read_only_registry_files_keep_uncheckpointed_wal_reads_observational",
        "absent_global_semantic_index_keeps_read_only_cli_reads_observational",
    ] {
        let notice = format!("SKIP: {name}: host permits writes to the read-only fixture");
        assert!(
            remaining.contains(&notice.as_str()),
            "missing {notice}: {stderr}"
        );
        remaining.retain(|line| *line != notice);
    }
    assert!(
        !stderr
            .lines()
            .any(|line| line.starts_with("qa-scenario-pass:")),
        "unavailable read-only coverage must receive no scenario-pass credit: {stderr}"
    );
    // Consume only the three intentional skips checked above; forward any
    // unexpected notices through the normal child-test contract.
    test_env::assert_child_test_passed(TEST, output.status, output.stdout, remaining.join("\n"));
}
