//! [ORB-16699] A claimed leaf's escalated final recovery names a review-reset
//! and a resume that work when run as written, from a directory that is not
//! the checkout.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_common::process::identity::{ProcessLiveness, probe_process_liveness};
use orbit_common::test_env;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::OrbitRuntimeRoots;
use orbit_mcp::federated::{Destination, FederatedMcpHost, SshDestinationProbe};
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::{
    FinalRecoveryCheckpoint, FinalRecoveryDecision, FinalRecoveryKey, JobRunState, PipelineState,
};
use rusqlite::Connection;
use serde_json::{Value, json};

use super::isolated_test;

const JOB: &str = "claimed_review_resume";
const RUN_ID: &str = "jrun-claimed-review-source";
const REQUEST_ID: &str = "claimed-review-admission";
const RESET_REASON: &str = "operator reset of exhausted claimed-leaf review budget";

#[test]
fn claimed_leaf_review_reset_command_resets_and_resumes_from_validate() {
    if isolated_test::run_in_child(
        "review_reset_claimed::claimed_leaf_review_reset_command_resets_and_resumes_from_validate",
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
            "Claimed review reset",
            "--description",
            "A claimed leaf exhausted its review budget.",
            "--acceptance-criteria",
            "The reset command resumes the run.",
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
            "Reset the claimed review ledger and resume.",
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
    let request = json!({
        "request_id": REQUEST_ID,
        "caller_version": probed["binary_version"],
        "caller_schema": probed["protocol_schema"],
        "caller_before_pr": false,
        "run_context": {
            "run_id": "follower-drain",
            "job_name": "workspace_pull_pipeline",
            "machine_name": "follower-host"
        },
        "ship": probed["ship"]
    });
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
    let settlement = json!({
        "claim_id": claim_id,
        "run_id": RUN_ID,
        "settlement": {"Fail": {
            "summary": "review budget exhausted",
            "comment": null,
            "artifacts": [],
            "failure": {"class": "candidate", "reason": "review_budget_exhausted"},
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
    let commands = orbit_commands(&action);
    assert_eq!(commands.len(), 2, "{action}");
    assert!(
        commands[0].contains("task review-reset") && commands[0].contains("--workspace"),
        "{action}"
    );
    assert!(
        commands[1].contains("job resume")
            && commands[1].contains(RUN_ID)
            && !commands[1].contains("--wait"),
        "{action}"
    );

    seed_follower_run(
        &follower,
        &owner,
        &task_id,
        &claim_id,
        &lineage_key,
        &admitted,
        &request,
    );
    let neutral = tempfile::tempdir().unwrap();
    let refused = sh(&commands[1], &follower.home, neutral.path(), &path);
    let refused_text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        !refused.status.success(),
        "resume before the reset must be refused: {refused_text}"
    );
    assert!(
        refused_text.contains(
            "claimed execution cannot use generic resume; deliberately recover and admit a new attempt"
        ),
        "{refused_text}"
    );

    let reset = sh(&commands[0], &owner.home, neutral.path(), &path);
    assert!(
        reset.status.success(),
        "review-reset command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&reset.stdout),
        String::from_utf8_lossy(&reset.stderr)
    );
    let owner_ledger = ledger_json(&owner, &lineage_key);
    assert_eq!(owner_ledger["consumed_seconds"], 0, "{owner_ledger}");
    assert_eq!(
        owner_ledger["decisions"][0]["reason"], RESET_REASON,
        "{owner_ledger}"
    );

    let resumed = sh(&commands[1], &follower.home, neutral.path(), &path);
    let resumed_text = String::from_utf8_lossy(&resumed.stdout).to_string();
    assert!(
        resumed.status.success(),
        "resume command failed\nstdout:\n{resumed_text}\nstderr:\n{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let new_run = resumed_text
        .lines()
        .find_map(|line| line.strip_prefix("Run ID: "))
        .unwrap_or_else(|| panic!("resume did not name a run: {resumed_text}"))
        .trim()
        .to_string();
    wait_for_run(&follower, &new_run);

    let runtime = follower.runtime();
    let state = runtime
        .read_run_state(&new_run)
        .unwrap()
        .unwrap_or_else(|| panic!("resumed run has no checkpoint"));
    assert!(state.final_recovery.is_none(), "{state:?}");
    // Success compacts step outputs into pointers. The pipeline object keeps
    // the skipped prepare checkpoint and the validate guard that re-ran.
    assert_eq!(state.step_states.get(&0), Some(&JobRunState::Success));
    assert_eq!(
        state
            .pipeline
            .get("prepare")
            .and_then(|output| output.get("checkpoint")),
        Some(&json!("prepare")),
        "prepare was re-executed: {state:?}"
    );
    assert_eq!(
        state
            .pipeline
            .get("validate")
            .and_then(|output| output.get("succeeded")),
        Some(&json!(true)),
        "validate did not run: {state:?}"
    );
    drop(runtime);
    let follower_ledger = ledger_json(&follower, &lineage_key);
    assert_eq!(
        follower_ledger["decisions"][0]["reason"], RESET_REASON,
        "{follower_ledger}"
    );
    assert_eq!(follower_ledger["consumed_seconds"], 0, "{follower_ledger}");
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

fn call(host: &FederatedMcpHost, selector: &str, name: &str, mut input: Value) -> Value {
    input["workspace"] = json!(selector);
    host.call_internal_drain(name, input, ToolSessionContext::default())
        .unwrap_or_else(|error| panic!("{name} failed: {error}"))
}

fn seed_follower_run(
    follower: &Host,
    owner: &Host,
    task_id: &str,
    claim_id: &str,
    lineage_key: &str,
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
    let jobs = follower.global.join("resources/jobs");
    fs::create_dir_all(&jobs).unwrap();
    let job = json!({
        "schemaVersion": 2,
        "kind": "Job",
        "metadata": {"name": JOB},
        "spec": {
            "state": "enabled",
            "kind": "workflow",
            "steps": [
                {
                    "id": "prepare",
                    "default_input": {"result": {"status": "failed", "run_id": "prepare-must-stay-skipped"}},
                    "spec": {"type": "deterministic", "action": "pipeline_success_guard", "config": {}}
                },
                {
                    "id": "validate",
                    "default_input": {"result": {"status": "succeeded"}},
                    "spec": {"type": "deterministic", "action": "pipeline_success_guard", "config": {}}
                }
            ]
        }
    });
    fs::write(jobs.join(format!("{JOB}.yaml")), job.to_string()).unwrap();

    let now = chrono::Utc::now().to_rfc3339();
    let db = Connection::open(follower.global.join("orbit.db")).unwrap();
    db.execute(
        "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,finished_at,created_at) VALUES (?1,?2,?3,1,'failed',?4,?5,?5,?5,?5)",
        rusqlite::params![RUN_ID, follower.partition_id, JOB, "{}", now],
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
    db.execute(
        "INSERT INTO review_lineages (workspace_id,lineage_key,revision,ledger_json) VALUES (?1,?2,1,?3)",
        rusqlite::params![
            follower.partition_id,
            lineage_key,
            json!({
                "lineage_key": lineage_key,
                "task_ids": [task_id],
                "budget": {"minutes": 30},
                "attempts": [],
                "consumed_seconds": 1800,
                "revision": 1,
                "updated_at": "2026-10-18T10:40:00Z"
            })
            .to_string()
        ],
    )
    .unwrap();
    drop(db);

    let runtime = follower.runtime();
    let mut state = PipelineState::new(RUN_ID.to_string(), JOB.to_string(), json!({}));
    let prepare = json!({"checkpoint": "prepare"});
    state.record_step(0, JobRunState::Success, Some(prepare.clone()), None);
    state.record_pipeline_output("prepare", prepare);
    state.record_step(
        1,
        JobRunState::Failed,
        Some(json!({"checkpoint": "validate-failed"})),
        None,
    );
    state.final_recovery = Some(FinalRecoveryCheckpoint {
        key: FinalRecoveryKey {
            run_id: RUN_ID.to_string(),
            attempt: 1,
        },
        failed_step_id: "validate".into(),
        task_id: task_id.to_string(),
        observed: None,
        base_ref: None,
        admitted_at: chrono::Utc::now(),
        decision: Some(FinalRecoveryDecision::Escalate {
            diagnosis: "review budget exhausted".into(),
            human_action: "Authorize a review reset".into(),
        }),
        repair_commit: None,
        resume_context: None,
        outcome: None,
    });
    runtime.write_run_state(RUN_ID, &state).unwrap();
}

fn wait_for_run(follower: &Host, run_id: &str) {
    let runtime = follower.runtime();
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut last;
    let mut worker: Option<(u32, Option<String>)> = None;
    loop {
        let shown = runtime.show_job_run_observed(run_id).unwrap();
        if let Some(pid) = shown.pid {
            worker = Some((pid, shown.pid_start_time.clone()));
        }
        if matches!(
            shown.state,
            JobRunState::Success | JobRunState::Failed | JobRunState::Timeout
        ) {
            let (pid, started) = worker.expect("resumed run records a worker");
            while probe_process_liveness(pid, started.as_deref()) != ProcessLiveness::Exited {
                if Instant::now() >= deadline {
                    stop_worker(pid);
                    panic!("worker {pid} did not exit; last state {}", shown.state);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            assert_eq!(shown.state, JobRunState::Success, "{shown:?}");
            return;
        }
        last = format!("{shown:?}");
        if Instant::now() >= deadline {
            if let Some((pid, _)) = worker {
                stop_worker(pid);
            }
            panic!("run {run_id} did not finish: {last}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn stop_worker(pid: u32) {
    let _ = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
}

fn ledger_json(host: &Host, lineage_key: &str) -> Value {
    let db = Connection::open(host.global.join("orbit.db")).unwrap();
    let raw: String = db
        .query_row(
            "SELECT ledger_json FROM review_lineages WHERE workspace_id=?1 AND lineage_key=?2",
            rusqlite::params![host.partition_id, lineage_key],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("ledger {lineage_key} on {}: {error}", host.partition_id));
    serde_json::from_str(&raw).unwrap()
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
