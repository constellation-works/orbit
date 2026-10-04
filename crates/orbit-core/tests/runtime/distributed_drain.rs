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
//!
//! Host resource pressure is injected through the follower's resource probe.

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
use orbit_core::application::routines::{
    DiscoveredWorkspaces, RoutineMachineIdentity, RoutineWorkspaceProvider, SweepOptions,
    run_sweep_at_with_providers,
};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::{
    ClaimMutation, JobRunStepParams, JobRunStoreBackend, LocalPullAdmission, LocalPullMutation,
    LocalPullPhase,
};
use orbit_tools::{DrainOwnerTransport, OwnerCoordinator, ToolContext};
use orbit_types::policy::Role;
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::handoff::{
    HandoffCandidate, HandoffDelivery, HandoffReview, HandoffReviewDisposition, TaskHandoff,
};
use orbit_types::workflow::{
    ExecutorDef, ExecutorType, JobRunState, JobTargetType, PipelineState, ReviewTiming,
};
use orbit_types::workspace::{Workspace, WorkspaceStatus};
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
    /// When set, the owner answers task reads with a structured tool error.
    task_reads_remote_error: Mutex<Option<(String, String)>>,
    /// When set, every tool call fails at the transport: an owner outage.
    unreachable: Mutex<bool>,
    /// An older owner fixture: revision 1 rejects the new crews field.
    protocol: Mutex<Option<u32>>,
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
        if *self.unreachable.lock().unwrap() {
            return Err(OrbitError::UnreachableDestination(
                "ssh: connect to host owner port 22: Connection timed out".into(),
            ));
        }
        // The owner verifies a handoff against its published pull request,
        // which no test here has; the wire answers as an owner that did.
        if name == "orbit.drain.claim.settle" && input["settlement"].get("AcceptHandoff").is_some()
        {
            return Ok(json!({"phase": "handed_off"}));
        }
        let session = ToolSessionContext {
            caller_machine_id: Some(FOLLOWER.to_string()),
            process_machine_id: Some(OWNER.to_string()),
            transport: Some(McpTransport::SshMcp),
            effective_capabilities: BTreeSet::from([McpCapability::Agent]),
            ..ToolSessionContext::default()
        };
        let mut answer = self.owner.run_tool_with_context_and_role(
            name,
            input,
            Role::Admin,
            ToolContext {
                session_context: session,
                ..ToolContext::default()
            },
        )?;
        if name == "orbit.drain.probe"
            && let Some(revision) = *self.protocol.lock().unwrap()
        {
            answer["protocol_schema"] = json!(revision);
            answer["admits"] = json!(false);
            answer["refusal"] = json!("version_mismatch");
            answer["diagnostics"] = json!(["older owner requires protocol revision 1"]);
        }
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
        if let Some((code, message)) = self.task_reads_remote_error.lock().unwrap().clone() {
            return Err(OrbitError::RemoteTool {
                code: code.clone(),
                message: message.clone(),
                payload: json!({"code": code, "message": message}),
            });
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

/// An approved owner task scoped to `file`, ready for admission, on `crew`
/// when one is named.
fn backlog_task(owner: &OrbitRuntime, repo: &Path, file: &str, crew: Option<&str>) -> String {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join(file), "fn work() {}\n").unwrap();
    let mut input = json!({
        "title": format!("Change {file}"),
        "description": "Distributed drain fixture task.",
        "acceptance_criteria": ["Changed."],
        "complexity": "low",
        "workspace": repo.to_string_lossy(),
        "type": "chore",
        "context_files": [format!("file:{file}")],
        "model": "codex"
    });
    if let Some(crew) = crew {
        input["crew"] = json!(crew);
    }
    let task = owner.run_tool("orbit.task.add", input).expect("add task");
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
        Self::with_crews(&vec![None; tasks])
    }

    /// An owner with one backlog task per entry of `crews`, in that order,
    /// each on its own file and on the named crew, and a replica follower
    /// routed to it. The follower can launch every provider it configures,
    /// so its window preflight runs every crew until a test says otherwise.
    fn with_crews(crews: &[Option<&str>]) -> Self {
        let root = TempDir::new().unwrap();
        let (owner, owner_repo) = open_runtime(root.path(), OWNER);
        let tasks = crews
            .iter()
            .enumerate()
            .map(|(n, crew)| backlog_task(&owner, &owner_repo, &format!("src/f{n}.rs"), *crew))
            .collect();
        let workspace_id = owner.workspace_id().unwrap();
        let wire = Arc::new(Wire {
            owner,
            calls: Mutex::default(),
            lose: Mutex::default(),
            task_reads: Mutex::default(),
            task_reads_fail: Mutex::default(),
            task_reads_remote_error: Mutex::default(),
            unreachable: Mutex::default(),
            protocol: Mutex::default(),
        });
        let (follower, follower_repo) = open_runtime(root.path(), FOLLOWER);
        let follower = follower
            .with_coordination_write_owner(Some(OWNER.into()))
            .with_drain_owner_transport(wire.clone());
        let follower_jobs = orbit_store::compose::workspace_job_run_store(
            follower.sqlite_store().unwrap(),
            follower.workspace_id().unwrap(),
        );
        let providers = follower
            .configured_crew_registry_projection()
            .crews
            .into_iter()
            .map(|crew| crew.provider)
            .collect::<BTreeSet<_>>();
        for provider in providers {
            follower_cli(&follower, &provider, "sh");
        }
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

    /// Point the follower's `provider` at `command`, as an operator's
    /// executor definition does.
    fn follower_cli(&self, provider: &str, command: &str) {
        follower_cli(&self.follower, provider, command);
    }

    /// The leaf ends the way a leaf whose step failed ends: the step's error
    /// recorded, then the run terminal, with no settlement of its own.
    fn leaf_fails_with(&self, leaf: &str, error: &str) {
        let now = Utc::now();
        self.follower_jobs
            .complete_job_run_step(
                leaf,
                &JobRunStepParams {
                    step_index: 0,
                    target_type: JobTargetType::Activity,
                    target_id: "implement_one".into(),
                    started_at: now,
                    finished_at: now,
                    duration_ms: None,
                    exit_code: Some(1),
                    agent_response_json: None,
                    state: JobRunState::Failed,
                    error_code: None,
                    error_message: Some(error.into()),
                },
            )
            .expect("failed step");
        self.follower_jobs
            .finalize_job_run(leaf, JobRunState::Failed, now, None)
            .expect("leaf failed");
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
        self.pass_with(drain, 1)
    }

    /// A pass with `slots` leaf slots.
    fn pass_with(&self, drain: &str, slots: u64) -> Value {
        self.follower
            .run_deterministic(
                "pull_refill",
                &json!({}),
                &json!({
                    "run_id": drain,
                    "destination": self.destination,
                    "window_expired": false,
                    "max_active_leaf_runs": slots,
                }),
                ToolContext::default(),
            )
            .expect("a pass reports its errors instead of failing the drain")
    }

    /// A running drain that names its owner in its input, as a submitted
    /// `orbit run auto --pull` does, so it carries the owner's admissions an
    /// ended drain left behind.
    fn run_owner_drain(&self, worker: u32) -> String {
        let run = self
            .follower_jobs
            .insert_job_run(
                "workspace_pull_pipeline",
                1,
                Utc::now(),
                Some(json!({"destination": self.destination})),
                None,
            )
            .expect("drain run");
        self.follower
            .write_run_state(
                &run.run_id,
                &PipelineState::new(run.run_id.clone(), run.job_id, json!({})),
            )
            .expect("drain state");
        self.follower_jobs
            .mark_job_run_running(&run.run_id, Utc::now(), worker)
            .expect("drain running");
        run.run_id
    }

    /// A drain whose worker is this process, as a running drain is; a cancel
    /// treats it as live and never signals it.
    fn run_drain(&self) -> String {
        self.run_drain_with_worker(std::process::id())
    }

    /// A running drain owned by the supplied worker process.
    fn run_drain_with_worker(&self, worker: u32) -> String {
        let drain = self.start_drain();
        self.follower_jobs
            .mark_job_run_running(&drain, Utc::now(), worker)
            .expect("drain running");
        drain
    }

    /// Admit the owner's next task as a queued leaf: the bind reply is lost,
    /// so the pass stops with the leaf created and the owner's claim bound.
    fn queued_leaf(&self, drain: &str, slots: u64) -> String {
        let before = self.leaf_runs();
        self.wire.lose_next_reply("orbit.drain.claim.bind");
        let pass = self.pass_with(drain, slots);
        assert!(error_of(&pass).contains("dropped"), "{pass}");
        self.leaf_runs()
            .into_iter()
            .find(|leaf| !before.contains(leaf))
            .expect("a new leaf")
    }

    /// A queued leaf taken through its launch the way the drain launches one,
    /// its worker this process — which no cancel here can stop.
    fn running_leaf(&self, drain: &str, slots: u64) -> String {
        self.launched_leaf(drain, slots, std::process::id())
    }

    /// A launched leaf whose worker is a real process in its own group, as a
    /// leaf's worker runs, so a forced cancel can stop it and see it gone.
    fn running_leaf_with_worker(&self, drain: &str, slots: u64) -> (String, Worker) {
        let worker = Worker::spawn();
        (self.launched_leaf(drain, slots, worker.pid), worker)
    }

    fn launched_leaf(&self, drain: &str, slots: u64, worker: u32) -> String {
        let leaf = self.queued_leaf(drain, slots);
        self.advance(&leaf, LocalPullMutation::Bound);
        self.advance(&leaf, LocalPullMutation::LaunchIntent);
        self.follower_jobs
            .mark_job_run_running(&leaf, Utc::now(), worker)
            .expect("leaf running");
        self.advance(&leaf, LocalPullMutation::Launched);
        leaf
    }

    fn admission(&self, leaf: &str) -> LocalPullAdmission {
        self.follower_jobs
            .local_pull_for_run(leaf)
            .expect("admission")
            .expect("claimed leaf")
    }

    fn advance(&self, leaf: &str, mutation: LocalPullMutation) {
        let record = self.admission(leaf);
        self.follower_jobs
            .mutate_local_pull(&record.destination, &record.request.request_id, &mutation)
            .expect("admission mutation");
    }

    /// The leaf ends the way a successful worker ends it: its handoff
    /// recorded as the settlement, then the run terminal.
    fn leaf_hands_off(&self, leaf: &str) {
        let record = self.admission(leaf);
        self.advance(
            leaf,
            LocalPullMutation::Settle(Box::new(ClaimMutation::AcceptHandoff(handoff(&record)))),
        );
        self.follower_jobs
            .finalize_job_run(leaf, JobRunState::Success, Utc::now(), None)
            .expect("leaf finished");
    }

    fn run_state(&self, run: &str) -> JobRunState {
        self.follower_jobs
            .get_job_run(run)
            .expect("run")
            .expect("run exists")
            .state
    }

    fn claimed_task(&self, leaf: &str) -> String {
        self.admission(leaf)
            .receipt
            .and_then(|receipt| receipt.claim)
            .expect("claim")
            .task_id
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

/// Register `provider`'s executor on `runtime` as launching `command`.
fn follower_cli(runtime: &OrbitRuntime, provider: &str, command: &str) {
    runtime
        .upsert_executor_def(&ExecutorDef {
            name: provider.to_string(),
            executor_type: ExecutorType::DirectAgent,
            command: Some(command.to_string()),
            args: vec![],
            stdout_format: None,
            model_pair_override: None,
            model_flag: None,
            timeout_seconds: None,
            env: Default::default(),
            sandbox: None,
            allow_fallback: false,
            created_at: None,
            updated_at: None,
        })
        .expect("executor");
}

/// A stand-in leaf worker: on Unix it leads its own process group, and is
/// reaped the moment it exits so a stop can see it gone.
struct Worker {
    pid: u32,
    exited: std::sync::mpsc::Receiver<()>,
}

impl Worker {
    fn spawn() -> Self {
        let mut command = std::process::Command::new("sleep");
        command.arg("600");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("worker process");
        let pid = child.id();
        let (exited, on_exit) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = child.wait();
            let _ = exited.send(());
        });
        Self {
            pid,
            exited: on_exit,
        }
    }

    /// Whether the process has exited and been reaped.
    fn stopped(&self) -> bool {
        self.exited.recv_timeout(Duration::from_secs(5)).is_ok()
    }

    fn running(&self) -> bool {
        self.exited.try_recv().is_err()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = std::process::Command::new("kill")
            .args(["-9", &self.pid.to_string()])
            .status();
    }
}

/// The typed handoff a successful leaf records for its claim.
fn handoff(record: &LocalPullAdmission) -> TaskHandoff {
    let claim = record
        .receipt
        .as_ref()
        .and_then(|receipt| receipt.claim.as_ref())
        .expect("claim");
    TaskHandoff {
        schema_version: 1,
        workspace_id: record.destination.owner_workspace_id.clone(),
        task_id: claim.task_id.clone(),
        claim_id: claim.claim_id.clone(),
        machine_id: claim.executed_on.machine_id.clone(),
        run_id: record.leaf_run_id.clone().expect("leaf"),
        candidate: HandoffCandidate {
            repository: "owner/repository".into(),
            source_branch: format!("orbit/{}", claim.task_id),
            base_branch: "main".into(),
            landing_branch: "main".into(),
            candidate: SourceRevision {
                commit: "a".repeat(40),
                tree: "b".repeat(40),
            },
            base: SourceRevision {
                commit: "c".repeat(40),
                tree: "d".repeat(40),
            },
            delivery: HandoffDelivery::PullRequest { number: 42 },
        },
        review: HandoffReview {
            policy: ReviewTiming::None,
            disposition: HandoffReviewDisposition::NotRequired,
        },
        execution_summary: "Outcome: success".into(),
        validation: vec![],
    }
}

fn error_of(pass: &Value) -> &str {
    pass["error"].as_str().unwrap_or_default()
}

/// Whether a pass ended at a refused leaf launch rather than at the owner.
fn launch_refused(pass: &Value) -> bool {
    error_of(pass).contains("re-exec")
}

/// An unreadable persisted cancel request fails the whole pass visibly; it
/// cannot probe, request or launch work without readable control state.
#[test]
fn unreadable_cancel_state_fails_visibly_without_admission() {
    if !isolated("unreadable_cancel_state_fails_visibly_without_admission") {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    pair.follower
        .cancel_job_run_with_options(&drain, "operator", "cli", None, false)
        .expect("persist graceful cancellation");
    let state = pair.follower.read_run_state(&drain).unwrap().unwrap();
    assert!(state.drain_cancelling());
    let store = pair.follower.sqlite_store().unwrap();
    let workspace = pair.follower.workspace_id().unwrap();
    store
        .with_transaction(|tx| {
            let changed = tx.connection().execute(
                "UPDATE job_runs SET pipeline_state_json = '{' WHERE workspace_id = ?1 AND run_id = ?2",
                [workspace.as_str(), drain.as_str()],
            ).unwrap();
            assert_eq!(changed, 1);
            Ok(())
        })
        .unwrap();
    let read_error = pair
        .follower
        .read_run_state(&drain)
        .unwrap_err()
        .to_string();

    for expired in [false, true] {
        let failure = pair
            .follower
            .run_deterministic(
                "pull_refill",
                &json!({}),
                &json!({"run_id": drain, "destination": pair.destination,
                "window_expired": expired}),
                ToolContext::default(),
            )
            .expect_err("unreadable pass health fails visibly, without touching admissions");
        assert!(failure.to_string().contains(&read_error), "{failure}");
    }
    assert!(pair.wire.calls("orbit.drain.probe").is_empty());
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
    assert!(pair.owner_claims().is_empty());
    assert!(pair.leaf_runs().is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");

    pair.follower.write_run_state(&drain, &state).unwrap();
    let recovered = pair.pass(&drain);
    assert!(recovered["error"].is_null(), "{recovered}");
    assert_eq!(recovered["cancelling"], true, "{recovered}");
    assert_eq!(recovered["done"], true, "{recovered}");
    assert_eq!(pair.run_state(&drain), JobRunState::Cancelled);
    assert!(pair.wire.calls("orbit.drain.probe").is_empty());
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
}

/// Revision 1 owners reject additive fields despite equal binary versions.
/// Negotiation must stop the newer follower before it sends `crews`.
#[test]
fn an_older_owner_is_refused_before_a_newer_request_is_sent() {
    if !isolated("an_older_owner_is_refused_before_a_newer_request_is_sent") {
        return;
    }
    let pair = Pair::new(1);
    *pair.wire.protocol.lock().unwrap() = Some(1);
    let drain = pair.start_drain();
    let pass = pair.pass(&drain);
    let refusal = pass["refusal"].as_str().unwrap();
    assert!(refusal.starts_with("protocol_mismatch:"), "{pass}");
    assert!(
        refusal.contains("caller revision 2; owner revision 1"),
        "{pass}"
    );
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
    assert!(pair.owner_claims().is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");

    // A newer owner's boundary likewise refuses an older follower by type.
    *pair.wire.protocol.lock().unwrap() = None;
    let probe = pair
        .wire
        .call(
            "",
            "orbit.drain.probe",
            json!({
                "caller_version": orbit_core::application::distributed::owner_binary_version(),
                "caller_schema": 1, "caller_review_policy": "none",
            }),
        )
        .unwrap();
    assert_eq!(probe["refusal"], "protocol_mismatch");
    assert!(
        probe["diagnostics"]
            .to_string()
            .contains("caller revision 1; owner revision 2")
    );
    let request = json!({"request_id": "old-request", "caller_version": probe["binary_version"],
        "caller_schema": 1, "caller_review_policy": "none", "ship": probe["ship"],
        "run_context": {"run_id": "old-drain", "job_name": "workspace_pull_pipeline"}});
    let failure = pair.wire.call("", "orbit.task.pull", request).unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("protocol_mismatch: caller revision 1; owner revision 2"),
        "{failure}"
    );
    assert!(pair.owner_claims().is_empty());
}

/// Transport errors persist, a successful pass resets a transient streak,
/// and three failures latch a warning instead of idling invisibly forever.
#[test]
fn repeated_pass_failures_degrade_the_drain_and_a_new_drain_resets_it() {
    if !isolated("repeated_pass_failures_degrade_the_drain_and_a_new_drain_resets_it") {
        return;
    }
    let pair = Pair::new(0);
    let drain = pair.start_drain();
    *pair.wire.unreachable.lock().unwrap() = true;
    assert_eq!(pair.pass(&drain)["consecutive_pass_failures"], 1);
    *pair.wire.unreachable.lock().unwrap() = false;
    let recovered = pair.pass(&drain);
    assert_eq!(recovered["consecutive_pass_failures"], 0);
    assert!(recovered["last_pass_error"].is_null());
    *pair.wire.unreachable.lock().unwrap() = true;
    for count in 1..=3 {
        let pass = pair.pass(&drain);
        assert_eq!(pass["consecutive_pass_failures"], count);
        assert_eq!(pass["degraded"], count == 3);
        let state = pair.follower.read_run_state(&drain).unwrap().unwrap();
        let health = state.drain_last_pass.unwrap();
        assert_eq!(health.consecutive_pass_failures, count);
        assert!(
            health
                .last_pass_error
                .unwrap()
                .contains("Connection timed out")
        );
    }
    *pair.wire.unreachable.lock().unwrap() = false;
    let before = pair.wire.calls("orbit.drain.probe").len();
    let held = pair.pass(&drain);
    assert_eq!(held["admitting"], false);
    assert_eq!(held["degraded"], true);
    assert!(
        held["refusal"]
            .as_str()
            .unwrap()
            .starts_with("pass_failures:")
    );
    assert_eq!(pair.wire.calls("orbit.drain.probe").len(), before);
    let fresh = pair.pass(&pair.start_drain());
    assert_eq!(fresh["degraded"], false);
    assert_eq!(fresh["consecutive_pass_failures"], 0);
}

/// A readable run state with no cancellation still probes and admits work.
#[test]
fn readable_state_without_cancel_permits_refill() {
    if !isolated("readable_state_without_cancel_permits_refill") {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.start_drain();
    assert!(
        !pair
            .follower
            .read_run_state(&drain)
            .unwrap()
            .unwrap()
            .drain_cancelling()
    );

    let pass = pair.pass(&drain);
    assert!(
        launch_refused(&pass),
        "the admitted leaf reaches launch: {pass}"
    );
    assert_eq!(pass["cancelling"], false, "{pass}");
    assert_eq!(pair.wire.calls("orbit.drain.probe").len(), 1);
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 1);
    assert_eq!(pair.owner_claims().len(), 1);
    assert_eq!(pair.leaf_runs().len(), 1);
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

fn excluded(pass: &Value, crew: &str) -> Value {
    pass["crews"]["excluded"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|exclusion| exclusion["crew"] == crew)
        .cloned()
        .unwrap_or(Value::Null)
}

/// A follower whose window preflight cannot find crew `antigravity`'s CLI is
/// never handed an `antigravity` task [ORB-13941]. The owner admits the next
/// task it can run instead and, once only the unrunnable one is left, answers
/// idle and names it, so the task stays in the backlog for the owner or
/// another follower rather than being claimed and failed here.
#[test]
fn a_follower_never_receives_a_claim_for_a_crew_its_window_cannot_run() {
    if !isolated("a_follower_never_receives_a_claim_for_a_crew_its_window_cannot_run") {
        return;
    }
    let pair = Pair::with_crews(&[Some("antigravity"), Some("sol")]);
    pair.follower_cli("antigravity", "orbit-test-no-such-provider-cli");
    let (unrunnable, runnable) = (&pair.tasks[0], &pair.tasks[1]);
    let drain = pair.start_drain();

    let first = pair.pass(&drain);
    assert!(launch_refused(&first), "{first}");
    let exclusion = excluded(&first, "antigravity");
    assert_eq!(exclusion["source"], "preflight", "{first}");
    assert!(
        exclusion["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("orbit-test-no-such-provider-cli")),
        "{first}"
    );
    let idle = pair.pass(&drain);
    assert_eq!(idle["admitted"], 0, "{idle}");

    let claims = pair.owner_claims();
    assert_eq!(claims.len(), 1, "{claims:#?}");
    assert_eq!(claims[0]["claim"]["task_id"], runnable.as_str());
    assert_eq!(pair.owner_status(unrunnable), "backlog");
    let pulls = pair.wire.calls("orbit.task.pull");
    assert_eq!(pulls.len(), 2, "{pulls:?}");
    for pull in &pulls {
        let runnable = pull["crews"]["runnable"]
            .as_array()
            .expect("declared crews");
        assert!(runnable.iter().any(|crew| crew == "sol"), "{pull}");
        assert!(runnable.iter().all(|crew| crew != "antigravity"), "{pull}");
    }
    let idle_receipt = pair
        .follower_jobs
        .local_pull_admissions()
        .unwrap()
        .into_iter()
        .find(|record| record.phase == LocalPullPhase::Idle)
        .and_then(|record| record.receipt)
        .expect("the owner answered idle");
    assert!(
        idle_receipt
            .crew_unavailable
            .iter()
            .any(|skipped| &skipped.task_id == unrunnable && skipped.reason.contains("antigravity")),
        "{idle_receipt:#?}"
    );

    let window = pair
        .follower
        .pull_drain_crew_window(&drain)
        .unwrap()
        .expect("a pull drain has a crew window");
    assert!(window.checked_at.is_some());
    assert!(
        window
            .excluded
            .iter()
            .any(|exclusion| exclusion.crew == "antigravity"),
        "{window:#?}"
    );
}

/// A claimed leaf whose provider refused to authenticate gives its claim
/// back [ORB-13941]: the owner's task returns to the backlog rather than
/// `blocked`, the failure breaker does not count it, and the drain offers
/// that crew no more for the rest of its window, so the same task is not
/// pulled straight back to fail the same way.
#[test]
fn a_provider_auth_failure_releases_the_claim_and_excludes_the_crew_for_the_window() {
    if !isolated("a_provider_auth_failure_releases_the_claim_and_excludes_the_crew_for_the_window")
    {
        return;
    }
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    pair.leaf_fails_with(
        &leaf,
        "[provider_unavailable] claude provider authentication failure (HTTP 401): \
         Failed to authenticate: OAuth token revoked. Please log in again or contact your administrator.",
    );

    let pass = pair.pass(&drain);
    assert_eq!(pair.owner_status(&task), "backlog", "{pass}");
    assert_eq!(pass["consecutive_failures"], 0, "{pass}");
    assert_eq!(pass["admitted"], 0, "{pass}");
    let exclusion = excluded(&pass, "sol");
    assert_eq!(exclusion["source"], "provider_unavailable", "{pass}");
    assert!(
        exclusion["reason"].as_str().is_some_and(
            |reason| reason.contains(task.as_str()) && reason.contains("OAuth token revoked")
        ),
        "{pass}"
    );

    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 1, "{settles:?}");
    let release = &settles[0]["settlement"]["Release"];
    assert_eq!(
        release["provider_unavailable"]["crew"], "sol",
        "{settles:?}"
    );
    let claims = pair.owner_claims();
    assert_eq!(
        claims.len(),
        1,
        "the released task is not pulled back: {claims:#?}"
    );
    assert_eq!(claims[0]["claim"]["phase"], "revoked");
    assert!(
        comments_of(&pair.owner_task(&task)).contains("could not use the provider of crew `sol`"),
        "{}",
        pair.owner_task(&task)
    );
    let pulls = pair.wire.calls("orbit.task.pull");
    let last = pulls.last().expect("the pass asked again");
    assert!(
        last["crews"]["excluded"]
            .as_array()
            .is_some_and(|excluded| excluded.iter().any(|exclusion| exclusion["crew"] == "sol")),
        "{last}"
    );

    let window = pair
        .follower
        .pull_drain_crew_window(&drain)
        .unwrap()
        .expect("a pull drain has a crew window");
    assert!(
        window
            .runnable
            .as_ref()
            .is_some_and(|runnable| !runnable.iter().any(|crew| crew == "sol")),
        "{window:#?}"
    );
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

/// [ORB-13901] While sustained host pressure throttles the follower, its
/// drain requests no claim but still delivers a settlement it owes; once
/// memory is back below its resume mark the next pass pulls again.
#[test]
fn a_throttled_pull_drain_keeps_settling_and_pulls_again_below_resume() {
    if !isolated("a_throttled_pull_drain_keeps_settling_and_pulls_again_below_resume") {
        return;
    }
    let mut pair = Pair::new(2);
    let probe = super::dispatch_admission::PressureProbe::calm();
    pair.follower = pair
        .follower
        .clone()
        .with_host_resource_probe(probe.clone());
    let drain = pair.start_drain();

    // The first claim fails at launch and its settlement reply is lost, so
    // the drain owes the owner that settlement.
    pair.wire.lose_next_reply("orbit.drain.claim.settle");
    let owed = pair.pass(&drain);
    assert!(error_of(&owed).contains("dropped"), "{owed}");
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 1);

    probe.sustain_memory(&pair.follower, 96.0);
    let held = pair.pass(&drain);
    assert_eq!(held["admitting"], false, "{held}");
    assert_eq!(held["admitted"], 0, "{held}");
    assert_eq!(
        held["resource_throttle"]["resources"][0]["resource"], "memory",
        "{held}"
    );
    assert_eq!(held["sleep_seconds"], 30, "a throttled drain polls: {held}");
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 1, "no new claim");
    assert_eq!(
        pair.follower.pending_pull_settlements().unwrap().count,
        0,
        "the owed settlement is delivered while throttled"
    );
    assert_eq!(pair.wire.calls("orbit.drain.claim.settle").len(), 2);
    let unclaimed = pair
        .tasks
        .iter()
        .filter(|id| pair.owner_status(id) == "backlog")
        .count();
    assert_eq!(unclaimed, 1, "the second task stays on the owner");
    let recorded = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .drain_last_pass
        .and_then(|pass| pass.resource_throttle)
        .expect("the pull pass records the throttle");
    assert_eq!(recorded.resources[0].percent, 96.0);

    probe.memory(70.0, Utc::now());
    let resumed = pair.pass(&drain);
    assert!(launch_refused(&resumed), "{resumed}");
    assert_eq!(resumed["resource_throttle"], Value::Null, "{resumed}");
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 2);
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

/// [ORB-13920] An accepted handoff is all a follower needs to give back its
/// leaf's disk. The drain's next pass reclaims the leaf's `target/` and keeps
/// the checkout; worktree GC then removes the checkout on the strength of the
/// settled admission alone. Every owner task read would fail here, and none
/// is made.
#[test]
fn an_accepted_handoff_gives_back_its_build_output_and_then_its_worktree() {
    if !isolated("an_accepted_handoff_gives_back_its_build_output_and_then_its_worktree") {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    let (leaf, worker) = pair.running_leaf_with_worker(&drain, 1);
    pair.leaf_hands_off(&leaf);
    assert!(
        std::process::Command::new("kill")
            .args(["-TERM", &worker.pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert!(worker.stopped(), "the handed-off leaf's worker has exited");
    let settled = pair.pass(&drain);
    assert!(settled["error"].is_null(), "{settled}");
    assert!(matches!(
        pair.admission(&leaf).settlement,
        Some(ClaimMutation::AcceptHandoff(_))
    ));
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
        "an accepted handoff needs no task read or follower task records"
    );
}

/// [ORB-13950] A forced release settles the admission but returns unfinished
/// work to the owner's backlog. GC must keep the clean checkout and its
/// unhanded commits, including when the owner cannot be queried.
#[test]
fn a_released_claim_keeps_its_backlogged_worktree_and_unhanded_commit() {
    if !isolated("a_released_claim_keeps_its_backlogged_worktree_and_unhanded_commit") {
        return;
    }
    let pair = Pair::new(1);
    let drain_worker = Worker::spawn();
    let drain = pair.run_drain_with_worker(drain_worker.pid);
    let (leaf, worker) = pair.running_leaf_with_worker(&drain, 1);
    let task = pair.claimed_task(&leaf);
    let (worktree, _) = leaf_worktree(&pair, &leaf);
    std::fs::write(worktree.join("unfinished.rs"), "fn unfinished() {}\n").unwrap();
    git(&worktree, &["add", "unfinished.rs"]);
    git(&worktree, &["commit", "-q", "-m", "unfinished work"]);
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    assert!(git(&worktree, &["status", "--porcelain"]).is_empty());

    pair.follower
        .cancel_job_run_with_options(&drain, "operator", "cli", Some("host maintenance"), true)
        .expect("forced cancel");
    assert!(worker.stopped());
    assert_eq!(pair.run_state(&leaf), JobRunState::Cancelled);
    assert_eq!(pair.owner_status(&task), "backlog");
    let admission = pair.admission(&leaf);
    assert_eq!(admission.phase, LocalPullPhase::Settled);
    assert!(matches!(
        admission.settlement,
        Some(ClaimMutation::Release(_))
    ));

    let gc = pair
        .follower
        .gc_worktrees(true, None, None, false, false)
        .unwrap();
    let report = gc_report(&gc, &leaf);
    assert_eq!(
        report["action"], "skipped:task_status_ineligible",
        "{report:#}"
    );
    assert_eq!(report["task_status"], "backlog", "{report:#}");
    assert!(worktree.join("unfinished.rs").exists());
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        *pair.wire.task_reads.lock().unwrap(),
        vec![pair.destination["selector"].as_str().unwrap().to_string()],
        "a release still asks its owner over the claim's route"
    );

    *pair.wire.task_reads_fail.lock().unwrap() = Some("owner unreachable".into());
    let gc = pair
        .follower
        .gc_worktrees(true, None, None, false, false)
        .unwrap();
    let report = gc_report(&gc, &leaf);
    assert_eq!(report["action"], "skipped:owner_unreachable", "{report:#}");
    assert!(worktree.join("unfinished.rs").exists());
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), head);
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
    *pair.wire.task_reads_remote_error.lock().unwrap() = Some((
        "execution_failed".into(),
        "owner task store unavailable".into(),
    ));
    let owner_error = pair
        .follower
        .gc_worktrees(false, None, None, false, false)
        .unwrap();
    let report = gc_report(&owner_error, &leaf);
    assert_eq!(
        report["action"], "skipped:owner_lookup_failed",
        "{report:#}"
    );
    assert!(
        report["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("owner task store unavailable")),
        "a structured owner error proves the route answered: {report:#}"
    );

    *pair.wire.task_reads_remote_error.lock().unwrap() = None;
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

/// The owner's comments on `task`, as one searchable text.
fn comments_of(task: &Value) -> String {
    task["comments"].to_string()
}

/// [ORB-13892] Cancelling a running pull drain is graceful. The request
/// returns at once naming the leaf it waits for; the drain's next pass stops
/// requesting and returns the claim it never launched to the owner's backlog
/// with a comment naming the drain, while the launched leaf keeps running.
/// Once that leaf hands off, the next pass delivers it and the drain ends
/// `cancelled`.
#[test]
fn a_graceful_drain_cancel_releases_unlaunched_claims_and_waits_for_running_leaves() {
    if !isolated("a_graceful_drain_cancel_releases_unlaunched_claims_and_waits_for_running_leaves")
    {
        return;
    }
    let pair = Pair::new(3);
    let drain = pair.run_drain();
    let running = pair.running_leaf(&drain, 2);
    let queued = pair.queued_leaf(&drain, 2);
    let (running_task, queued_task) = (pair.claimed_task(&running), pair.claimed_task(&queued));

    let cancel = pair
        .follower
        .cancel_job_run_with_options(&drain, "operator", "cli", Some("host maintenance"), false)
        .expect("cancel");
    assert_eq!(cancel.outcome, "cancelling");
    let waiting: Vec<_> = cancel
        .waiting_leaves
        .iter()
        .map(|leaf| leaf.leaf_run_id.as_str())
        .collect();
    assert_eq!(waiting, vec![running.as_str()]);
    assert_eq!(pair.run_state(&drain), JobRunState::Running);

    let waits = pair.pass_with(&drain, 2);
    assert_eq!(waits["cancelling"], true, "{waits}");
    assert_eq!(waits["done"], false, "{waits}");
    assert!(waits["error"].is_null(), "{waits}");
    assert_eq!(
        waits["waiting_leaves"][0]["leaf_run_id"],
        running.as_str(),
        "{waits}"
    );
    assert_eq!(
        pair.wire.calls("orbit.task.pull").len(),
        2,
        "no new request"
    );
    assert_eq!(
        pair.owner_status(&queued_task),
        "backlog",
        "the unlaunched claim is released"
    );
    let released = pair.owner_task(&queued_task);
    assert!(comments_of(&released).contains(&drain), "{released:#}");
    assert!(
        comments_of(&released).contains("host maintenance"),
        "{released:#}"
    );
    assert_eq!(pair.run_state(&queued), JobRunState::Cancelled);
    assert_eq!(
        pair.run_state(&running),
        JobRunState::Running,
        "the launched leaf keeps going"
    );
    assert_eq!(pair.owner_status(&running_task), "in-progress");
    assert_eq!(pair.run_state(&drain), JobRunState::Running);

    pair.leaf_hands_off(&running);
    let done = pair.pass_with(&drain, 2);
    assert_eq!(done["done"], true, "{done}");
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert!(
        settles
            .iter()
            .any(|settle| settle["run_id"] == running.as_str()
                && settle["settlement"].get("AcceptHandoff").is_some()),
        "the leaf's own success is delivered: {settles:?}"
    );
    assert_eq!(pair.run_state(&drain), JobRunState::Cancelled);
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 0);
    let state = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .expect("state");
    assert!(
        state.drain_cancelling(),
        "the cancel request stays on record"
    );
}

/// A drain owner that cannot be confirmed stopped must keep every carried
/// claim and leave both the drain and its leaves unfinalized.
#[test]
fn a_forced_drain_cancel_refuses_an_unconfirmed_drain_owner() {
    if !isolated("a_forced_drain_cancel_refuses_an_unconfirmed_drain_owner") {
        return;
    }
    let pair = Pair::new(2);
    let drain = pair.run_drain();
    let (running, worker) = pair.running_leaf_with_worker(&drain, 2);
    let queued = pair.queued_leaf(&drain, 2);
    let claims_before = pair.owner_claims();
    let state_before = pair.follower.read_run_state(&drain).unwrap();

    let error = pair
        .follower
        .cancel_job_run_with_options(&drain, "operator", "cli", None, true)
        .expect_err("an unconfirmed drain stop cannot release claims");
    assert!(matches!(error, OrbitError::Execution(_)), "{error}");
    let diagnostic = error.to_string();
    assert!(diagnostic.contains(&drain), "{diagnostic}");
    assert!(diagnostic.contains("stopped"), "{diagnostic}");
    assert!(diagnostic.contains("self_not_signalled"), "{diagnostic}");
    assert_eq!(pair.run_state(&drain), JobRunState::Running);
    assert_eq!(pair.follower.read_run_state(&drain).unwrap(), state_before);
    assert!(worker.running(), "the leaf is never signalled");
    assert_eq!(pair.run_state(&running), JobRunState::Running);
    assert_eq!(pair.run_state(&queued), JobRunState::Pending);
    for leaf in [&running, &queued] {
        assert_eq!(pair.admission(leaf).settlement, None);
        assert_eq!(pair.owner_status(&pair.claimed_task(leaf)), "in-progress");
    }
    assert_eq!(pair.owner_claims(), claims_before);
    assert!(pair.wire.calls("orbit.drain.claim.settle").is_empty());
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 0);
}

/// An owner that already exited still permits forced leaf cancellation and
/// delivery of both launched and unlaunched claim releases.
#[test]
fn a_forced_drain_cancel_accepts_an_already_exited_drain_owner() {
    if !isolated("a_forced_drain_cancel_accepts_an_already_exited_drain_owner") {
        return;
    }
    let pair = Pair::new(2);
    let drain_worker = Worker::spawn();
    let drain = pair.run_drain_with_worker(drain_worker.pid);
    let (running, worker) = pair.running_leaf_with_worker(&drain, 2);
    let queued = pair.queued_leaf(&drain, 2);
    assert!(
        std::process::Command::new("kill")
            .args(["-9", &drain_worker.pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        drain_worker.stopped(),
        "the owner has exited and been reaped"
    );

    let cancel = pair
        .follower
        .cancel_job_run_with_options(&drain, "operator", "cli", None, true)
        .expect("an already-exited owner permits forced cancellation");
    assert_eq!(cancel.signal_outcome.as_deref(), Some("already_exited"));
    assert_eq!(cancel.outcome, "cancelled");
    assert_eq!(cancel.forced_runs, vec![running.clone()]);
    assert!(cancel.unstopped_leaves.is_empty(), "{cancel:?}");
    assert!(worker.stopped());
    assert_eq!(pair.run_state(&drain), JobRunState::Cancelled);
    for leaf in [&running, &queued] {
        assert_eq!(pair.run_state(leaf), JobRunState::Cancelled);
        assert_eq!(pair.owner_status(&pair.claimed_task(leaf)), "backlog");
        assert_eq!(pair.admission(leaf).phase, LocalPullPhase::Settled);
    }
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 2, "{settles:?}");
    assert!(
        settles
            .iter()
            .all(|settle| settle["settlement"].get("Release").is_some())
    );
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 0);
}

/// [ORB-13892] `force` stops a running leaf instead of waiting for it: its
/// claim is released before the leaf is cancelled, and both the running and
/// the unlaunched claim go back to the owner's backlog with a comment naming
/// the drain and the reason.
#[test]
fn a_forced_drain_cancel_stops_running_leaves_and_returns_their_tasks_to_backlog() {
    if !isolated("a_forced_drain_cancel_stops_running_leaves_and_returns_their_tasks_to_backlog") {
        return;
    }
    let pair = Pair::new(2);
    let drain_worker = Worker::spawn();
    let drain = pair.run_drain_with_worker(drain_worker.pid);
    let (running, worker) = pair.running_leaf_with_worker(&drain, 2);
    let queued = pair.queued_leaf(&drain, 2);

    let cancel = pair
        .follower
        .cancel_job_run_with_options(&drain, "operator", "cli", Some("host maintenance"), true)
        .expect("forced cancel");
    assert_eq!(cancel.outcome, "cancelled");
    assert!(
        drain_worker.stopped(),
        "the drain worker stops before release"
    );
    assert!(
        matches!(
            cancel.signal_outcome.as_deref(),
            Some(
                "terminated_process_group"
                    | "killed_process_group"
                    | "terminated_owner"
                    | "killed_owner"
            )
        ),
        "{cancel:?}"
    );
    assert_eq!(cancel.forced_runs, vec![running.clone()]);
    assert!(cancel.unstopped_leaves.is_empty(), "{cancel:?}");
    assert!(worker.stopped(), "the leaf's worker is stopped");
    assert_eq!(pair.run_state(&drain), JobRunState::Cancelled);
    for leaf in [&running, &queued] {
        assert_eq!(pair.run_state(leaf), JobRunState::Cancelled, "{leaf}");
        let task = pair.claimed_task(leaf);
        let owner = pair.owner_task(&task);
        assert_eq!(owner["status"], "backlog", "{owner:#}");
        assert!(comments_of(&owner).contains(&drain), "{owner:#}");
        assert!(
            comments_of(&owner).contains("host maintenance"),
            "{owner:#}"
        );
    }
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert!(
        settles
            .iter()
            .all(|settle| settle["settlement"].get("Release").is_some()),
        "nothing is failed: {settles:?}"
    );
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 0);
}

/// [ORB-13892] A settlement the owner could not take while it was down stays
/// pending, and the cancelling drain waits for it rather than ending; once
/// the owner answers again, the drain's next pass delivers it and the drain
/// ends. No new drain is needed.
#[test]
fn a_pending_settlement_is_retried_after_an_owner_outage_before_the_drain_ends() {
    if !isolated("a_pending_settlement_is_retried_after_an_owner_outage_before_the_drain_ends") {
        return;
    }
    let pair = Pair::new(2);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let cancel = pair
        .follower
        .cancel_job_run_with_options(&drain, "operator", "cli", None, false)
        .expect("cancel");
    assert_eq!(cancel.outcome, "cancelling");

    pair.leaf_hands_off(&leaf);
    *pair.wire.unreachable.lock().unwrap() = true;
    for _ in 0..3 {
        pair.pass(&drain);
    }
    let outage = pair.pass(&drain);
    assert_eq!(outage["degraded"], true, "{outage}");
    assert_eq!(outage["done"], false, "{outage}");
    assert!(
        error_of(&outage).contains("Connection timed out"),
        "{outage}"
    );
    assert_eq!(pair.run_state(&drain), JobRunState::Running);
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 1);

    *pair.wire.unreachable.lock().unwrap() = false;
    let recovered = pair.pass(&drain);
    assert_eq!(recovered["done"], true, "{recovered}");
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 0);
    assert_eq!(pair.run_state(&drain), JobRunState::Cancelled);
    assert_eq!(
        pair.wire.calls("orbit.task.pull").len(),
        1,
        "no new request"
    );
    let claim = pair
        .follower
        .pull_leaf_claim(&leaf)
        .unwrap()
        .expect("claimed leaf");
    assert_eq!(claim.settlement_phase, "settled");
    assert_eq!(claim.refusal, None, "delivered, not closed as obsolete");
}

/// [ORB-13892] `force` stops only what the cancelled drain carries: a leaf
/// another live drain for the same owner admitted keeps running, and its
/// task stays claimed.
#[test]
fn a_forced_drain_cancel_leaves_another_live_drains_leaves_alone() {
    if !isolated("a_forced_drain_cancel_leaves_another_live_drains_leaves_alone") {
        return;
    }
    let pair = Pair::new(2);
    let drain_worker = Worker::spawn();
    let cancelled = pair.run_owner_drain(drain_worker.pid);
    let other = pair.run_owner_drain(std::process::id());
    let (mine, my_worker) = pair.running_leaf_with_worker(&cancelled, 2);
    let (theirs, their_worker) = pair.running_leaf_with_worker(&other, 2);

    let cancel = pair
        .follower
        .cancel_job_run_with_options(&cancelled, "operator", "cli", None, true)
        .expect("forced cancel");
    assert_eq!(cancel.forced_runs, vec![mine.clone()]);
    assert!(my_worker.stopped());
    assert_eq!(pair.owner_status(&pair.claimed_task(&mine)), "backlog");

    assert!(
        their_worker.running(),
        "another drain's leaf is not signalled"
    );
    assert_eq!(pair.run_state(&theirs), JobRunState::Running);
    assert_eq!(pair.run_state(&other), JobRunState::Running);
    assert_eq!(
        pair.owner_status(&pair.claimed_task(&theirs)),
        "in-progress"
    );
    assert_eq!(pair.admission(&theirs).settlement, None);
}

/// [ORB-13892] A leaf `force` cannot stop and see gone keeps its claim: no
/// release is recorded or sent, the cancel reports it as unstopped, and the
/// leaf's own outcome still reaches the owner when it ends.
#[test]
fn a_forced_drain_cancel_keeps_the_claim_of_a_leaf_it_cannot_confirm_stopped() {
    if !isolated("a_forced_drain_cancel_keeps_the_claim_of_a_leaf_it_cannot_confirm_stopped") {
        return;
    }
    let pair = Pair::new(1);
    let drain_worker = Worker::spawn();
    let drain = pair.run_drain_with_worker(drain_worker.pid);
    // Its worker is this process, which a cancel never signals.
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);

    let cancel = pair
        .follower
        .cancel_job_run_with_options(&drain, "operator", "cli", None, true)
        .expect("forced cancel");
    assert!(cancel.forced_runs.is_empty(), "{cancel:?}");
    assert_eq!(cancel.unstopped_leaves.len(), 1, "{cancel:?}");
    assert_eq!(cancel.unstopped_leaves[0].leaf_run_id, leaf);
    assert!(
        cancel.unstopped_leaves[0].reason.contains("claim stays"),
        "{cancel:?}"
    );
    assert_eq!(pair.run_state(&drain), JobRunState::Cancelled);
    assert_eq!(pair.run_state(&leaf), JobRunState::Running);
    assert_eq!(
        pair.admission(&leaf).settlement,
        None,
        "nothing was released"
    );
    assert_eq!(pair.owner_status(&task), "in-progress");
    assert!(pair.wire.calls("orbit.drain.claim.settle").is_empty());

    pair.leaf_hands_off(&leaf);
    pair.follower.deliver_recorded_pull_settlements();
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 1, "{settles:?}");
    assert!(
        settles[0]["settlement"].get("AcceptHandoff").is_some(),
        "{settles:?}"
    );
}

/// [ORB-13892] The existing MCP stop control takes `force`: it stops
/// admissions, cancels the drain, stops its running leaf and returns the
/// leaf's task to the owner's backlog.
#[test]
fn the_mcp_stop_control_with_force_stops_claimed_leaves() {
    if !isolated("the_mcp_stop_control_with_force_stops_claimed_leaves") {
        return;
    }
    let pair = Pair::new(1);
    let drain_worker = Worker::spawn();
    let drain = pair.run_drain_with_worker(drain_worker.pid);
    let (leaf, worker) = pair.running_leaf_with_worker(&drain, 1);
    let task = pair.claimed_task(&leaf);

    let stopped = pair
        .follower
        .run_tool_with_context_and_role(
            "orbit.workflow.auto",
            json!({
                "workspace": pair.follower.workspace_id().unwrap(),
                "action": "stop",
                "force": true,
            }),
            Role::Admin,
            ToolContext {
                session_context: ToolSessionContext {
                    transport: Some(McpTransport::Local),
                    effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                    ..ToolSessionContext::default()
                },
                ..ToolContext::default()
            },
        )
        .expect("forced stop");
    assert_eq!(stopped["outcome"], "force_cancelled", "{stopped:#}");
    assert_eq!(stopped["coordinators"][0]["run_id"], drain.as_str());
    assert_eq!(
        stopped["coordinators"][0]["forced_runs"],
        json!([leaf]),
        "{stopped:#}"
    );
    assert!(worker.stopped());
    assert_eq!(pair.run_state(&drain), JobRunState::Cancelled);
    assert_eq!(pair.run_state(&leaf), JobRunState::Cancelled);
    assert_eq!(pair.owner_status(&task), "backlog");
}

/// Discovery for a follower host with one replica checkout and no owner
/// checkout: nothing to schedule, only settlements to deliver.
struct ReplicaOnly(OrbitRuntime);

impl RoutineWorkspaceProvider for ReplicaOnly {
    fn discover_workspaces(&self, _: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        let workspace = Workspace {
            id: self.0.workspace_id()?,
            name: "replica".into(),
            owner_machine_id: Some(OWNER.into()),
            git_remote: None,
            ship_mode: None,
            base_branch: "main".into(),
            status: WorkspaceStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        Ok(DiscoveredWorkspaces {
            replicas: vec![(workspace, self.0.clone())],
            ..DiscoveredWorkspaces::default()
        })
    }
}

/// [ORB-13892] A forced release the owner was down for is retried by the
/// clock sweep once the leaf's worker and the drain are both gone: no new
/// drain, and the task returns to the owner's backlog.
#[test]
fn a_forced_release_the_owner_missed_is_delivered_by_the_clock_sweep() {
    if !isolated("a_forced_release_the_owner_missed_is_delivered_by_the_clock_sweep") {
        return;
    }
    let pair = Pair::new(1);
    let drain_worker = Worker::spawn();
    let drain = pair.run_drain_with_worker(drain_worker.pid);
    let (leaf, worker) = pair.running_leaf_with_worker(&drain, 1);
    let task = pair.claimed_task(&leaf);

    *pair.wire.unreachable.lock().unwrap() = true;
    let cancel = pair
        .follower
        .cancel_job_run_with_options(&drain, "operator", "cli", Some("host maintenance"), true)
        .expect("forced cancel");
    assert_eq!(cancel.forced_runs, vec![leaf.clone()]);
    assert!(worker.stopped());
    assert_eq!(pair.run_state(&leaf), JobRunState::Cancelled);
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 1);
    assert_eq!(pair.owner_status(&task), "in-progress");

    *pair.wire.unreachable.lock().unwrap() = false;
    let sweep = run_sweep_at_with_providers(
        &pair.follower.global_root(),
        SweepOptions::default(),
        RoutineMachineIdentity {
            machine_id: FOLLOWER.into(),
            machine_name: "follower".into(),
        },
        &ReplicaOnly(pair.follower.clone()),
    )
    .expect("sweep");
    assert!(!sweep.lock_busy);
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 0);
    let owner = pair.owner_task(&task);
    assert_eq!(owner["status"], "backlog", "{owner:#}");
    assert!(
        comments_of(&owner).contains("host maintenance"),
        "{owner:#}"
    );
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert!(
        settles
            .iter()
            .all(|settle| settle["settlement"].get("Release").is_some()),
        "{settles:?}"
    );
    assert_eq!(
        pair.wire.calls("orbit.task.pull").len(),
        1,
        "no new request"
    );
    assert_eq!(
        pair.follower_jobs
            .list_job_runs("workspace_pull_pipeline")
            .unwrap()
            .len(),
        1,
        "no new drain"
    );
}

/// A failed detached child does not hide the parent or prevent a later child
/// from stopping. Both an unconfirmed worker and a missing run stay visible.
#[test]
fn a_forced_local_drain_cancel_reports_mixed_child_outcomes() {
    if !isolated("a_forced_local_drain_cancel_reports_mixed_child_outcomes") {
        return;
    }
    let pair = Pair::new(0);
    let jobs = &pair.follower_jobs;
    let drain = jobs
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    let failed = jobs
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    jobs.mark_job_run_running(&failed.run_id, Utc::now(), std::process::id())
        .unwrap();
    let worker = Worker::spawn();
    let stopped = jobs
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    jobs.mark_job_run_running(&stopped.run_id, Utc::now(), worker.pid)
        .unwrap();
    let terminal = jobs
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    pair.follower.cancel_job_run(&terminal.run_id).unwrap();
    let missing = "jrun-missing-child";
    let mut state = PipelineState::new(drain.run_id.clone(), drain.job_id, json!({}));
    for child in [
        failed.run_id.as_str(),
        missing,
        &stopped.run_id,
        &terminal.run_id,
    ] {
        state.record_child_dispatch(orbit_types::workflow::ChildDispatch::submitted(
            child.into(),
            "task_auto_pipeline".into(),
            "dispatch".into(),
            false,
            false,
            Utc::now(),
        ));
    }
    pair.follower
        .write_run_state(&drain.run_id, &state)
        .unwrap();
    let cancel = pair
        .follower
        .cancel_job_run_with_options(&drain.run_id, "operator", "cli", None, true)
        .unwrap();
    assert_eq!(cancel.outcome, "cancelled");
    assert_eq!(cancel.final_state, "cancelled");
    assert_eq!(cancel.forced_runs, vec![stopped.run_id.clone()]);
    assert!(worker.stopped());
    assert_eq!(pair.run_state(&drain.run_id), JobRunState::Cancelled);
    assert_eq!(pair.run_state(&stopped.run_id), JobRunState::Cancelled);
    assert_eq!(pair.run_state(&failed.run_id), JobRunState::Running);
    assert_eq!(cancel.unstopped_children.len(), 2, "{cancel:?}");
    assert_eq!(cancel.unstopped_children[0].child_run_id, failed.run_id);
    assert!(
        cancel.unstopped_children[0]
            .reason
            .contains("could not confirm"),
        "{cancel:?}"
    );
    assert_eq!(cancel.unstopped_children[1].child_run_id, missing);
    assert!(!cancel.unstopped_children[1].reason.is_empty());

    // The workspace stop control is another consumer of the cancel result:
    // it must fail with the same child identity rather than drop the field.
    let another = jobs
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    let mut state = PipelineState::new(another.run_id.clone(), another.job_id, json!({}));
    state.record_child_dispatch(orbit_types::workflow::ChildDispatch::submitted(
        failed.run_id.clone(),
        "task_auto_pipeline".into(),
        "dispatch".into(),
        false,
        false,
        Utc::now(),
    ));
    pair.follower
        .write_run_state(&another.run_id, &state)
        .unwrap();
    let error = pair.follower.run_tool_with_context_and_role(
        "orbit.workflow.auto",
        json!({"workspace": pair.follower.workspace_id().unwrap(), "action": "stop", "force": true}),
        Role::Admin,
        ToolContext {
            session_context: ToolSessionContext {
                transport: Some(McpTransport::Local),
                effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                ..ToolSessionContext::default()
            },
            ..ToolContext::default()
        },
    ).expect_err("an unconfirmed detached child makes the forced stop fail");
    assert!(matches!(error, OrbitError::Execution(_)), "{error}");
    assert!(error.to_string().contains(&failed.run_id), "{error}");
    assert!(error.to_string().contains("could not confirm"), "{error}");
    assert_eq!(pair.run_state(&another.run_id), JobRunState::Cancelled);
    assert_eq!(pair.run_state(&failed.run_id), JobRunState::Running);
}
