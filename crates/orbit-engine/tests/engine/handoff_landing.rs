//! The owner's landing of a claimed candidate.
//!
//! A fast-forward has no provider identity, so the owner's delivery consumers
//! can attribute it only from a direct landing intent retained before the
//! branch moves. The landing names the accepted handoff and its task in that
//! intent; a candidate that cannot fast-forward lands nothing and records none
//! [ORB-13894]. PR reconciliation resolves external uncertainty before stopping
//! a mismatched delivery, so subsequent attempts see the settled state. A stop
//! on a conflicting or stale base is marked repairable [ORB-14261]. A hosted
//! check the review owed the owner gates the merge, and only an explicit
//! success counts [ORB-15677] [ORB-15812]. A failed check is classified from
//! its job log: one inherited from the base updates the branch, a transient
//! one is rerun once, and the candidate's own earns one repair [ORB-15956].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use orbit_common::OrbitError;
use orbit_engine::{
    HandoffLandingContext, HandoffLandingStep, HandoffLandingUpdate, RuntimeHost,
    execute_deterministic_action, observe_candidate,
};
use orbit_types::workflow::automation::DirectLandingRequest;
use orbit_types::workflow::handoff::HandoffDelivery;
use serde_json::{Value, json};
use tempfile::tempdir;

const TASK_ID: &str = "T1";
const RUN_ID: &str = "jrun-landing-test";

struct LandingHost {
    repo: PathBuf,
    context: HandoffLandingContext,
    steps: Mutex<Vec<HandoffLandingStep>>,
    /// Each retained intent with the landing branch's tip when it was retained.
    intents: Mutex<Vec<(DirectLandingRequest, String)>>,
}

impl orbit_engine::test_support::NullRuntimeHost for LandingHost {
    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo.to_string_lossy().into_owned())
    }

    fn handoff_landing_context(
        &self,
        _handoff_id: &str,
    ) -> Result<HandoffLandingContext, OrbitError> {
        Ok(self.context.clone())
    }

    fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Result<(), OrbitError> {
        self.steps.lock().unwrap().push(update.step.clone());
        Ok(())
    }

    fn record_direct_landing_intent(
        &self,
        request: &DirectLandingRequest,
    ) -> Result<(), OrbitError> {
        let tip = git(&self.repo, &["rev-parse", "main"]);
        self.intents.lock().unwrap().push((request.clone(), tip));
        Ok(())
    }
}

/// An owner checkout on `main` with the claimed candidate on `orbit/T1`, and
/// the host the landing job hands the accepted handoff to.
fn owner_checkout(repo: &Path) -> LandingHost {
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "base\n").unwrap();
    git(repo, &["add", "README.md"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    git(repo, &["checkout", "-q", "-b", "orbit/T1"]);
    std::fs::write(repo.join("work.txt"), "delivered\n").unwrap();
    git(repo, &["add", "work.txt"]);
    git(repo, &["commit", "-q", "-m", "candidate"]);
    git(repo, &["checkout", "-q", "main"]);
    let candidate = observe_candidate(
        repo,
        Some("orbit/T1"),
        "main",
        "main",
        HandoffDelivery::LocalCandidate,
        "workspace",
        orbit_engine::BaseSyncMode::Local,
    )
    .unwrap();
    LandingHost {
        repo: repo.to_path_buf(),
        context: HandoffLandingContext {
            handoff_id: "handoff-1".into(),
            task_id: TASK_ID.into(),
            claim_id: "claim-1".into(),
            candidate,
            unresolved_merge_intent: None,
            workspace_path: repo.to_path_buf(),
            owed_hosted_checks: Vec::new(),
            automatic_repair_spent: false,
        },
        steps: Mutex::default(),
        intents: Mutex::default(),
    }
}

fn land(host: &impl RuntimeHost) -> Result<Value, OrbitError> {
    land_with(host, json!({}))
}

/// [`land`] with `extra` merged into the step input.
fn land_with(host: &impl RuntimeHost, extra: Value) -> Result<Value, OrbitError> {
    let mut input = json!({"handoff_id": "handoff-1", "task_id": TASK_ID, "run_id": RUN_ID});
    if let (Some(input), Value::Object(extra)) = (input.as_object_mut(), extra) {
        input.extend(extra);
    }
    execute_deterministic_action(
        host,
        "handoff_land",
        &json!({}),
        &input,
        false,
        &HashMap::new(),
        None,
    )
}

#[test]
fn a_local_candidate_landing_names_its_task_before_the_branch_moves() {
    let sandbox = tempdir().unwrap();
    let host = owner_checkout(sandbox.path());
    let base = git(&host.repo, &["rev-parse", "main"]);
    let candidate = host.context.candidate.candidate.commit.clone();

    land(&host).expect("the candidate fast-forwards");

    assert_eq!(git(&host.repo, &["rev-parse", "main"]), candidate);
    assert_eq!(
        *host.intents.lock().unwrap(),
        vec![(
            DirectLandingRequest {
                run_id: RUN_ID.into(),
                branch: "main".into(),
                before_commit: base.clone(),
                after_commit: candidate,
                task_ids: vec![TASK_ID.into()],
                handoff_id: Some("handoff-1".into()),
            },
            base,
        )],
        "one intent, naming the handoff and its task, retained while main was still at the base"
    );
    assert_eq!(
        host.steps.lock().unwrap().last(),
        Some(&HandoffLandingStep::Complete)
    );
}

#[test]
fn a_candidate_that_cannot_fast_forward_records_no_landing_intent() {
    let sandbox = tempdir().unwrap();
    let host = owner_checkout(sandbox.path());
    std::fs::write(host.repo.join("other.txt"), "another landing\n").unwrap();
    git(&host.repo, &["add", "other.txt"]);
    git(&host.repo, &["commit", "-q", "-m", "another landing"]);

    land(&host).expect_err("a diverged landing branch stops the attempt");

    assert!(host.intents.lock().unwrap().is_empty());
    assert_eq!(
        host.steps.lock().unwrap().last(),
        Some(&HandoffLandingStep::Stop { repairable: true }),
        "a landing branch that moved past the candidate's base earns an automatic repair"
    );
}

/// Model the owner's journal guards at the RuntimeHost boundary: Stop needs a
/// resolved intent, and a settled attempt cannot accept more landing writes.
struct ReconciliationHost {
    context: Mutex<HandoffLandingContext>,
    updates: Mutex<Vec<HandoffLandingUpdate>>,
    status: Value,
}

impl orbit_engine::test_support::NullRuntimeHost for ReconciliationHost {
    fn handoff_landing_context(
        &self,
        _handoff_id: &str,
    ) -> Result<HandoffLandingContext, OrbitError> {
        Ok(self.context.lock().unwrap().clone())
    }

    fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Result<(), OrbitError> {
        let mut updates = self.updates.lock().unwrap();
        if updates
            .last()
            .is_some_and(|u| matches!(u.step, HandoffLandingStep::Stop { .. }))
        {
            return Err(OrbitError::InvalidInput(
                "landing attempt is already settled".into(),
            ));
        }
        let mut context = self.context.lock().unwrap();
        match &update.step {
            HandoffLandingStep::ResolveIntent { intent_id, .. } => {
                assert_eq!(context.unresolved_merge_intent.as_ref(), Some(intent_id));
                context.unresolved_merge_intent = None;
            }
            HandoffLandingStep::Stop { .. } => {
                assert!(
                    context.unresolved_merge_intent.is_none(),
                    "Stop must follow intent resolution"
                );
            }
            other => panic!("a mismatched reconciled PR must not reach {other:?}"),
        }
        updates.push(update.clone());
        Ok(())
    }

    fn run_private_vcs_operation(
        &self,
        operation: &str,
        _input: Value,
    ) -> Result<Value, OrbitError> {
        assert_eq!(
            operation, "pr.status",
            "reconciliation must not send another merge"
        );
        Ok(json!({"pull_request": self.status}))
    }
}

#[test]
fn a_mismatched_merged_pr_resolves_the_intent_before_stopping_later_attempts() {
    for (field, replacement) in [
        ("headRefName", "another-branch"),
        ("baseRefName", "another-base"),
        ("headRefOid", "another-commit"),
    ] {
        let sandbox = tempdir().unwrap();
        let mut context = owner_checkout(sandbox.path()).context;
        context.candidate.delivery = HandoffDelivery::PullRequest { number: 42 };
        let intent_id = "handoff-1:42".to_string();
        // A lost merge reply leaves this intent for the next attempt to read.
        context.unresolved_merge_intent = Some(intent_id.clone());
        let mut status = json!({
            "state": "MERGED",
            "headRefName": context.candidate.source_branch,
            "baseRefName": context.candidate.base_branch,
            "headRefOid": context.candidate.candidate.commit,
            "mergeCommit": {"oid": "provider-merge"},
            // Unknown to the typed answer; the evidence still quotes it.
            "futureField": {"nested": [1, "x", null]},
        });
        status[field] = json!(replacement);
        let host = ReconciliationHost {
            context: Mutex::new(context),
            updates: Mutex::default(),
            status,
        };

        let error = land(&host).expect_err("a foreign merged identity cannot complete the handoff");
        let updates = host.updates.lock().unwrap().clone();
        assert_eq!(
            updates.len(),
            2,
            "one resolved intent and one durable Stop for {field}"
        );
        assert_eq!(
            updates[0].step,
            HandoffLandingStep::ResolveIntent {
                intent_id,
                merged: true
            }
        );
        assert_eq!(
            updates[1].step,
            HandoffLandingStep::Stop { repairable: false },
            "stale delivery evidence is not a base conflict a rebase repairs"
        );
        let reason: String = serde_json::from_str(&updates[1].evidence).unwrap();
        assert!(
            reason.contains("delivery_evidence_stale")
                && reason.contains(field)
                && reason.contains(replacement),
            "the Stop must retain the observed identity mismatch: {reason}"
        );
        assert!(
            matches!(error, OrbitError::Execution(ref message) if message == &format!("handoff_land: {reason}"))
        );
        assert_eq!(
            updates[0].evidence,
            json!({
                "reconciled": "pull_request_merged",
                "pull_request": 42,
                "provider_state": host.status,
                "delivery_error": reason,
            })
            .to_string(),
            "reconciliation evidence quotes the provider's answer byte for byte"
        );
        assert!(
            host.handoff_landing_context("handoff-1")
                .unwrap()
                .unresolved_merge_intent
                .is_none()
        );

        let retry = land(&host).expect_err("a later attempt sees the settled Stop");
        assert!(
            matches!(retry, OrbitError::InvalidInput(ref message) if message == "landing attempt is already settled")
        );
        assert_eq!(
            *host.updates.lock().unwrap(),
            updates,
            "retry retains the original Stop evidence"
        );
    }
}

/// A pull request the provider reports open with a fixed merge state and
/// status rollup. It answers only status reads; any other operation is
/// recorded and refused, so a test sees whether the landing tried to merge.
struct OpenPullRequestHost {
    context: HandoffLandingContext,
    steps: Mutex<Vec<HandoffLandingStep>>,
    merge_state: &'static str,
    rollup: Value,
    sent: Mutex<Vec<String>>,
}

impl OpenPullRequestHost {
    fn new(context: HandoffLandingContext, merge_state: &'static str, rollup: Value) -> Self {
        Self {
            context,
            steps: Mutex::default(),
            merge_state,
            rollup,
            sent: Mutex::default(),
        }
    }
}

impl orbit_engine::test_support::NullRuntimeHost for OpenPullRequestHost {
    fn handoff_landing_context(
        &self,
        _handoff_id: &str,
    ) -> Result<HandoffLandingContext, OrbitError> {
        Ok(self.context.clone())
    }

    fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Result<(), OrbitError> {
        self.steps.lock().unwrap().push(update.step.clone());
        Ok(())
    }

    fn run_private_vcs_operation(
        &self,
        operation: &str,
        _input: Value,
    ) -> Result<Value, OrbitError> {
        if operation == "ci.commit_jobs" {
            return Ok(json!({"jobs": []}));
        }
        if operation != "pr.status" {
            self.sent.lock().unwrap().push(operation.to_string());
            return Err(OrbitError::Execution(format!(
                "fixture sends no {operation}"
            )));
        }
        let candidate = &self.context.candidate;
        Ok(json!({"pull_request": {
            "state": "OPEN",
            "headRefName": candidate.source_branch,
            "baseRefName": candidate.base_branch,
            "headRefOid": candidate.candidate.commit,
            "mergeStateStatus": self.merge_state,
            "reviewDecision": "",
            "statusCheckRollup": self.rollup,
        }}))
    }
}

/// [ORB-14261] Only a stop a rebase can cure earns the automatic repair: a
/// conflicting or stale base. A protection refusal stays an operator's call.
#[test]
fn only_a_conflicting_or_stale_base_stops_a_pull_request_for_repair() {
    for (merge_state, repairable) in [
        ("DIRTY", true),
        ("BEHIND", true),
        ("DRAFT", false),
        (" DIRTY ", false),
    ] {
        let sandbox = tempdir().unwrap();
        let mut context = owner_checkout(sandbox.path()).context;
        context.candidate.delivery = HandoffDelivery::PullRequest { number: 42 };
        let host = OpenPullRequestHost::new(context, merge_state, json!([]));

        land_with(&host, json!({"max_wait_seconds": 0}))
            .expect_err("an unmergeable pull request stops the landing");

        assert_eq!(
            *host.steps.lock().unwrap(),
            vec![HandoffLandingStep::Stop { repairable }],
            "merge state {merge_state}"
        );
        assert!(
            host.sent.lock().unwrap().is_empty(),
            "a stopped landing sends no merge"
        );
    }
}

/// [ORB-15677] [ORB-15812] A check the before-landing review owed the pull
/// request's hosted CI gates the merge itself, even where branch protection
/// does not require it: the landing merges only once that check reports an
/// explicit success on the pinned head, stops when it failed, and never
/// merges while it is absent, still running, skipped, or neutral. A
/// StatusContext success still passes. A local candidate, which no hosted CI
/// runs, never lands with one.
#[test]
fn an_owed_hosted_check_must_pass_before_the_owner_merges() {
    const OWED: &str = "CodeQL / Analyze (rust)";
    let check = |conclusion: Option<&str>| {
        json!({
            "__typename": "CheckRun",
            "name": "Analyze (rust)",
            "workflowName": "CodeQL",
            "status": if conclusion.is_some() { "COMPLETED" } else { "IN_PROGRESS" },
            "conclusion": conclusion.unwrap_or_default(),
        })
    };
    let other = json!({"__typename": "CheckRun", "name": "fast", "workflowName": "CI",
                       "status": "COMPLETED", "conclusion": "SUCCESS"});
    let status_success = json!({
        "__typename": "StatusContext",
        "context": OWED,
        "state": "SUCCESS",
    });
    for (case, rollup, merges, stop_reason) in [
        ("passed", json!([other, check(Some("SUCCESS"))]), true, None),
        (
            "skipped",
            json!([other, check(Some("SKIPPED"))]),
            false,
            Some("not yet passed"),
        ),
        (
            "neutral",
            json!([other, check(Some("NEUTRAL"))]),
            false,
            Some("not yet passed"),
        ),
        (
            "failed",
            json!([other, check(Some("FAILURE"))]),
            false,
            Some("failed"),
        ),
        (
            "running",
            json!([other, check(None)]),
            false,
            Some("not yet passed"),
        ),
        ("absent", json!([other]), false, Some("not yet passed")),
        (
            "status context passed",
            json!([other, status_success]),
            true,
            None,
        ),
    ] {
        let sandbox = tempdir().unwrap();
        let mut context = owner_checkout(sandbox.path()).context;
        context.candidate.delivery = HandoffDelivery::PullRequest { number: 42 };
        context.owed_hosted_checks = vec![OWED.into()];
        let host = OpenPullRequestHost::new(context, "CLEAN", rollup);

        let error = land_with(&host, json!({"max_wait_seconds": 0}))
            .expect_err("the fixture never reports a merge");

        let sent = host.sent.lock().unwrap().clone();
        assert_eq!(!sent.is_empty(), merges, "{case}: {sent:?} {error}");
        let steps = host.steps.lock().unwrap().clone();
        match stop_reason {
            None => assert!(steps.is_empty(), "{case}: {steps:?}"),
            Some(reason) => {
                assert_eq!(
                    steps,
                    vec![HandoffLandingStep::Stop { repairable: false }],
                    "{case}"
                );
                let message = error.to_string();
                assert!(
                    message.contains(OWED) && message.contains(reason),
                    "{case}: {message}"
                );
            }
        }
    }

    let sandbox = tempdir().unwrap();
    let mut host = owner_checkout(sandbox.path());
    host.context.owed_hosted_checks = vec![OWED.into()];
    let tip = git(&host.repo, &["rev-parse", "main"]);
    let error = land(&host).expect_err("no hosted CI runs a local candidate");
    assert!(error.to_string().contains(OWED), "{error}");
    assert_eq!(
        *host.steps.lock().unwrap(),
        vec![HandoffLandingStep::Stop { repairable: false }]
    );
    assert_eq!(
        git(&host.repo, &["rev-parse", "main"]),
        tip,
        "nothing landed"
    );
}

/// The hosted CI one failed check job ran: its run, job and log.
struct FakeJob {
    run_id: u64,
    job_id: u64,
    conclusion: &'static str,
    log: &'static str,
}

/// What the fake forge reports for one commit: its check's job, if any ran.
#[derive(Default)]
struct ForgeState {
    head: String,
    merged: bool,
    /// The `CI / clippy` job on each commit.
    jobs: HashMap<String, FakeJob>,
    /// What a rerun of a failed job leaves on the head.
    after_rerun: Option<FakeJob>,
    /// What the check reports on a head the branch update produced.
    after_update: Option<FakeJob>,
}

/// A forge whose pull request #42 carries the claimed candidate, with one
/// `CI / clippy` check per commit. It answers the reads a landing makes,
/// reruns and updates the branch the way the provider does — an update
/// merges `main` into the head as a real commit — and records every
/// operation, so a test sees what the landing asked for.
struct FakeForge {
    repo: PathBuf,
    context: HandoffLandingContext,
    state: Mutex<ForgeState>,
    ops: Mutex<Vec<(String, Value)>>,
    updates: Mutex<Vec<HandoffLandingUpdate>>,
}

impl FakeForge {
    fn new(sandbox: &Path, owed: bool) -> Self {
        let host = owner_checkout(sandbox);
        let mut context = host.context;
        context.candidate.delivery = HandoffDelivery::PullRequest { number: 42 };
        if owed {
            context.owed_hosted_checks = vec!["CI / clippy".into()];
        }
        Self {
            repo: host.repo,
            state: Mutex::new(ForgeState {
                head: context.candidate.candidate.commit.clone(),
                ..ForgeState::default()
            }),
            context,
            ops: Mutex::default(),
            updates: Mutex::default(),
        }
    }

    fn job(&self, commit: &str, job: FakeJob) {
        self.state
            .lock()
            .unwrap()
            .jobs
            .insert(commit.to_string(), job);
    }

    fn sent(&self, operation: &str) -> Vec<Value> {
        self.ops
            .lock()
            .unwrap()
            .iter()
            .filter(|(sent, _)| sent == operation)
            .map(|(_, input)| input.clone())
            .collect()
    }

    fn steps(&self) -> Vec<HandoffLandingStep> {
        self.updates
            .lock()
            .unwrap()
            .iter()
            .map(|update| update.step.clone())
            .collect()
    }

    fn last_evidence(&self) -> String {
        self.updates
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .evidence
            .clone()
    }

    fn job_json(job: &FakeJob) -> Value {
        json!({
            "run_id": job.run_id, "job_id": job.job_id, "workflow": "CI", "name": "clippy",
            "status": "completed", "conclusion": job.conclusion,
            "failed_step": (job.conclusion == "failure").then_some("Run clippy"),
        })
    }
}

impl orbit_engine::test_support::NullRuntimeHost for FakeForge {
    fn handoff_landing_context(
        &self,
        _handoff_id: &str,
    ) -> Result<HandoffLandingContext, OrbitError> {
        Ok(self.context.clone())
    }

    fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Result<(), OrbitError> {
        self.updates.lock().unwrap().push(update.clone());
        Ok(())
    }

    fn run_private_vcs_operation(
        &self,
        operation: &str,
        input: Value,
    ) -> Result<Value, OrbitError> {
        self.ops
            .lock()
            .unwrap()
            .push((operation.to_string(), input.clone()));
        let mut state = self.state.lock().unwrap();
        let candidate = &self.context.candidate;
        match operation {
            "pr.status" => {
                let job = state.jobs.get(&state.head);
                let (status, conclusion) = match job.map(|job| job.conclusion) {
                    None => ("IN_PROGRESS", ""),
                    Some("success") => ("COMPLETED", "SUCCESS"),
                    Some(_) => ("COMPLETED", "FAILURE"),
                };
                let merge_state = match conclusion {
                    "SUCCESS" => "CLEAN",
                    "" => "PENDING",
                    _ => "BLOCKED",
                };
                Ok(json!({"pull_request": {
                    "number": 42,
                    "state": if state.merged { "MERGED" } else { "OPEN" },
                    "headRefName": candidate.source_branch,
                    "baseRefName": candidate.base_branch,
                    "headRefOid": state.head,
                    "mergeStateStatus": merge_state,
                    "reviewDecision": "",
                    "mergeCommit": state.merged.then(|| json!({"oid": "provider-merge"})),
                    "statusCheckRollup": [{
                        "__typename": "CheckRun", "name": "clippy", "workflowName": "CI",
                        "status": status, "conclusion": conclusion,
                    }],
                }}))
            }
            "ci.commit_jobs" => {
                let commit = input["commit"].as_str().unwrap();
                let jobs: Vec<Value> = state
                    .jobs
                    .get(commit)
                    .map(Self::job_json)
                    .into_iter()
                    .collect();
                Ok(json!({"commit": commit, "jobs": jobs}))
            }
            "ci.job_log" => {
                let job_id: u64 = input["job_id"].as_str().unwrap().parse().unwrap();
                let log = state
                    .jobs
                    .values()
                    .find(|job| job.job_id == job_id)
                    .map(|job| job.log)
                    .expect("a log for a job the forge reported");
                Ok(json!({"log": log}))
            }
            "ci.rerun_failed" => {
                let rerun = state.after_rerun.take().expect("a scripted rerun");
                let head = state.head.clone();
                state.jobs.insert(head, rerun);
                Ok(json!({}))
            }
            "pr.update_branch" => {
                assert_eq!(input["expected_head_sha"], state.head.as_str());
                let tree = git(
                    &self.repo,
                    &["merge-tree", "--write-tree", &state.head, "main"],
                );
                let merge = git(
                    &self.repo,
                    &[
                        "commit-tree",
                        &tree,
                        "-p",
                        &state.head,
                        "-p",
                        "main",
                        "-m",
                        "Merge branch 'main' into orbit/T1",
                    ],
                );
                let after = state.after_update.take().expect("a scripted update");
                state.jobs.insert(merge.clone(), after);
                state.head = merge;
                Ok(json!({}))
            }
            "pr.merge_capabilities" => Ok(json!({"repository": {
                "name_with_owner": candidate.repository,
                "base_branch": candidate.base_branch,
                "allow_squash_merge": true, "allow_rebase_merge": false,
                "allow_merge_commit": false, "allow_auto_merge": false,
                "requires_linear_history": false,
            }})),
            "pr.merge" => {
                assert_eq!(input["reviewed_head_sha"], state.head.as_str());
                state.merged = true;
                Ok(json!({"landed_commit": "provider-merge"}))
            }
            other => panic!("the landing sent an unexpected {other}"),
        }
    }
}

const CLIPPY_DEAD_CODE: &str = "Run cargo clippy --workspace -- -D warnings\n\
     error: function `stale_helper` is never used\n\
     --> crates/orbit-engine/src/lib.rs:10:4\n\
     ##[error]Process completed with exit code 101.";
const CANDIDATE_ERROR: &str = "Run cargo clippy --workspace -- -D warnings\n\
     error[E0425]: cannot find value `delivered` in this scope\n\
     ##[error]Process completed with exit code 101.";
const NETWORK_FLAKE: &str = "Run cargo fetch\n\
     error: failed to download from `https://index.crates.io/config.json`\n\
     curl: (28) Operation timed out after 30000 milliseconds\n\
     ##[error]Process completed with exit code 101.";

fn failed(run_id: u64, job_id: u64, log: &'static str) -> FakeJob {
    FakeJob {
        run_id,
        job_id,
        conclusion: "failure",
        log,
    }
}

fn passed(run_id: u64, job_id: u64) -> FakeJob {
    FakeJob {
        run_id,
        job_id,
        conclusion: "success",
        log: "",
    }
}

/// [ORB-15956] The incident: a pull request goes red on its base's own
/// dead-code error, which the base fails the same way, while `main` has
/// since moved past a fix and passes. The landing merges `main` into the
/// branch, waits for the check on that head, and lands it — no stop, so no
/// repair and no agent run. The owner's observation stays the validated
/// candidate; the merge is pinned to the updated head.
#[test]
fn a_check_failure_inherited_from_the_base_updates_the_branch_and_lands() {
    let sandbox = tempdir().unwrap();
    let forge = FakeForge::new(sandbox.path(), true);
    let candidate = forge.context.candidate.candidate.commit.clone();
    let base = forge.context.candidate.base.commit.clone();
    std::fs::write(forge.repo.join("fix.txt"), "the base's fix\n").unwrap();
    git(&forge.repo, &["add", "fix.txt"]);
    git(&forge.repo, &["commit", "-q", "-m", "fix the base"]);
    let fixed = git(&forge.repo, &["rev-parse", "main"]);
    forge.job(&candidate, failed(1, 11, CLIPPY_DEAD_CODE));
    forge.job(&base, failed(2, 21, CLIPPY_DEAD_CODE));
    forge.job(&fixed, passed(3, 31));
    forge.state.lock().unwrap().after_update = Some(passed(4, 41));

    let landed = land_with(&forge, json!({"max_wait_seconds": 0})).expect("the update lands");

    assert_eq!(landed["phase"], "landed", "{landed}");
    assert_eq!(forge.sent("pr.update_branch").len(), 1);
    assert!(forge.sent("ci.rerun_failed").is_empty());
    let head = forge.state.lock().unwrap().head.clone();
    assert_eq!(
        git(
            &forge.repo,
            &["rev-parse", &format!("{head}^1"), &format!("{head}^2")]
        ),
        format!("{candidate}\n{fixed}"),
        "the merged head is the candidate merged with the fixed base"
    );
    assert_eq!(
        forge.sent("pr.merge")[0]["reviewed_head_sha"],
        head.as_str()
    );
    let steps = forge.steps();
    assert_eq!(
        steps.last(),
        Some(&HandoffLandingStep::Complete),
        "{steps:?}"
    );
    assert!(
        !steps
            .iter()
            .any(|step| matches!(step, HandoffLandingStep::Stop { .. })),
        "{steps:?}"
    );
    let updates = forge.updates.lock().unwrap().clone();
    assert!(
        updates
            .iter()
            .filter_map(|update| update.observed.as_ref())
            .all(|observed| observed.candidate.commit == candidate),
        "the owner observes the validated candidate, never the merged head"
    );
    let evidence: Value = serde_json::from_str(&forge.last_evidence()).unwrap();
    assert_eq!(evidence["updated_head"], head.as_str(), "{evidence}");
    assert!(
        evidence["check_recovery"][0]
            .as_str()
            .is_some_and(|action| action.contains("inherited") && action.contains(&fixed)),
        "{evidence}"
    );
}

/// [ORB-15956] A check the candidate itself fails stops the landing for the
/// one automatic repair, whose evidence carries the failed step's signature
/// and log excerpt. The repair's own landing failing again stops in review,
/// classified, instead of asking for a second repair.
#[test]
fn a_candidate_caused_check_failure_gets_one_repair_then_stops_in_review() {
    for (repair_spent, repairable) in [(false, true), (true, false)] {
        let sandbox = tempdir().unwrap();
        let mut forge = FakeForge::new(sandbox.path(), false);
        forge.context.automatic_repair_spent = repair_spent;
        let candidate = forge.context.candidate.candidate.commit.clone();
        let base = forge.context.candidate.base.commit.clone();
        forge.job(&candidate, failed(1, 11, CANDIDATE_ERROR));
        forge.job(&base, passed(2, 21));

        let error = land_with(&forge, json!({"max_wait_seconds": 0}))
            .expect_err("the candidate's failure stops the landing");

        assert_eq!(
            forge.steps(),
            vec![HandoffLandingStep::Stop { repairable }],
            "repair spent: {repair_spent}"
        );
        let reason: String = serde_json::from_str(&forge.last_evidence()).unwrap();
        assert!(
            reason.contains("classified `candidate`")
                && reason.contains("cannot find value `delivered` in this scope")
                && reason.contains("Process completed with exit code 101"),
            "the stop carries the classification, signature and log excerpt: {reason}"
        );
        assert!(
            error.to_string().contains("classified `candidate`"),
            "{error}"
        );
        assert!(forge.sent("pr.merge").is_empty());
        assert!(forge.sent("pr.update_branch").is_empty());
        assert!(forge.sent("ci.rerun_failed").is_empty());
    }
}

/// [ORB-15956] A check that failed on the network is rerun once before it is
/// judged: a passing rerun lands, and a second transient failure stops in
/// review with the classification rather than rerunning again.
#[test]
fn a_transient_check_failure_is_rerun_once_before_it_is_judged() {
    for (rerun, lands) in [(passed(1, 12), true), (failed(1, 12, NETWORK_FLAKE), false)] {
        let sandbox = tempdir().unwrap();
        let forge = FakeForge::new(sandbox.path(), true);
        let candidate = forge.context.candidate.candidate.commit.clone();
        forge.job(&candidate, failed(1, 11, NETWORK_FLAKE));
        forge.state.lock().unwrap().after_rerun = Some(rerun);

        let outcome = land_with(&forge, json!({"max_wait_seconds": 0}));

        assert_eq!(
            forge.sent("ci.rerun_failed"),
            vec![json!({"run_id": "1", "workspace_path": forge.repo.to_string_lossy()})]
        );
        assert!(forge.sent("pr.update_branch").is_empty());
        if lands {
            assert_eq!(outcome.expect("the rerun passes")["phase"], "landed");
        } else {
            outcome.expect_err("a second transient failure stops");
            assert_eq!(
                forge.steps(),
                vec![HandoffLandingStep::Stop { repairable: false }]
            );
            let reason: String = serde_json::from_str(&forge.last_evidence()).unwrap();
            assert!(
                reason.contains("classified `transient`") && reason.contains("one rerun"),
                "{reason}"
            );
        }
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Orbit Test",
            "-c",
            "user.email=test@orbit.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}
