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
//! Evidence that cannot be read decides none of those and spends nothing,
//! and a branch update is verified against the base as the provider merged
//! it, not as the owner last fetched it [ORB-15970].
//! A cached updated head cannot bypass a failed initial fetch [ORB-15976].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    log: FakeLog,
}

/// What a read of one job's log answers.
#[derive(Clone, Copy)]
enum FakeLog {
    Text(&'static str),
    /// No text, with the reason the log reader's per-job fallback gives.
    Unrecovered,
    /// The read itself fails.
    Unreadable,
}

/// The head a provider branch update leaves on the published branch.
#[derive(Clone, Copy, Default, Debug)]
enum UpdateShape {
    /// The candidate merged cleanly with the advanced base.
    #[default]
    CleanMerge,
    /// A clean merge with a commit the landing branch does not contain.
    Foreign,
    /// A merge whose tree is not the clean merge of its parents.
    Dirty,
    /// One parent, not a merge.
    NotAMerge,
    /// A clean merge, after which the landing branch cannot be fetched.
    BaseGone,
}

/// What the fake forge reports for one commit: its check's job, if any ran.
#[derive(Default)]
struct ForgeState {
    head: String,
    merged: bool,
    /// The `CI / clippy` job on each commit.
    jobs: HashMap<String, FakeJob>,
    /// Commits whose job query fails.
    unreadable: Vec<String>,
    /// What a rerun of a failed job leaves on the head.
    after_rerun: Option<FakeJob>,
    /// What the check reports on a head the branch update produced.
    after_update: Option<FakeJob>,
    /// The head a branch update on a published remote produces.
    update: UpdateShape,
    /// Provider observations for the bounded owner wait, independent of jobs.
    merge_states: std::collections::VecDeque<&'static str>,
    review_decision: Option<&'static str>,
}

/// A forge whose pull request #42 carries the claimed candidate, with one
/// `CI / clippy` check per commit. It answers the reads a landing makes,
/// reruns and updates the branch the way the provider does — an update
/// merges `main` into the head as a real commit — and records every
/// operation, so a test sees what the landing asked for. Once published, the
/// update runs on the remote, the way the provider merges its own base.
struct FakeForge {
    repo: PathBuf,
    remote: Option<PathBuf>,
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
            remote: None,
            state: Mutex::new(ForgeState {
                head: context.candidate.candidate.commit.clone(),
                ..ForgeState::default()
            }),
            context,
            ops: Mutex::default(),
            updates: Mutex::default(),
        }
    }

    /// Publish the owner checkout to a bare `origin` at `remote`, so the
    /// landing reads `origin/main` and fetches it.
    fn publish(&mut self, remote: &Path) {
        let repo = self.repo.to_string_lossy().into_owned();
        git(remote, &["clone", "-q", "--bare", &repo, "."]);
        git(
            &self.repo,
            &["remote", "add", "origin", &remote.to_string_lossy()],
        );
        git(&self.repo, &["fetch", "-q", "origin"]);
        self.remote = Some(remote.to_path_buf());
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
                let scripted_state = state.merge_states.front().copied();
                if state.merge_states.len() > 1 {
                    state.merge_states.pop_front();
                }
                let job = state.jobs.get(&state.head);
                let (status, conclusion) = match job.map(|job| job.conclusion) {
                    None => ("IN_PROGRESS", ""),
                    Some("success") => ("COMPLETED", "SUCCESS"),
                    Some(_) => ("COMPLETED", "FAILURE"),
                };
                let merge_state = scripted_state.unwrap_or(match conclusion {
                    "SUCCESS" => "CLEAN",
                    "" => "PENDING",
                    _ => "BLOCKED",
                });
                Ok(json!({"pull_request": {
                    "number": 42,
                    "state": if state.merged { "MERGED" } else { "OPEN" },
                    "headRefName": candidate.source_branch,
                    "baseRefName": candidate.base_branch,
                    "headRefOid": state.head,
                    "mergeStateStatus": merge_state,
                    "reviewDecision": state.review_decision.unwrap_or(""),
                    "mergeCommit": state.merged.then(|| json!({"oid": "provider-merge"})),
                    "statusCheckRollup": [{
                        "__typename": "CheckRun", "name": "clippy", "workflowName": "CI",
                        "status": status, "conclusion": conclusion,
                    }],
                }}))
            }
            "ci.commit_jobs" => {
                let commit = input["commit"].as_str().unwrap();
                if state
                    .unreadable
                    .iter()
                    .any(|unreadable| unreadable == commit)
                {
                    return Err(OrbitError::Execution(
                        "gh api failed: HTTP 401: Bad credentials".into(),
                    ));
                }
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
                match log {
                    FakeLog::Text(log) => {
                        Ok(json!({"log": log, "truncated": false, "fallback_error": null}))
                    }
                    FakeLog::Unrecovered => Ok(json!({
                        "log": "", "truncated": false,
                        "fallback_error": format!("job {job_id} (`clippy`) returned no log text"),
                    })),
                    FakeLog::Unreadable => Err(OrbitError::Execution(
                        "gh api job logs failed: HTTP 502".into(),
                    )),
                }
            }
            "ci.rerun_failed" => {
                let rerun = state.after_rerun.take().expect("a scripted rerun");
                let head = state.head.clone();
                state.jobs.insert(head, rerun);
                Ok(json!({}))
            }
            "pr.update_branch" => {
                assert_eq!(input["expected_head_sha"], state.head.as_str());
                let merge = match &self.remote {
                    Some(remote) => provider_update(remote, &state.head, state.update),
                    None => {
                        let tree = git(
                            &self.repo,
                            &["merge-tree", "--write-tree", &state.head, "main"],
                        );
                        git(
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
                        )
                    }
                };
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

/// The provider's branch update of `head` on the published `remote`, after
/// `main` there advanced past what the owner fetched, shaped by `shape`.
fn provider_update(remote: &Path, head: &str, shape: UpdateShape) -> String {
    let base_tree = git(remote, &["rev-parse", "main^{tree}"]);
    let advanced = git(
        remote,
        &[
            "commit-tree",
            &base_tree,
            "-p",
            "main",
            "-m",
            "the base advances",
        ],
    );
    git(remote, &["update-ref", "refs/heads/main", &advanced]);
    let base_side = match shape {
        UpdateShape::Foreign => git(
            remote,
            &[
                "commit-tree",
                &base_tree,
                "-p",
                &advanced,
                "-m",
                "not on main",
            ],
        ),
        _ => advanced,
    };
    let clean = git(remote, &["merge-tree", "--write-tree", head, &base_side]);
    let mut args = vec!["commit-tree".to_string()];
    match shape {
        UpdateShape::Dirty => args.push(git(remote, &["rev-parse", &format!("{head}^{{tree}}")])),
        _ => args.push(clean),
    }
    args.extend(["-p".to_string(), head.to_string()]);
    if !matches!(shape, UpdateShape::NotAMerge) {
        args.extend(["-p".to_string(), base_side]);
    }
    args.extend([
        "-m".to_string(),
        "Merge branch 'main' into orbit/T1".to_string(),
    ]);
    let merge = git(remote, &args.iter().map(String::as_str).collect::<Vec<_>>());
    git(remote, &["update-ref", "refs/heads/orbit/T1", &merge]);
    if matches!(shape, UpdateShape::BaseGone) {
        git(remote, &["update-ref", "-d", "refs/heads/main"]);
    }
    merge
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
    failed_reading(run_id, job_id, FakeLog::Text(log))
}

fn failed_reading(run_id: u64, job_id: u64, log: FakeLog) -> FakeJob {
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
        log: FakeLog::Text(""),
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
/// classified, instead of asking for a second repair. A base whose job query
/// succeeds with no jobs ran nothing to fail, which is not a failed read.
#[test]
fn a_candidate_caused_check_failure_gets_one_repair_then_stops_in_review() {
    for (repair_spent, base_ran, repairable) in [
        (false, true, true),
        (true, true, false),
        (false, false, true),
    ] {
        let sandbox = tempdir().unwrap();
        let mut forge = FakeForge::new(sandbox.path(), false);
        forge.context.automatic_repair_spent = repair_spent;
        let candidate = forge.context.candidate.candidate.commit.clone();
        let base = forge.context.candidate.base.commit.clone();
        forge.job(&candidate, failed(1, 11, CANDIDATE_ERROR));
        if base_ran {
            forge.job(&base, passed(2, 21));
        }

        let error = land_with(&forge, json!({"max_wait_seconds": 0}))
            .expect_err("the candidate's failure stops the landing");

        assert_eq!(
            forge.steps(),
            vec![HandoffLandingStep::Stop { repairable }],
            "repair spent: {repair_spent}, base ran: {base_ran}"
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

/// [ORB-15970] A classification needs every read it rests on. A failed job
/// query on the base or its moved tip, a base log that cannot be read or
/// recovered no text, and a log that names only its failed step decide
/// nothing: the landing stops in review, unrepaired, and spends no rerun,
/// update or repair. In every row the base branch has moved to a tip where
/// the check passes, so a misread would have updated the branch or repaired.
#[test]
fn a_check_failure_whose_evidence_cannot_be_read_spends_nothing() {
    use FakeLog::{Text, Unreadable, Unrecovered};
    for (case, head_log, base_log, base_readable, tip_readable, why) in [
        (
            "base jobs unreadable",
            Text(CLIPPY_DEAD_CODE),
            Some(Text(CLIPPY_DEAD_CODE)),
            false,
            true,
            "the jobs on base",
        ),
        (
            "current base jobs unreadable",
            Text(CANDIDATE_ERROR),
            None,
            true,
            false,
            "the jobs on the current base",
        ),
        (
            "base log unreadable",
            Text(CLIPPY_DEAD_CODE),
            Some(Unreadable),
            true,
            true,
            "HTTP 502",
        ),
        (
            "base log recovered no text",
            Text(CLIPPY_DEAD_CODE),
            Some(Unrecovered),
            true,
            true,
            "recovered no text",
        ),
        (
            "head log recovered no text",
            Unrecovered,
            Some(Text("")),
            true,
            true,
            "recovered no text",
        ),
        (
            "step names only",
            Text(""),
            Some(Text("")),
            true,
            true,
            "names no diagnostic",
        ),
    ] {
        let sandbox = tempdir().unwrap();
        let forge = FakeForge::new(sandbox.path(), true);
        let candidate = forge.context.candidate.candidate.commit.clone();
        let base = forge.context.candidate.base.commit.clone();
        std::fs::write(forge.repo.join("fix.txt"), "the base's fix\n").unwrap();
        git(&forge.repo, &["add", "fix.txt"]);
        git(&forge.repo, &["commit", "-q", "-m", "fix the base"]);
        let fixed = git(&forge.repo, &["rev-parse", "main"]);
        forge.job(&candidate, failed_reading(1, 11, head_log));
        forge.job(
            &base,
            base_log.map_or_else(|| passed(2, 21), |log| failed_reading(2, 21, log)),
        );
        forge.job(&fixed, passed(3, 31));
        {
            let mut state = forge.state.lock().unwrap();
            if !base_readable {
                state.unreadable.push(base);
            }
            if !tip_readable {
                state.unreadable.push(fixed);
            }
        }

        land_with(&forge, json!({"max_wait_seconds": 0}))
            .expect_err("unreadable evidence stops the landing");

        assert_eq!(
            forge.steps(),
            vec![HandoffLandingStep::Stop { repairable: false }],
            "{case}: no automatic repair"
        );
        let reason: String = serde_json::from_str(&forge.last_evidence()).unwrap();
        assert!(
            reason.contains(why) && reason.contains("stays in review"),
            "{case}: {reason}"
        );
        for spent in ["ci.rerun_failed", "pr.update_branch", "pr.merge"] {
            assert!(forge.sent(spent).is_empty(), "{case}: sent {spent}");
        }
    }
}

/// [ORB-15970] The provider merges the base as it is when the branch update
/// runs, which may be past the landing ref the owner fetched when it
/// classified the failure. A clean two-parent merge of the candidate with
/// that newer base is verified against a freshly fetched landing ref and
/// lands; a merge with a commit off the landing branch, one whose tree is
/// not the clean merge, a head that is not a merge, and a landing branch
/// that cannot be fetched are still refused.
#[test]
fn a_branch_update_is_verified_against_the_base_it_merged() {
    for (shape, lands) in [
        (UpdateShape::CleanMerge, true),
        (UpdateShape::Foreign, false),
        (UpdateShape::Dirty, false),
        (UpdateShape::NotAMerge, false),
        (UpdateShape::BaseGone, false),
    ] {
        let sandbox = tempdir().unwrap();
        let remote = tempdir().unwrap();
        let mut forge = FakeForge::new(sandbox.path(), true);
        let candidate = forge.context.candidate.candidate.commit.clone();
        let base = forge.context.candidate.base.commit.clone();
        std::fs::write(forge.repo.join("fix.txt"), "the base's fix\n").unwrap();
        git(&forge.repo, &["add", "fix.txt"]);
        git(&forge.repo, &["commit", "-q", "-m", "fix the base"]);
        let fixed = git(&forge.repo, &["rev-parse", "main"]);
        forge.publish(remote.path());
        forge.job(&candidate, failed(1, 11, CLIPPY_DEAD_CODE));
        forge.job(&base, failed(2, 21, CLIPPY_DEAD_CODE));
        forge.job(&fixed, passed(3, 31));
        {
            let mut state = forge.state.lock().unwrap();
            state.after_update = Some(passed(4, 41));
            state.update = shape;
        }

        let outcome = land_with(&forge, json!({"max_wait_seconds": 0}));

        assert_eq!(forge.sent("pr.update_branch").len(), 1, "{shape:?}");
        let head = forge.state.lock().unwrap().head.clone();
        if lands {
            assert_eq!(
                outcome.expect("the verified update lands")["phase"],
                "landed",
                "{shape:?}"
            );
            assert_eq!(
                forge.sent("pr.merge")[0]["reviewed_head_sha"],
                head.as_str()
            );
        } else {
            outcome.expect_err("an unverified head stops the landing");
            assert_eq!(
                forge.steps(),
                vec![HandoffLandingStep::Stop { repairable: false }],
                "{shape:?}"
            );
            assert!(forge.sent("pr.merge").is_empty(), "{shape:?}");
        }
    }
}

/// [ORB-15976] An earlier provider update and its base are already cached
/// locally, but origin becomes unreachable before landing starts. Cached
/// containment cannot authorize adoption: stop without spending recovery.
/// Restoring origin with the same objects lets the same head land.
#[test]
fn a_cached_branch_update_requires_a_fresh_landing_base() {
    super::git_fixture::isolated(
        module_path!(),
        "a_cached_branch_update_requires_a_fresh_landing_base",
        || {
            let sandbox = tempdir().unwrap();
            let remote = tempdir().unwrap();
            let mut forge = FakeForge::new(sandbox.path(), true);
            let candidate = forge.context.candidate.candidate.commit.clone();
            forge.publish(remote.path());
            let head = provider_update(remote.path(), &candidate, UpdateShape::CleanMerge);
            git(&forge.repo, &["fetch", "-q", "origin"]);
            let base = git(&forge.repo, &["rev-parse", "origin/main"]);
            assert_eq!(
                git(&forge.repo, &["rev-list", "--parents", "-n", "1", &head]),
                format!("{head} {candidate} {base}"),
                "the genuine updated head and its base are cached before fetch fails"
            );
            forge.state.lock().unwrap().head = head.clone();
            forge.job(&head, passed(1, 11));
            let unreachable = remote.path().join("unreachable.git");
            git(
                &forge.repo,
                &[
                    "remote",
                    "set-url",
                    "origin",
                    &unreachable.to_string_lossy(),
                ],
            );

            land_with(&forge, json!({"max_wait_seconds": 0}))
                .expect_err("a cached branch update cannot land after the initial fetch fails");

            assert_eq!(
                forge.steps(),
                vec![HandoffLandingStep::Stop { repairable: false }]
            );
            for operation in ["ci.rerun_failed", "pr.update_branch", "pr.merge"] {
                assert!(
                    forge.sent(operation).is_empty(),
                    "unreachable origin sent {operation}"
                );
            }
            assert_eq!(git(&forge.repo, &["rev-parse", "origin/main"]), base);
            assert_eq!(forge.state.lock().unwrap().head, head);

            git(
                &forge.repo,
                &[
                    "remote",
                    "set-url",
                    "origin",
                    &remote.path().to_string_lossy(),
                ],
            );
            forge.ops.lock().unwrap().clear();
            forge.updates.lock().unwrap().clear();

            let outcome = land_with(&forge, json!({"max_wait_seconds": 0}))
                .expect("the identical cached head lands once origin is reachable");

            assert_eq!(outcome["phase"], "landed");
            assert_eq!(forge.sent("pr.merge").len(), 1);
            assert_eq!(
                forge.sent("pr.merge")[0]["reviewed_head_sha"],
                head.as_str()
            );
            assert!(forge.sent("ci.rerun_failed").is_empty());
            assert!(forge.sent("pr.update_branch").is_empty());
            assert_eq!(git(&forge.repo, &["rev-parse", "origin/main"]), base);
            let evidence: Value = serde_json::from_str(&forge.last_evidence()).unwrap();
            assert_eq!(evidence["updated_head"], head.as_str());
            assert!(evidence.get("check_recovery").is_none());
            assert_eq!(forge.steps().last(), Some(&HandoffLandingStep::Complete));
        },
    );
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = crate::git_test_support::command()
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

/// Behavior-distinct rows share one bounded family process.
#[test]
fn handoff_landing_contracts_1() {
    crate::git_fixture::isolated(module_path!(), "handoff_landing_contracts_1", || {
        crate::git_fixture::run_scenarios(&[
            (
                "a_local_candidate_landing_names_its_task_before_the_branch_moves",
                a_local_candidate_landing_names_its_task_before_the_branch_moves,
            ),
            (
                "a_candidate_that_cannot_fast_forward_records_no_landing_intent",
                a_candidate_that_cannot_fast_forward_records_no_landing_intent,
            ),
            (
                "a_mismatched_merged_pr_resolves_the_intent_before_stopping_later_attempts",
                a_mismatched_merged_pr_resolves_the_intent_before_stopping_later_attempts,
            ),
        ]);
    });
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn handoff_landing_contracts_2() {
    crate::git_fixture::isolated(module_path!(), "handoff_landing_contracts_2", || {
        crate::git_fixture::run_scenarios(&[
            (
                "only_a_conflicting_or_stale_base_stops_a_pull_request_for_repair",
                only_a_conflicting_or_stale_base_stops_a_pull_request_for_repair,
            ),
            (
                "an_owed_hosted_check_must_pass_before_the_owner_merges",
                an_owed_hosted_check_must_pass_before_the_owner_merges,
            ),
        ]);
    });
}

/// The public landing action keeps UNKNOWN/PENDING bounded and never extends
/// that wait to a real review or administrative refusal.
#[test]
fn owner_landing_waits_for_unknown_then_clean_and_retains_expired_or_blocked_evidence() {
    crate::git_fixture::isolated(
        module_path!(),
        "owner_landing_waits_for_unknown_then_clean_and_retains_expired_or_blocked_evidence",
        || {
            for (case, states, review) in [
                ("settles", vec!["UNKNOWN", "PENDING", "CLEAN"], ""),
                ("expires", vec!["UNKNOWN"], ""),
                ("review", vec!["BLOCKED"], "REVIEW_REQUIRED"),
                ("administrative", vec!["BLOCKED"], ""),
            ] {
                let sandbox = tempdir().unwrap();
                let forge = FakeForge::new(sandbox.path(), false);
                forge.job(&forge.context.candidate.candidate.commit, passed(1, 11));
                {
                    let mut state = forge.state.lock().unwrap();
                    state.merge_states = states.into();
                    state.review_decision = Some(review);
                }
                let result = land_with(
                    &forge,
                    json!({"max_wait_seconds": if case == "settles" {10} else {0}, "poll_interval_seconds":5}),
                );
                if case == "settles" {
                    assert_eq!(result.unwrap()["phase"], "landed");
                    assert_eq!(forge.sent("pr.merge").len(), 1);
                    assert_eq!(
                        forge.sent("pr.status").len(),
                        4,
                        "bounded observations settle and verify the one merge"
                    );
                } else {
                    assert!(result.is_err(), "{case}");
                    assert_eq!(
                        forge.sent("pr.status").len(),
                        1,
                        "no administrative wait expansion: {case}"
                    );
                    assert!(forge.sent("pr.merge").is_empty(), "{case}");
                    assert_eq!(
                        forge.steps(),
                        [HandoffLandingStep::Stop { repairable: false }],
                        "durable refusal without repair: {case}"
                    );
                    assert!(
                        !forge.last_evidence().is_empty(),
                        "expired/refused observations retain evidence"
                    );
                }
            }
        },
    );
}
