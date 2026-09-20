//! The executable owner-local claimed leaf [ORB-12616].
//!
//! This is the scenario the pull foundation could not reach: a real admission
//! on a real owner, a real claim binding, a real worktree, real required
//! validation captured on the exact candidate, and a typed handoff the owner
//! accepts from its own observation — on a checkout with no remote at all, so
//! nothing here can quietly depend on PR credentials.
//!
//! Only two things are replaced, and both are named where they are used: the
//! implementation activity (a provider is not available to a unit test) and
//! `fork`/`exec` of the leaf worker (the `orbit` binary is not either). The
//! launcher's real binding derivation is still what produces the runtime the
//! pipeline runs under.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_engine::RuntimeHost;
use orbit_store::contracts::*;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::PipelineState;
use orbit_types::workflow::handoff::{HandoffDelivery, HandoffValidationLog};
use serde_json::json;

use super::review_gate::git_stdout;
use super::{git_in, resolved_job, seed_default_catalogs, try_execute_job};
use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::pull::{PullDrain, PullLauncher, PullPeer};
use crate::adapter::engine_host::v2_host::pull_adapters::{LeafPullLauncher, OwnerPullPeer};
use crate::application::distributed::owner_binary_version;
use crate::application::task::TaskAddParams;

const BASE_BRANCH: &str = "agent-main";
const MACHINE: &str = "owner-machine";
const VALIDATION: &str = "echo claimed-candidate-validated";
/// The number the fixture `gh` reports for every pull request it creates.
const FIXTURE_PR_NUMBER: u64 = 4242;

fn workspace_config() -> String {
    format!(
        "[workflow]\nbase_branch = \"{BASE_BRANCH}\"\nrequired_validation_commands = \
         [\"{VALIDATION}\", \"git rev-parse HEAD\"]\n"
    )
}

/// A checkout shaped like an owner-local workspace: a real repository on the
/// delivery base branch, with nothing to publish to.
fn init_remoteless_repo(repo_root: &Path) {
    git_in(repo_root, &["init"]);
    git_in(repo_root, &["config", "user.name", "Orbit Test"]);
    git_in(
        repo_root,
        &["config", "user.email", "orbit-test@example.invalid"],
    );
    std::fs::create_dir_all(repo_root.join("src")).expect("src");
    std::fs::write(repo_root.join("src/lib.rs"), "// base\n").expect("seed source");
    git_in(repo_root, &["add", "."]);
    git_in(repo_root, &["commit", "-m", "initial"]);
    git_in(repo_root, &["checkout", "-b", BASE_BRANCH]);
    let remotes = std::process::Command::new("git")
        .current_dir(repo_root)
        .arg("remote")
        .output()
        .expect("git remote");
    assert!(
        String::from_utf8_lossy(&remotes.stdout).trim().is_empty(),
        "the fixture checkout must have no push remote"
    );
}

/// A checkout shaped like a follower's: a real repository with a real
/// `origin` it can publish to. The remote is a local bare repository, so the
/// push half of PR delivery is genuine Git rather than a stub.
fn init_published_repo(repo_root: &Path, origin: &Path) {
    std::fs::create_dir_all(origin).expect("origin dir");
    git_in(origin, &["init", "--bare", "--initial-branch", BASE_BRANCH]);
    git_in(repo_root, &["init"]);
    git_in(repo_root, &["config", "user.name", "Orbit Test"]);
    git_in(
        repo_root,
        &["config", "user.email", "orbit-test@example.invalid"],
    );
    git_in(
        repo_root,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    std::fs::create_dir_all(repo_root.join("src")).expect("src");
    std::fs::write(repo_root.join("src/lib.rs"), "// base\n").expect("seed source");
    git_in(repo_root, &["add", "."]);
    git_in(repo_root, &["commit", "-m", "initial"]);
    git_in(repo_root, &["checkout", "-b", BASE_BRANCH]);
    git_in(repo_root, &["push", "-u", "origin", BASE_BRANCH]);
}

/// Push a new commit to `origin/<BASE_BRANCH>` from a throwaway clone, leaving
/// the executor checkout's local base ref where it was. That is the ordinary
/// remote-sync state: local `agent-main` lags `origin/agent-main`.
fn advance_origin_base(origin: &Path) -> String {
    let temp = tempfile::tempdir().expect("origin seed");
    let seed = temp.path().join("seed");
    git_in(
        temp.path(),
        &["clone", &origin.to_string_lossy(), &seed.to_string_lossy()],
    );
    git_in(&seed, &["config", "user.name", "Orbit Test"]);
    git_in(
        &seed,
        &["config", "user.email", "orbit-test@example.invalid"],
    );
    std::fs::write(seed.join("src/advance.txt"), "// origin moved\n").expect("advance file");
    git_in(&seed, &["add", "."]);
    git_in(&seed, &["commit", "-m", "advance origin base"]);
    git_in(&seed, &["push", "origin", BASE_BRANCH]);
    git_stdout(&seed, &["rev-parse", "HEAD"])
}

/// Replace the seeded agent loop with a deterministic local command, so the
/// fixture exercises the whole delivery tail on the real claimed-leaf host
/// rather than a scripted stand-in. Only the implementation step is replaced:
/// a unit test has no provider to launch.
fn stub_agent_implement(global_root: &Path) {
    std::fs::write(
        global_root.join("resources/activities/agent_implement.yaml"),
        r#"schemaVersion: 2
kind: Activity
metadata:
  name: agent_implement
spec:
  type: deterministic
  description: Test stub standing in for the claimed implementation step.
  input_schema_json:
    type: object
    properties:
      workspace_path:
        type: string
  output_schema_json:
    type: object
  action: local_shell
  config:
    executor: local-shell
    command: /bin/sh
    args: ["-c", "printf '// claimed implementation\n' >> src/lib.rs"]
    cwd: "."
    timeout_ms: 60000
"#,
    )
    .expect("stub agent implementation activity");
}

fn seed_local_shell_executor(runtime: &OrbitRuntime) {
    let now = chrono::Utc::now();
    runtime
        .upsert_executor_def(&orbit_types::workflow::ExecutorDef {
            name: "local-shell".to_string(),
            executor_type: orbit_types::workflow::ExecutorType::LocalShell,
            command: None,
            args: Vec::new(),
            stdout_format: None,
            model_pair_override: None,
            model_flag: None,
            timeout_seconds: Some(120),
            env: std::collections::HashMap::new(),
            sandbox: None,
            allow_fallback: false,
            created_at: Some(now),
            updated_at: Some(now),
        })
        .expect("seed local shell executor");
}

/// Runs the claimed leaf in this process instead of spawning it.
///
/// The binding comes from the real [`LeafPullLauncher`]; only the fork/exec of
/// the worker is replaced, because a unit test has no `orbit` binary to exec.
struct InProcessLauncher<'a> {
    real: LeafPullLauncher<'a>,
    runtime: &'a OrbitRuntime,
    repo_root: PathBuf,
    /// The leaf definition the admission selected. Asserted against the run's
    /// own `job_id` so the fixture cannot execute a definition the claim did
    /// not choose.
    job_name: &'static str,
    launched: RefCell<Vec<String>>,
    outcome: RefCell<Option<Result<bool, String>>>,
}

impl PullLauncher for InProcessLauncher<'_> {
    fn launch(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        let bound = self.real.bound_runtime(admission)?;
        let binding = bound.worker_invocation().expect("bound").clone();
        self.launched
            .borrow_mut()
            .push(binding.bound_run_id.clone());
        let run = self
            .runtime
            .stores()
            .jobs()
            .get_job_run(&binding.bound_run_id)
            .expect("leaf run")
            .expect("leaf run row");
        assert_eq!(run.job_id, self.job_name);
        let job = resolved_job(self.runtime, self.job_name);
        let input = run.input.expect("leaf run input");
        let outcome = try_execute_job(
            self.runtime,
            &self.repo_root,
            &bound,
            job,
            input,
            &binding.bound_run_id,
        );
        *self.outcome.borrow_mut() = Some(match outcome {
            Ok(outcome) => Ok(outcome.success),
            Err(error) => Err(error.to_string()),
        });
        Ok(())
    }
}

fn drain_run(runtime: &OrbitRuntime) -> String {
    let jobs = runtime.stores().jobs();
    let parent = jobs
        .insert_job_run("workspace_auto_pipeline", 1, chrono::Utc::now(), None, None)
        .expect("drain run");
    jobs.write_run_state(
        &parent.run_id,
        &PipelineState::new(parent.run_id.clone(), parent.job_id, json!({})),
    )
    .expect("drain state");
    parent.run_id
}

fn admission_request(runtime: &OrbitRuntime, drain: &str, mode: &str) -> AdmissionRequest {
    AdmissionRequest {
        request_id: "template".into(),
        caller_version: owner_binary_version().into(),
        caller_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
        caller_review_policy: "none".into(),
        run_context: AdmissionRunContext {
            run_id: drain.to_string(),
            job_name: "workspace_auto_pipeline".into(),
            host_id: None,
        },
        ship: AdmissionShipContract {
            mode: mode.into(),
            base_branch: runtime.workflow_base_branch().to_string(),
            landing_branch: runtime.workflow_base_branch().to_string(),
            review_policy: "none".into(),
            completion: "review".into(),
            authorization_reference: None,
        },
    }
}

fn destination(runtime: &OrbitRuntime, execution_machine: &str) -> PullDestination {
    PullDestination {
        owner_machine_id: MACHINE.into(),
        owner_workspace_id: runtime.workspace_id().expect("workspace"),
        selector: format!("{MACHINE}/{}", runtime.workspace_id().expect("workspace")),
        execution_machine_id: execution_machine.into(),
    }
}

fn owner_runtime() -> (tempfile::TempDir, OrbitRuntime, PathBuf, PathBuf) {
    let (root, runtime, repo_root, global_root) =
        super::test_runtime_with_workspace_config(&workspace_config());
    seed_default_catalogs(&global_root);
    stub_agent_implement(&global_root);
    init_remoteless_repo(&repo_root);
    seed_local_shell_executor(&runtime);
    let runtime = runtime.with_automation_machine_identity(Some(MACHINE.to_string()));
    (root, runtime, repo_root, global_root)
}

/// The same owner, on a checkout that has somewhere to publish.
fn publishing_runtime() -> (tempfile::TempDir, OrbitRuntime, PathBuf, PathBuf) {
    let (root, runtime, repo_root, global_root) =
        super::test_runtime_with_workspace_config(&workspace_config());
    seed_default_catalogs(&global_root);
    stub_agent_implement(&global_root);
    init_published_repo(&repo_root, &root.path().join("origin.git"));
    seed_local_shell_executor(&runtime);
    let runtime = runtime.with_automation_machine_identity(Some(MACHINE.to_string()));
    (root, runtime, repo_root, global_root)
}

fn seed_claimable_task(runtime: &OrbitRuntime) -> String {
    runtime
        .add_task(TaskAddParams {
            title: "Claimed owner-local delivery".to_string(),
            description: "Fixture task admitted through the pull admission transaction."
                .to_string(),
            acceptance_criteria: vec!["The candidate is delivered and handed off.".to_string()],
            plan: "Implement, validate, hand off.".to_string(),
            context_files: vec!["src/lib.rs".to_string()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed claimable task")
        .id
}

#[test]
fn owner_local_claim_executes_validates_and_hands_off_without_merging() {
    if isolated_claimed_test(
        "application::job::tests::exec::claimed_leaf::owner_local_claim_executes_validates_and_hands_off_without_merging",
    ) {
        return;
    }
    let (_root, runtime, repo_root, _global) = owner_runtime();
    let task_id = seed_claimable_task(&runtime);
    let drain = drain_run(&runtime);
    let destination = destination(&runtime, MACHINE);
    let template = admission_request(&runtime, &drain, "local");

    let peer = OwnerPullPeer { runtime: &runtime };
    let launcher = InProcessLauncher {
        real: LeafPullLauncher { runtime: &runtime },
        runtime: &runtime,
        repo_root: repo_root.clone(),
        job_name: "task_claimed_local_pipeline",
        launched: RefCell::new(Vec::new()),
        outcome: RefCell::new(None),
    };
    let jobs = runtime.stores().jobs();
    let admitted = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
    }
    .refill(&destination, &template, 1)
    .expect("one owner-local admission executes end to end");
    assert_eq!(admitted, 1);
    assert_eq!(
        launcher.outcome.borrow().clone(),
        Some(Ok(true)),
        "the claimed leaf pipeline must succeed"
    );
    let leaf = launcher.launched.borrow()[0].clone();

    // The leaf definition a claim selects is the handoff-only one.
    assert_eq!(
        jobs.get_job_run(&leaf).expect("leaf").expect("row").job_id,
        "task_claimed_local_pipeline"
    );

    // The admission settled through the owner, and the task is in review
    // awaiting completion authority — not done, and nothing was merged.
    let record = jobs.local_pull_admissions().expect("records").remove(0);
    assert_eq!(record.phase, LocalPullPhase::Settled);
    assert!(matches!(
        record.settlement,
        Some(ClaimMutation::AcceptHandoff(_))
    ));
    let task = runtime.get_task(&task_id).expect("task");
    assert_eq!(task.status, TaskStatus::Review);
    let claim_id = record
        .receipt
        .as_ref()
        .and_then(|receipt| receipt.claim.as_ref())
        .expect("claim")
        .claim_id
        .clone();
    let accepted = runtime
        .accepted_task_handoff(&claim_id)
        .expect("owner accepted the typed handoff");
    assert_eq!(
        accepted.handoff.candidate.delivery,
        HandoffDelivery::LocalCandidate,
        "an owner-local claim delivers a local candidate, not a pull request"
    );
    assert_eq!(
        accepted.required_commands,
        vec![VALIDATION, "git rev-parse HEAD"]
    );
    assert!(
        runtime.landing_start_requests().expect("outbox").is_empty(),
        "a review-only handoff records no landing authority"
    );

    // Validation is independently captured on the exact candidate and base,
    // and the base is the branch tip the candidate descends from.
    assert_eq!(accepted.handoff.validation.len(), 2);
    let artifacts = runtime.get_task_artifacts(&task_id).expect("artifacts");
    for reference in &accepted.handoff.validation {
        let artifact = artifacts
            .iter()
            .find(|artifact| artifact.path == reference.path)
            .unwrap_or_else(|| panic!("owner holds {}", reference.path));
        let log: HandoffValidationLog =
            serde_json::from_slice(&artifact.content).expect("validation log");
        assert_eq!(log.exit_code, 0);
        assert_eq!(log.tested_head, accepted.handoff.candidate.candidate.commit);
        assert_eq!(log.candidate, accepted.handoff.candidate);
        assert!(!log.output.trim().is_empty(), "captured output is evidence");
    }
    let base_tip = String::from_utf8(
        std::process::Command::new("git")
            .current_dir(&repo_root)
            .args(["rev-parse", BASE_BRANCH])
            .output()
            .expect("base tip")
            .stdout,
    )
    .expect("utf8");
    assert_eq!(accepted.handoff.candidate.base.commit, base_tip.trim());
    assert_ne!(
        accepted.handoff.candidate.candidate.commit,
        base_tip.trim(),
        "the candidate is real work, not the base itself"
    );

    // Nothing landed: the base branch still points where it did, and the
    // candidate lives only on its own branch.
    assert_eq!(
        String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&repo_root)
                .args(["rev-parse", BASE_BRANCH])
                .output()
                .expect("base after")
                .stdout
        )
        .expect("utf8")
        .trim(),
        base_tip.trim()
    );

    // Settlement is idempotent against the real owner: a disconnected drain
    // that retries the same persisted settlement replays the recorded
    // acceptance instead of creating a second handoff or a second transition.
    peer.settle(&record).expect("replayed settlement");
    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Review
    );
    assert_eq!(
        runtime
            .accepted_task_handoff(&claim_id)
            .expect("still one accepted handoff")
            .handoff_id,
        accepted.handoff_id
    );
}

/// [ORB-12616] The owner observes a published pull request before it accepts
/// one, and this slice does not implement that observation. The boundary is
/// pinned here so the gap is an explicit refusal — leaving the settlement
/// durable and retryable — rather than an acceptance on the worker's word.
#[test]
fn owner_side_acceptance_of_a_published_pull_request_is_refused_for_now() {
    if isolated_claimed_test(
        "application::job::tests::exec::claimed_leaf::owner_side_acceptance_of_a_published_pull_request_is_refused_for_now",
    ) {
        return;
    }
    let (_root, runtime, _repo_root, _global) = owner_runtime();
    let task_id = seed_claimable_task(&runtime);
    let drain = drain_run(&runtime);
    let destination = destination(&runtime, MACHINE);
    let template = admission_request(&runtime, &drain, "local");
    let peer = OwnerPullPeer { runtime: &runtime };
    let jobs = runtime.stores().jobs();
    let launcher = RefusingLauncher;
    let _ = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
    }
    .refill(&destination, &template, 1);
    let mut record = jobs.local_pull_admissions().expect("records").remove(0);
    let claim = record
        .receipt
        .as_ref()
        .and_then(|receipt| receipt.claim.clone())
        .expect("claim");
    record.settlement = Some(ClaimMutation::AcceptHandoff(
        orbit_types::workflow::handoff::TaskHandoff {
            schema_version: 1,
            workspace_id: destination.owner_workspace_id.clone(),
            task_id: task_id.clone(),
            claim_id: claim.claim_id.clone(),
            machine_id: MACHINE.into(),
            run_id: record.leaf_run_id.clone().expect("leaf"),
            candidate: orbit_types::workflow::handoff::HandoffCandidate {
                repository: "owner/repo".into(),
                source_branch: "attempt".into(),
                base_branch: BASE_BRANCH.into(),
                landing_branch: BASE_BRANCH.into(),
                candidate: orbit_types::workflow::automation::SourceRevision {
                    commit: "a".repeat(40),
                    tree: "b".repeat(40),
                },
                base: orbit_types::workflow::automation::SourceRevision {
                    commit: "c".repeat(40),
                    tree: "d".repeat(40),
                },
                delivery: HandoffDelivery::PullRequest { number: 7 },
            },
            review: orbit_types::workflow::handoff::HandoffReview {
                policy: orbit_types::workflow::ReviewTiming::None,
                disposition: orbit_types::workflow::handoff::HandoffReviewDisposition::NotRequired,
            },
            execution_summary: "fixture".into(),
            validation: Vec::new(),
        },
    ));
    let error = peer
        .settle(&record)
        .expect_err("a pull-request delivery is not observable by this owner yet");
    assert!(
        error.to_string().contains("not part of this slice"),
        "{error}"
    );
    assert_ne!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Review,
        "a refused observation promotes nothing"
    );
}

#[test]
fn a_follower_cannot_admit_an_owner_local_claim() {
    if isolated_claimed_test(
        "application::job::tests::exec::claimed_leaf::a_follower_cannot_admit_an_owner_local_claim",
    ) {
        return;
    }
    let (_root, runtime, _repo_root, _global) = owner_runtime();
    seed_claimable_task(&runtime);
    let drain = drain_run(&runtime);
    // A follower executes on its own machine; the owner is elsewhere.
    let destination = destination(&runtime, "follower-machine");
    let template = admission_request(&runtime, &drain, "local");
    let error = runtime
        .stores()
        .jobs()
        .allocate_pull_request(&destination, &template, 1)
        .expect_err("a follower may not run owner-local mode");
    assert!(
        error
            .to_string()
            .contains("followers cannot execute owner-local leaves"),
        "{error}"
    );
}

#[test]
fn a_claimed_leaf_refuses_generic_execution_without_its_binding() {
    if isolated_claimed_test(
        "application::job::tests::exec::claimed_leaf::a_claimed_leaf_refuses_generic_execution_without_its_binding",
    ) {
        return;
    }
    let (_root, runtime, _repo_root, _global) = owner_runtime();
    seed_claimable_task(&runtime);
    let drain = drain_run(&runtime);
    let destination = destination(&runtime, MACHINE);
    let template = admission_request(&runtime, &drain, "local");
    let peer = OwnerPullPeer { runtime: &runtime };
    let launcher = RefusingLauncher;
    let jobs = runtime.stores().jobs();
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
    };
    assert!(drain.refill(&destination, &template, 1).is_err());
    let leaf = jobs.local_pull_admissions().expect("records")[0]
        .leaf_run_id
        .clone()
        .expect("leaf run");

    // The unbound runtime is exactly what a generic worker would be.
    let error = runtime
        .execute_pipeline_run_worker(&leaf)
        .expect_err("a claimed leaf is not generically executable");
    assert!(
        error.to_string().contains("handoff execution adapter"),
        "{error}"
    );

    // A payload cannot supply the missing authority either: the claimed-leaf
    // activities read the process binding, never their input.
    let error = <OrbitRuntime as RuntimeHost>::claim_execution_context(&runtime)
        .expect_err("no trusted binding, no claimed execution");
    assert!(
        error.to_string().contains("trusted worker binding"),
        "{error}"
    );
}

/// [ORB-12616] A claimed definition submitted by hand has no claim to run for,
/// and is refused before it can build a worktree or spend an agent step.
#[test]
fn a_hand_submitted_claimed_definition_is_refused_before_it_does_any_work() {
    if isolated_claimed_test(
        "application::job::tests::exec::claimed_leaf::a_hand_submitted_claimed_definition_is_refused_before_it_does_any_work",
    ) {
        return;
    }
    let (_root, runtime, _repo_root, _global) = owner_runtime();
    let task_id = seed_claimable_task(&runtime);
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_claimed_local_pipeline",
            1,
            chrono::Utc::now(),
            Some(json!({ "task_ids": [task_id] })),
            None,
        )
        .expect("hand-submitted run");
    let error = runtime
        .execute_pipeline_run_worker(&run.run_id)
        .expect_err("a claimed definition is not directly submittable");
    assert!(
        error
            .to_string()
            .contains("has no claim and cannot hand off"),
        "{error}"
    );
}

/// Where a fault injection severs the pull protocol [ORB-12617].
///
/// Each cut is the *response* half of a step whose owner-side effect already
/// committed, except [`Cut::AfterCreate`], which is the executor dying with a
/// created leaf it never announced. That asymmetry is the point: the caller
/// cannot tell a lost reply from a call that never landed, so every cut must
/// be replayable without producing a second claim or a second leaf.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cut {
    /// The owner admitted and committed the claim; the receipt never arrived.
    Request,
    /// The leaf run exists locally; the process died before it called bind.
    AfterCreate,
    /// The owner recorded the binding; the acknowledgment never arrived.
    Bind,
    /// The owner is unreachable when the settlement is delivered.
    Settle,
}

/// The real [`OwnerPullPeer`], with one severable response.
struct CuttingPeer<'a> {
    owner: OwnerPullPeer<'a>,
    cut: RefCell<Option<Cut>>,
    requests: Cell<usize>,
    binds: Cell<usize>,
    settlements: Cell<usize>,
}

impl<'a> CuttingPeer<'a> {
    fn new(runtime: &'a OrbitRuntime) -> Self {
        Self {
            owner: OwnerPullPeer { runtime },
            cut: RefCell::new(None),
            requests: Cell::new(0),
            binds: Cell::new(0),
            settlements: Cell::new(0),
        }
    }

    /// Arm the next cut. Consumed the first time its step runs, so the retry
    /// after it is an ordinary call.
    fn cut(&self, cut: Cut) {
        *self.cut.borrow_mut() = Some(cut);
    }

    fn take(&self, cut: Cut) -> bool {
        let mut armed = self.cut.borrow_mut();
        if *armed == Some(cut) {
            *armed = None;
            return true;
        }
        false
    }
}

impl PullPeer for CuttingPeer<'_> {
    fn request(
        &self,
        destination: &PullDestination,
        request: &AdmissionRequest,
    ) -> Result<AdmissionReceipt, OrbitError> {
        self.requests.set(self.requests.get() + 1);
        // The owner commits first; only the reply is lost.
        let receipt = self.owner.request(destination, request)?;
        if self.take(Cut::Request) {
            return Err(OrbitError::Execution("fixture severed the receipt".into()));
        }
        Ok(receipt)
    }

    fn bind(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        if self.take(Cut::AfterCreate) {
            return Err(OrbitError::Execution(
                "fixture killed the executor before it announced its leaf".into(),
            ));
        }
        self.binds.set(self.binds.get() + 1);
        self.owner.bind(admission)?;
        if self.take(Cut::Bind) {
            return Err(OrbitError::Execution(
                "fixture severed the bind acknowledgment".into(),
            ));
        }
        Ok(())
    }

    fn settle(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        if self.take(Cut::Settle) {
            return Err(OrbitError::Execution(
                "fixture disconnected the owner".into(),
            ));
        }
        self.owner.settle(admission)?;
        self.settlements.set(self.settlements.get() + 1);
        Ok(())
    }
}

/// Derives the real binding the way [`LeafPullLauncher`] does, and stops there.
///
/// Enough to prove the launch seam was reached with a usable bound runtime,
/// without spending a whole pipeline execution on each of four cuts. The
/// end-to-end execution is
/// [`owner_local_claim_executes_validates_and_hands_off_without_merging`]'s.
struct BindingOnlyLauncher<'a> {
    real: LeafPullLauncher<'a>,
    launched: RefCell<Vec<String>>,
    fail: Cell<bool>,
}

impl PullLauncher for BindingOnlyLauncher<'_> {
    fn launch(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        let bound = self.real.bound_runtime(admission)?;
        let binding = bound.worker_invocation().expect("bound").clone();
        self.launched.borrow_mut().push(binding.bound_run_id);
        if self.fail.get() {
            return Err(OrbitError::Execution("fixture failed the launch".into()));
        }
        Ok(())
    }
}

fn claimed_runs(runtime: &OrbitRuntime) -> Vec<String> {
    runtime
        .stores()
        .jobs()
        .list_job_runs("task_claimed_local_pipeline")
        .expect("claimed runs")
        .into_iter()
        .map(|run| run.run_id)
        .collect()
}

/// [ORB-12617] Every request/create/bind/launch cut, against the real owner
/// and the real launcher binding.
///
/// The invariant under all four is the same one the protocol exists for: one
/// claim, one leaf, and no state the next pass cannot resume from. The claim
/// is admitted once no matter how many times the request is replayed, the leaf
/// is created once no matter how many times the executor restarts before
/// announcing it, and the binding is idempotent against the owner's claim
/// journal rather than against the caller's memory of it.
#[test]
fn fault_injection_at_every_cut_yields_at_most_one_leaf_per_claim() {
    if isolated_claimed_test(
        "application::job::tests::exec::claimed_leaf::fault_injection_at_every_cut_yields_at_most_one_leaf_per_claim",
    ) {
        return;
    }
    let (_root, runtime, _repo_root, _global) = owner_runtime();
    let task_id = seed_claimable_task(&runtime);
    let drain = drain_run(&runtime);
    let destination = destination(&runtime, MACHINE);
    let template = admission_request(&runtime, &drain, "local");
    let peer = CuttingPeer::new(&runtime);
    let launcher = BindingOnlyLauncher {
        real: LeafPullLauncher { runtime: &runtime },
        launched: RefCell::new(Vec::new()),
        fail: Cell::new(false),
    };
    let jobs = runtime.stores().jobs();
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
    };

    // Cut 1 — the owner admitted the claim and the receipt was lost.
    peer.cut(Cut::Request);
    assert!(drain.refill(&destination, &template, 1).is_err());
    let record = jobs.local_pull_admissions().expect("records").remove(0);
    assert_eq!(record.phase, LocalPullPhase::Requested);
    assert!(record.leaf_run_id.is_none());
    assert!(claimed_runs(&runtime).is_empty());

    // Cut 2 — the receipt is replayed onto the same request and the leaf is
    // created, then the executor dies before it announces it.
    peer.cut(Cut::AfterCreate);
    assert!(drain.refill(&destination, &template, 1).is_err());
    let record = jobs.local_pull_admissions().expect("records").remove(0);
    assert_eq!(record.phase, LocalPullPhase::Created);
    let leaf = record.leaf_run_id.clone().expect("created leaf");
    assert_eq!(claimed_runs(&runtime), vec![leaf.clone()]);
    assert_eq!(peer.binds.get(), 0);

    // Cut 3 — the owner records the binding; the acknowledgment is lost. The
    // retry binds the *same* run against the owner's journal.
    peer.cut(Cut::Bind);
    assert!(drain.refill(&destination, &template, 1).is_err());
    assert_eq!(
        jobs.local_pull_admissions().expect("records")[0].phase,
        LocalPullPhase::Created,
        "an unacknowledged bind is not a binding the caller may assume"
    );
    assert!(launcher.launched.borrow().is_empty());

    // Cut 4 — binding replays, the launch is reached and fails, and the owner
    // is unreachable for the settlement it produced.
    launcher.fail.set(true);
    peer.cut(Cut::Settle);
    assert!(drain.refill(&destination, &template, 1).is_err());
    let record = jobs.local_pull_admissions().expect("records").remove(0);
    assert_eq!(
        record.phase,
        LocalPullPhase::Settling,
        "a failed launch leaves one immutable settlement the next pass retries"
    );
    assert!(matches!(record.settlement, Some(ClaimMutation::Fail(_))));
    assert_eq!(record.leaf_run_id.as_deref(), Some(leaf.as_str()));
    assert_eq!(
        launcher.launched.borrow().as_slice(),
        std::slice::from_ref(&leaf)
    );
    assert_eq!(peer.binds.get(), 2, "the bind was retried, not skipped");

    // The retry settles idempotently. Nothing anywhere created a second claim
    // or a second leaf across four interruptions.
    drain
        .refill(&destination, &template, 0)
        .expect("settlement retry");
    assert_eq!(
        jobs.local_pull_admissions().expect("records")[0].phase,
        LocalPullPhase::Settled
    );
    assert_eq!(peer.settlements.get(), 1);
    assert_eq!(claimed_runs(&runtime), vec![leaf]);
    assert_eq!(
        jobs.local_pull_admissions().expect("records").len(),
        1,
        "one request survived every cut"
    );
    let claims = runtime.inspect_execution_claims().expect("claims");
    assert_eq!(claims.len(), 1, "one claim, however often it was requested");
    assert_eq!(claims[0].claim.task_id, task_id);
    assert_eq!(claims[0].claim.phase, ExecutionClaimPhase::Failed);
}

/// [ORB-12617] An execution that may already have started is not resumable.
///
/// The launch-intent record is committed before the process is spawned, so a
/// caller that finds one cannot know whether a worker is live. Every generic
/// path must refuse it, and — just as importantly — nothing may quietly settle
/// or revoke it on the assumption that it failed.
#[test]
fn an_uncertain_launch_refuses_generic_resume_and_waits_for_deliberate_recovery() {
    if isolated_claimed_test(
        "application::job::tests::exec::claimed_leaf::an_uncertain_launch_refuses_generic_resume_and_waits_for_deliberate_recovery",
    ) {
        return;
    }
    let (_root, runtime, _repo_root, _global) = owner_runtime();
    let task_id = seed_claimable_task(&runtime);
    let drain = drain_run(&runtime);
    let destination = destination(&runtime, MACHINE);
    let template = admission_request(&runtime, &drain, "local");
    let peer = CuttingPeer::new(&runtime);
    let launcher = KillingLauncher {
        real: LeafPullLauncher { runtime: &runtime },
    };
    let jobs = runtime.stores().jobs();
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
    };

    // The launch intent commits, then the process dies mid-spawn. Unwinding
    // out of `launch` is this fixture's kill: the drain never reaches its
    // acknowledgment, so the durable record stops at the one checkpoint that
    // means "a worker may be running right now".
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let killed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drain.refill(&destination, &template, 1)
    }));
    std::panic::set_hook(previous);
    assert!(killed.is_err(), "the fixture kills the launching process");

    let record = jobs.local_pull_admissions().expect("records").remove(0);
    assert_eq!(record.phase, LocalPullPhase::Launching);
    assert!(record.settlement.is_none());
    let leaf = record.leaf_run_id.clone().expect("leaf");

    // A later pass finds the uncertainty and refuses to guess either way: it
    // neither relaunches nor settles.
    let error = drain
        .refill(&destination, &template, 1)
        .expect_err("an uncertain launch is not resumable");
    assert!(
        error
            .to_string()
            .contains("deliberate recovery is required"),
        "{error}"
    );
    assert!(
        error.to_string().contains("never generic resume"),
        "{error}"
    );

    // Generic resume and generic execution both refuse the bound leaf.
    let error = runtime
        .submit_resume_run(&leaf, None, None)
        .expect_err("generic resume is refused");
    assert!(
        error.to_string().contains("deliberately recover"),
        "{error}"
    );
    let error = runtime
        .execute_pipeline_run_worker(&leaf)
        .expect_err("generic execution is refused");
    assert!(
        error.to_string().contains("handoff execution adapter"),
        "{error}"
    );

    // Nothing settled, nothing revoked, and the leaf was never re-created on
    // a guess that the first attempt died.
    let record = jobs.local_pull_admissions().expect("records").remove(0);
    assert_eq!(record.phase, LocalPullPhase::Launching);
    assert!(record.settlement.is_none());
    assert_eq!(claimed_runs(&runtime), vec![leaf.clone()]);
    assert_eq!(
        jobs.get_job_run(&leaf).expect("leaf").expect("run").state,
        orbit_types::workflow::JobRunState::Pending
    );
    let claims = runtime.inspect_execution_claims().expect("claims");
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].claim.task_id, task_id);
    assert_eq!(
        claims[0].claim.phase,
        ExecutionClaimPhase::Running,
        "an uncertain attempt keeps its authority until a human takes it away"
    );
}

/// Unwinds out of the launch seam, after the real binding is derived — the
/// fixture's stand-in for a process killed between the launch-intent commit
/// and any acknowledgment of the spawn.
struct KillingLauncher<'a> {
    real: LeafPullLauncher<'a>,
}

impl PullLauncher for KillingLauncher<'_> {
    fn launch(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        let _bound = self.real.bound_runtime(admission)?;
        panic!("fixture kills the launching process");
    }
}

/// [ORB-12617] Stopping a drain stops admission, not execution.
///
/// The parent's job is to decide whether more work starts. A claimed child is
/// already carrying an owner-side reservation, so cancelling the parent must
/// leave both the run and the claim exactly where they were — otherwise
/// closing a drain window would strand the owner holding a footprint for work
/// nobody is allowed to finish.
#[test]
fn stopping_parent_admission_leaves_a_live_claimed_child_alone() {
    if isolated_claimed_test(
        "application::job::tests::exec::claimed_leaf::stopping_parent_admission_leaves_a_live_claimed_child_alone",
    ) {
        return;
    }
    let (_root, runtime, _repo_root, _global) = owner_runtime();
    seed_claimable_task(&runtime);
    let parent = drain_run(&runtime);
    let destination = destination(&runtime, MACHINE);
    let template = admission_request(&runtime, &parent, "local");
    let peer = CuttingPeer::new(&runtime);
    let launcher = BindingOnlyLauncher {
        real: LeafPullLauncher { runtime: &runtime },
        launched: RefCell::new(Vec::new()),
        fail: Cell::new(false),
    };
    let jobs = runtime.stores().jobs();
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
    };
    drain.refill(&destination, &template, 1).expect("admit");
    let leaf = jobs.local_pull_admissions().expect("records")[0]
        .leaf_run_id
        .clone()
        .expect("leaf");

    jobs.finalize_job_run(
        &parent,
        orbit_types::workflow::JobRunState::Cancelled,
        chrono::Utc::now(),
        None,
    )
    .expect("stop the parent drain");

    let requests_before = peer.requests.get();
    assert_eq!(
        drain
            .refill(&destination, &template, 10)
            .expect("a stopped parent admits nothing"),
        0
    );
    assert_eq!(
        peer.requests.get(),
        requests_before,
        "a stopped parent does not even ask the owner for more work"
    );
    assert_eq!(
        jobs.get_job_run(&leaf).expect("leaf").expect("run").state,
        orbit_types::workflow::JobRunState::Pending,
        "the child run survives its parent"
    );
    let claims = runtime.inspect_execution_claims().expect("claims");
    assert_eq!(claims.len(), 1);
    assert_eq!(
        claims[0].claim.phase,
        ExecutionClaimPhase::Running,
        "stopping admission revokes nothing"
    );
    assert_eq!(
        claims[0].bound_run.as_ref().map(|run| run.run_id.as_str()),
        Some(leaf.as_str())
    );
}

/// [ORB-12617] A published claimed PR leaf hands off, and stops there.
///
/// This is the executor half of a follower's delivery, run on the real
/// `task_claimed_pr_pipeline`: a real worktree, a real commit, a real push to
/// a real (local, bare) `origin`, a real `pr_open` against a fixture `gh`, the
/// owner's required validation on the published candidate, and the typed
/// handoff. Nothing merges and nothing completes: the definition contains no
/// merge step to reach, and the durable settlement the run leaves behind is a
/// handoff for the owner's landing consumer to act on later.
///
/// The destination is owner-local because that is the only destination this
/// slice serves at all — a genuine follower destination is refused at the
/// distributed-mutation gate rather than faked, which the tail of this test
/// pins. The leaf definition, its steps and the handoff it produces are
/// chosen by the owner-resolved *ship mode*, not by the executing machine, so
/// they are the same ones a follower runs.
#[test]
fn a_published_pr_claim_hands_off_a_pull_request_without_merging() {
    if isolated_claimed_test(
        "application::job::tests::exec::claimed_leaf::a_published_pr_claim_hands_off_a_pull_request_without_merging",
    ) {
        return;
    }
    let (_root, runtime, repo_root, _global) = publishing_runtime();
    let task_id = seed_claimable_task(&runtime);
    let drain = drain_run(&runtime);
    let destination = destination(&runtime, MACHINE);
    let template = admission_request(&runtime, &drain, "pr");

    let peer = OwnerPullPeer { runtime: &runtime };
    let launcher = InProcessLauncher {
        real: LeafPullLauncher { runtime: &runtime },
        runtime: &runtime,
        repo_root: repo_root.clone(),
        job_name: "task_claimed_pr_pipeline",
        launched: RefCell::new(Vec::new()),
        outcome: RefCell::new(None),
    };
    let jobs = runtime.stores().jobs();
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
    };
    // The settlement is the one thing this slice cannot complete: accepting a
    // *published* pull request needs an owner-side provider observation that
    // is not implemented yet, so the refill ends on that refusal with the
    // handoff already durable. The run itself must have succeeded.
    let admitted = drain.refill(&destination, &template, 1);
    assert_eq!(
        launcher.outcome.borrow().clone(),
        Some(Ok(true)),
        "the claimed PR pipeline must run to its handoff"
    );
    let error = admitted.expect_err("owner acceptance of a published PR is not implemented yet");
    assert!(
        error.to_string().contains("not part of this slice"),
        "{error}"
    );
    let leaf = launcher.launched.borrow()[0].clone();
    assert_eq!(
        jobs.get_job_run(&leaf).expect("leaf").expect("row").job_id,
        "task_claimed_pr_pipeline"
    );

    // The handoff is durable, typed, and names the published pull request.
    let record = jobs.local_pull_admissions().expect("records").remove(0);
    assert_eq!(
        record.phase,
        LocalPullPhase::Settling,
        "a handoff the owner cannot accept yet stays pending, not lost"
    );
    let Some(ClaimMutation::AcceptHandoff(handoff)) = record.settlement.clone() else {
        panic!("a claimed PR leaf settles with a typed handoff: {record:?}");
    };
    assert_eq!(
        handoff.candidate.delivery,
        HandoffDelivery::PullRequest {
            number: FIXTURE_PR_NUMBER
        },
        "the delivery names the pull request pr_open actually observed"
    );
    assert_eq!(handoff.task_id, task_id);
    assert_eq!(handoff.validation.len(), 2, "both required commands ran");
    assert_eq!(
        handoff.candidate.base_branch, BASE_BRANCH,
        "the candidate is pinned to the base it was admitted for"
    );

    // Nothing merged. The base branch is untouched on both sides, the task is
    // not complete, and no landing authority was published.
    let local_base = git_stdout(&repo_root, &["rev-parse", BASE_BRANCH]);
    assert_eq!(handoff.candidate.base.commit, local_base);
    assert_ne!(handoff.candidate.candidate.commit, local_base);
    assert_eq!(
        git_stdout(
            &repo_root,
            &["rev-parse", &format!("refs/remotes/origin/{BASE_BRANCH}")],
        ),
        local_base,
        "the published branch is the candidate's; the base on origin never moved"
    );
    assert_ne!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Done,
        "a claimed leaf never completes its own task"
    );
    assert!(
        runtime.landing_start_requests().expect("outbox").is_empty(),
        "handing off is not authorizing a landing"
    );

    // And a genuine follower destination is refused rather than served.
    let follower = PullDestination {
        execution_machine_id: "follower-machine".into(),
        ..destination.clone()
    };
    let error = peer
        .request(&follower, &template)
        .expect_err("this adapter serves only its own machine");
    assert!(
        error.to_string().contains("distributed") || error.to_string().contains("not this machine"),
        "{error}"
    );
}

/// [ORB-12642] A remote-sync claimed leaf must observe `origin/<base>` when
/// the executor's local base ref lags the remote tip — the ordinary state of
/// a working checkout, and the case that made `claim_validate` refuse its
/// own `sync_base` checkpoint.
#[test]
fn a_published_pr_claim_observes_the_remote_base_when_local_lags() {
    if isolated_claimed_test(
        "application::job::tests::exec::claimed_leaf::a_published_pr_claim_observes_the_remote_base_when_local_lags",
    ) {
        return;
    }
    let (root, runtime, repo_root, _global) = publishing_runtime();
    let local_base = git_stdout(&repo_root, &["rev-parse", BASE_BRANCH]);
    let remote_tip = advance_origin_base(&root.path().join("origin.git"));
    assert_ne!(
        remote_tip, local_base,
        "the fixture must leave the local base behind origin"
    );
    assert_eq!(
        git_stdout(&repo_root, &["rev-parse", BASE_BRANCH]),
        local_base,
        "advancing origin must not move the local base ref"
    );

    let task_id = seed_claimable_task(&runtime);
    let drain = drain_run(&runtime);
    let destination = destination(&runtime, MACHINE);
    let template = admission_request(&runtime, &drain, "pr");
    let peer = OwnerPullPeer { runtime: &runtime };
    let launcher = InProcessLauncher {
        real: LeafPullLauncher { runtime: &runtime },
        runtime: &runtime,
        repo_root: repo_root.clone(),
        job_name: "task_claimed_pr_pipeline",
        launched: RefCell::new(Vec::new()),
        outcome: RefCell::new(None),
    };
    let jobs = runtime.stores().jobs();
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
    };
    let admitted = drain.refill(&destination, &template, 1);
    assert_eq!(
        launcher.outcome.borrow().clone(),
        Some(Ok(true)),
        "the claimed PR pipeline must not refuse a lagging local base"
    );
    let error = admitted.expect_err("owner acceptance of a published PR is not implemented yet");
    assert!(
        error.to_string().contains("not part of this slice"),
        "{error}"
    );

    let record = jobs.local_pull_admissions().expect("records").remove(0);
    let Some(ClaimMutation::AcceptHandoff(handoff)) = record.settlement.clone() else {
        panic!("a claimed PR leaf settles with a typed handoff: {record:?}");
    };
    assert_eq!(
        handoff.candidate.base.commit, remote_tip,
        "remote-sync observation must pin the origin tip the candidate was synchronized onto"
    );
    assert_ne!(
        handoff.candidate.base.commit, local_base,
        "a lagging local base must not be recorded as the observed base"
    );
    assert_eq!(
        git_stdout(&repo_root, &["rev-parse", BASE_BRANCH]),
        local_base,
        "the leaf must not have fast-forwarded the local base ref"
    );
    assert_eq!(handoff.task_id, task_id);
}

/// Records nothing and fails the launch, so the fixture can inspect a queued
/// but never-executed claimed leaf.
struct RefusingLauncher;
impl PullLauncher for RefusingLauncher {
    fn launch(&self, _admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        Err(OrbitError::Execution("fixture withholds the launch".into()))
    }
}

/// A `gh` that serves exactly the three calls `pr_open` makes, and fails
/// loudly on anything else.
///
/// Installed ahead of the real one for every isolated child, so a fixture can
/// never quietly reach a live GitHub account — and a step that starts calling
/// some other `gh` subcommand fails instead of silently doing something real.
fn install_fixture_gh(bin: &Path) {
    // `pr_view`'s selector guard accepts a bare number or a github.com PR URL,
    // so the fixture speaks that shape. Nothing here is ever contacted: every
    // call the run makes is answered by this script.
    let url = format!("https://github.com/orbit-fixture/repo/pull/{FIXTURE_PR_NUMBER}");
    std::fs::create_dir_all(bin).expect("fixture bin");
    let script = format!(
        r#"#!/bin/sh
case "$1 $2" in
  "pr list") printf '[]
' ;;
  "pr create") printf '{url}
' ;;
  "pr view") printf '{{"number":{FIXTURE_PR_NUMBER},"title":"fixture","body":"fixture","headRefName":"fixture","files":[],"commits":[],"url":"{url}"}}
' ;;
  *) echo "fixture gh does not implement: $*" >&2; exit 1 ;;
esac
"#
    );
    let path = bin.join("gh");
    std::fs::write(&path, script).expect("write fixture gh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("fixture gh is executable");
    }
}

/// These fixtures mutate host-global Orbit state (worker bindings, admission
/// receipts), so each runs in its own process with an isolated HOME.
fn isolated_claimed_test(name: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_CLAIMED_LEAF_CHILD";
    if std::env::var(CHILD).ok().as_deref() == Some(name) {
        return false;
    }
    let home = tempfile::tempdir().expect("isolated home");
    let bin = home.path().join("bin");
    install_fixture_gh(&bin);
    let path = match std::env::var_os("PATH") {
        Some(inherited) => {
            let mut entries = vec![bin.clone()];
            entries.extend(std::env::split_paths(&inherited));
            std::env::join_paths(entries).expect("fixture PATH")
        }
        None => bin.clone().into_os_string(),
    };
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, name)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("PATH", path)
        .output()
        .expect("isolated claimed-leaf child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{name}: {stdout}\n{stderr}");
    assert!(
        stdout.contains("test result: ok. 1 passed;"),
        "child did not execute exact test: {stdout}"
    );
    true
}
