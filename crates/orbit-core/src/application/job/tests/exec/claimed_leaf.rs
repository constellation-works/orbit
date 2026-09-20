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

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_engine::RuntimeHost;
use orbit_store::contracts::*;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::PipelineState;
use orbit_types::workflow::handoff::{HandoffDelivery, HandoffValidationLog};
use serde_json::json;

use super::{git_in, resolved_job, seed_default_catalogs, try_execute_job};
use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::pull::{PullDrain, PullLauncher};
use crate::adapter::engine_host::v2_host::pull_adapters::{LeafPullLauncher, OwnerPullPeer};
use crate::application::distributed::owner_binary_version;
use crate::application::task::TaskAddParams;

const BASE_BRANCH: &str = "agent-main";
const MACHINE: &str = "owner-machine";
const VALIDATION: &str = "echo claimed-candidate-validated";

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
        let job = resolved_job(self.runtime, "task_claimed_local_pipeline");
        let input = self
            .runtime
            .stores()
            .jobs()
            .get_job_run(&binding.bound_run_id)
            .expect("leaf run")
            .expect("leaf run row")
            .input
            .expect("leaf run input");
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
    use crate::adapter::engine_host::v2_host::pull::PullPeer;
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
    use crate::adapter::engine_host::v2_host::pull::PullPeer;
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

/// Records nothing and fails the launch, so the fixture can inspect a queued
/// but never-executed claimed leaf.
struct RefusingLauncher;
impl PullLauncher for RefusingLauncher {
    fn launch(&self, _admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        Err(OrbitError::Execution("fixture withholds the launch".into()))
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
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, name)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
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
