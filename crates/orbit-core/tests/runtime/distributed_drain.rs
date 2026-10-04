//! A follower's pull drain against a real owner [ORB-13625].
//!
//! Two composed runtimes share one process: an owner holding backlog tasks and
//! a follower whose drain reaches it through the composition-supplied
//! [`DrainOwnerTransport`]. The transport delivers every call to the owner's
//! own tool boundary under the follower's trusted SSH session, and can lose a
//! reply after the owner has committed it — the fault a dropped connection
//! causes. Each drain iteration is the `pull_refill` action the
//! `workspace_pull_pipeline` job runs.
//!
//! This test binary is not a worker-capable Orbit entry point, so every leaf
//! launch is refused (STD-03 §R19) and no worker process starts. A claimed leaf
//! therefore ends at its launch and its failure settlement goes to the owner in
//! the pass that bound it: the systemic executor fault the breaker exists for.

#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_engine::RuntimeHost;
use orbit_store::contracts::JobRunStoreBackend;
use orbit_tools::{DrainOwnerTransport, OwnerCoordinator, ToolContext};
use orbit_types::policy::Role;
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};
use tempfile::TempDir;

const OWNER: &str = "hm_owner";
const FOLLOWER: &str = "hm_follower";
const LEAF_JOB: &str = "task_claimed_pr_pipeline";

/// How long one isolated test may run before it is killed and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(180);

/// Run `test` alone in a child of this binary with inherited Orbit authority
/// cleared and a disposable `HOME`; `true` inside that child. The parent
/// waits in-process up to [`CHILD_DEADLINE`] and reaps the child on any exit.
fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_DISTRIBUTED_DRAIN_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let home = TempDir::new().unwrap();
    let stdout_path = home.path().join("stdout.log");
    let stderr_path = home.path().join("stderr.log");
    // libtest names a test by its module path below the crate root.
    let qualified = format!(
        "{}::{test}",
        module_path!().split_once("::").expect("test module").1
    );
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", &qualified, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&stdout_path).unwrap())
        .stderr(std::fs::File::create(&stderr_path).unwrap());
    let mut child = ChildGuard(command.spawn().unwrap());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break Some(status);
        }
        if started.elapsed() > CHILD_DEADLINE {
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    drop(child);
    let read = |path: &Path| {
        let mut text = String::new();
        std::fs::File::open(path)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        text
    };
    let (stdout, stderr) = (read(&stdout_path), read(&stderr_path));
    let status = status
        .unwrap_or_else(|| panic!("`{test}` ran past {CHILD_DEADLINE:?}:\n{stdout}\n{stderr}"));
    assert!(status.success(), "`{test}` failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("test result: ok. 1 passed;"),
        "the child must run `{test}` itself:\n{stdout}"
    );
    false
}

/// Kills and reaps the isolated child however the parent leaves.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The follower's route to the owner. Every call reaches the owner's tool
/// boundary as the follower's SSH session would; a reply marked lost is
/// dropped after the owner committed it.
struct Wire {
    owner: OrbitRuntime,
    calls: Mutex<Vec<(String, Value)>>,
    lose: Mutex<Vec<&'static str>>,
    /// The selector of every task read, in order.
    task_reads: Mutex<Vec<String>>,
    /// When set, every task read fails at the transport with this error.
    task_reads_fail: Mutex<Option<String>>,
}

impl Wire {
    /// Drop the owner's next reply to `tool`.
    fn lose_next_reply(&self, tool: &'static str) {
        self.lose.lock().unwrap().push(tool);
    }

    /// The input of every call to `tool` that reached the owner.
    fn calls(&self, tool: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| name == tool)
            .map(|(_, input)| input.clone())
            .collect()
    }
}

impl DrainOwnerTransport for Wire {
    fn call(&self, _selector: &str, name: &str, input: Value) -> Result<Value, OrbitError> {
        self.calls
            .lock()
            .unwrap()
            .push((name.to_string(), input.clone()));
        let session = ToolSessionContext {
            caller_machine_id: Some(FOLLOWER.to_string()),
            process_machine_id: Some(OWNER.to_string()),
            transport: Some(McpTransport::SshMcp),
            effective_capabilities: BTreeSet::from([McpCapability::Agent]),
            ..ToolSessionContext::default()
        };
        let answer = self.owner.run_tool_with_context_and_role(
            name,
            input,
            Role::Admin,
            ToolContext {
                session_context: session,
                ..ToolContext::default()
            },
        )?;
        let mut lose = self.lose.lock().unwrap();
        if let Some(at) = lose.iter().position(|tool| *tool == name) {
            lose.remove(at);
            return Err(OrbitError::OutcomeUnknown {
                mcp_call_id: format!("lost-{name}"),
                message: "the connection dropped after the owner answered".into(),
            });
        }
        Ok(answer)
    }

    fn show_task(&self, selector: &str, input: Value) -> Result<Value, OrbitError> {
        self.task_reads.lock().unwrap().push(selector.to_string());
        if let Some(error) = self.task_reads_fail.lock().unwrap().clone() {
            return Err(OrbitError::UnreachableDestination(error));
        }
        self.owner.run_tool("orbit.task.show", input)
    }

    fn worker_coordinator(&self) -> Arc<dyn OwnerCoordinator> {
        Arc::new(NoWorkerRoute)
    }
}

/// No leaf worker starts in these tests, so none coordinates with the owner.
struct NoWorkerRoute;

impl OwnerCoordinator for NoWorkerRoute {
    fn call(&self, name: &str, _: Value, _: ToolSessionContext) -> Result<Value, OrbitError> {
        Err(OrbitError::PolicyDenied(format!(
            "no leaf worker runs in this test, got {name}"
        )))
    }
}

struct Pair {
    _root: TempDir,
    wire: Arc<Wire>,
    follower: OrbitRuntime,
    follower_repo: PathBuf,
    follower_jobs: Arc<dyn JobRunStoreBackend>,
    destination: Value,
    tasks: Vec<String>,
}

fn open_runtime(root: &Path, machine: &str) -> (OrbitRuntime, PathBuf) {
    let global = root.join(machine).join("global");
    let repo = root.join(machine).join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit"))
        .expect("runtime")
        .with_automation_machine_identity(Some(machine.to_string()));
    (runtime, repo)
}

/// An approved owner task scoped to `file`, ready for admission.
fn backlog_task(owner: &OrbitRuntime, repo: &Path, file: &str) -> String {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join(file), "fn work() {}\n").unwrap();
    let task = owner
        .run_tool(
            "orbit.task.add",
            json!({
                "title": format!("Change {file}"),
                "description": "Distributed drain fixture task.",
                "acceptance_criteria": ["Changed."],
                "complexity": "low",
                "workspace": repo.to_string_lossy(),
                "type": "chore",
                "context_files": [format!("file:{file}")],
                "model": "codex"
            }),
        )
        .expect("add task");
    let id = task["id"].as_str().unwrap().to_string();
    for update in [
        json!({"id": id, "plan": "1. Change it.", "model": "codex"}),
        json!({"id": id, "status": "backlog", "model": "codex"}),
    ] {
        owner
            .run_tool("orbit.task.update", update)
            .expect("approve");
    }
    id
}

impl Pair {
    /// An owner with `tasks` backlog tasks, each on its own file, and a
    /// replica follower routed to it.
    fn new(tasks: usize) -> Self {
        let root = TempDir::new().unwrap();
        let (owner, owner_repo) = open_runtime(root.path(), OWNER);
        let tasks = (0..tasks)
            .map(|n| backlog_task(&owner, &owner_repo, &format!("src/f{n}.rs")))
            .collect();
        let workspace_id = owner.workspace_id().unwrap();
        let wire = Arc::new(Wire {
            owner,
            calls: Mutex::default(),
            lose: Mutex::default(),
            task_reads: Mutex::default(),
            task_reads_fail: Mutex::default(),
        });
        let (follower, follower_repo) = open_runtime(root.path(), FOLLOWER);
        let follower = follower
            .with_coordination_write_owner(Some(OWNER.into()))
            .with_drain_owner_transport(wire.clone());
        let follower_jobs = orbit_store::compose::workspace_job_run_store(
            follower.sqlite_store().unwrap(),
            follower.workspace_id().unwrap(),
        );
        Self {
            _root: root,
            wire,
            follower,
            follower_repo,
            follower_jobs,
            destination: json!({
                "owner_machine_id": OWNER,
                "owner_workspace_id": workspace_id,
                "selector": format!("{OWNER}/{workspace_id}"),
                "execution_machine_id": FOLLOWER,
            }),
            tasks,
        }
    }

    /// A live follower drain run, as `orbit run auto --pull` leaves it once
    /// submitted.
    fn start_drain(&self) -> String {
        let run = self
            .follower_jobs
            .insert_job_run("workspace_pull_pipeline", 1, Utc::now(), None, None)
            .expect("drain run");
        self.follower
            .write_run_state(
                &run.run_id,
                &PipelineState::new(run.run_id.clone(), run.job_id, json!({})),
            )
            .expect("drain state");
        run.run_id
    }

    /// One drain iteration with its window open and one leaf slot. A pass
    /// never fails the drain; it reports what stopped it.
    fn pass(&self, drain: &str) -> Value {
        self.follower
            .run_deterministic(
                "pull_refill",
                &json!({}),
                &json!({
                    "run_id": drain,
                    "destination": self.destination,
                    "window_expired": false,
                    "max_active_leaf_runs": 1,
                }),
                ToolContext::default(),
            )
            .expect("a pass reports its errors instead of failing the drain")
    }

    fn owner_claims(&self) -> Vec<Value> {
        self.wire
            .owner
            .inspect_distributed_claims()
            .expect("owner claims")
    }

    fn owner_task(&self, id: &str) -> Value {
        self.wire
            .owner
            .run_tool("orbit.task.show", json!({"id": id}))
            .expect("owner task")
    }

    fn owner_status(&self, id: &str) -> String {
        self.owner_task(id)["status"].as_str().unwrap().to_string()
    }

    fn leaf_runs(&self) -> Vec<String> {
        self.follower_jobs
            .list_job_runs(LEAF_JOB)
            .expect("leaf runs")
            .into_iter()
            .map(|run| run.run_id)
            .collect()
    }
}

fn error_of(pass: &Value) -> &str {
    pass["error"].as_str().unwrap_or_default()
}

/// Whether a pass ended at a refused leaf launch rather than at the owner.
fn launch_refused(pass: &Value) -> bool {
    error_of(pass).contains("re-exec")
}

/// A lost pull reply and then a lost bind reply are both retried under the
/// identity the owner already committed: one request, one claim, one leaf,
/// bound and launched once. A drain that gave up on the unanswered request
/// would pull the owner's second task as well.
#[test]
fn lost_pull_and_bind_replies_recover_the_same_claim_and_leaf_exactly_once() {
    if !isolated("lost_pull_and_bind_replies_recover_the_same_claim_and_leaf_exactly_once") {
        return;
    }
    let pair = Pair::new(2);
    let drain = pair.start_drain();

    pair.wire.lose_next_reply("orbit.task.pull");
    let lost_pull = pair.pass(&drain);
    assert!(error_of(&lost_pull).contains("dropped"), "{lost_pull}");
    assert_eq!(pair.owner_claims().len(), 1, "the owner committed the pull");
    assert!(pair.leaf_runs().is_empty());

    pair.wire.lose_next_reply("orbit.drain.claim.bind");
    let lost_bind = pair.pass(&drain);
    assert!(error_of(&lost_bind).contains("dropped"), "{lost_bind}");
    let pulls = pair.wire.calls("orbit.task.pull");
    assert_eq!(pulls.len(), 2, "{pulls:?}");
    assert_eq!(
        pulls[0]["request_id"], pulls[1]["request_id"],
        "the unanswered request is re-sent, never replaced"
    );

    let recovered = pair.pass(&drain);
    assert!(launch_refused(&recovered), "{recovered}");
    let binds = pair.wire.calls("orbit.drain.claim.bind");
    assert_eq!(binds.len(), 2, "{binds:?}");
    assert_eq!(binds[0], binds[1], "the lost bind is replayed unchanged");

    let claims = pair.owner_claims();
    assert_eq!(claims.len(), 1, "{claims:#?}");
    let leaves = pair.leaf_runs();
    assert_eq!(leaves.len(), 1, "one leaf for the one claim: {leaves:?}");
    assert_eq!(claims[0]["bound_run"]["run_id"], leaves[0].as_str());
    assert_eq!(claims[0]["claim"]["phase"], "failed");
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 1, "{settles:?}");
    assert!(
        settles[0]["settlement"]["Fail"]["summary"]
            .as_str()
            .is_some_and(|summary| summary.starts_with("leaf launch failed")),
        "the bound leaf reached its launch: {settles:?}"
    );
    let claimed = claims[0]["claim"]["task_id"].as_str().unwrap();
    let untouched = pair.tasks.iter().find(|id| *id != claimed).unwrap();
    assert_eq!(pair.owner_status(untouched), "backlog");
}

/// Three claims in a row that this drain settled as failures open its
/// breaker: the next pass requests nothing and reports why. A new drain
/// starts with a closed breaker and pulls again.
#[test]
fn three_failed_claims_open_the_breaker_and_a_new_drain_resets_it() {
    if !isolated("three_failed_claims_open_the_breaker_and_a_new_drain_resets_it") {
        return;
    }
    let pair = Pair::new(4);
    let drain = pair.start_drain();

    for failed in 1..=3 {
        let pass = pair.pass(&drain);
        assert!(launch_refused(&pass), "{pass}");
        assert_eq!(pass["consecutive_failures"], failed, "{pass}");
    }
    let opened = pair.pass(&drain);
    assert_eq!(opened["admitting"], false, "{opened}");
    assert!(
        opened["refusal"]
            .as_str()
            .is_some_and(|refusal| refusal.starts_with("circuit_open")),
        "{opened}"
    );
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 3);
    let backlog = |pair: &Pair| {
        pair.tasks
            .iter()
            .filter(|id| pair.owner_status(id) == "backlog")
            .count()
    };
    assert_eq!(backlog(&pair), 1, "the fourth task is left on the owner");

    let restarted = pair.start_drain();
    let reset = pair.pass(&restarted);
    assert!(launch_refused(&reset), "{reset}");
    assert_eq!(reset["consecutive_failures"], 1, "{reset}");
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 4);
    assert_eq!(backlog(&pair), 0);
    assert_eq!(pair.owner_claims().len(), 4);
}

/// A settlement whose reply is lost stays recorded and is delivered again;
/// the owner replays the outcome it already applied rather than applying it
/// twice or refusing it, and a further replay changes nothing.
#[test]
fn a_settlement_whose_reply_is_lost_is_redelivered_and_applied_once() {
    if !isolated("a_settlement_whose_reply_is_lost_is_redelivered_and_applied_once") {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.start_drain();
    let task = pair.tasks[0].clone();

    pair.wire.lose_next_reply("orbit.drain.claim.settle");
    let lost = pair.pass(&drain);
    assert!(error_of(&lost).contains("dropped"), "{lost}");
    assert_eq!(pair.owner_status(&task), "blocked", "the owner applied it");
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 1);
    let applied = pair.owner_task(&task);

    let redelivered = pair.pass(&drain);
    assert!(redelivered["error"].is_null(), "{redelivered}");
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 2);
    assert_eq!(settles[0], settles[1], "the recorded settlement, re-sent");
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 0);
    let leaf = pair.leaf_runs().pop().expect("leaf");
    let claim = pair
        .follower
        .pull_leaf_claim(&leaf)
        .unwrap()
        .expect("claimed leaf");
    assert_eq!(claim.settlement_phase, "settled");
    assert_eq!(claim.refusal, None, "delivered, not closed as obsolete");

    let replay = pair
        .wire
        .call("", "orbit.drain.claim.settle", settles[0].clone())
        .expect("a replayed settlement answers with the recorded outcome");
    assert_eq!(replay["phase"], "failed", "{replay}");
    let after = pair.owner_task(&task);
    for field in ["status", "execution_summary", "comments", "history"] {
        assert_eq!(after[field], applied[field], "{field} changed on replay");
    }
    let blocked = after["history"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["to_status"] == "blocked")
        .count();
    assert_eq!(blocked, 1, "{after:#}");
}

/// A local drain's admission of `task` on the owner, as its gate leaves it
/// while waiting for context locks: the wrapper and gate carry the task, and
/// the task is still `backlog` with nothing reserved.
fn local_drain_admission(owner: &OrbitRuntime, task: &str) -> Vec<String> {
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    ["task_auto_pipeline", "task_gate_pipeline"]
        .into_iter()
        .map(|job| {
            jobs.insert_job_run(job, 1, Utc::now(), Some(json!({"task_ids": [task]})), None)
                .expect("local run")
                .run_id
        })
        .collect()
}

/// Move a run into the runner's transient retry sleep state. The public run
/// API treats this state as live, but its general `active_only` query is
/// intentionally limited to pending/running rows.
fn set_run_state(owner: &OrbitRuntime, run_id: &str, state: &str) {
    let store = owner.sqlite_store().unwrap();
    let workspace_id = owner.workspace_id().unwrap();
    store
        .with_transaction(|tx| {
            let changed = tx
                .connection()
                .execute(
                    "UPDATE job_runs SET state = ?1 WHERE workspace_id = ?2 AND run_id = ?3",
                    [state, workspace_id.as_str(), run_id],
                )
                .unwrap();
            assert_eq!(changed, 1, "fixture run exists");
            Ok(())
        })
        .unwrap();
}

/// [ORB-13918] A task the owner's local drain admitted is not pulled while
/// that admission is live, even though its gate has not yet moved it out of
/// `backlog` or reserved its footprint; once the local runs end, it is.
#[test]
fn a_task_a_local_drain_admitted_is_not_pulled_until_that_admission_ends() {
    if !isolated("a_task_a_local_drain_admitted_is_not_pulled_until_that_admission_ends") {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let local = local_drain_admission(&pair.wire.owner, &task);
    for run in &local {
        set_run_state(&pair.wire.owner, run, "retrying");
    }
    let drain = pair.start_drain();

    let held = pair.pass(&drain);
    assert!(error_of(&held).is_empty(), "{held}");
    assert!(pair.owner_claims().is_empty(), "{:#?}", pair.owner_claims());
    assert_eq!(pair.owner_status(&task), "backlog");
    assert!(pair.leaf_runs().is_empty());

    let owner_jobs = orbit_store::compose::workspace_job_run_store(
        pair.wire.owner.sqlite_store().unwrap(),
        pair.wire.owner.workspace_id().unwrap(),
    );
    for run in &local {
        set_run_state(&pair.wire.owner, run, "running");
        owner_jobs
            .finalize_job_run(
                run,
                orbit_types::workflow::JobRunState::Cancelled,
                Utc::now(),
                None,
            )
            .unwrap();
    }
    let released = pair.pass(&drain);
    assert!(launch_refused(&released), "{released}");
    let claims = pair.owner_claims();
    assert_eq!(claims.len(), 1, "{claims:#?}");
    assert_eq!(claims[0]["claim"]["task_id"], task.as_str());
}

/// [ORB-13918] While a follower's claim on a task is live, the owner's local
/// drain neither selects it nor lets a gate that queued it before the claim
/// landed dispatch it: the gate's pre-dispatch admission stops as a no-op and
/// no delivery run starts.
#[test]
fn a_task_under_a_live_claim_is_never_admitted_by_the_local_drain() {
    if !isolated("a_task_under_a_live_claim_is_never_admitted_by_the_local_drain") {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let drain = pair.start_drain();
    // The owner commits the claim; the lost reply leaves it unbound and live.
    pair.wire.lose_next_reply("orbit.task.pull");
    let lost = pair.pass(&drain);
    assert!(error_of(&lost).contains("dropped"), "{lost}");
    let claims = pair.owner_claims();
    assert_eq!(claims.len(), 1, "{claims:#?}");
    assert_eq!(claims[0]["claim"]["phase"], "claimed");
    let owner = &pair.wire.owner;

    let wave = owner
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"max_active_leaf_runs": 4}),
            ToolContext::default(),
        )
        .expect("classify");
    assert_eq!(wave["loose_task_ids"], json!([]), "{wave}");

    let gate = owner
        .run_deterministic(
            "invoke_and_wait",
            &json!({}),
            &json!({
                "job_name": "task_pr_pipeline",
                "run_input": {"task_ids": [task]},
                "admission_task_ids": [task],
                "admission_workflow": "worktree_setup",
                "timeout_seconds": 5,
            }),
            ToolContext::default(),
        )
        .expect("gate dispatch");
    assert_eq!(gate["skipped"], true, "{gate}");
    assert_eq!(gate["status"], "success", "{gate}");
    assert!(gate.get("error").is_none(), "{gate}");
    let owner_jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    assert!(
        owner_jobs
            .list_job_runs("task_pr_pipeline")
            .unwrap()
            .is_empty(),
        "no local delivery run starts beside the claim"
    );
    assert_eq!(pair.owner_claims()[0]["claim"]["phase"], "claimed");
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Orbit Test",
            "-c",
            "user.email=test@orbit.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// The checkout setup gives a claimed leaf: a Git worktree of the follower's
/// checkout on the leaf's own branch, holding a Cargo `target/` that the
/// checkout ignores. Returns the worktree and the build output's size.
fn leaf_worktree(pair: &Pair, leaf: &str) -> (PathBuf, u64) {
    let repo = &pair.follower_repo;
    if !repo.join(".git").exists() {
        git(repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join(".gitignore"), "/target/\n/.orbit/\n").unwrap();
        git(repo, &["add", ".gitignore"]);
        git(repo, &["commit", "-q", "-m", "init"]);
    }
    let worktree = repo
        .join(".orbit/state/worktrees")
        .join(format!("orbit-{leaf}"));
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("orbit/{leaf}"),
            worktree.to_str().unwrap(),
        ],
    );
    let build = worktree.join("target/debug");
    std::fs::create_dir_all(&build).unwrap();
    std::fs::write(build.join("orbit"), vec![0u8; 4096]).unwrap();
    (worktree, 4096)
}

fn gc_report(result: &orbit_engine::WorktreeGcResult, leaf: &str) -> Value {
    let result = serde_json::to_value(result).unwrap();
    result["reports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|report| report["run_id"] == leaf)
        .cloned()
        .unwrap_or_else(|| panic!("no report for {leaf}: {result:#}"))
}

/// [ORB-13920] A settled claim is all a follower needs to give back its
/// leaf's disk. The drain's next pass reclaims the leaf's `target/` and keeps
/// the checkout; worktree GC then removes the checkout on the strength of the
/// settled admission alone. Every owner task read would fail here, and none
/// is made.
#[test]
fn a_settled_claimed_leaf_gives_back_its_build_output_and_then_its_worktree() {
    if !isolated("a_settled_claimed_leaf_gives_back_its_build_output_and_then_its_worktree") {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.start_drain();
    let settled = pair.pass(&drain);
    assert!(launch_refused(&settled), "{settled}");
    let leaf = pair.leaf_runs().pop().expect("leaf");
    let claim = pair
        .follower
        .pull_leaf_claim(&leaf)
        .unwrap()
        .expect("claimed leaf");
    assert_eq!(claim.settlement_phase, "settled");
    let (worktree, build_bytes) = leaf_worktree(&pair, &leaf);
    *pair.wire.task_reads_fail.lock().unwrap() =
        Some("ssh: connect to host owner port 22: Connection timed out".into());

    let next = pair.pass(&drain);
    assert!(
        next["reclaimed_build_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes >= build_bytes),
        "{next}"
    );
    assert!(!worktree.join("target").exists(), "build output reclaimed");
    assert!(
        worktree.join(".gitignore").exists(),
        "the checkout stays for worktree GC"
    );

    let gc = pair
        .follower
        .gc_worktrees(true, None, None, false, false)
        .unwrap();
    let report = gc_report(&gc, &leaf);
    assert_eq!(report["action"], "removed", "{report:#}");
    assert!(
        report["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("claim settled")),
        "{report:#}"
    );
    assert!(!worktree.exists());
    assert!(
        pair.wire.task_reads.lock().unwrap().is_empty(),
        "a settled claim needs no task read"
    );
}

/// [ORB-13920] A claim not yet settled leaves the decision to its task's
/// status on the owner, read over the claim's own route. A transport failure
/// is reported as `owner_unreachable` carrying the transport's error; once
/// the owner answers, its answer decides.
#[test]
fn an_unsettled_claimed_worktree_asks_its_owner_and_reports_a_transport_failure() {
    if !isolated("an_unsettled_claimed_worktree_asks_its_owner_and_reports_a_transport_failure") {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.start_drain();
    pair.wire.lose_next_reply("orbit.drain.claim.settle");
    let lost = pair.pass(&drain);
    assert!(error_of(&lost).contains("dropped"), "{lost}");
    let leaf = pair.leaf_runs().pop().expect("leaf");
    let claim = pair
        .follower
        .pull_leaf_claim(&leaf)
        .unwrap()
        .expect("claimed leaf");
    assert_eq!(claim.settlement_phase, "settling");
    // The leaf has finished; only its settlement is still owed to the owner.
    set_run_state(&pair.follower, &leaf, "running");
    pair.follower_jobs
        .finalize_job_run(
            &leaf,
            orbit_types::workflow::JobRunState::Failed,
            Utc::now(),
            None,
        )
        .unwrap();
    let (worktree, _) = leaf_worktree(&pair, &leaf);
    let timeout = "ssh: connect to host owner port 22: Connection timed out";
    *pair.wire.task_reads_fail.lock().unwrap() = Some(timeout.into());

    let unreachable = pair
        .follower
        .gc_worktrees(false, None, None, false, false)
        .unwrap();
    let report = gc_report(&unreachable, &leaf);
    assert_eq!(report["action"], "skipped:owner_unreachable", "{report:#}");
    assert!(
        report["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains(timeout)),
        "the transport's error is reported: {report:#}"
    );
    let selector = pair.destination["selector"].as_str().unwrap();
    assert_eq!(
        *pair.wire.task_reads.lock().unwrap(),
        vec![selector.to_string()],
        "asked over the claim's route"
    );

    *pair.wire.task_reads_fail.lock().unwrap() = None;
    let answered = pair
        .follower
        .gc_worktrees(false, None, None, false, false)
        .unwrap();
    let report = gc_report(&answered, &leaf);
    assert_eq!(
        report["action"], "skipped:task_status_ineligible",
        "{report:#}"
    );
    assert_eq!(
        report["task_status"], "blocked",
        "the owner's answer decides: {report:#}"
    );
    assert!(worktree.exists());
}
