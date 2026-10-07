//! [ORB-14449] Task ids route to the host their prefix names, and `--host`
//! names the host for `--workspace` and `--pull`.
//!
//! Host A is the fleet's local install (`LB`); host B is a second install
//! (`BR`) reached through the fake `ssh` (see `host_fleet`). Everything B
//! reports — its workspace, its refusals — is what B's own binary answered.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde_json::{Value, json};

use crate::git_repo;
use crate::host_fleet::{Fleet, Install};

/// A fleet whose local host has registered `bravo`, with one task on each.
struct Routed {
    fleet: Fleet,
    bravo: Install,
    remote_task: String,
    local_task: String,
}

impl Routed {
    fn new() -> Self {
        let fleet = Fleet::new();
        let bravo = fleet.install("bravo", "bravo", "BR");
        fleet.route("bravo", &bravo, "plain");
        fleet.json(&["host", "add", "bravo", "--json"]);
        let remote_task = add_task(&fleet, &bravo.home, &bravo.repo(), "held by bravo");
        let local_task = add_task(&fleet, &fleet.local.home, &fleet.local_repo, "held here");
        assert!(
            remote_task.starts_with("BR-"),
            "bravo mints BR ids: {remote_task}"
        );
        assert!(
            local_task.starts_with("LB-"),
            "this host mints LB ids: {local_task}"
        );
        Self {
            fleet,
            bravo,
            remote_task,
            local_task,
        }
    }

    /// Run on host A from its checkout and parse the JSON answer.
    fn json(&self, args: &[&str]) -> Value {
        let output = self.fleet.orbit_ok(&self.fleet.local_repo, args);
        parse(&output.stdout, args)
    }

    /// Run on host A from its checkout, expecting a refusal; returns
    /// `(code, message)`.
    fn refused(&self, args: &[&str]) -> (String, String) {
        self.refused_with(args, &[])
    }

    fn refused_with(&self, args: &[&str], env: &[(&str, &str)]) -> (String, String) {
        let mut command = self
            .fleet
            .orbit_in(&self.fleet.local.home, &self.fleet.local_repo, args);
        for (name, value) in env {
            command.env(name, value);
        }
        let output = command.output().expect("spawn orbit");
        assert!(!output.status.success(), "{args:?} must be refused");
        let error: Value = serde_json::from_slice(&output.stderr)
            .or_else(|_| serde_json::from_slice(&output.stdout))
            .unwrap_or_else(|_| {
                panic!(
                    "{args:?} error JSON: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
            });
        (
            error["code"].as_str().unwrap_or_default().to_string(),
            error["error"]
                .as_str()
                .or(error["message"].as_str())
                .unwrap_or_default()
                .to_string(),
        )
    }

    /// The comment bodies on `id` as bravo itself reads them.
    fn comments_on_bravo(&self, id: &str) -> Vec<String> {
        comments_in(&self.fleet, &self.bravo.home, &self.bravo.repo(), id)
    }

    /// The comment bodies on `id` in host A's own store.
    fn comments_here(&self, id: &str) -> Vec<String> {
        comments_in(
            &self.fleet,
            &self.fleet.local.home,
            &self.fleet.local_repo,
            id,
        )
    }

    /// The remote commands dialed since the last call, then forget them.
    fn take_remote_argv(&self) -> Vec<String> {
        let log = self.fleet.routes.with_extension("argv");
        let lines = fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(ToOwned::to_owned)
            .collect();
        fs::write(&log, "").expect("reset the argv log");
        lines
    }

    /// The raw host-qualified token for bravo's workspace, as federated
    /// discovery lists it.
    fn bravo_selector(&self) -> String {
        let shown = self.fleet.json(&["host", "show", "bravo", "--json"]);
        let workspace = shown["workspaces"]
            .as_array()
            .and_then(|workspaces| workspaces.iter().find(|ws| ws["name"] == "bravo-ws"))
            .unwrap_or_else(|| panic!("bravo lists bravo-ws: {shown}"));
        format!(
            "{}/{}",
            self.bravo.machine_id,
            workspace["id"].as_str().expect("workspace id")
        )
    }
}

fn parse(stdout: &[u8], args: &[&str]) -> Value {
    serde_json::from_slice(stdout).unwrap_or_else(|error| {
        panic!(
            "{args:?} JSON: {error}: {}",
            String::from_utf8_lossy(stdout)
        )
    })
}

fn add_task(fleet: &Fleet, home: &Path, repo: &Path, title: &str) -> String {
    let output = fleet
        .orbit_in(
            home,
            repo,
            &[
                "task",
                "add",
                "--title",
                title,
                "--complexity",
                "low",
                "--json",
            ],
        )
        .output()
        .expect("spawn orbit");
    assert!(
        output.status.success(),
        "task add: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let added = parse(&output.stdout, &["task", "add"]);
    added["id"]
        .as_str()
        .unwrap_or_else(|| panic!("task add reports an id: {added}"))
        .to_string()
}

fn comments_in(fleet: &Fleet, home: &Path, repo: &Path, id: &str) -> Vec<String> {
    let args = ["task", "show", id, "--fields", "comments", "--json"];
    let output = fleet
        .orbit_in(home, repo, &args)
        .output()
        .expect("spawn orbit");
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let comments = parse(&output.stdout, &args);
    comments
        .as_array()
        .unwrap_or_else(|| panic!("comments are a list: {comments}"))
        .iter()
        .filter_map(|comment| {
            ["body", "text", "content", "message"]
                .iter()
                .find_map(|key| comment[*key].as_str())
        })
        .map(ToOwned::to_owned)
        .collect()
}

#[test]
fn cli_task_ids_route_to_the_host_their_prefix_names() {
    let routed = Routed::new();
    let remote = routed.remote_task.as_str();
    routed.take_remote_argv();

    let shown = routed.json(&["task", "show", remote, "--json"]);
    assert_eq!(shown["id"], remote);
    assert_eq!(shown["host"]["name"], "bravo", "B answered: {shown}");
    assert_eq!(shown["host"]["machine_id"], routed.bravo.machine_id);
    assert_eq!(
        shown["workspace"]["name"], "bravo-ws",
        "the answer reports B's workspace: {shown}"
    );

    let updated = routed.json(&[
        "task",
        "update",
        remote,
        "--comment",
        "routed from A",
        "--json",
    ]);
    assert_eq!(updated["host"]["name"], "bravo");
    assert!(
        routed
            .comments_on_bravo(remote)
            .contains(&"routed from A".to_string()),
        "the comment landed in B's store"
    );

    let tool = routed.json(&[
        "tool",
        "run",
        "orbit.task.show",
        "--input",
        &json!({ "id": remote }).to_string(),
    ]);
    assert_eq!(tool["id"], remote, "`tool run` routes the same way: {tool}");
    assert!(
        !routed.take_remote_argv().is_empty(),
        "the remote calls went over SSH"
    );

    let local = routed.json(&["task", "show", &routed.local_task, "--json"]);
    assert_eq!(local["id"], routed.local_task);
    assert!(local.get("host").is_none(), "a local id runs in-process");
    assert!(
        routed.take_remote_argv().is_empty(),
        "a local-prefix id opens no remote session"
    );

    let (code, message) = routed.refused(&["task", "show", "ZZ-1", "--json"]);
    assert_eq!(code, "unknown_task_prefix", "{message}");
    assert!(
        message.contains("ZZ"),
        "the refusal names the prefix: {message}"
    );
    assert!(
        routed.take_remote_argv().is_empty(),
        "an unregistered prefix dials no host"
    );

    routed.fleet.unroute("bravo");
    let (code, message) = routed.refused(&["task", "show", remote, "--json"]);
    assert_eq!(
        code, "owner_unreachable",
        "an unreachable owner is not a missing task: {message}"
    );
    assert!(
        message.contains("bravo"),
        "the refusal names the host: {message}"
    );
}

#[test]
fn federated_mcp_routes_an_id_only_call_by_prefix() {
    let routed = Routed::new();
    let remote = routed.remote_task.as_str();
    let mut client = StdioClient::spawn(&routed.fleet, &["mcp", "serve", "--mode", "federated"]);

    let shown = client.call_ok("orbit_task_show", json!({ "id": remote }));
    assert_eq!(shown["id"], remote);
    assert_eq!(
        shown["workspace"]["name"], "bravo-ws",
        "an id-only federated read reaches B: {shown}"
    );
    client.call_ok(
        "orbit_task_update",
        json!({ "id": remote, "comment": "routed over federated MCP" }),
    );
    assert!(
        routed
            .comments_on_bravo(remote)
            .contains(&"routed over federated MCP".to_string()),
        "the federated write landed in B's store"
    );

    routed.take_remote_argv();
    let local = client.call_ok("orbit_task_show", json!({ "id": routed.local_task }));
    assert_eq!(local["id"], routed.local_task);
    assert!(
        routed.take_remote_argv().is_empty(),
        "a local-prefix id is served in-process"
    );

    let unknown = client.call_err("orbit_task_show", json!({ "id": "ZZ-1" }));
    assert_eq!(unknown["code"], "unknown_task_prefix", "{unknown}");
    drop(client);

    // A v1 server does not relay: it names the holder instead of reporting
    // the task missing.
    let mut v1 = StdioClient::spawn(&routed.fleet, &["mcp", "serve"]);
    routed.take_remote_argv();
    let refused = v1.call_err("orbit_task_show", json!({ "id": remote }));
    assert_eq!(refused["code"], "task_prefix_remote", "{refused}");
    assert!(
        refused["message"]
            .as_str()
            .is_some_and(|message| message.contains("bravo")),
        "the refusal names the holder: {refused}"
    );
    assert!(
        routed.take_remote_argv().is_empty(),
        "a v1 server dials nothing"
    );
}

#[test]
fn a_routed_call_carries_the_callers_authority_and_no_more() {
    let routed = Routed::new();
    let remote = routed.remote_task.as_str();
    let reset = [
        "task",
        "review-reset",
        remote,
        "--lineage",
        "review:none",
        "--reason",
        "routed",
        "--json",
    ];
    routed.take_remote_argv();

    // An agent that also sets the operator override is still an agent: the
    // destination is asked for no operator session and refuses an
    // operator-only call as it refuses any agent's.
    let (code, message) = routed.refused_with(
        &reset,
        &[
            ("ORBIT_OPERATOR", "1"),
            ("ORBIT_AGENT_NAME", "routing-test"),
        ],
    );
    let dialed = routed.take_remote_argv();
    assert!(
        !dialed.is_empty() && dialed.iter().all(|line| !line.contains("--operator")),
        "an agent session never asks for operator authority: {dialed:?}"
    );
    assert!(
        ["capability_denied", "capability_refused"].contains(&code.as_str()),
        "B refuses the agent: {code}: {message}"
    );

    // The same call as an operator gets past authorization on B and fails
    // on the missing lineage instead.
    let (operator_code, operator_message) = routed.refused_with(&reset, &[("ORBIT_OPERATOR", "1")]);
    let dialed = routed.take_remote_argv();
    assert!(
        dialed.iter().any(|line| line.contains("--operator")),
        "an operator CLI is an operator on B: {dialed:?}"
    );
    assert_ne!(
        operator_code, code,
        "the operator is not refused for its authority: {operator_message}"
    );
}

#[test]
fn a_write_through_a_mirror_selector_is_refused_by_that_hosts_sole_writer_rule() {
    let routed = Routed::new();
    // B keeps a replica checkout of A's workspace: a mirror of A's tasks.
    let mirror = routed.bravo.home.join("mirror-of-a");
    git_repo::init(&mirror);
    let init = routed
        .fleet
        .orbit_in(
            &routed.bravo.home,
            &mirror,
            &[
                "workspace",
                "init",
                "--name",
                "a-mirror",
                "--role",
                "replica",
                "--owner",
                &routed.fleet.local.machine_id,
            ],
        )
        .output()
        .expect("spawn orbit");
    assert!(
        init.status.success(),
        "replica init on B: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let local = routed.local_task.as_str();
    let (code, message) = routed.refused(&[
        "task",
        "update",
        local,
        "--host",
        "bravo",
        "--workspace",
        "a-mirror",
        "--comment",
        "written on a mirror",
        "--json",
    ]);
    // The route refuses on the capability classes B advertises for its
    // replica checkout, before any write is delivered.
    assert_eq!(code, "capability_refused", "B's replica refuses: {message}");
    assert!(
        message.contains("control_plane"),
        "the refusal is the replica's write class: {message}"
    );
    // And B's own store refuses the same write when it is handed one.
    let direct = routed
        .fleet
        .orbit_in(
            &routed.bravo.home,
            &mirror,
            &[
                "task",
                "update",
                local,
                "--workspace",
                "a-mirror",
                "--comment",
                "x",
                "--json",
            ],
        )
        .output()
        .expect("spawn orbit");
    assert!(!direct.status.success(), "B refuses a write on its mirror");
    let refusal: Value = serde_json::from_slice(&direct.stderr)
        .or_else(|_| serde_json::from_slice(&direct.stdout))
        .expect("refusal JSON");
    assert_eq!(refusal["code"], "capability_refused", "{refusal}");
    assert!(
        refusal["error"]
            .as_str()
            .is_some_and(|error| error.contains(&routed.fleet.local.machine_id)),
        "B names the task's writer, this host: {refusal}"
    );
    assert!(
        !routed
            .comments_here(local)
            .contains(&"written on a mirror".to_string()),
        "nothing reached the owner's copy"
    );
}

#[test]
fn host_flag_resolves_to_the_selector_that_host_lists() {
    let routed = Routed::new();
    let remote = routed.remote_task.as_str();
    let selector = routed.bravo_selector();

    let by_host = routed.json(&[
        "task",
        "show",
        remote,
        "--host",
        "bravo",
        "--workspace",
        "bravo-ws",
        "--json",
    ]);
    let by_selector = routed.json(&["task", "show", remote, "--workspace", &selector, "--json"]);
    assert_eq!(by_host, by_selector, "`--host` is the raw selector");
    assert_eq!(by_host["workspace"]["name"], "bravo-ws");

    let list_input = |workspace: &str| json!({ "workspace": workspace }).to_string();
    let by_host = routed.json(&[
        "tool",
        "run",
        "orbit.task.list",
        "--host",
        &routed.bravo.machine_id,
        "--input",
        &list_input("bravo-ws"),
    ]);
    let by_selector = routed.json(&[
        "tool",
        "run",
        "orbit.task.list",
        "--input",
        &list_input(&selector),
    ]);
    assert_eq!(
        by_host, by_selector,
        "a host's machine id resolves the same way"
    );

    // `--pull` resolves to the same selector, so both forms meet the same
    // pull admission on this (non-replica) checkout.
    let pull = |args: &[&str]| {
        let output = routed.fleet.orbit(&routed.fleet.local_repo, args);
        assert!(
            !output.status.success(),
            "{args:?}: this checkout is no replica"
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    assert_eq!(
        pull(&["run", "auto", "--pull", "bravo-ws", "--host", "bravo"]),
        pull(&["run", "auto", "--pull", &selector]),
        "`--pull <ws> --host` meets the raw selector's admission"
    );

    routed.take_remote_argv();
    let (code, _) = routed.refused(&[
        "task",
        "update",
        remote,
        "--host",
        "nowhere",
        "--workspace",
        "bravo-ws",
        "--comment",
        "never",
        "--json",
    ]);
    assert_eq!(code, "unknown_host");
    assert!(
        routed.take_remote_argv().is_empty(),
        "an unknown host dials nothing"
    );

    let (code, message) = routed.refused(&[
        "task",
        "update",
        remote,
        "--host",
        "bravo",
        "--workspace",
        "no-such-ws",
        "--comment",
        "never",
        "--json",
    ]);
    assert_eq!(code, "stale_route", "{message}");
    assert!(
        message.contains("bravo-ws"),
        "the refusal lists what the host does list: {message}"
    );
    assert!(
        !routed
            .comments_on_bravo(remote)
            .contains(&"never".to_string()),
        "a refused resolution delivers nothing"
    );

    let (code, message) = routed.refused(&["run", "auto", "--pull", "bravo-ws", "--json"]);
    assert!(
        message.contains("--host"),
        "a bare `--pull` workspace asks for its host: {code}: {message}"
    );

    let output = routed.fleet.orbit(
        &routed.fleet.local_repo,
        &["workspace", "list", "--host", "bravo"],
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "a host-local command takes no --host"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ssh bravo orbit workspace list"),
        "the usage error says where to run it: {stderr}"
    );

    routed.fleet.unroute("bravo");
    let (code, message) = routed.refused(&[
        "task",
        "show",
        remote,
        "--host",
        "bravo",
        "--workspace",
        "bravo-ws",
        "--json",
    ]);
    assert_eq!(code, "unreachable_destination", "{message}");
}

/// A line-delimited MCP client over a spawned server's stdio.
struct StdioClient {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    lines: Receiver<String>,
    next_id: i64,
}

impl StdioClient {
    fn spawn(fleet: &Fleet, args: &[&str]) -> Self {
        let mut child = fleet
            .process(&fleet.local.home, &fleet.local_repo, args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn MCP server");
        let stdin = child.stdin.take().expect("server stdin");
        let stdout = child.stdout.take().expect("server stdout");
        let (sender, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            stdin,
            lines,
            next_id: 0,
        };
        client.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "host-routing", "version": "0" },
            }),
        );
        client.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        client
    }

    fn send(&mut self, message: &Value) {
        let mut line = message.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .expect("write to server");
        self.stdin.flush().expect("flush server stdin");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(60))
                .unwrap_or_else(|error| panic!("no answer to {method}: {error}"));
            let Ok(response) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if response["id"].as_i64() == Some(id) {
                return response;
            }
        }
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        response
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("{name} returned no result: {response}"))
    }

    fn call_ok(&mut self, name: &str, arguments: Value) -> Value {
        let result = self.call(name, arguments);
        assert_eq!(result["isError"], false, "{name} failed: {result}");
        result["structuredContent"].clone()
    }

    fn call_err(&mut self, name: &str, arguments: Value) -> Value {
        let result = self.call(name, arguments);
        assert_eq!(result["isError"], true, "{name} succeeded: {result}");
        result["structuredContent"].clone()
    }
}

impl Drop for StdioClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
