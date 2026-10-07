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
use orbit_common::security::release::sha256_hex;
use orbit_core::OrbitRuntime;
use orbit_core::application::routines::{
    DiscoveredWorkspaces, RoutineMachineIdentity, RoutineWorkspaceProvider, SweepOptions,
    run_sweep_at_with_providers,
};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::{
    ClaimEvidence, ClaimInvocation, ClaimMutation, ClaimRun, HandoffObservation, JobRunStepParams,
    JobRunStoreBackend, LocalPullAdmission, LocalPullMutation, LocalPullPhase, SettlementRefusal,
};
use orbit_tools::{DrainOwnerTransport, OwnerCoordinator, ToolContext};
use orbit_types::policy::Role;
use orbit_types::task::TaskArtifact;
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::handoff::{
    HandoffArtifactRef, HandoffCandidate, HandoffDelivery, HandoffReview, HandoffReviewDisposition,
    HandoffValidationLog, TaskHandoff,
};
use orbit_types::workflow::{
    ExecutorDef, ExecutorType, FinalRecoveryCheckpoint, FinalRecoveryDecision, FinalRecoveryKey,
    JobRunState, JobTargetType, PipelineState, ReviewTiming,
};
use orbit_types::workspace::{Workspace, WorkspaceStatus};
use serde_json::{Value, json};
use tempfile::TempDir;

mod admission;
mod allow_crew;
mod before_pr;
mod cancel;
mod candidate_carry;
mod claimed_owner;
mod claimed_review;
mod desktop_completion;
mod failure_class;
mod landing_attribution;
mod landing_repair;
mod no_diff;
mod pilot;
mod recovery;
mod settlement;
mod single_pass;
mod worktree_gc;

const OWNER: &str = "hm_owner";
const FOLLOWER: &str = "hm_follower";
const LEAF_JOB: &str = "task_claimed_pr_pipeline";
const LOCAL_LEAF_JOB: &str = "task_claimed_local_pipeline";

/// How long one isolated test may run before it is killed and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(180);

/// Run `test` (declared in `module_path`, the caller's `module_path!()`)
/// alone in a child of this binary with inherited Orbit authority
/// cleared and a disposable `HOME`; `true` inside that child. `HOME/bin`
/// leads the child's `PATH`, so a test can stand in for a provider CLI. The
/// parent waits in-process up to [`CHILD_DEADLINE`] and reaps the child on
/// any exit.
fn isolated(module_path: &str, test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_DISTRIBUTED_DRAIN_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let home = TempDir::new().unwrap();
    let stdout_path = home.path().join("stdout.log");
    let stderr_path = home.path().join("stderr.log");
    // libtest names a test by its module path below the crate root; the
    // caller's `module_path!()` carries the concern module the test lives in.
    let qualified = format!(
        "{}::{test}",
        module_path.split_once("::").expect("test module").1
    );
    let path = std::env::join_paths(std::iter::once(home.path().join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", &qualified, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("PATH", path)
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
    /// The machine whose trusted SSH session every call arrives under.
    caller: String,
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
    fingerprint: Mutex<Option<Value>>,
    /// When set, the owner refuses every settlement with this policy denial
    /// while it keeps the claim, as an owner whose configuration cannot
    /// accept a handoff does.
    refuse_settle: Mutex<Option<String>>,
    /// When set, the owner accepts a handoff into its claim journal, with
    /// the candidate the handoff names standing in for its provider read.
    accept_handoffs: Mutex<bool>,
    /// When set, every call arrives as the owner's own local session, as an
    /// owner-local drain's does, and a settlement reaches the owner's real
    /// observation of its own checkout.
    local: Mutex<bool>,
}

impl Wire {
    /// The owner's acceptance of a follower's handoff, as its settle tool
    /// records it once the provider confirmed the named candidate. The
    /// owner captures its own required commands; when it has any, the leaf
    /// first published a passing log of each for its exact candidate.
    fn accept(&self, mut handoff: TaskHandoff) -> Result<(), OrbitError> {
        let required_commands = self.owner.workflow_required_validation_commands().to_vec();
        let context = ClaimInvocation::trusted_worker(
            handoff.task_id.clone(),
            handoff.claim_id.clone(),
            handoff.machine_id.clone(),
            Some(ClaimRun {
                machine_id: handoff.machine_id.clone(),
                run_id: handoff.run_id.clone(),
            }),
        );
        if !required_commands.is_empty() {
            let artifacts: Vec<TaskArtifact> = required_commands
                .iter()
                .enumerate()
                .map(|(index, command)| {
                    let log = HandoffValidationLog {
                        schema_version: 1,
                        workspace_id: handoff.workspace_id.clone(),
                        task_id: handoff.task_id.clone(),
                        claim_id: handoff.claim_id.clone(),
                        machine_id: handoff.machine_id.clone(),
                        run_id: handoff.run_id.clone(),
                        candidate: handoff.candidate.clone(),
                        tested_head: handoff.candidate.candidate.commit.clone(),
                        command: command.clone(),
                        exit_code: 0,
                        output: format!("{command} passed"),
                    };
                    TaskArtifact {
                        path: format!("validation/{}/{index}.json", handoff.claim_id),
                        content: serde_json::to_vec(&log).unwrap(),
                        media_type: "application/json".into(),
                        created_by: None,
                    }
                })
                .collect();
            handoff.validation = artifacts
                .iter()
                .map(|artifact| HandoffArtifactRef {
                    path: artifact.path.clone(),
                    sha256: sha256_hex(&artifact.content),
                })
                .collect();
            self.owner.mutate_execution_claim(
                Some(&context),
                "validation-logs",
                &ClaimMutation::Evidence(ClaimEvidence {
                    artifacts,
                    ..Default::default()
                }),
            )?;
        }
        let observation = HandoffObservation {
            footprint_widening: vec![],
            candidate: handoff.candidate.clone(),
            required_commands,
            owner_completion_authority: None,
            review: None,
        };
        let request = format!("handoff:{}", handoff.claim_id);
        self.owner
            .accept_task_handoff(&context, &request, handoff, observation)?;
        Ok(())
    }

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
        if name == "orbit.drain.claim.settle"
            && let Some(message) = self.refuse_settle.lock().unwrap().clone()
        {
            return Err(OrbitError::RemoteTool {
                code: "policy_denied".into(),
                message: message.clone(),
                payload: json!({"code": "policy_denied", "message": message}),
            });
        }
        // The owner verifies a handoff against its published pull request,
        // which no test here has; the wire answers as an owner that did, and
        // records the acceptance when the test asks for it.
        let local = *self.local.lock().unwrap();
        if name == "orbit.drain.claim.settle"
            && !local
            && let Some(handoff) = input["settlement"].get("AcceptHandoff")
            && handoff["candidate"]["delivery"]["kind"] != "no_diff"
        {
            if *self.accept_handoffs.lock().unwrap() {
                self.accept(serde_json::from_value(handoff.clone()).unwrap())?;
            }
            return Ok(json!({"phase": "handed_off"}));
        }
        let session = ToolSessionContext {
            caller_machine_id: Some(self.caller.clone()),
            process_machine_id: Some(OWNER.to_string()),
            transport: Some(if local {
                McpTransport::Local
            } else {
                McpTransport::SshMcp
            }),
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
        if name == "orbit.drain.probe"
            && let Some(fingerprint) = self.fingerprint.lock().unwrap().clone()
        {
            answer["protocol_fingerprint"] = fingerprint;
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
    _root: Arc<TempDir>,
    wire: Arc<Wire>,
    owner_repo: PathBuf,
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
    std::fs::create_dir_all(&repo).unwrap();
    let git_boundary = std::process::Command::new("git")
        .args(["init", "--bare", "--initial-branch=main", "-q"])
        .arg(repo.join(".git"))
        .output()
        .unwrap();
    assert!(
        git_boundary.status.success(),
        "initialize fixture Git discovery boundary: {}",
        String::from_utf8_lossy(&git_boundary.stderr)
    );
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
    owner
        .run_tool(
            "orbit.task.update",
            json!({"id": id, "plan": "1. Change it.", "model": "codex"}),
        )
        .expect("plan");
    owner
        .update_task_as_human(
            &id,
            orbit_core::application::task::TaskUpdateParams {
                status: Some(orbit_types::task::TaskStatus::Backlog),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .expect("human approval");
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
        Self::with_owner_config("", crews)
    }

    /// [`Self::with_crews`] with the owner opened over `config` as its
    /// workspace `config.toml`.
    fn with_owner_config(config: &str, crews: &[Option<&str>]) -> Self {
        Self::with_configs(config, "", crews)
    }

    /// [`Self::with_owner_config`] with `follower_config` on the follower.
    /// An empty follower config leaves the built-in crews. The follower's
    /// window groups providers from its own registry, so an alias such as
    /// `anthropic` has to be configured there to be excluded with `claude`.
    fn with_configs(owner_config: &str, follower_config: &str, crews: &[Option<&str>]) -> Self {
        let root = TempDir::new().unwrap();
        let orbit = root.path().join(OWNER).join("repo/.orbit");
        std::fs::create_dir_all(&orbit).unwrap();
        std::fs::write(orbit.join("config.toml"), owner_config).unwrap();
        if !follower_config.is_empty() {
            let follower_orbit = root.path().join(FOLLOWER).join("repo/.orbit");
            std::fs::create_dir_all(&follower_orbit).unwrap();
            std::fs::write(follower_orbit.join("config.toml"), follower_config).unwrap();
        }
        let (owner, owner_repo) = open_runtime(root.path(), OWNER);
        let tasks = crews
            .iter()
            .enumerate()
            .map(|(n, crew)| backlog_task(&owner, &owner_repo, &format!("src/f{n}.rs"), *crew))
            .collect();
        Self::follower_of(Arc::new(root), owner, owner_repo, tasks, FOLLOWER)
    }

    /// A second follower host of this pair's owner: its own runtime,
    /// repository and object store, reaching the owner as `machine`
    /// [ORB-14338].
    fn another_host(&self, machine: &str) -> Self {
        Self::follower_of(
            self._root.clone(),
            self.wire.owner.clone(),
            self.owner_repo.clone(),
            self.tasks.clone(),
            machine,
        )
    }

    /// A replica follower `machine` routed to `owner`.
    fn follower_of(
        root: Arc<TempDir>,
        owner: OrbitRuntime,
        owner_repo: PathBuf,
        tasks: Vec<String>,
        machine: &str,
    ) -> Self {
        let workspace_id = owner.workspace_id().unwrap();
        let wire = Arc::new(Wire {
            owner,
            caller: machine.to_string(),
            calls: Mutex::default(),
            lose: Mutex::default(),
            task_reads: Mutex::default(),
            task_reads_fail: Mutex::default(),
            task_reads_remote_error: Mutex::default(),
            unreachable: Mutex::default(),
            protocol: Mutex::default(),
            fingerprint: Mutex::default(),
            refuse_settle: Mutex::default(),
            accept_handoffs: Mutex::default(),
            local: Mutex::default(),
        });
        let (follower, follower_repo) = open_runtime(root.path(), machine);
        let follower = follower
            .with_coordination_write_owner(Some(OWNER.into()))
            .with_drain_owner_transport(wire.clone());
        let follower_jobs = orbit_store::compose::workspace_job_run_store(
            follower.sqlite_store().unwrap(),
            follower.workspace_id().unwrap(),
        );
        launchable_providers(&follower);
        Self {
            _root: root,
            wire,
            owner_repo,
            follower,
            follower_repo,
            follower_jobs,
            destination: json!({
                "owner_machine_id": OWNER,
                "owner_workspace_id": workspace_id,
                "selector": format!("{OWNER}/{workspace_id}"),
                "execution_machine_id": machine,
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
    /// reports transient errors; typed protocol skew fails the drain.
    fn pass(&self, drain: &str) -> Value {
        self.pass_with(drain, 1)
    }

    /// A pass with `slots` leaf slots.
    fn pass_with(&self, drain: &str, slots: u64) -> Value {
        self.pass_over(drain, json!({"max_active_leaf_runs": slots}))
    }

    /// A pass whose input is the open-window, one-slot default with
    /// `overrides` (say `for_seconds` and `window_expired`, as the job
    /// forwards them) laid over it.
    fn pass_over(&self, drain: &str, overrides: Value) -> Value {
        let mut input = json!({
            "run_id": drain,
            "destination": self.destination,
            "window_expired": false,
            "max_active_leaf_runs": 1,
        });
        for (key, value) in overrides.as_object().expect("override object") {
            input[key] = value.clone();
        }
        self.follower
            .run_deterministic("pull_refill", &json!({}), &input, ToolContext::default())
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

    /// A running drain submitted with `input` as its run input, the way
    /// `orbit run auto --pull` persists the operator's options.
    fn run_drain_with_input(&self, input: Value) -> String {
        let run = self
            .follower_jobs
            .insert_job_run("workspace_pull_pipeline", 1, Utc::now(), Some(input), None)
            .expect("drain run");
        self.follower
            .write_run_state(
                &run.run_id,
                &PipelineState::new(run.run_id.clone(), run.job_id, json!({})),
            )
            .expect("drain state");
        self.follower_jobs
            .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
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

    /// Every claimed leaf run, a follower's PR leaves and an owner-local
    /// drain's local ones alike.
    fn leaf_runs(&self) -> Vec<String> {
        [LEAF_JOB, LOCAL_LEAF_JOB]
            .into_iter()
            .flat_map(|job| self.follower_jobs.list_job_runs(job).expect("leaf runs"))
            .map(|run| run.run_id)
            .collect()
    }
}

/// Register `provider`'s executor on `runtime` as launching `command`.
/// Make every provider `runtime` configures launchable, so its window
/// preflight runs every crew whether or not the host has the provider CLIs.
fn launchable_providers(runtime: &OrbitRuntime) {
    let providers = runtime
        .configured_crew_registry_projection()
        .crews
        .into_iter()
        .map(|crew| crew.provider)
        .collect::<BTreeSet<_>>();
    for provider in providers {
        follower_cli(runtime, &provider, "sh");
    }
}

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

/// A stand-in worker, reaped the moment it exits so a stop can see it gone.
/// On Unix it leads its own group unless a test supplies a shared group.
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
        Self::spawn_command(command)
    }

    #[cfg(unix)]
    fn spawn_in_group(pgid: libc::pid_t) -> Self {
        use std::os::unix::process::CommandExt;

        let mut command = std::process::Command::new("sleep");
        command.arg("600").process_group(pgid);
        Self::spawn_command(command)
    }

    fn spawn_command(mut command: std::process::Command) -> Self {
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
    let ship = &record.request.ship;
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
            base_branch: ship.base_branch.clone(),
            landing_branch: ship.landing_branch.clone(),
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
        footprint_widening: vec![],
    }
}

fn error_of(pass: &Value) -> &str {
    pass["error"].as_str().unwrap_or_default()
}

/// Whether a pass ended at a refused leaf launch rather than at the owner.
fn launch_refused(pass: &Value) -> bool {
    error_of(pass).contains("re-exec")
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

/// Publish the current branch to a bare repo that stands in for `origin`.
///
/// The origin URL stays the configured GitHub address so delivery identity
/// does not change. `url.<bare>.insteadOf` sends fetch and push to the bare
/// repo. Observation then has to fetch; a commit that exists only in this
/// worktree is invisible.
fn publish_origin(repo: &Path) {
    let url = git(repo, &["config", "--get", "remote.origin.url"]);
    let url = url.trim();
    assert!(!url.is_empty(), "origin url");
    let bare = repo.with_file_name("origin.git");
    if !bare.join("HEAD").exists() {
        git(
            bare.parent().unwrap(),
            &["init", "--bare", "-q", bare.to_str().unwrap()],
        );
    }
    let branch = git(repo, &["branch", "--show-current"]);
    let branch = branch.trim();
    git(
        &bare,
        &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
    );
    let key = format!("url.{}.insteadOf", bare.display());
    git(repo, &["config", &key, url]);
    git(
        repo,
        &["push", "-q", "origin", &format!("HEAD:refs/heads/{branch}")],
    );
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

/// The owner's comments on `task`, as one searchable text.
fn comments_of(task: &Value) -> String {
    task["comments"].to_string()
}
