//! Hydration through the operator CLI reads a follower run over the
//! authenticated federated route and leaves an orphaned leaf, its admitting
//! drain and its reservation stored as they were.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_common::test_env;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::OrbitRuntimeRoots;
use orbit_mcp::federated::{Destination, FederatedMcpHost, SshDestinationProbe};
use orbit_types::tool::ToolSessionContext;
use rusqlite::Connection;
use serde_json::{Value, json};

use super::isolated_test;

const LEAF_RUN: &str = "jrun-hydrate-orphan";
const DRAIN_RUN: &str = "follower-drain";
const REQUEST_ID: &str = "hydrate-observation";

#[test]
fn hydration_through_operator_route_leaves_an_orphaned_leaf_unchanged() {
    if isolated_test::run_in_child(
        "hydrate_follower_observation::hydration_through_operator_route_leaves_an_orphaned_leaf_unchanged",
    ) {
        return;
    }

    let owner = Host::init("owner-host", "OWN");
    let follower = Host::init("follower-host", "FLW");
    fs::write(owner.repo.join("work.rs"), "fn work() {}\n").unwrap();
    let task_id = owner.orbit_json(
        &owner.repo,
        &[
            "task",
            "add",
            "--title",
            "Hydrate an orphaned leaf",
            "--description",
            "The follower still holds the leaf that admitted this claim.",
            "--acceptance-criteria",
            "Hydration reports the nonterminal leaf without changing it.",
            "--complexity",
            "low",
            "--context",
            "file:work.rs",
            "--json",
        ],
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    owner.orbit(
        &owner.repo,
        &[
            "task",
            "update",
            &task_id,
            "--plan",
            "Observe the follower leaf through the operator route.",
        ],
    );
    owner.orbit(
        &owner.repo,
        &["task", "update", &task_id, "--status", "backlog"],
    );

    let bin = follower.home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    install_orbit_wrapper(&bin);
    install_exec_ssh(&bin, &owner.home, &owner.repo);
    let path = stub_first_path(&bin);

    let selector = format!("{}/{}", owner.machine_id, owner.logical_id);
    let probe = federated(&owner.machine_id, &follower.machine_id, &path);
    let probed = call(&probe, &selector, "orbit.drain.probe", json!({}));
    assert!(
        probed["refusal"].is_null(),
        "probe refused admission: {probed}"
    );
    let admitted = call(&probe, &selector, "orbit.task.pull", pull_request(&probed));
    let claim = admitted["receipt"]["claim"].clone();
    let claim_id = claim["claim_id"]
        .as_str()
        .unwrap_or_else(|| panic!("pull admitted no claim: {admitted}"))
        .to_string();
    assert_eq!(claim["task_id"], task_id, "{admitted}");
    let bound = call(
        &probe,
        &selector,
        "orbit.drain.claim.bind",
        json!({"claim_id": claim_id, "run_id": LEAF_RUN, "ship": probed["ship"]}),
    );
    assert_eq!(bound["phase"], "running", "{bound}");
    let settled = call(
        &probe,
        &selector,
        "orbit.drain.claim.settle",
        json!({
            "claim_id": claim_id,
            "run_id": LEAF_RUN,
            "settlement": {"Fail": {
                "summary": "leaf ended before hydration",
                "comment": null,
                "artifacts": []
            }}
        }),
    );
    assert_eq!(settled["phase"], "failed", "{settled}");
    drop(probe);

    let before_task = owner.orbit_json(&owner.repo, &["task", "show", &task_id, "--json"]);
    let before_settlements = owner.orbit_json(
        &owner.repo,
        &["run", "settlements", "--no-reconcile", "--json"],
    );
    let settlement = &before_settlements["settlements"][0];
    assert_eq!(settlement["task_id"], task_id, "{before_settlements}");
    assert_eq!(settlement["leaf_run_id"], LEAF_RUN, "{before_settlements}");
    assert_eq!(
        settlement["drain_run_id"], DRAIN_RUN,
        "{before_settlements}"
    );
    assert!(
        settlement["run_evidence"]["provenance"].is_null(),
        "the settlement must still need observation: {before_settlements}"
    );
    let claim_machine = settlement["machine_id"]
        .as_str()
        .unwrap_or_else(|| panic!("settlement has no machine: {settlement}"))
        .to_string();

    let log = follower.home.join("ssh-wire.log");
    install_observing_ssh(
        &bin,
        &ObservingSsh {
            home: follower.home.clone(),
            work: follower.repo.clone(),
            log: log.clone(),
            db: follower.global.join("orbit.db"),
            partition: follower.partition_id.clone(),
            logical: follower.logical_id.clone(),
            orbit_dir: follower.repo.join(".orbit"),
            leaf: LEAF_RUN.to_string(),
            drain: DRAIN_RUN.to_string(),
            task: task_id.clone(),
            follower_machine: claim_machine,
            owner_machine: owner.machine_id.clone(),
            owner_logical: owner.logical_id.clone(),
        },
    );
    fs::write(
        owner.global.join("hosts.toml"),
        format!(
            "schema_version = 1\n\n[[hosts]]\nname = \"follower-host\"\nmachine_id = \"{}\"\nssh = \"follower-ssh\"\ntask_prefix = \"FLW\"\n",
            follower.machine_id
        ),
    )
    .unwrap();

    let hydrated = orbit_output(
        &owner.home,
        &owner.repo,
        &path,
        &[
            "run",
            "settlements-hydrate",
            "--follower-workspace",
            "follower-host",
            "--json",
        ],
        true,
    );
    let wire = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        hydrated.status.success(),
        "hydration failed\nstdout:\n{}\nstderr:\n{}\nwire:\n{wire}",
        String::from_utf8_lossy(&hydrated.stdout),
        String::from_utf8_lossy(&hydrated.stderr)
    );
    let report: Value = serde_json::from_slice(&hydrated.stdout).unwrap_or_else(|error| {
        panic!(
            "hydration json ({error}): {}\n{}\nwire:\n{wire}",
            String::from_utf8_lossy(&hydrated.stdout),
            String::from_utf8_lossy(&hydrated.stderr)
        )
    });
    let row = &report["rows"][0];
    assert_eq!(
        row["outcome"], "mismatch",
        "wire:\n{wire}\nreport: {report}"
    );
    let detail = row["detail"].as_str().unwrap_or("");
    assert!(
        detail.contains("not terminal"),
        "a nonterminal leaf must be the mismatch, not an identity miss: {report}\nwire:\n{wire}"
    );
    assert!(
        wire.contains("--operator") && wire.contains("orbit_workflow_run_observe"),
        "hydration did not call the operator observation route\nwire:\n{wire}"
    );
    let seeded = wire.find("SEEDED").unwrap_or_else(|| {
        panic!("the follower run was not seeded after the session opened\nwire:\n{wire}")
    });
    let forwarded = wire
        .find("FORWARD_OBSERVE")
        .unwrap_or_else(|| panic!("the observation call was not forwarded\nwire:\n{wire}"));
    assert!(
        seeded < forwarded,
        "the orphan must exist only after this session's runtime is open and before observation\nwire:\n{wire}"
    );

    let db = Connection::open(follower.global.join("orbit.db")).unwrap();
    for run_id in [LEAF_RUN, DRAIN_RUN] {
        let (state, finished_at, duration_ms, pid): (String, Option<String>, Option<i64>, Option<i64>) = db
            .query_row(
                "SELECT state, finished_at, duration_ms, pid FROM job_runs WHERE workspace_id=?1 AND run_id=?2",
                rusqlite::params![follower.partition_id, run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap_or_else(|error| panic!("{run_id} was not stored ({error})\nwire:\n{wire}"));
        assert_eq!(state, "running", "{run_id} was reconciled\nwire:\n{wire}");
        assert_eq!(finished_at, None, "{run_id} gained a finish time");
        assert_eq!(duration_ms, None, "{run_id} gained a duration");
        assert_eq!(pid, Some(i64::from(u32::MAX)), "{run_id} pid changed");
    }
    let released_at: Option<String> = db
        .query_row(
            "SELECT released_at FROM task_reservations WHERE reservation_id='fixture-reservation'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("reservation missing ({error})\nwire:\n{wire}"));
    assert_eq!(released_at, None, "observation released the reservation");
    drop(db);

    let after_task = owner.orbit_json(&owner.repo, &["task", "show", &task_id, "--json"]);
    assert_eq!(after_task["status"], before_task["status"], "{after_task}");
    let after_settlements = owner.orbit_json(
        &owner.repo,
        &["run", "settlements", "--no-reconcile", "--json"],
    );
    assert_eq!(
        after_settlements, before_settlements,
        "hydration rewrote the owner's settlement"
    );
}

struct ObservingSsh {
    home: PathBuf,
    work: PathBuf,
    log: PathBuf,
    db: PathBuf,
    partition: String,
    logical: String,
    orbit_dir: PathBuf,
    leaf: String,
    drain: String,
    task: String,
    follower_machine: String,
    owner_machine: String,
    owner_logical: String,
}

struct Host {
    _temp: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
    global: PathBuf,
    machine_id: String,
    logical_id: String,
    partition_id: String,
}

impl Host {
    fn init(name: &str, prefix: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&repo).unwrap();
        let repo = repo.canonicalize().unwrap();
        super::git_repo::init(&repo);
        let host = Self {
            home,
            repo,
            global: PathBuf::new(),
            machine_id: String::new(),
            logical_id: String::new(),
            partition_id: String::new(),
            _temp: temp,
        };
        host.orbit(
            &host.repo,
            &[
                "init",
                "--non-interactive",
                "--skip-host-prerequisites",
                "--machine-name",
                name,
                "--task-prefix",
                prefix,
            ],
        );
        host.orbit(&host.repo, &["workspace", "init", "--name", name]);
        let global = host.home.join(".orbit");
        let runtime = open_runtime(&global, &host.repo.join(".orbit"));
        let logical_id = runtime
            .workspace_runtime_binding()
            .expect("registered workspace")
            .logical_workspace_id
            .clone();
        let partition_id = runtime.workspace_id().unwrap();
        let machine_id = runtime
            .automation_machine_identity()
            .expect("machine identity")
            .to_string();
        drop(runtime);
        Self {
            global,
            machine_id,
            logical_id,
            partition_id,
            ..host
        }
    }

    fn orbit(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = orbit_command(&self.home, cwd).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "orbit {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn orbit_json(&self, cwd: &Path, args: &[&str]) -> Value {
        let output = self.orbit(cwd, args);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "orbit json ({error}): {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
    }
}

fn open_runtime(global: &Path, workspace: &Path) -> OrbitRuntime {
    RegisteredRuntimeFactory::open_resolved_roots(OrbitRuntimeRoots {
        global_root: global.to_path_buf(),
        shared_root: workspace.to_path_buf(),
        local_root: workspace.to_path_buf(),
    })
    .unwrap_or_else(|error| panic!("open runtime: {error}"))
}

fn orbit_command(home: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}

fn orbit_output(
    home: &Path,
    cwd: &Path,
    path: &std::ffi::OsStr,
    args: &[&str],
    operator: bool,
) -> Output {
    let mut command = orbit_command(home, cwd);
    command.args(args).env("PATH", path);
    if operator {
        command.env("ORBIT_OPERATOR", "1");
    }
    command.output().unwrap()
}

fn federated(
    owner_machine: &str,
    follower_machine: &str,
    path: &std::ffi::OsStr,
) -> FederatedMcpHost {
    unsafe {
        std::env::set_var("PATH", path);
    }
    FederatedMcpHost::new(
        vec![Destination::ssh("owner-ssh", owner_machine)],
        std::sync::Arc::new(SshDestinationProbe::new(
            follower_machine.to_string(),
            Duration::from_secs(60),
            Duration::from_secs(60),
            None,
            orbit_mcp::McpSessionAuthority::Agent,
        )),
    )
}

fn call(host: &FederatedMcpHost, selector: &str, name: &str, mut input: Value) -> Value {
    input["workspace"] = json!(selector);
    host.call_internal_drain(name, input, ToolSessionContext::default())
        .unwrap_or_else(|error| panic!("{name} failed: {error}"))
}

fn pull_request(probed: &Value) -> Value {
    json!({
        "request_id": REQUEST_ID,
        "caller_version": probed["binary_version"],
        "caller_schema": probed["protocol_schema"],
        "caller_before_pr": false,
        "review_gate": true,
        "run_context": {
            "run_id": DRAIN_RUN,
            "job_name": "workspace_pull_pipeline",
            "machine_name": "follower-host"
        },
        "ship": probed["ship"]
    })
}

fn install_orbit_wrapper(bin: &Path) {
    let script = format!("#!/bin/sh\nexec {} \"$@\"\n", env!("CARGO_BIN_EXE_orbit"));
    let path = bin.join("orbit");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// SSH during admission: the follower's client executes the owner directly.
fn install_exec_ssh(bin: &Path, home: &Path, work: &Path) {
    let script = format!(
        r#"#!/usr/bin/env python3
import os, shlex, sys
argv = shlex.split(sys.argv[-1])
assert argv.pop(0) == "orbit", argv
env = os.environ.copy()
env["HOME"] = {home}
env["USERPROFILE"] = {home}
os.chdir({work})
os.execve({binary}, [{binary}] + argv, env)
"#,
        home = serde_json::to_string(home).unwrap(),
        work = serde_json::to_string(work).unwrap(),
        binary = serde_json::to_string(&env!("CARGO_BIN_EXE_orbit")).unwrap(),
    );
    write_ssh(bin, &script);
}

/// SSH during hydration. The follower process is already open before the
/// orphan exists: a fresh `orbit mcp serve` reconciles stale runs while
/// opening its workspace, so the orphan is inserted only after a priming
/// read has opened that runtime and immediately before the real observation.
fn install_observing_ssh(bin: &Path, target: &ObservingSsh) {
    let script = r#"#!/usr/bin/env python3
import json, os, shlex, sqlite3, subprocess, sys, threading
from datetime import datetime, timedelta, timezone

argv = shlex.split(sys.argv[-1])
with open(__LOG__, "a", encoding="utf-8") as evidence:
    evidence.write("ARGV " + sys.argv[-1] + "\n")
assert argv.pop(0) == "orbit", argv
env = os.environ.copy()
env["HOME"] = __HOME__
env["USERPROFILE"] = __HOME__
env.pop("ORBIT_MANAGED_RUN_CONTEXT", None)
env.pop("ORBIT_RUN_ID", None)
child = subprocess.Popen(
    [__BINARY__] + argv,
    cwd=__WORK__,
    env=env,
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
)
primed = threading.Event()

def note(text):
    with open(__LOG__, "a", encoding="utf-8") as evidence:
        evidence.write(text)

def pump_err():
    for line in child.stderr:
        note("ERR " + line.decode("utf-8", "replace"))

def seed():
    started = (datetime.now(timezone.utc) - timedelta(minutes=5)).strftime("%Y-%m-%dT%H:%M:%SZ")
    now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    executed = json.dumps({"machine_id": __FOLLOWER_MACHINE__, "machine_name": "follower-host"})
    leaf_input = json.dumps({"task_ids": [__TASK__], "claimed": True})
    drain_input = json.dumps({
        "task_ids": [__TASK__],
        "destination": {
            "owner_machine_id": __OWNER_MACHINE__,
            "owner_workspace_id": __OWNER_LOGICAL__,
            "execution_machine_id": __FOLLOWER_MACHINE__,
        },
    })
    conn = sqlite3.connect(__DB__, timeout=5)
    try:
        conn.execute("PRAGMA busy_timeout=5000")
        def insert(run_id, job_id, payload):
            conn.execute(
                "INSERT INTO job_runs("
                "run_id, workspace_id, job_id, attempt, state, scheduled_at, "
                "started_at, finished_at, duration_ms, created_at, pid, pid_start_time, "
                "input_json, executed_on_json) "
                "VALUES (?,?,?,1,'running',?,?,NULL,NULL,?,4294967295,NULL,?,?)",
                (run_id, __PARTITION__, job_id, started, started, now, payload, executed),
            )
            conn.execute(
                "INSERT OR IGNORE INTO job_run_id_allocations(workspace_id, run_id) VALUES (?,?)",
                (__PARTITION__, run_id),
            )
        insert(__LEAF__, "task_claimed_pr_pipeline", leaf_input)
        insert(__DRAIN__, "workspace_pull_pipeline", drain_input)
        conn.execute(
            "INSERT INTO task_reservations ("
            "reservation_id, workspace_orbit_dir, workspace_id, task_ids_json, files_json, "
            "actor, created_at, expires_at, owner_run_id) "
            "VALUES (?,?,?,?, '[]', 'fixture', ?, '2999-01-01T00:00:00Z', ?)",
            ("fixture-reservation", __ORBIT_DIR__, __PARTITION__, json.dumps([__TASK__]), now, __LEAF__),
        )
        conn.commit()
    finally:
        conn.close()
    note("SEEDED\n")

def read_out():
    for line in child.stdout:
        try:
            message = json.loads(line.decode("utf-8"))
        except json.JSONDecodeError:
            message = None
        if isinstance(message, dict) and message.get("id") == "fixture-prime" and not primed.is_set():
            note("PRIME_RESULT " + line.decode("utf-8", "replace"))
            try:
                seed()
            except Exception as error:
                note(f"SEED_ERROR {error}\n")
            primed.set()
            continue
        sys.stdout.buffer.write(line)
        sys.stdout.buffer.flush()
    child.wait()
    sys.stdout.flush()
    os._exit(child.returncode or 0)

threading.Thread(target=pump_err, daemon=True).start()
threading.Thread(target=read_out, daemon=True).start()
observe_names = {"orbit_workflow_run_observe", "orbit.workflow.run.observe"}
for raw in sys.stdin.buffer:
    note("IN " + raw.decode("utf-8", "replace"))
    try:
        message = json.loads(raw.decode("utf-8"))
    except json.JSONDecodeError:
        message = None
    name = None
    if isinstance(message, dict) and message.get("method") == "tools/call":
        name = (message.get("params") or {}).get("name")
    if name in observe_names and not primed.is_set():
        prime = {
            "jsonrpc": "2.0",
            "id": "fixture-prime",
            "method": "tools/call",
            "params": {
                "name": "orbit_workflow_run_observe",
                "arguments": {"id": "jrun-fixture-prime", "workspace": __LOGICAL__},
            },
        }
        blob = (json.dumps(prime) + "\n").encode()
        note("PRIME " + blob.decode("utf-8"))
        child.stdin.write(blob)
        child.stdin.flush()
        if not primed.wait(60):
            note("PRIME_TIMEOUT\n")
            child.kill()
            os._exit(1)
    if name in observe_names:
        note("FORWARD_OBSERVE\n")
    child.stdin.write(raw)
    child.stdin.flush()
child.stdin.close()
os._exit(0)
"#;
    let script = script
        .replace("__HOME__", &serde_json::to_string(&target.home).unwrap())
        .replace("__WORK__", &serde_json::to_string(&target.work).unwrap())
        .replace(
            "__BINARY__",
            &serde_json::to_string(&env!("CARGO_BIN_EXE_orbit")).unwrap(),
        )
        .replace("__LOG__", &serde_json::to_string(&target.log).unwrap())
        .replace("__DB__", &serde_json::to_string(&target.db).unwrap())
        .replace(
            "__PARTITION__",
            &serde_json::to_string(&target.partition).unwrap(),
        )
        .replace(
            "__LOGICAL__",
            &serde_json::to_string(&target.logical).unwrap(),
        )
        .replace(
            "__ORBIT_DIR__",
            &serde_json::to_string(&target.orbit_dir).unwrap(),
        )
        .replace("__LEAF__", &serde_json::to_string(&target.leaf).unwrap())
        .replace("__DRAIN__", &serde_json::to_string(&target.drain).unwrap())
        .replace("__TASK__", &serde_json::to_string(&target.task).unwrap())
        .replace(
            "__FOLLOWER_MACHINE__",
            &serde_json::to_string(&target.follower_machine).unwrap(),
        )
        .replace(
            "__OWNER_MACHINE__",
            &serde_json::to_string(&target.owner_machine).unwrap(),
        )
        .replace(
            "__OWNER_LOGICAL__",
            &serde_json::to_string(&target.owner_logical).unwrap(),
        );
    write_ssh(bin, &script);
}

fn write_ssh(bin: &Path, script: &str) {
    let path = bin.join("ssh");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn stub_first_path(bin: &Path) -> std::ffi::OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![bin.to_path_buf()];
    entries.extend(std::env::split_paths(&inherited));
    std::env::join_paths(entries).unwrap()
}
