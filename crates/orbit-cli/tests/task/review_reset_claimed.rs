//! [ORB-16699] A claimed leaf's escalated final recovery names a command
//! that works when run as written, from a directory that is not the checkout.
//!
//! [ORB-16727] A claim is one attempt, so an exhausted claimed review is
//! recovered by the owner's `task requeue`, not by resuming the failed leaf:
//! a resumed run on the production claimed definition has no claim, and its
//! worker refuses it before validation. The task's next claim is a new owner
//! admission that carries the kept candidate.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_common::OrbitError;
use orbit_common::test_env;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::OrbitRuntimeRoots;
use orbit_mcp::federated::{Destination, FederatedMcpHost, SshDestinationProbe};
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::{JobRunState, PipelineState};
use rusqlite::Connection;
use serde_json::{Value, json};

use super::isolated_test;

/// The production claimed PR leaf definition.
const JOB: &str = "task_claimed_pr_pipeline";
const RUN_ID: &str = "jrun-claimed-review-source";
const NEXT_RUN_ID: &str = "jrun-claimed-review-next";
const REQUEST_ID: &str = "claimed-review-admission";
const NEXT_REQUEST_ID: &str = "claimed-review-requeue";

#[test]
fn claimed_leaf_review_escalation_requeues_onto_a_new_claim() {
    if isolated_test::run_in_child(
        "review_reset_claimed::claimed_leaf_review_escalation_requeues_onto_a_new_claim",
    ) {
        return;
    }

    let owner = Host::init("owner-host", "OWN");
    let follower = Host::init("follower-host", "FLW");
    fs::write(owner.repo.join("work.rs"), "fn work() {}\n").unwrap();
    // `task add` refuses plan and status; admission requires both, set after.
    let task_id = owner.orbit_json(
        &owner.repo,
        &[
            "task",
            "add",
            "--title",
            "Claimed review requeue",
            "--description",
            "A claimed leaf exhausted its review budget.",
            "--acceptance-criteria",
            "The requeue continues the kept candidate.",
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
            "Requeue the claimed task onto its kept candidate.",
        ],
    );
    owner.orbit(
        &owner.repo,
        &["task", "update", &task_id, "--status", "backlog"],
    );

    let bin = follower.home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    install_orbit_wrapper(&bin);
    install_owner_ssh(&bin, &owner.home, &owner.repo);
    let path = stub_first_path(&bin);

    let selector = format!("{}/{}", owner.machine_id, owner.logical_id);
    let probe = federated(&owner.machine_id, &follower.machine_id, &path);
    let probed = call(&probe, &selector, "orbit.drain.probe", json!({}));
    assert!(
        probed["refusal"].is_null(),
        "probe refused admission: {probed}"
    );
    let request = pull_request(&probed, REQUEST_ID);
    let admitted = call(&probe, &selector, "orbit.task.pull", request.clone());
    let claim = admitted["receipt"]["claim"].clone();
    let claim_id = claim["claim_id"]
        .as_str()
        .unwrap_or_else(|| panic!("pull admitted no claim: {admitted}"))
        .to_string();
    assert_eq!(claim["task_id"], task_id, "{admitted}");
    // A failure is stale until the claim is bound to the leaf run.
    let bound = call(
        &probe,
        &selector,
        "orbit.drain.claim.bind",
        json!({"claim_id": claim_id, "run_id": RUN_ID, "ship": probed["ship"]}),
    );
    assert_eq!(bound["phase"], "running", "{bound}");

    let lineage_key = format!("{}/{task_id}/agent-main/{claim_id}", follower.partition_id);
    let ledger = json!({
        "lineage_key": lineage_key,
        "task_ids": [task_id],
        "budget": {"minutes": 30},
        "attempts": [],
        "consumed_seconds": 1800,
        "revision": 1,
        "updated_at": "2026-10-18T10:40:00Z"
    });
    let kept_branch = format!("orbit/{task_id}-kept");
    let kept_head = "e".repeat(40);
    let settlement = json!({
        "claim_id": claim_id,
        "run_id": RUN_ID,
        "settlement": {"Fail": {
            "summary": "review budget exhausted",
            "comment": null,
            "artifacts": [],
            "failure": {
                "class": "candidate",
                "reason": "review_budget_exhausted",
                "candidate": {
                    "branch": kept_branch,
                    "head_sha": kept_head,
                    "source_run_id": RUN_ID,
                    "failed_step_id": "review_gate_settle",
                    "published": true
                }
            },
            "final_recovery": {
                "run_id": RUN_ID,
                "decision": {
                    "decision": "escalate",
                    "diagnosis": "review budget exhausted",
                    "human_action": "Authorize a review reset"
                }
            },
            "claimed_review": {
                "executor_machine_id": follower.machine_id,
                "executor_data_root": follower.global.display().to_string(),
                "executor_workspace_id": follower.logical_id,
                "ledgers": [ledger]
            }
        }}
    });
    let settled = call(&probe, &selector, "orbit.drain.claim.settle", settlement);
    assert_eq!(settled["phase"], "failed", "{settled}");
    drop(probe);

    let shown = owner.orbit_json(&owner.repo, &["task", "show", &task_id, "--json"]);
    let action = shown["comments"]
        .as_array()
        .unwrap_or_else(|| panic!("task show has no comments: {shown}"))
        .iter()
        .filter_map(|comment| comment["message"].as_str())
        .find(|message| message.contains("human_action:"))
        .unwrap_or_else(|| panic!("no human_action comment: {shown}"))
        .to_string();
    // [ORB-16727] The reviewed revision named an owner review-reset and a
    // follower `job resume`, whose new run had no claim and was refused by
    // the worker before validation.
    let commands = orbit_commands(&action);
    assert_eq!(commands.len(), 1, "one owner command: {action}");
    assert!(
        commands[0].contains(&format!("task requeue {task_id} "))
            && commands[0].contains("--workspace")
            && commands[0].contains("ORBIT_OPERATOR=1"),
        "{action}"
    );

    // The failed leaf, on the production claimed definition, stays out of
    // generic resume whatever the claim's state: live, settled, or after the
    // owner has requeued the task. None of them inserts a run.
    seed_follower_run(&follower, &owner, &claim_id, &admitted, &request);
    let neutral = tempfile::tempdir().unwrap();
    let resume = format!(
        "orbit --root '{}' --workspace '{}' job resume {RUN_ID}",
        follower.global.display(),
        follower.logical_id
    );
    set_pull_phase(&follower, "launched");
    assert_resume_refused(&follower, &resume, neutral.path(), &path, "a live claim");
    set_pull_phase(&follower, "settled");
    assert_resume_refused(&follower, &resume, neutral.path(), &path, "a failed claim");

    let requeued = sh(&commands[0], &owner.home, neutral.path(), &path);
    assert!(
        requeued.status.success(),
        "requeue command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    let shown = owner.orbit_json(&owner.repo, &["task", "show", &task_id, "--json"]);
    assert_eq!(shown["status"], "backlog", "{shown}");
    assert_resume_refused(&follower, &resume, neutral.path(), &path, "a requeued task");

    // The continuation is a new owner admission whose receipt carries the
    // kept candidate, and the owner binds it to the new leaf alone.
    let probe = federated(&owner.machine_id, &follower.machine_id, &path);
    let next_request = pull_request(&probed, NEXT_REQUEST_ID);
    let next = call(&probe, &selector, "orbit.task.pull", next_request);
    let next_claim = next["receipt"]["claim"].clone();
    let next_claim_id = next_claim["claim_id"]
        .as_str()
        .unwrap_or_else(|| panic!("requeued task was not claimed: {next}"))
        .to_string();
    assert_eq!(next_claim["task_id"], task_id, "{next}");
    assert_ne!(next_claim_id, claim_id, "{next}");
    let offered = &next["receipt"]["task"]["resume_candidate"];
    assert_eq!(offered["branch"], kept_branch.as_str(), "{next}");
    assert_eq!(offered["head_sha"], kept_head.as_str(), "{next}");
    let stale = try_call(
        &probe,
        &selector,
        "orbit.drain.claim.bind",
        json!({"claim_id": claim_id, "run_id": NEXT_RUN_ID, "ship": probed["ship"]}),
    );
    assert!(
        refused_binding(&stale),
        "the failed claim cannot be bound to a new run: {stale:?}"
    );
    let rebound = call(
        &probe,
        &selector,
        "orbit.drain.claim.bind",
        json!({"claim_id": next_claim_id, "run_id": NEXT_RUN_ID, "ship": probed["ship"]}),
    );
    assert_eq!(rebound["phase"], "running", "{rebound}");
    let foreign = try_call(
        &probe,
        &selector,
        "orbit.drain.claim.bind",
        json!({"claim_id": next_claim_id, "run_id": RUN_ID, "ship": probed["ship"]}),
    );
    assert!(
        refused_binding(&foreign),
        "the new claim cannot be rebound to the failed leaf: {foreign:?}"
    );
    drop(probe);
    assert_resume_refused(&follower, &resume, neutral.path(), &path, "a rebound task");
}

/// `orbit job resume` of the failed leaf, run as `script` on `follower`,
/// is refused as claimed execution and leaves the leaf the only claimed run.
fn assert_resume_refused(
    follower: &Host,
    script: &str,
    cwd: &Path,
    path: &std::ffi::OsStr,
    case: &str,
) {
    let refused = sh(script, &follower.home, cwd, path);
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!refused.status.success(), "{case}: resume ran: {text}");
    assert!(
        text.contains(
            "claimed execution cannot use generic resume; deliberately recover and admit a new attempt"
        ),
        "{case}: {text}"
    );
    let db = Connection::open(follower.global.join("orbit.db")).unwrap();
    let runs: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM job_runs WHERE workspace_id=?1 AND job_id=?2",
            rusqlite::params![follower.partition_id, JOB],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(runs, 1, "{case}: resume inserted a run");
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

    fn runtime(&self) -> OrbitRuntime {
        open_runtime(&self.global, &self.repo.join(".orbit"))
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

fn federated(
    owner_machine: &str,
    follower_machine: &str,
    path: &std::ffi::OsStr,
) -> FederatedMcpHost {
    // This child is already isolated. ssh for the pull and for the later
    // resume lookup both resolve through this PATH.
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

fn call(host: &FederatedMcpHost, selector: &str, name: &str, input: Value) -> Value {
    try_call(host, selector, name, input).unwrap_or_else(|error| panic!("{name} failed: {error}"))
}

fn try_call(
    host: &FederatedMcpHost,
    selector: &str,
    name: &str,
    mut input: Value,
) -> Result<Value, OrbitError> {
    input["workspace"] = json!(selector);
    host.call_internal_drain(name, input, ToolSessionContext::default())
}

/// The owner's claim journal refused a bind that does not match the claim.
fn refused_binding(answer: &Result<Value, OrbitError>) -> bool {
    answer
        .as_ref()
        .is_err_and(|error| error.to_string().contains("mutation_mismatch"))
}

/// A follower drain's pull request under `request_id`.
fn pull_request(probed: &Value, request_id: &str) -> Value {
    json!({
        "request_id": request_id,
        "caller_version": probed["binary_version"],
        "caller_schema": probed["protocol_schema"],
        "caller_before_pr": false,
        "run_context": {
            "run_id": "follower-drain",
            "job_name": "workspace_pull_pipeline",
            "machine_name": "follower-host"
        },
        "ship": probed["ship"]
    })
}

/// The failed leaf on the follower as the drain leaves it: a failed run of
/// the production claimed definition with a reusable checkpoint, and its
/// settled pull admission.
fn seed_follower_run(
    follower: &Host,
    owner: &Host,
    claim_id: &str,
    admitted: &Value,
    request: &Value,
) {
    fs::write(
        follower.global.join("hosts.toml"),
        format!(
            "schema_version = 1\n\n[[hosts]]\nname = \"owner-host\"\nmachine_id = \"{}\"\nssh = \"owner-ssh\"\ntask_prefix = \"OWN\"\n",
            owner.machine_id
        ),
    )
    .unwrap();
    let task_id = &admitted["receipt"]["claim"]["task_id"];
    let now = chrono::Utc::now().to_rfc3339();
    let input = json!({"task_ids": [task_id], "claimed": true}).to_string();
    let db = Connection::open(follower.global.join("orbit.db")).unwrap();
    db.execute(
        "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,finished_at,created_at) VALUES (?1,?2,?3,1,'failed',?4,?5,?5,?5,?5)",
        rusqlite::params![RUN_ID, follower.partition_id, JOB, input, now],
    )
    .unwrap();
    let record = json!({
        "destination": {
            "owner_machine_id": owner.machine_id,
            "owner_workspace_id": owner.logical_id,
            "execution_machine_id": follower.machine_id,
            "selector": format!("{}/{}", owner.machine_id, owner.logical_id)
        },
        "request": request,
        "receipt": admitted["receipt"],
        "leaf_run_id": RUN_ID,
        "phase": "settled",
        "settlement": null
    });
    db.execute(
        "INSERT INTO local_pull_admissions VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        rusqlite::params![
            follower.partition_id,
            owner.machine_id,
            owner.logical_id,
            follower.machine_id,
            REQUEST_ID,
            claim_id,
            RUN_ID,
            record.to_string()
        ],
    )
    .unwrap();
    drop(db);

    let runtime = follower.runtime();
    let mut state = PipelineState::new(RUN_ID.to_string(), JOB.to_string(), json!({}));
    let worktree = json!({"checkpoint": "worktree"});
    state.record_step(0, JobRunState::Success, Some(worktree.clone()), None);
    state.record_pipeline_output("worktree", worktree);
    runtime.write_run_state(RUN_ID, &state).unwrap();
}

/// Move the failed leaf's pull admission to `phase`.
fn set_pull_phase(follower: &Host, phase: &str) {
    let db = Connection::open(follower.global.join("orbit.db")).unwrap();
    let changed = db
        .execute(
            "UPDATE local_pull_admissions SET record_json=json_set(record_json,'$.phase',?1) \
             WHERE workspace_id=?2 AND leaf_run_id=?3",
            rusqlite::params![phase, follower.partition_id, RUN_ID],
        )
        .unwrap();
    assert_eq!(changed, 1, "the failed leaf's admission");
}

fn orbit_commands(action: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut rest = action;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else {
            break;
        };
        let span = &after[..end];
        if span.contains("orbit") {
            commands.push(span.to_string());
        }
        rest = &after[end + 1..];
    }
    commands
}

fn sh(script: &str, home: &Path, cwd: &Path, path: &std::ffi::OsStr) -> Output {
    let mut command = Command::new("sh");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .arg("-c")
        .arg(script)
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", path)
        .output()
        .unwrap()
}

fn install_orbit_wrapper(bin: &Path) {
    let script = format!("#!/bin/sh\nexec {} \"$@\"\n", env!("CARGO_BIN_EXE_orbit"));
    let path = bin.join("orbit");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn install_owner_ssh(bin: &Path, owner_home: &Path, owner_repo: &Path) {
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
        home = serde_json::to_string(&owner_home).unwrap(),
        work = serde_json::to_string(&owner_repo).unwrap(),
        binary = serde_json::to_string(&env!("CARGO_BIN_EXE_orbit")).unwrap(),
    );
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
