//! The owner's landing of a claimed candidate.
//!
//! Direct fast-forward landings retain a landing intent naming the handoff and
//! task before `main` moves; a diverged branch records none [ORB-13894]. PR
//! reconciliation settles external state before stopping mismatches. Conflicting
//! or stale bases stop repairable [ORB-14261]. Owed hosted checks gate merging
//! on explicit success [ORB-15677] [ORB-15812]. Failed checks classify from job
//! logs (inherited -> update branch, surviving on green tip -> one repair
//! [ORB-16322], transient -> retry budget, candidate -> requeue [ORB-15956]);
//! unreadable evidence spends nothing, branch updates verify against the merged
//! base [ORB-15970], and cached heads require a fresh base fetch [ORB-15976].

mod log_read;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use orbit_common::{Env, OrbitError};
use orbit_engine::test_support::{NullRuntimeHost, with_prerequisite_clock, with_vcs_retry_wait};
use orbit_engine::{
    BaseSyncMode, CandidateCheckout, DispatchError, HandoffLandingContext, HandoffLandingStep,
    HandoffLandingUpdate, PrConfig, RuntimeHost, TaskAutomationUpdate, dispatch_error_to_orbit,
    execute_deterministic_action, execute_job_with_resume, observe_candidate,
};
use orbit_engine::{ReviewGate, RunLifecycle};
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::automation::DirectLandingRequest;
use orbit_types::workflow::handoff::{
    HandoffArtifactRef, HandoffDelivery, LANDING_WAITING_FOR_BASE_EVENT,
};
use orbit_types::workflow::{
    CI_BASE_SUSPECT_RECLASSIFIED_EVENT, CI_FAILURE_RECLASSIFIED_EVENT, CI_FAILURE_RECORDED_EVENT,
    CiBaseSuspectReclassification, CiFailureClass, CiInfrastructureRetryPolicy, CiRerunAdmission,
    CiRerunPlan, DeliveryRefusal, HeldFailure, JobRunState, PR_BASE_UPDATE_RESERVED_EVENT,
    PrBaseUpdateAdmission, PrBaseUpdateReservation, ci_failure_ledger, decide_ci_rerun_with_policy,
    decide_pr_base_update,
};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use HandoffLandingStep::{Complete, PublishIntent, ResolveIntent, Stop, StopForRequeue};

macro_rules! scenario_family {
    ($(#[$attr:meta])* $name:ident: $($row:ident),+ $(,)?) => {
        $(#[$attr])*
        #[test]
        fn $name() {
            crate::git_fixture::isolated(module_path!(), stringify!($name), || {
                crate::git_fixture::run_scenarios(&[$((stringify!($row), $row as fn())),+]);
            });
        }
    };
}

const TASK_ID: &str = "T1";
const RUN_ID: &str = "jrun-landing-test";
const STOP: HandoffLandingStep = Stop { repairable: false };
const NO_RERUN_UPDATE_OR_MERGE: &[&str] = &["ci.rerun_failed", "pr.update_branch", "pr.merge"];
const NO_RERUN_OR_UPDATE: &[&str] = &["ci.rerun_failed", "pr.update_branch"];
const NO_MERGE_OR_UPDATE: &[&str] = &["pr.merge", "pr.update_branch"];

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
const UNTOUCHED_TEST_FAILURE: &str = "error: shutdown test timed out\n --> README.md:1:1\n";
const RETRY_UNREADABLE: &str =
    "workflow prerequisite evidence is unavailable (retry requires a completed failed attempt)";

struct LandingHost {
    repo: PathBuf,
    context: HandoffLandingContext,
    steps: Mutex<Vec<HandoffLandingStep>>,
    intents: Mutex<Vec<(DirectLandingRequest, String)>>,
}

impl LandingHost {
    fn last_step(&self) -> Option<HandoffLandingStep> {
        self.steps.lock().unwrap().last().cloned()
    }
}

impl NullRuntimeHost for LandingHost {
    fn run_settings(&self) -> orbit_engine::RunSettings {
        orbit_engine::RunSettings {
            repo_root: Some(self.repo.to_string_lossy().into_owned()),
            ..orbit_engine::RunSettings::default()
        }
    }
    fn handoff_landing_context(&self, _: &str) -> Result<HandoffLandingContext, OrbitError> {
        Ok(self.context.clone())
    }
    fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Result<(), OrbitError> {
        self.steps.lock().unwrap().push(update.step.clone());
        Ok(())
    }
    fn record_direct_landing_intent(&self, req: &DirectLandingRequest) -> Result<(), OrbitError> {
        let tip = git_rev(&self.repo, "main");
        self.intents.lock().unwrap().push((req.clone(), tip));
        Ok(())
    }
}

/// Rechecked stale NoDiff/already-landed evidence stops and holds in review [ORB-25394].
struct StaleEvidenceHost {
    context: HandoffLandingContext,
    steps: Mutex<Vec<HandoffLandingStep>>,
    stops: Mutex<Vec<String>>,
}

impl NullRuntimeHost for StaleEvidenceHost {
    fn handoff_landing_context(&self, _: &str) -> Result<HandoffLandingContext, OrbitError> {
        Ok(self.context.clone())
    }
    fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Result<(), OrbitError> {
        if matches!(update.step, Complete) {
            return Err(OrbitError::DeliveryRefused {
                refusal: DeliveryRefusal::EvidenceStale,
                message: "already_landed_unverified: task scope changed at scope.relations; \
                          revalidate the current requirements, selectors and comments"
                    .into(),
            });
        }
        if matches!(update.step, Stop { .. }) {
            self.stops.lock().unwrap().push(update.evidence.clone());
        }
        self.steps.lock().unwrap().push(update.step.clone());
        Ok(())
    }
}

/// Journal guards: Stop needs a resolved intent; settled attempts reject further writes.
struct ReconciliationHost {
    context: Mutex<HandoffLandingContext>,
    updates: Mutex<Vec<HandoffLandingUpdate>>,
    status: Value,
}

impl NullRuntimeHost for ReconciliationHost {
    fn handoff_landing_context(&self, _: &str) -> Result<HandoffLandingContext, OrbitError> {
        Ok(self.context.lock().unwrap().clone())
    }
    fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Result<(), OrbitError> {
        let mut updates = self.updates.lock().unwrap();
        if updates
            .last()
            .is_some_and(|u| matches!(u.step, Stop { .. }))
        {
            return Err(OrbitError::InvalidInput(
                "landing attempt is already settled".into(),
            ));
        }
        let mut ctx = self.context.lock().unwrap();
        match &update.step {
            ResolveIntent { intent_id, .. } => {
                assert_eq!(ctx.unresolved_merge_intent.as_ref(), Some(intent_id));
                ctx.unresolved_merge_intent = None;
            }
            Stop { .. } => assert!(
                ctx.unresolved_merge_intent.is_none(),
                "Stop follows resolution"
            ),
            other => panic!("a mismatched reconciled PR must not reach {other:?}"),
        }
        updates.push(update.clone());
        Ok(())
    }
    fn run_private_vcs_operation(&self, op: &str, _: Value) -> Result<Value, OrbitError> {
        assert_eq!(
            op, "pr.status",
            "reconciliation must not send another merge"
        );
        Ok(json!({"pull_request": self.status}))
    }
}

/// An open PR fixture with fixed merge state and rollup; records any non-read op.
struct OpenPullRequestHost {
    _sandbox: TempDir,
    context: HandoffLandingContext,
    steps: Mutex<Vec<HandoffLandingStep>>,
    merge_state: &'static str,
    rollup: Value,
    review_decision: &'static str,
    sent: Mutex<Vec<String>>,
}

impl OpenPullRequestHost {
    fn steps(&self) -> Vec<HandoffLandingStep> {
        self.steps.lock().unwrap().clone()
    }
    fn sent_ops(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
    fn new(merge_state: &'static str, rollup: Value) -> Self {
        let sandbox = tempdir().unwrap();
        let context = pr_context(sandbox.path());
        Self {
            _sandbox: sandbox,
            context,
            steps: Mutex::default(),
            merge_state,
            rollup,
            review_decision: "",
            sent: Mutex::default(),
        }
    }
}

impl NullRuntimeHost for OpenPullRequestHost {
    fn handoff_landing_context(&self, _: &str) -> Result<HandoffLandingContext, OrbitError> {
        Ok(self.context.clone())
    }
    fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Result<(), OrbitError> {
        self.steps.lock().unwrap().push(update.step.clone());
        Ok(())
    }
    fn run_private_vcs_operation(&self, op: &str, _: Value) -> Result<Value, OrbitError> {
        let c = &self.context.candidate;
        let head = &c.candidate.commit;
        match op {
            "ci.commit_jobs" => Ok(json!({"jobs": []})),
            "pr.required_checks" => Ok(
                json!({"pull_request":{"headRefOid":head,"commits":{"nodes":[{"commit":{"oid":head,"statusCheckRollup":null}}]}}}),
            ),
            "pr.status" => Ok(
                json!({"pull_request":{"state":"OPEN","headRefName":c.source_branch,"baseRefName":c.base_branch,"headRefOid":head,"mergeStateStatus":self.merge_state,"reviewDecision":self.review_decision,"statusCheckRollup":self.rollup}}),
            ),
            _ => {
                self.sent.lock().unwrap().push(op.to_string());
                Err(OrbitError::Execution(format!("fixture sends no {op}")))
            }
        }
    }
}

struct FakeJob {
    run_id: u64,
    job_id: u64,
    conclusion: &'static str,
    log: FakeLog,
    run_attempt: u64,
    event: &'static str,
}

#[derive(Clone, Copy)]
enum FakeLog {
    Text(&'static str),
    Unrecovered,
    Unreadable,
}

#[derive(Clone, Copy, Default, Debug)]
enum UpdateShape {
    #[default]
    CleanMerge,
    Foreign,
    Dirty,
    NotAMerge,
    BaseGone,
}

#[derive(Default)]
struct ForgeState {
    head: String,
    merged: bool,
    merge_in_progress: Option<bool>,
    merge_commit: Option<String>,
    jobs: HashMap<String, FakeJob>,
    job_times: HashMap<u64, (String, String)>,
    base_push_provider: bool,
    real_rerun_plan: bool,
    unreadable: Vec<String>,
    log_timeouts: HashMap<u64, u64>,
    after_rerun: Option<FakeJob>,
    rerun_plan_error: Option<&'static str>,
    queued_rerun: bool,
    after_update: Option<FakeJob>,
    update: UpdateShape,
    merge_states: VecDeque<&'static str>,
    review_decision: Option<&'static str>,
    /// [ORB-16185] Failed required `CodeQL` aggregate whose annotation names this file.
    aggregate: Option<&'static str>,
    after_recovery_aggregate: Option<&'static str>,
    extra_unanswered_check: bool,
    observations: VecDeque<Value>,
    policies: VecDeque<Value>,
    gates: VecDeque<Value>,
}

/// PR #42 fake forge with one `CI / clippy` check per commit and real Git updates.
struct FakeForge {
    repo: PathBuf,
    remote: Option<PathBuf>,
    context: HandoffLandingContext,
    state: Mutex<ForgeState>,
    ops: Mutex<Vec<(String, Value)>>,
    updates: Mutex<Vec<HandoffLandingUpdate>>,
    history: Mutex<Vec<TaskHistoryEntry>>,
    ci_retry_policy: CiInfrastructureRetryPolicy,
}

impl FakeForge {
    fn new(sandbox: &Path, owed: bool) -> Self {
        let mut ctx = pr_context(sandbox);
        if owed {
            ctx.owed_hosted_checks = vec!["CI / clippy".into()];
        }
        let head = ctx.candidate.candidate.commit.clone();
        Self {
            repo: ctx.workspace_path.clone(),
            remote: None,
            context: ctx,
            state: Mutex::new(ForgeState {
                head,
                ..ForgeState::default()
            }),
            ops: Mutex::default(),
            updates: Mutex::default(),
            history: Mutex::default(),
            ci_retry_policy: Default::default(),
        }
    }

    fn with_state<R>(&self, f: impl FnOnce(&mut ForgeState) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }
    fn set_after_rerun(&self, job: FakeJob) {
        self.with_state(|s| s.after_rerun = Some(job));
    }
    fn set_after_update(&self, job: FakeJob) {
        self.with_state(|s| s.after_update = Some(job));
    }
    fn publish(&mut self, remote: &Path) {
        let (repo, origin) = (
            self.repo.to_string_lossy().into_owned(),
            remote.to_string_lossy(),
        );
        git(remote, &["clone", "-q", "--bare", &repo, "."]);
        git(&self.repo, &["remote", "add", "origin", &origin]);
        git(&self.repo, &["fetch", "-q", "origin"]);
        self.remote = Some(remote.to_path_buf());
    }
    fn job(&self, commit: &str, job: FakeJob) {
        self.with_state(|s| {
            s.jobs.insert(commit.to_string(), job);
        });
    }
    fn sent(&self, op: &str) -> Vec<Value> {
        self.ops
            .lock()
            .unwrap()
            .iter()
            .filter(|(s, _)| s == op)
            .map(|(_, v)| v.clone())
            .collect()
    }
    fn steps(&self) -> Vec<HandoffLandingStep> {
        self.updates
            .lock()
            .unwrap()
            .iter()
            .map(|u| u.step.clone())
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
    fn stop_reason(&self) -> String {
        evidence_of(self)["reason"]["value"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn history_count(&self, event: &str) -> usize {
        self.history
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.event == event)
            .count()
    }
    fn history_notes(&self, event: &str) -> Vec<Value> {
        let hist = self.history.lock().unwrap();
        hist.iter()
            .filter(|r| r.event == event)
            .map(|r| serde_json::from_str(r.note.as_deref().unwrap()).unwrap())
            .collect()
    }
    fn push_history(&self, row: TaskHistoryEntry) {
        self.history.lock().unwrap().push(row);
    }
    fn assert_sent(&self, op: &str, expected: usize, label: &str) {
        assert_eq!(self.sent(op).len(), expected, "{label}: {op}");
    }
    fn assert_none(&self, ops: &[&str], label: &str) {
        assert_not_sent(self, ops, label);
    }
    fn assert_step(&self, step: HandoffLandingStep, label: &str) {
        assert_steps(&self.steps(), &[step], label);
    }
    fn assert_reason(&self, label: &str, parts: &[&str]) {
        assert_contains(&self.stop_reason(), parts, label);
    }
}

impl NullRuntimeHost for FakeForge {
    fn run_settings(&self) -> orbit_engine::RunSettings {
        orbit_engine::RunSettings {
            pr_config: PrConfig {
                ci_infrastructure_retry: self.ci_retry_policy,
                ..Default::default()
            },
            ..orbit_engine::RunSettings::default()
        }
    }
    fn apply_task_automation_update(
        &self,
        _: &str,
        _: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        let msg = "active execution claim requires a claim-scoped mutation";
        Err(OrbitError::InvalidInput(msg.into()))
    }
    fn admit_ci_rerun(
        &self,
        _: &[String],
        _: Option<&str>,
        plan: &CiRerunPlan,
        now_ms: u64,
    ) -> Result<CiRerunAdmission, OrbitError> {
        let mut h = self.history.lock().unwrap();
        let d = decide_ci_rerun_with_policy(&h, plan, self.ci_retry_policy, now_ms);
        if matches!(d, CiRerunAdmission::Reserved) {
            let ts = chrono::DateTime::from_timestamp_millis(now_ms as i64).unwrap();
            h.push(plan.reservation_entry(ts));
        }
        Ok(d)
    }
    fn admit_handoff_base_update(
        &self,
        _: &str,
        req: &PrBaseUpdateReservation,
    ) -> Result<PrBaseUpdateAdmission, OrbitError> {
        ReviewGate::admit_pr_base_update(self, &[TASK_ID.into()], req)
    }
    fn get_task_history(&self, _: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        Ok(self.history.lock().unwrap().clone())
    }
    fn admit_pr_base_update(
        &self,
        _: &[String],
        req: &PrBaseUpdateReservation,
    ) -> Result<PrBaseUpdateAdmission, OrbitError> {
        let mut h = self.history.lock().unwrap();
        let d = decide_pr_base_update(&h, req);
        if matches!(d, PrBaseUpdateAdmission::Reserved) {
            let note = Some(serde_json::to_string(req).unwrap());
            h.push(history_row("owner", PR_BASE_UPDATE_RESERVED_EVENT, note));
        }
        Ok(d)
    }
    fn handoff_landing_context(&self, _: &str) -> Result<HandoffLandingContext, OrbitError> {
        Ok(self.context.clone())
    }
    fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Result<(), OrbitError> {
        self.updates.lock().unwrap().push(update.clone());
        Ok(())
    }
    fn record_handoff_landing_wait(&self, _: &str, reason: &str) -> Result<(), OrbitError> {
        let row = history_row("owner", LANDING_WAITING_FOR_BASE_EVENT, Some(reason.into()));
        self.push_history(row);
        Ok(())
    }
    fn record_handoff_landing_history(
        &self,
        id: &str,
        ev: &str,
        note: &str,
    ) -> Result<(), OrbitError> {
        let allowed = matches!(
            ev,
            "ci_check_rerun"
                | "ci_base_suspect"
                | "ci_infrastructure_retry_backoff"
                | "ci_infrastructure_retry_exhausted"
                | CI_FAILURE_RECORDED_EVENT
                | CI_FAILURE_RECLASSIFIED_EVENT
                | CI_BASE_SUSPECT_RECLASSIFIED_EVENT
        );
        if id != self.context.handoff_id || !allowed {
            let msg = "current handoff claim does not authorize this history event";
            return Err(OrbitError::InvalidInput(msg.into()));
        }
        self.push_history(history_row("system", ev, Some(note.into())));
        Ok(())
    }
    fn run_private_vcs_operation(&self, op: &str, input: Value) -> Result<Value, OrbitError> {
        self.ops
            .lock()
            .unwrap()
            .push((op.to_string(), input.clone()));
        let mut s = self.state.lock().unwrap();
        let c = &self.context.candidate;
        match op {
            "ci.rerun_plan" => {
                if s.real_rerun_plan {
                    drop(s);
                    return orbit_engine::run_private_vcs_operation(&Env::capture(), op, &input);
                }
                if let Some(err) = s.rerun_plan_error {
                    return Err(OrbitError::Execution(err.into()));
                }
                let job = s.jobs.get(&s.head).unwrap();
                if job.event != "pull_request" {
                    return Err(OrbitError::Execution("foreign retry event".into()));
                }
                let v = json!({"plan":CiRerunPlan{schema_version:1,repository:c.repository.clone(),repository_id:1,pr_number:"42".into(),head_sha:s.head.clone(),head_branch:c.source_branch.clone(),base_branch:c.base_branch.clone(),accepted_base:c.base.commit.clone(),run_id:job.run_id,run_attempt:job.run_attempt,workflow_id:1,roots:serde_json::from_value(input["roots"].clone()).unwrap(),failed_jobs:vec![json!({"id":job.job_id,"name":"clippy","started_at":"2026-10-26T14:00:00Z","completed_at":"2026-10-26T14:01:00Z"})],authentication:json!({})}});
                Ok(v)
            }
            "ci.current_gates" => {
                if !s.gates.is_empty() {
                    return pop_or_auth(&mut s.gates);
                }
                Ok(json!({"gates_passed":true,"required":[],"missing":[]}))
            }
            "ci.emission_evidence" => Ok(json!({"evidence":null})),
            "pr.status" => {
                let scripted =
                    (!s.merge_states.is_empty()).then(|| pop_keep_last(&mut s.merge_states));
                let (status, conc) = match s.jobs.get(&s.head).map(|j| j.conclusion) {
                    None if s.queued_rerun => ("COMPLETED", "FAILURE"),
                    None => ("IN_PROGRESS", ""),
                    Some("success") => ("COMPLETED", "SUCCESS"),
                    Some(_) => ("COMPLETED", "FAILURE"),
                };
                let ms = scripted.unwrap_or(match conc {
                    _ if s.aggregate.is_some() => "BLOCKED",
                    "SUCCESS" => "CLEAN",
                    "" => "PENDING",
                    _ => "BLOCKED",
                });
                let mut rollup = vec![check_run("clippy", "CI", status, conc)];
                if s.aggregate.is_some() {
                    rollup.push(check_run("CodeQL", "", "COMPLETED", "FAILURE"));
                }
                if s.extra_unanswered_check {
                    rollup.push(check_run("optional", "Extra", "COMPLETED", "FAILURE"));
                }
                let mut pr = json!({"number":42,"url":"https://github.com/owner/repository/pull/42","state":pick(s.merged,"MERGED","OPEN"),"headRefName":c.source_branch,"baseRefName":c.base_branch,"headRefOid":s.head,"mergeStateStatus":ms,"reviewDecision":s.review_decision.unwrap_or(""),"mergeCommit":s.merged.then(|| json!({"oid":s.merge_commit.as_deref().unwrap_or("provider-merge")})),"statusCheckRollup":rollup});
                if !s.merged && !s.observations.is_empty() {
                    let patch = s.observations.front().unwrap().as_object().unwrap().clone();
                    if let Some(err) = patch.get("error").and_then(Value::as_str) {
                        return Err(OrbitError::Execution(err.into()));
                    }
                    pr.as_object_mut().unwrap().extend(patch);
                    if s.observations.len() > 1 {
                        s.observations.pop_front();
                    }
                }
                Ok(json!({"pull_request": pr}))
            }
            "pr.required_checks" => {
                if !s.policies.is_empty() {
                    return pop_or_auth(&mut s.policies);
                }
                let st = pick(s.jobs.contains_key(&s.head), "COMPLETED", "IN_PROGRESS");
                let mut policy = pending_policy(&s.head, st, true);
                if s.aggregate.is_some() {
                    policy_nodes(&mut policy).as_array_mut().unwrap().push(json!({"__typename":"CheckRun","name":"CodeQL","status":"COMPLETED","conclusion":"FAILURE","isRequired":true,"databaseId":7001,"checkSuite":{"app":{"databaseId":57789},"workflowRun":null}}));
                }
                Ok(policy)
            }
            "ci.check_annotations" => {
                let path = s.aggregate.expect("an aggregate was scripted");
                let got = (&input["check_run_id"], &input["app_id"], &input["head_sha"]);
                assert_eq!(got, (&json!(7001), &json!(57789), &json!(s.head)));
                let ann = json!({"path":path,"start_line":1,"end_line":1,"blob_href":format!("https://github.com/owner/repository/blob/{}/{path}", s.head),"annotation_level":"failure","title":"Uncontrolled data used in path expression","message":"This path depends on a user-provided value."});
                let v = json!({"repository":"owner/repository","pr":42,"head_sha":s.head,"check_run":{"id":7001,"name":"CodeQL","app_id":57789,"conclusion":"failure","annotations_count":1},"annotations":[ann]});
                Ok(v)
            }
            "ci.commit_jobs" => {
                if s.base_push_provider && input["base_push_tip"].is_string() {
                    return orbit_engine::run_private_vcs_operation(&Env::capture(), op, &input);
                }
                let commit = input["commit"].as_str().unwrap();
                if s.unreadable.iter().any(|bad| bad == commit) {
                    let msg = "gh api failed: HTTP 401: Bad credentials";
                    return Err(OrbitError::Execution(msg.into()));
                }
                let jobs: Vec<Value> = s.jobs.iter().filter(|(sha, _)| match input["base_push_tip"].as_str() {
                    Some(tip) => git(&self.repo, &["merge-base", commit, sha]) == commit && git(&self.repo, &["merge-base", sha, tip]) == **sha,
                    None => *sha == commit,
                }).map(|(sha, j)| {
                    let mut v = json!({"run_id":j.run_id,"job_id":j.job_id,"workflow":"CI","workflow_id":1,"started_at":"2026-10-26T14:00:00Z","completed_at":"2026-10-26T14:01:00Z","name":"clippy","status":"completed","conclusion":j.conclusion,"failed_step":(j.conclusion=="failure").then_some("Run clippy"),"run_attempt":j.run_attempt,"event":j.event,"pull_requests":[{"number":42}],"pull_request_numbers":[42],"head_sha":sha,"head_branch":c.source_branch,"repository_id":1,"repository":c.repository});
                    if let Some((start, end)) = s.job_times.get(&j.job_id) {
                        (v["started_at"], v["completed_at"], v["run_started_at"], v["run_status"]) = (json!(start), json!(end), json!(start), json!("completed"));
                    }
                    v
                }).collect();
                Ok(json!({"commit": commit, "jobs": jobs}))
            }
            "ci.job_log" => {
                let job_id: u64 = input["job_id"].as_str().unwrap().parse().unwrap();
                if let Some(rem) = s.log_timeouts.get_mut(&job_id)
                    && *rem > 0
                {
                    *rem -= 1;
                    let v = json!({"log":"","fallback_error":"process timed out after 15000ms: gh run view --log"});
                    return Ok(v);
                }
                let log = s
                    .jobs
                    .values()
                    .find(|j| j.job_id == job_id)
                    .expect("log")
                    .log;
                match log {
                    FakeLog::Text(log) => {
                        Ok(json!({"log":log,"truncated":false,"fallback_error":null}))
                    }
                    FakeLog::Unrecovered => {
                        let v = json!({"log":"","truncated":false,"fallback_error":format!("job {job_id} (`clippy`) returned no log text")});
                        Ok(v)
                    }
                    FakeLog::Unreadable => Err(OrbitError::Execution(
                        "gh api job logs failed: HTTP 502".into(),
                    )),
                }
            }
            "ci.rerun_failed" => {
                let head = s.head.clone();
                if s.queued_rerun {
                    s.jobs.remove(&head);
                } else {
                    let mut rerun = s.after_rerun.take().expect("a scripted rerun");
                    let orig = s.jobs.get(&head).unwrap();
                    (rerun.run_attempt, rerun.event) = (orig.run_attempt + 1, orig.event);
                    s.job_times.insert(rerun.job_id, (at("14:02"), at("14:03")));
                    s.jobs.insert(head, rerun);
                }
                s.aggregate = s.after_recovery_aggregate.take();
                Ok(json!({}))
            }
            "pr.update_branch" => {
                assert_eq!(input["expected_head_sha"], s.head.as_str());
                let merge = match &self.remote {
                    Some(remote) => provider_update(remote, &s.head, s.update),
                    None => {
                        let t = git(&self.repo, &["merge-tree", "--write-tree", &s.head, "main"]);
                        let msg = "Merge branch 'main' into orbit/T1";
                        commit_tree(&self.repo, &t, &[&s.head, "main"], msg)
                    }
                };
                let after = s.after_update.take().expect("a scripted update");
                s.jobs.insert(merge.clone(), after);
                s.head = merge;
                s.aggregate = s.after_recovery_aggregate.take();
                Ok(json!({}))
            }
            "pr.merge_capabilities" => {
                let v = json!({"repository":{"name_with_owner":c.repository,"base_branch":c.base_branch,"allow_squash_merge":true,"allow_rebase_merge":false,"allow_merge_commit":false,"allow_auto_merge":false,"requires_linear_history":false}});
                Ok(v)
            }
            "pr.merge" => {
                assert_eq!(input["reviewed_head_sha"], s.head.as_str());
                if let Some(merged) = s.merge_in_progress {
                    s.merged = merged;
                    let v = json!({"merged":false,"refusal":"merge_in_progress","stderr":"gh: Merge already in progress (HTTP 405)"});
                    return Ok(v);
                }
                s.merged = true;
                Ok(json!({"landed_commit": "provider-merge"}))
            }
            other => panic!("the landing sent an unexpected {other}"),
        }
    }
}

fn pick<T>(cond: bool, t: T, f: T) -> T {
    if cond { t } else { f }
}

fn pop_keep_last<T: Clone>(q: &mut VecDeque<T>) -> T {
    let val = q.front().unwrap().clone();
    if q.len() > 1 {
        q.pop_front();
    }
    val
}

fn pop_or_auth(q: &mut VecDeque<Value>) -> Result<Value, OrbitError> {
    let ans = pop_keep_last(q);
    if ans == json!("auth") {
        Err(OrbitError::Execution("HTTP 401: Bad credentials".into()))
    } else {
        Ok(ans)
    }
}

fn git_rev(repo: &Path, rev: &str) -> String {
    git(repo, &["rev-parse", rev])
}

fn commit_file(repo: &Path, path: &str, content: &str, msg: &str) -> String {
    let target = repo.join(path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(target, content).unwrap();
    git(repo, &["add", path]);
    git(repo, &["commit", "-q", "-m", msg]);
    git_rev(repo, "HEAD")
}

fn commit_tree(repo: &Path, tree: &str, parents: &[&str], msg: &str) -> String {
    let mut args = vec!["commit-tree", tree];
    for p in parents {
        args.extend(["-p", p]);
    }
    args.extend(["-m", msg]);
    git(repo, &args)
}

fn observe_local(
    repo: &Path,
    src: &str,
    base: &str,
) -> orbit_types::workflow::handoff::HandoffCandidate {
    observe_candidate(
        &Env::capture(),
        CandidateCheckout {
            workspace_path: repo,
            source: Some(src),
            base_branch: base,
            landing_branch: base,
            delivery: HandoffDelivery::LocalCandidate,
            fallback_repository: "workspace",
            base_sync: BaseSyncMode::Local,
        },
    )
    .unwrap()
}

fn owner_checkout(repo: &Path) -> LandingHost {
    git(repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("README.md"), "base\n").unwrap();
    fs::create_dir_all(repo.join(".github/workflows")).unwrap();
    fs::write(repo.join(".github/workflows/ci.yml"), "name: CI\n").unwrap();
    git(repo, &["add", "README.md", ".github/workflows/ci.yml"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    git(repo, &["checkout", "-q", "-b", "orbit/T1"]);
    commit_file(repo, "work.txt", "delivered\n", "candidate");
    git(repo, &["checkout", "-q", "main"]);
    LandingHost {
        repo: repo.to_path_buf(),
        context: HandoffLandingContext {
            handoff_id: "handoff-1".into(),
            task_id: TASK_ID.into(),
            claim_id: "claim-1".into(),
            producing_run_id: "run-1".into(),
            candidate: observe_local(repo, "orbit/T1", "main"),
            unresolved_merge_intent: None,
            workspace_path: repo.to_path_buf(),
            owed_hosted_checks: Vec::new(),
            automatic_repair_spent: false,
            owner_distributed_completion_done: false,
        },
        steps: Mutex::default(),
        intents: Mutex::default(),
    }
}

fn owner_fixture() -> (TempDir, LandingHost) {
    let sandbox = tempdir().unwrap();
    let host = owner_checkout(sandbox.path());
    (sandbox, host)
}

fn land(host: &impl RuntimeHost) -> Result<Value, OrbitError> {
    land_with(host, json!({}))
}

fn land_with(host: &impl RuntimeHost, extra: Value) -> Result<Value, OrbitError> {
    let mut input = json!({"handoff_id": "handoff-1", "task_id": TASK_ID, "run_id": RUN_ID});
    if let (Some(i), Value::Object(e)) = (input.as_object_mut(), extra) {
        i.extend(e);
    }
    let empty = HashMap::new();
    execute_deterministic_action(
        host,
        "handoff_land",
        &json!({}),
        &input,
        false,
        &empty,
        None,
    )
}

fn land_now(host: &impl RuntimeHost) -> Result<Value, OrbitError> {
    land_with(host, json!({"max_wait_seconds": 0}))
}

fn land_cycles(host: &impl RuntimeHost, cycles: u64) -> Result<Value, OrbitError> {
    let input =
        json!({"max_wait_seconds": 30, "poll_interval_seconds": 5, "max_base_ci_cycles": cycles});
    with_vcs_retry_wait(|_| {}, || land_with(host, input))
}

fn counted_land(host: &impl RuntimeHost, input: Value) -> (Result<Value, OrbitError>, Vec<u64>) {
    let waits = Rc::new(RefCell::new(Vec::new()));
    let cap = waits.clone();
    let res = with_vcs_retry_wait(
        move |d| cap.borrow_mut().push(d.as_secs()),
        || land_with(host, input),
    );
    let durations = waits.borrow().clone();
    (res, durations)
}

fn land_poll(host: &impl RuntimeHost, max: u64, iv: u64) -> (Result<Value, OrbitError>, Vec<u64>) {
    counted_land(
        host,
        json!({"max_wait_seconds": max, "poll_interval_seconds": iv}),
    )
}

fn land_counting_polls(forge: &FakeForge) -> (Result<Value, OrbitError>, usize) {
    let polls = Arc::new(AtomicUsize::new(0));
    let c = polls.clone();
    let input = json!({"max_wait_seconds": 60, "poll_interval_seconds": 5});
    let res = with_vcs_retry_wait(
        move |_| {
            c.fetch_add(1, Ordering::SeqCst);
        },
        || land_with(forge, input),
    );
    (res, polls.load(Ordering::SeqCst))
}

fn pr_context(sandbox: &Path) -> HandoffLandingContext {
    let mut ctx = owner_checkout(sandbox).context;
    ctx.candidate.delivery = HandoffDelivery::PullRequest { number: 42 };
    ctx.candidate.repository = "owner/repository".into();
    ctx
}

fn fix_base(repo: &Path) -> String {
    commit_file(repo, "fix.txt", "the base's fix\n", "fix the base")
}

/// A fresh forge with candidate and base commits.
fn forge_fixture(owed: bool, spent: bool) -> (TempDir, FakeForge, String, String) {
    let sandbox = tempdir().unwrap();
    let mut forge = FakeForge::new(sandbox.path(), owed);
    forge.context.automatic_repair_spent = spent;
    let cand = forge.context.candidate.candidate.commit.clone();
    let base = forge.context.candidate.base.commit.clone();
    (sandbox, forge, cand, base)
}

fn published_forge(owed: bool, advance: bool) -> (TempDir, TempDir, FakeForge, String, String) {
    let remote = tempdir().unwrap();
    let (sandbox, mut forge, cand, base) = forge_fixture(owed, false);
    if advance {
        fix_base(&forge.repo);
    }
    forge.publish(remote.path());
    (remote, sandbox, forge, cand, base)
}

#[cfg(unix)]
fn install_fake_gh(repo: &Path, script: &str) -> impl Drop {
    use std::os::unix::fs::PermissionsExt;
    let bin = repo.join("bin");
    fs::create_dir(&bin).unwrap();
    let gh = bin.join("gh");
    fs::write(&gh, script).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(paths).unwrap();
    orbit_common::test_env::scoped([("PATH", Some(path.to_str().unwrap()))])
}

fn change_candidate_workflow(forge: &mut FakeForge) -> String {
    let src = forge.context.candidate.source_branch.clone();
    let base = forge.context.candidate.base_branch.clone();
    git(&forge.repo, &["checkout", "-q", &src]);
    let wf = ".github/workflows/ci.yml";
    commit_file(&forge.repo, wf, "name: changed CI\n", "change workflow");
    git(&forge.repo, &["checkout", "-q", &base]);
    let mut cand = observe_local(&forge.repo, &src, &base);
    cand.delivery = HandoffDelivery::PullRequest { number: 42 };
    cand.repository = "owner/repository".into();
    let commit = cand.candidate.commit.clone();
    forge.context.candidate = cand;
    forge.with_state(|s| s.head = commit.clone());
    commit
}

fn inherited_jobs(forge: &FakeForge, cand: &str, base: &str, fixed: &str, tip: FakeJob) {
    forge.job(cand, failed(1, 11, CLIPPY_DEAD_CODE));
    forge.job(base, failed(2, 21, CLIPPY_DEAD_CODE));
    forge.job(fixed, tip);
}

fn at(hhmm: &str) -> String {
    format!("2026-10-26T{hhmm}:00Z")
}

fn set_times(forge: &FakeForge, times: &[(u64, &str, &str)]) {
    forge.with_state(|s| {
        for (job, start, end) in times {
            s.job_times.insert(*job, (at(start), at(end)));
        }
    });
}

fn history_row(by: &str, event: &str, note: Option<String>) -> TaskHistoryEntry {
    TaskHistoryEntry {
        at: chrono::Utc::now(),
        by: by.into(),
        event: event.into(),
        note,
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
        surface: None,
        caller: None,
    }
}

fn check_run(name: &str, wf: &str, status: &str, conclusion: &str) -> Value {
    json!({"__typename":"CheckRun","name":name,"workflowName":wf,"status":status,"conclusion":conclusion})
}

fn assert_not_sent(forge: &FakeForge, ops: &[&str], label: &str) {
    for op in ops {
        assert!(forge.sent(op).is_empty(), "{label}: sent {op}");
    }
}

fn assert_steps(steps: &[HandoffLandingStep], expected: &[HandoffLandingStep], label: &str) {
    assert_eq!(steps, expected, "{label}");
}

fn assert_landed(outcome: Result<Value, OrbitError>, label: &str) {
    let val = outcome.expect(label);
    assert_eq!(val["phase"], "landed", "{label}: {val}");
}

fn is_base_pending(error: &OrbitError) -> bool {
    matches!(error, OrbitError::DeliveryRefused { refusal, .. } if refusal.error_code() == "delivery_base_checks_pending")
}

fn ledger_classes(forge: &FakeForge) -> Vec<CiFailureClass> {
    ci_failure_ledger(&forge.history.lock().unwrap())
        .into_iter()
        .map(|e| e.class)
        .collect()
}

fn assert_contains(text: &str, parts: &[&str], label: &str) {
    for part in parts {
        assert!(text.contains(part), "{label}: {text}");
    }
}

fn evidence_of(forge: &FakeForge) -> Value {
    serde_json::from_str(&forge.last_evidence()).unwrap()
}

fn head_of(forge: &FakeForge) -> String {
    forge.with_state(|s| s.head.clone())
}

fn policy_commit(p: &mut Value) -> &mut Value {
    &mut p["pull_request"]["commits"]["nodes"][0]["commit"]
}
fn policy_contexts(p: &mut Value) -> &mut Value {
    &mut policy_commit(p)["statusCheckRollup"]["contexts"]
}
fn policy_nodes(p: &mut Value) -> &mut Value {
    &mut policy_contexts(p)["nodes"]
}
fn policy_first(p: &mut Value) -> &mut Value {
    &mut policy_nodes(p)[0]
}
fn rm_key(v: &mut Value, k: &str) {
    v.as_object_mut().unwrap().remove(k);
}

fn a_local_candidate_landing_names_its_task_before_the_branch_moves() {
    let (_sb, host) = owner_fixture();
    let base = git_rev(&host.repo, "main");
    let candidate = host.context.candidate.candidate.commit.clone();

    land(&host).expect("the candidate fast-forwards");

    assert_eq!(git_rev(&host.repo, "main"), candidate);
    let expected = DirectLandingRequest {
        run_id: RUN_ID.into(),
        branch: "main".into(),
        before_commit: base.clone(),
        after_commit: candidate,
        task_ids: vec![TASK_ID.into()],
        handoff_id: Some("handoff-1".into()),
    };
    assert_eq!(
        *host.intents.lock().unwrap(),
        vec![(expected, base)],
        "intent retained before main moved"
    );
    assert_eq!(host.last_step(), Some(Complete));
}

fn owner_local_landing_refuses_and_preserves_cache_shaped_work() {
    let (_sb, host) = owner_fixture();
    let base = git_rev(&host.repo, "main");
    fs::create_dir_all(host.repo.join("pkg/__pycache__")).unwrap();
    let files = [
        ("pkg/__pycache__/notes.txt", b"owner notes\r\n".as_slice()),
        (
            "pkg/__pycache__/mod.cpython-312.pyc",
            b"\0owner bytecode\xff",
        ),
        ("fixture.pyc", b"\0standalone bytecode\xff"),
    ];
    for (p, b) in files {
        fs::write(host.repo.join(p), b).unwrap();
    }
    let status_args = ["status", "--porcelain", "--untracked-files=all"];
    let status = git(&host.repo, &status_args);

    let result = land(&host);

    for (p, b) in files {
        let got = fs::read(host.repo.join(p)).unwrap_or_else(|e| panic!("read {p}: {e}"));
        assert_eq!(got, b, "{p}");
    }
    let msg = result
        .expect_err("cache-shaped files make the owner checkout dirty")
        .to_string();
    assert_contains(
        &msg,
        &["owner landing checkout", "must be clean before"],
        "refusal",
    );
    for (p, _) in files {
        assert!(msg.contains(p), "missing {p} in refusal: {msg}");
    }
    assert_eq!(git_rev(&host.repo, "main"), base);
    assert_eq!(git(&host.repo, &status_args), status);
    assert!(host.intents.lock().unwrap().is_empty());
}

fn a_candidate_that_cannot_fast_forward_records_no_landing_intent() {
    let (_sb, host) = owner_fixture();
    commit_file(
        &host.repo,
        "other.txt",
        "another landing\n",
        "another landing",
    );

    land(&host).expect_err("a diverged landing branch stops the attempt");

    assert!(host.intents.lock().unwrap().is_empty());
    assert_eq!(host.last_step(), Some(Stop { repairable: true }));
}

fn an_already_landed_landing_holds_when_its_evidence_went_stale() {
    let (_sb, host) = owner_fixture();
    let mut ctx = host.context;
    ctx.candidate.candidate = ctx.candidate.base.clone();
    let ev_ref = HandoffArtifactRef {
        path: "checkpoint.json".into(),
        sha256: "0".repeat(64),
    };
    ctx.candidate.delivery = HandoffDelivery::NoDiff { evidence: ev_ref };
    let host = StaleEvidenceHost {
        context: ctx,
        steps: Mutex::default(),
        stops: Mutex::default(),
    };

    let err = land(&host).expect_err("stale evidence cannot complete the landing");
    assert!(
        matches!(&err, OrbitError::DeliveryRefused { refusal: DeliveryRefusal::EvidenceStale, message } if message.contains("scope.relations") && message.contains("the candidate stays in review")),
        "the landing is held, not a failed run: {err:?}"
    );
    assert_eq!(
        *host.steps.lock().unwrap(),
        vec![STOP],
        "one recorded stop, and no completion"
    );
    assert_eq!(host.stops.lock().unwrap().len(), 1);
}

fn a_mismatched_merged_pr_resolves_the_intent_before_stopping_later_attempts() {
    let secret = format!("ghp_{}", "a".repeat(36));
    let rows = [
        ("headRefName", "another-branch", "head_branch"),
        ("baseRefName", "another-base", "base_branch"),
        ("headRefOid", "another-commit", "head"),
    ];
    for (field, repl, obs_field) in rows {
        let sandbox = tempdir().unwrap();
        let mut ctx = pr_context(sandbox.path());
        let id = "handoff-1:42".to_string();
        ctx.unresolved_merge_intent = Some(id.clone());
        let mut status = json!({"state":"MERGED","headRefName":ctx.candidate.source_branch,"baseRefName":ctx.candidate.base_branch,"headRefOid":ctx.candidate.candidate.commit,"mergeCommit":{"oid":"provider-merge"},"futureField":{"transcript":secret}});
        status[field] = json!(repl);
        let host = ReconciliationHost {
            context: Mutex::new(ctx),
            updates: Mutex::default(),
            status,
        };

        let err = land(&host).expect_err("a foreign merged identity cannot complete the handoff");
        let updates = host.updates.lock().unwrap().clone();
        assert_eq!(updates.len(), 2, "{field}: resolved intent and Stop");
        assert_eq!(
            updates[0].step,
            ResolveIntent {
                intent_id: id,
                merged: true
            },
            "{field}"
        );
        assert_eq!(updates[1].step, STOP, "{field}: stale evidence");
        let stopped: Value = serde_json::from_str(&updates[1].evidence).unwrap();
        let obs = &stopped["observation"];
        assert_eq!(stopped["kind"], "owner_landing_refusal");
        assert_eq!(obs["required_checks"]["state"], "not_collected");
        assert!(updates[1].observed.is_none());
        let reason = stopped["reason"]["value"].as_str().unwrap();
        assert_contains(reason, &["delivery_evidence_stale", field, repl], field);
        let exp_msg = format!("handoff_land: {reason}");
        assert!(matches!(err, OrbitError::Execution(ref m) if m == &exp_msg));
        let rec: Value = serde_json::from_str(&updates[0].evidence).unwrap();
        assert_eq!(rec["reconciled"], "pull_request_merged");
        assert_eq!(rec["pull_request"], 42);
        assert_eq!(rec["delivery_error"]["value"], reason);
        assert_eq!(rec["provider_state"][obs_field]["value"], repl, "{field}");
        assert_eq!(obs["pr"][obs_field]["value"], repl);
        for u in &updates {
            assert!(!u.evidence.contains("futureField") && !u.evidence.contains(&secret));
        }
        let ctx_now = ReviewGate::handoff_landing_context(&host, "handoff-1").unwrap();
        assert!(ctx_now.unresolved_merge_intent.is_none());

        let retry = land(&host).expect_err("a later attempt sees the settled Stop");
        assert!(
            matches!(retry, OrbitError::InvalidInput(ref m) if m == "landing attempt is already settled")
        );
        assert_eq!(
            *host.updates.lock().unwrap(),
            updates,
            "retry retains Stop evidence"
        );
    }
}

/// [ORB-14261] Only conflicting or stale base stops are marked repairable.
fn only_a_conflicting_or_stale_base_stops_a_pull_request_for_repair() {
    let rows = [
        ("DIRTY", true),
        ("BEHIND", true),
        ("DRAFT", false),
        (" DIRTY ", false),
    ];
    for (ms, repairable) in rows {
        let host = OpenPullRequestHost::new(ms, json!([]));
        land_now(&host).expect_err("an unmergeable pull request stops the landing");
        assert_steps(&host.steps(), &[Stop { repairable }], ms);
        assert!(
            host.sent_ops().is_empty(),
            "merge state {ms}: no merge sent"
        );
    }
}

/// [ORB-15677] [ORB-15812] An owed hosted check requires explicit success on the pinned head.
fn an_owed_hosted_check_must_pass_before_the_owner_merges() {
    const OWED: &str = "CodeQL / Analyze (rust)";
    let chk = |c: Option<&str>| {
        let st = pick(c.is_some(), "COMPLETED", "IN_PROGRESS");
        check_run("Analyze (rust)", "CodeQL", st, c.unwrap_or_default())
    };
    let ok = check_run("fast", "CI", "COMPLETED", "SUCCESS");
    let st_ok = json!({"__typename": "StatusContext", "context": OWED, "state": "SUCCESS"});
    let run = |case: &str, rollup: Value, merges: bool, stop_reason: Option<&str>| {
        let mut host = OpenPullRequestHost::new("CLEAN", rollup);
        host.context.owed_hosted_checks = vec![OWED.into()];
        let msg = land_now(&host)
            .expect_err("the fixture never reports a merge")
            .to_string();
        let sent = host.sent_ops();
        assert_eq!(!sent.is_empty(), merges, "{case}: {sent:?} {msg}");
        assert_steps(&host.steps(), &[STOP], case);
        match stop_reason {
            None => assert!(
                msg.contains("no permitted merge method") && !msg.contains(OWED),
                "{case}: {msg}"
            ),
            Some(r) => assert!(msg.contains(OWED) && msg.contains(r), "{case}: {msg}"),
        }
    };
    let p = Some("not yet passed");
    run("passed", json!([ok, chk(Some("SUCCESS"))]), true, None);
    run("skipped", json!([ok, chk(Some("SKIPPED"))]), false, p);
    run("neutral", json!([ok, chk(Some("NEUTRAL"))]), false, p);
    run(
        "failed",
        json!([ok, chk(Some("FAILURE"))]),
        false,
        Some("failed"),
    );
    run("running", json!([ok, chk(None)]), false, p);
    run("absent", json!([ok]), false, p);
    run("status context passed", json!([ok, st_ok]), true, None);

    let (_sb, mut host) = owner_fixture();
    host.context.owed_hosted_checks = vec![OWED.into()];
    let tip = git_rev(&host.repo, "main");
    let err = land(&host).expect_err("no hosted CI runs a local candidate");
    assert!(err.to_string().contains(OWED), "{err}");
    assert_eq!(*host.steps.lock().unwrap(), vec![STOP]);
    assert_eq!(git_rev(&host.repo, "main"), tip, "nothing landed");
}

fn pending_policy(head: &str, status: &str, required: bool) -> Value {
    json!({"pull_request":{"headRefOid":head,"commits":{"nodes":[{"commit":{"oid":head,"statusCheckRollup":{"contexts":{"pageInfo":{"hasNextPage":false},"nodes":[{"__typename":"CheckRun","name":"clippy","status":status,"conclusion":pick(status=="COMPLETED","SUCCESS",""),"isRequired":required,"checkSuite":{"workflowRun":{"workflow":{"name":"CI"}}}}]}}}}]}}})
}

fn pending_forge(
    obs: Vec<Value>,
    make_policies: impl FnOnce(&str) -> Vec<Value>,
) -> (TempDir, FakeForge, String) {
    let (sandbox, forge, head, _) = forge_fixture(false, false);
    forge.job(&head, passed(1, 11));
    let policies = make_policies(&head);
    forge.with_state(|s| {
        s.observations = obs.into();
        s.policies = policies.into();
    });
    (sandbox, forge, head)
}

fn blocked_aggregate() -> Value {
    json!({"mergeStateStatus":"BLOCKED","statusCheckRollup":[{"__typename":"CheckRun","name":"CodeQL","status":"COMPLETED","conclusion":"NEUTRAL"}]})
}

fn owner_landing_waits_for_exact_required_checks_before_one_verified_merge() {
    let obs = vec![
        blocked_aggregate(),
        blocked_aggregate(),
        json!({"mergeStateStatus":"CLEAN"}),
    ];
    let (_sb, forge, _) = pending_forge(obs, |h| {
        vec![
            pending_policy(h, "QUEUED", true),
            pending_policy(h, "IN_PROGRESS", true),
            pending_policy(h, "COMPLETED", true),
        ]
    });
    let (result, waits) = land_poll(&forge, 15, 5);
    assert_landed(
        result,
        "BLOCKED with conclusive exact-head required pending must wait",
    );
    assert_eq!(waits, [5, 5]);
    forge.assert_sent("pr.required_checks", 3, "exact");
    forge.assert_sent("pr.status", 4, "exact");
    forge.assert_sent("pr.merge", 1, "exact");
    let ops = forge.ops.lock().unwrap();
    let dl: Vec<u64> = ops
        .iter()
        .filter(|(op, _)| matches!(op.as_str(), "pr.status" | "pr.required_checks"))
        .map(|(_, i)| {
            i["deadline_ms"]
                .as_u64()
                .expect("poll reads carry deadline")
        })
        .collect();
    assert_eq!(dl.len(), 7);
    assert!(
        dl.iter().all(|d| *d == dl[0]),
        "deadline never resets: {dl:?}"
    );
    let merge = ops.iter().position(|(op, _)| op == "pr.merge").unwrap();
    let pre = ops[..merge]
        .iter()
        .filter(|(op, _)| op == "pr.status")
        .count();
    assert_eq!(pre, 3, "no merge before checks settle");
    let id = "handoff-1:42".to_string();
    let p = PublishIntent {
        intent_id: id.clone(),
    };
    let r = ResolveIntent {
        intent_id: id,
        merged: true,
    };
    assert_steps(&forge.steps(), &[p, r, Complete], "exact");
}

fn owner_landing_waits_out_pending_only_block_before_checks_pass() {
    for passes in [true, false] {
        let pend = json!({"mergeStateStatus":"BLOCKED","statusCheckRollup":[{"name":"clippy","workflowName":"CI","status":"IN_PROGRESS"}]});
        let obs = pick(
            passes,
            vec![
                pend.clone(),
                pend.clone(),
                json!({"mergeStateStatus":"CLEAN"}),
            ],
            vec![pend],
        );
        let (_sb, forge, _) = pending_forge(obs, |h| vec![pending_policy(h, "COMPLETED", false)]);
        let (result, waits) = land_poll(&forge, 15, 5);
        if passes {
            assert_landed(result, "pending-only BLOCKED must keep waiting");
            assert_eq!(waits, [5, 5]);
            forge.assert_sent("pr.merge", 1, "checks passed");
            assert_eq!(forge.steps().last(), Some(&Complete));
        } else {
            let err = result.unwrap_err().to_string();
            assert_eq!(
                evidence_of(&forge)["waiting_gate"],
                "required_checks_pending"
            );
            // [ORB-22485] The rollup-only wait names its running checks.
            assert_contains(
                &err,
                &["within 15s", "CI / clippy", "still running"],
                "deadline",
            );
            assert_eq!(waits, [5, 5, 5]);
            forge.assert_none(&["pr.merge"], "checks still pending");
            forge.assert_step(STOP, "deadline");
        }
        forge.assert_none(NO_RERUN_OR_UPDATE, "pending only");
    }
}

/// [ORB-16339] Wait for unreported required checks named by branch policy; unreadable gets grace polls.
fn owner_landing_waits_for_required_checks_the_head_has_not_reported() {
    let u = || json!({"mergeStateStatus":"BLOCKED","statusCheckRollup":[]});
    let miss = json!({"gates_passed":false,"required":[{"context":"clippy","app_id":null}],"missing":[{"context":"clippy","app_id":null}]});
    let run = |case: &str,
               appears: bool,
               auth: bool,
               max_wait: u64,
               exp: usize,
               stop: Option<&str>| {
        let obs = pick(
            appears,
            vec![u(), u(), json!({"mergeStateStatus":"CLEAN"})],
            vec![u()],
        );
        let (_sb, forge, _) = pending_forge(obs, |h| {
            let p = json!({"pull_request":{"headRefOid":h,"commits":{"nodes":[{"commit":{"oid":h,"statusCheckRollup":null}}]}}});
            vec![p]
        });
        let gate = pick(auth, json!("auth"), miss.clone());
        forge.with_state(|s| s.gates = vec![gate].into());
        let (result, waits) = land_poll(&forge, max_wait, 5);
        assert_eq!(waits.len(), exp, "{case}: {result:?}");
        forge.assert_sent("ci.current_gates", pick(auth, 5, 2), case);
        match stop {
            None => {
                assert_landed(result, case);
                forge.assert_sent("pr.merge", 1, case);
            }
            Some(reason) => {
                assert_contains(&result.unwrap_err().to_string(), &[reason], case);
                forge.assert_none(&["pr.merge"], case);
                forge.assert_step(STOP, case);
            }
        }
    };
    let s1 = "within 10s (required checks the head has not reported: clippy)";
    let s2 = "required reviews or checks are not satisfied";
    run("appears", true, false, 15, 2, None);
    run("never", false, false, 10, 2, Some(s1));
    run("unreadable", false, true, 60, 4, Some(s2));
}

fn check_pending_refusal(
    forge: &FakeForge,
    case: &str,
    head: &str,
    reason: &str,
    err: &str,
) -> (String, Value) {
    forge.assert_step(STOP, case);
    let updates = forge.updates.lock().unwrap();
    let stopped = updates.last().unwrap();
    assert!(
        stopped.observed.is_none(),
        "diagnostics never carry candidate authority"
    );
    let ev: Value = serde_json::from_str(&stopped.evidence).unwrap();
    assert_eq!(ev["schema_version"], 1, "{case}: {ev}");
    assert_eq!(ev["kind"], "owner_landing_refusal");
    assert_eq!(ev["repairable"], false);
    let ev_reason = ev["reason"]["value"].as_str().unwrap();
    assert!(ev_reason.contains(reason), "{case}: {ev_reason}");
    if case == "optional" {
        assert_contains(err, &["optional", "still running"], case);
        assert_contains(ev_reason, &["optional", "still running"], case);
    }
    assert_eq!(ev["accepted"]["candidate"]["value"], head);
    assert_eq!(ev["emission_proof"]["state"], "not_collected");
    assert_eq!(ev["workflow_prerequisites"]["state"], "not_collected");
    (stopped.evidence.clone(), ev)
}

/// Controls share the public accepted-candidate setup, without real sleeps.
#[allow(clippy::print_stderr)] // Identify each control in isolated child failure output.
fn owner_landing_pending_evidence_never_extends_refusal_or_merge_authority() {
    const CASES: &str = "optional empty_contexts null_contexts missing_contexts bounded_redacted \
        bounded_reason optional_failed_required_pending administrative review review_failed \
        draft_failed contradiction_failed missing_state changes_requested unknown_review \
        unreadable_rollup auth foreign_policy foreign_commit missing_policy missing_policy_head \
        truncated unknown_check contradiction head_changed base_changed policy_changed timeout \
        changing_checks_timeout cancelled owed neutral skipped owed_absent owed_foreign \
        required_neutral required_skipped";
    const ZERO_READS: &str = "review review_failed draft_failed contradiction_failed \
        missing_state changes_requested unknown_review unreadable_rollup contradiction bounded_reason";
    let secret = format!("ghp_{}", "a".repeat(36));
    for case in CASES.split_whitespace() {
        eprintln!("pending evidence control: {case}");
        let (_sb, mut forge, head) = pending_forge(vec![blocked_aggregate()], |_| vec![]);
        let (mut patch, mut policy) = (
            blocked_aggregate(),
            pending_policy(&head, "IN_PROGRESS", true),
        );
        let (mut after, mut next_pol, mut exp_waits, mut merges, mut reason) =
            (None, None, 0, false, "cannot be merged");
        match case {
            "empty_contexts" => {
                patch["statusCheckRollup"] = json!([]);
                *policy_nodes(&mut policy) = json!([]);
            }
            "null_contexts" => policy_commit(&mut policy)["statusCheckRollup"] = Value::Null,
            "missing_contexts" => rm_key(policy_commit(&mut policy), "statusCheckRollup"),
            "bounded_reason" => {
                (patch["mergedAt"], reason) = (
                    json!(format!("{secret}{}", "界".repeat(5000))),
                    "contradictory",
                )
            }
            "bounded_redacted" => {
                let big = format!("{secret} {}", "界".repeat(800));
                // NEUTRAL keeps this redaction case an immediate blocked refusal [ORB-22350].
                let mk = |c, req: Option<bool>| {
                    (0..40).map(|_| if let Some(r) = req { json!({"name":big,"status":"COMPLETED","conclusion":c,"isRequired":r}) } else { json!({"name":big,"status":"COMPLETED","conclusion":c}) }).collect::<Vec<_>>()
                };
                patch["statusCheckRollup"] = json!(mk("NEUTRAL", None));
                policy["transcript"] = json!("untrusted-raw-payload-sentinel");
                *policy_nodes(&mut policy) = json!(mk("SUCCESS", Some(true)));
            }
            "optional" => {
                policy = pending_policy(&head, "IN_PROGRESS", false);
                patch["statusCheckRollup"] = json!([{"name":"optional","status":"QUEUED"}]);
                (exp_waits, reason) = (2, "within 10s");
            }
            "optional_failed_required_pending" => {
                patch["statusCheckRollup"] = json!([{"name":"clippy","workflowName":"CI","status":"IN_PROGRESS"},{"name":"coverage","status":"COMPLETED","conclusion":"FAILURE"}]);
            }
            "administrative" => policy = pending_policy(&head, "COMPLETED", true),
            "review" => patch["reviewDecision"] = json!("REVIEW_REQUIRED"),
            "missing_state" => (patch["state"], reason) = (Value::Null, "unavailable"),
            "review_failed" | "draft_failed" | "contradiction_failed" => {
                forge.context.owed_hosted_checks = vec!["CI / clippy".into()];
                patch["statusCheckRollup"] = json!([{"name":"clippy","workflowName":"CI","status":"COMPLETED","conclusion":"FAILURE"}]);
                match case {
                    "review_failed" => patch["reviewDecision"] = json!("REVIEW_REQUIRED"),
                    "draft_failed" => patch["mergeStateStatus"] = json!("DRAFT"),
                    _ => {
                        (patch["mergedAt"], reason) =
                            (json!("2026-10-15T00:00:00Z"), "contradictory")
                    }
                }
            }
            "changes_requested" => patch["reviewDecision"] = json!("CHANGES_REQUESTED"),
            "unknown_review" => patch["reviewDecision"] = json!(42),
            "unreadable_rollup" => patch["statusCheckRollup"] = Value::Null,
            "auth" => (policy, reason) = (json!("auth"), "HTTP 401"),
            "foreign_policy" => {
                (policy, reason) = (pending_policy("foreign", "QUEUED", true), "current head")
            }
            "foreign_commit" => {
                (policy_commit(&mut policy)["oid"], reason) = (json!("foreign"), "current head")
            }
            "missing_policy" => (policy, reason) = (json!({}), "no pull request"),
            "required_neutral" | "required_skipped" => {
                policy = pending_policy(&head, "COMPLETED", true);
                let c = pick(case == "required_neutral", "NEUTRAL", "SKIPPED");
                policy_first(&mut policy)["conclusion"] = json!(c);
            }
            "missing_policy_head" => {
                rm_key(&mut policy["pull_request"], "headRefOid");
                reason = "current head";
            }
            "truncated" => {
                policy_contexts(&mut policy)["pageInfo"]["hasNextPage"] = json!(true);
                reason = "truncated";
            }
            "unknown_check" => {
                policy_first(&mut policy)["status"] = json!("UNRECOGNIZED");
                reason = "unreadable";
            }
            "contradiction" => {
                (patch["mergedAt"], reason) = (json!("2026-10-15T00:00:00Z"), "contradictory")
            }
            "head_changed" | "base_changed" => {
                let k = pick(case == "head_changed", "headRefOid", "baseRefName");
                (after, exp_waits, reason) =
                    (Some(json!({k: "foreign"})), 1, "delivery_evidence_stale");
            }
            "policy_changed" => {
                (next_pol, exp_waits) = (Some(pending_policy(&head, "QUEUED", false)), 1)
            }
            "timeout" | "changing_checks_timeout" => {
                (exp_waits, reason) = (2, "within 10s");
                if case == "changing_checks_timeout" {
                    let mut next = pending_policy(&head, "QUEUED", true);
                    policy_first(&mut next)["name"] = json!("another required job");
                    next_pol = Some(next);
                }
            }
            "cancelled" => {
                (after, exp_waits, reason) =
                    (Some(json!({"error":"execution cancelled"})), 1, "cancelled")
            }
            "owed" | "neutral" | "skipped" | "owed_absent" | "owed_foreign" => {
                forge.context.owed_hosted_checks = vec!["CI / clippy".into()];
                policy = pending_policy(&head, "IN_PROGRESS", false);
                if case == "owed_foreign" {
                    patch["statusCheckRollup"] =
                        json!([{"name":"clippy","workflowName":"Foreign","status":"QUEUED"}]);
                    (exp_waits, reason) = (2, "within 10s");
                } else if case != "owed_absent" {
                    let st = pick(case == "owed", "QUEUED", "COMPLETED");
                    let conc = pick(case == "neutral", "NEUTRAL", "SKIPPED");
                    patch["statusCheckRollup"] = json!([{"name":"clippy","workflowName":"CI","status":st,"conclusion":conc}]);
                }
                if case == "owed" {
                    (after, exp_waits, merges) =
                        (Some(json!({"mergeStateStatus":"CLEAN"})), 1, true);
                }
            }
            _ => unreachable!(),
        }
        forge.with_state(|s| {
            s.observations = [Some(patch), after].into_iter().flatten().collect();
            s.policies = [Some(policy), next_pol].into_iter().flatten().collect();
        });
        let (result, waits) = land_poll(&forge, 10, 5);
        assert_eq!(waits.len(), exp_waits, "{case}: {result:?}");
        forge.assert_none(NO_RERUN_OR_UPDATE, case);
        let jobs_n = usize::from(case == "optional_failed_required_pending");
        forge.assert_sent("ci.commit_jobs", jobs_n, case);
        forge.assert_sent("pr.merge", usize::from(merges), case);
        if merges {
            assert_landed(result, case);
            continue;
        }
        let err = result.unwrap_err().to_string();
        assert!(err.contains(reason), "{case}: {err}");
        if case == "cancelled" {
            continue;
        }
        let (raw, ev) = check_pending_refusal(&forge, case, &head, reason, &err);
        let (obs, req) = (&ev["observation"], &ev["observation"]["required_checks"]);
        match case {
            "empty_contexts" => {
                assert_eq!(obs["pr"]["rollup"]["original_count"], 0);
                assert_eq!(req["contexts"]["original_count"], 0);
                assert_eq!(req["outcome"], "parsed");
                let want = "blocked: required reviews or checks are not satisfied";
                assert_eq!(obs["classification"]["value"], want);
            }
            "null_contexts" | "missing_contexts" => {
                assert_eq!(req["outcome"], "parsed");
                assert_eq!(req["contexts"]["state"], "missing");
                let want = pick(case == "null_contexts", "null", "missing");
                assert_eq!(req["rollup_state"], want, "{case}");
            }
            "timeout" | "changing_checks_timeout" => {
                assert_eq!(req["outcome"], "parsed");
                assert_eq!(req["contexts"]["items"][0]["state"], "Pending");
                assert_eq!(obs["classification"]["value"], "pending");
            }
            "review" => {
                assert_eq!(obs["pr"]["review"]["value"], "REVIEW_REQUIRED");
                assert_eq!(req["state"], "not_collected");
            }
            "optional_failed_required_pending" => {
                assert_eq!(obs["pr"]["rollup"]["items"][1]["state"], "Failed");
                assert_eq!(req["outcome"], "parsed");
            }
            "auth" => assert_eq!(req["outcome"], "read_error"),
            "missing_policy" | "truncated" => assert_eq!(req["outcome"], "parse_error"),
            "foreign_policy" | "foreign_commit" | "missing_policy_head" => {
                assert_eq!(req["outcome"], "head_mismatch")
            }
            "unknown_check" => {
                assert_eq!(req["outcome"], "unreadable_state");
                assert_eq!(
                    req["contexts"]["items"][0]["provider_status"]["value"], "UNRECOGNIZED",
                    "{case}"
                );
            }
            "unreadable_rollup" => {
                assert_eq!(obs["pr"]["rollup"]["state"], "null");
                assert_eq!(req["state"], "not_collected");
            }
            "bounded_reason" => {
                let r = &ev["reason"];
                assert_eq!(r["truncated"], true);
                assert!(r["original_chars"].as_u64().unwrap() > 5000);
                let human = r["value"].as_str().unwrap();
                assert_eq!(human.chars().count(), 4096);
                assert!(human.ends_with("landing needs an unambiguous merged state"));
                assert!(!raw.contains(&secret), "{case}");
            }
            "bounded_redacted" => {
                assert!(
                    !raw.contains(&secret) && !raw.contains("untrusted-raw-payload-sentinel"),
                    "{case}"
                );
                for e in [&obs["pr"]["rollup"], &req["contexts"]] {
                    assert_eq!(
                        (&e["original_count"], &e["truncated"]),
                        (&json!(40), &json!(true))
                    );
                    assert_eq!(e["items"].as_array().unwrap().len(), 32);
                    let n = &e["items"][0]["name"];
                    assert_eq!(
                        (&n["original_chars"], &n["truncated"]),
                        (&json!(841), &json!(true))
                    );
                    assert_eq!(n["value"].as_str().unwrap().chars().count(), 512);
                }
                assert!(raw.len() < 160_000);
            }
            _ => {}
        }
        let reads = match case {
            c if ZERO_READS.split_whitespace().any(|z| z == c) => 0,
            "timeout" | "changing_checks_timeout" | "optional" | "owed_foreign" => exp_waits,
            "head_changed" | "base_changed" => 1,
            _ => exp_waits + 1,
        };
        forge.assert_sent("pr.required_checks", reads, case);
    }
}

fn provider_update(remote: &Path, head: &str, shape: UpdateShape) -> String {
    let base_tree = git_rev(remote, "main^{tree}");
    let advanced = commit_tree(remote, &base_tree, &["main"], "the base advances");
    git(remote, &["update-ref", "refs/heads/main", &advanced]);
    let base_side = match shape {
        UpdateShape::Foreign => commit_tree(remote, &base_tree, &[&advanced], "not on main"),
        _ => advanced,
    };
    let tree = match shape {
        UpdateShape::Dirty => git_rev(remote, &format!("{head}^{{tree}}")),
        _ => git(remote, &["merge-tree", "--write-tree", head, &base_side]),
    };
    let parents = pick(
        matches!(shape, UpdateShape::NotAMerge),
        vec![head],
        vec![head, &base_side],
    );
    let merge = commit_tree(remote, &tree, &parents, "Merge branch 'main' into orbit/T1");
    git(remote, &["update-ref", "refs/heads/orbit/T1", &merge]);
    if matches!(shape, UpdateShape::BaseGone) {
        git(remote, &["update-ref", "-d", "refs/heads/main"]);
    }
    merge
}

fn failed(run_id: u64, job_id: u64, log: &'static str) -> FakeJob {
    failed_reading(run_id, job_id, FakeLog::Text(log))
}

fn failed_reading(run_id: u64, job_id: u64, log: FakeLog) -> FakeJob {
    FakeJob {
        run_id,
        job_id,
        conclusion: "failure",
        log,
        run_attempt: 1,
        event: "pull_request",
    }
}

fn passed(run_id: u64, job_id: u64) -> FakeJob {
    FakeJob {
        run_id,
        job_id,
        conclusion: "success",
        log: FakeLog::Text(""),
        run_attempt: 1,
        event: "push",
    }
}

fn handoff_fake_refuses_unscoped_automation_updates() {
    let (_sb, forge, _, _) = forge_fixture(true, false);
    let u = TaskAutomationUpdate::default();
    let err = RunLifecycle::apply_task_automation_update(&forge, TASK_ID, u).expect_err("fenced");
    assert_contains(
        &err.to_string(),
        &["active execution claim requires a claim-scoped mutation"],
        "unscoped",
    );
}

/// ORB-19342: fresh base pass warrants a flake rerun before repair, including spent/shared/transient cases.
fn untouched_failure_reruns_once_after_fresh_base_pass() {
    const CASES: &str = "pass repeat repair_spent repeat_spent shared_budget transient_after_flake";
    for case in CASES.split_whitespace() {
        let spent = matches!(case, "repair_spent" | "repeat_spent");
        let (_sb, mut forge, head, base) = forge_fixture(true, spent);
        if case == "transient_after_flake" {
            forge.ci_retry_policy.max_attempts = 1;
        }
        let shared = case == "shared_budget";
        forge.job(
            &head,
            failed(1, 11, pick(shared, NETWORK_FLAKE, UNTOUCHED_TEST_FAILURE)),
        );
        forge.job(&base, passed(2, 21));
        let (bs, be) = pick(shared, ("14:06", "14:07"), ("14:02", "14:03"));
        let ts = [(11, "14:00", "14:01"), (21, bs, be), (12, "14:04", "14:05")];
        set_times(&forge, &ts);
        forge.set_after_rerun(match case {
            "pass" | "repair_spent" => passed(1, 12),
            "transient_after_flake" => failed(1, 12, NETWORK_FLAKE),
            _ => failed(1, 12, UNTOUCHED_TEST_FAILURE),
        });
        let outcome = land_now(&forge);
        forge.assert_sent("ci.rerun_failed", 1, case);
        forge.assert_none(&["pr.update_branch"], case);
        if matches!(case, "pass" | "repair_spent") {
            assert_landed(outcome, case);
            assert_eq!(forge.steps().last(), Some(&Complete));
            assert!(!forge.steps().iter().any(|s| matches!(s, Stop { .. })));
        } else {
            outcome.expect_err(case);
            let rep = matches!(case, "repeat" | "repeat_spent");
            forge.assert_step(pick(rep, StopForRequeue, STOP), case);
            let parts: &[&str] = match case {
                "transient_after_flake" => &[
                    "ci_infrastructure_retry_exhausted",
                    "classification=transient",
                    "attempts=1",
                ],
                "shared_budget" => &["CI retry was reserved", "observing without resending"],
                _ => &["classified `candidate`", "returns the task to the backlog"],
            };
            forge.assert_reason(case, parts);
        }
        let reruns = forge.history_notes("ci_check_rerun");
        assert_eq!(reruns.len(), 1, "{case}: record the rerun once");
        let r = &reruns[0];
        assert_eq!((&r["head"], &r["run_id"]), (&json!(head), &json!(1)));
        assert_eq!(
            (&r["jobs"][0]["job_id"], &r["jobs"][0]["check"]),
            (&json!(11), &json!("CI / clippy"))
        );
    }
}

/// Only a verified same-PR same-run later-attempt rerun releases a `ci_base_suspect` hold.
fn suspicion_only_history_cannot_admit_repair_from_a_non_pr_run() {
    let rows = [
        ("push_other_run", 3, 31, 1, "push"),
        ("dispatch_other_run", 3, 31, 1, "workflow_dispatch"),
        ("valid_pr_rerun", 1, 12, 2, "pull_request"),
    ];
    for (case, run_id, job_id, attempt, event) in rows {
        let (_sb, forge, head, base) = forge_fixture(true, false);
        let mut current = failed(run_id, job_id, UNTOUCHED_TEST_FAILURE);
        (current.run_attempt, current.event) = (attempt, event);
        forge.job(&head, current);
        forge.job(&base, passed(2, 21));
        set_times(
            &forge,
            &[(job_id, "14:04", "14:05"), (21, "14:02", "14:03")],
        );
        let note = json!({"head":head,"check":"CI / clippy","signature":"error: shutdown test timed out","run_id":1,"job_id":11,"run_attempt":1,"pr_number":42,"started_at":"2026-10-26T14:00:00Z","completed_at":"2026-10-26T14:01:00Z"});
        forge.push_history(history_row(
            "system",
            "ci_base_suspect",
            Some(note.to_string()),
        ));
        land_now(&forge).expect_err(case);
        forge.assert_none(NO_MERGE_OR_UPDATE, case);
        if case == "valid_pr_rerun" {
            forge.assert_step(StopForRequeue, case);
            forge.assert_reason(case, &["classified `candidate`"]);
            forge.assert_none(&["ci.rerun_failed"], case);
        } else {
            forge.assert_sent("ci.rerun_failed", 0, case);
            forge.assert_step(STOP, case);
        }
    }
}

fn untouched_failure_uses_a_fresh_pass_behind_the_moving_tip() {
    for case in ["fresh_base", "fresh_middle", "stale_middle"] {
        let (_sb, forge, head, base) = forge_fixture(true, false);
        let middle = fix_base(&forge.repo);
        git(
            &forge.repo,
            &["commit", "--allow-empty", "-m", "tip moved again"],
        );
        forge.job(&head, failed(1, 11, UNTOUCHED_TEST_FAILURE));
        forge.job(pick(case == "fresh_base", &base, &middle), passed(2, 21));
        let (bs, be) = pick(
            case == "stale_middle",
            ("12:00", "12:01"),
            ("14:02", "14:03"),
        );
        set_times(&forge, &[(11, "14:00", "14:01"), (21, bs, be)]);
        let after = pick(
            case == "stale_middle",
            failed(1, 12, UNTOUCHED_TEST_FAILURE),
            passed(1, 12),
        );
        forge.set_after_rerun(after);
        let outcome = land_now(&forge);
        forge.assert_sent("ci.rerun_failed", 1, case);
        if case == "stale_middle" {
            assert_contains(
                &outcome.expect_err(case).to_string(),
                &["classified `candidate`"],
                case,
            );
            forge.assert_step(StopForRequeue, case);
        } else {
            assert_landed(outcome, case);
        }
        forge.assert_none(&["pr.update_branch"], case);
    }
}

#[cfg(unix)]
fn untouched_failure_checks_push_history_through_the_real_adapter() {
    const CASES: &str = "fresh stale_job stale_run pull_request other_branch foreign_commit pending cancelled wrong_job";
    const GH: &str = "#!/bin/sh\nset -eu\ncase \"$*\" in\n *'/actions/runs/2/jobs'*) cat ci-jobs.json ;;\n *'/actions/runs') exit 1 ;;\n *'/actions/runs '*)\n  case \"$*\" in\n   *head_sha=*) printf '%s' '{\"total_count\":0,\"workflow_runs\":[]}' ;;\n   *event=push*branch=main*) cat ci-runs.json ;;\n   *) exit 1 ;;\n  esac ;;\n *) exit 1 ;;\nesac\n";
    for case in CASES.split_whitespace() {
        let (_sb, forge, head, base) = forge_fixture(true, false);
        let middle = fix_base(&forge.repo);
        git(
            &forge.repo,
            &["commit", "--allow-empty", "-m", "tip moved again"],
        );
        let tip = git_rev(&forge.repo, "main");
        forge.job(&head, failed(1, 11, UNTOUCHED_TEST_FAILURE));
        forge.job(&base, passed(9, 91));
        forge.with_state(|s| {
            s.base_push_provider = true;
            s.job_times.insert(11, (at("14:00"), at("14:01")));
            s.after_rerun = Some(passed(1, 12));
        });
        let runs = json!({"total_count":200,"workflow_runs":[
            {"id":2,"workflow_id":1,"name":"CI","event":pick(case=="pull_request","pull_request","push"),"head_branch":pick(case=="other_branch","other","main"),"head_sha":pick(case=="foreign_commit",&head,&middle),"status":pick(case=="pending","in_progress","completed"),"conclusion":pick(case=="cancelled","cancelled","success"),"run_started_at":pick(case=="stale_run","2026-10-26T12:00:00Z","2026-10-26T14:02:00Z")},
            {"id":3,"workflow_id":1,"name":"CI","event":"push","head_branch":"main","head_sha":tip,"status":"in_progress","conclusion":null,"run_started_at":"2026-10-26T14:04:00Z"}
        ]});
        let jobs = json!({"total_count":1,"jobs":[{"id":21,"name":pick(case=="wrong_job","other","clippy"),"status":"completed","conclusion":"success","started_at":pick(case=="stale_job","2026-10-26T12:00:00Z","2026-10-26T14:02:00Z"),"completed_at":"2026-10-26T14:03:00Z","steps":[]}]});
        fs::write(forge.repo.join("ci-runs.json"), runs.to_string()).unwrap();
        fs::write(forge.repo.join("ci-jobs.json"), jobs.to_string()).unwrap();
        let _env = install_fake_gh(&forge.repo, GH);

        let outcome = land_now(&forge);
        if matches!(case, "fresh" | "stale_job" | "stale_run") {
            assert_landed(outcome, case);
            forge.assert_sent("ci.rerun_failed", 1, case);
        } else {
            assert!(
                outcome.as_ref().is_err_and(is_base_pending),
                "{case}: {outcome:?}"
            );
            forge.assert_none(&["ci.rerun_failed", "pr.merge"], case);
        }
        forge.assert_none(&["pr.update_branch"], case);
    }
}

/// [ORB-15956] An inherited failure updates the branch onto the fixed base and lands.
fn a_check_failure_inherited_from_the_base_updates_the_branch_and_lands() {
    let (_sb, forge, cand, base) = forge_fixture(true, false);
    let fixed = fix_base(&forge.repo);
    inherited_jobs(&forge, &cand, &base, &fixed, passed(3, 31));
    forge.set_after_update(passed(4, 41));

    assert_landed(land_now(&forge), "the update lands");
    forge.assert_sent("pr.update_branch", 1, "inherited");
    forge.assert_none(&["ci.rerun_failed"], "inherited");
    assert_eq!(ledger_classes(&forge), [CiFailureClass::Infrastructure]);
    let head = head_of(&forge);
    let parents = git(
        &forge.repo,
        &["rev-parse", &format!("{head}^1"), &format!("{head}^2")],
    );
    assert_eq!(
        parents,
        format!("{cand}\n{fixed}"),
        "merged head has candidate + fixed base"
    );
    assert_eq!(
        forge.sent("pr.merge")[0]["reviewed_head_sha"],
        head.as_str()
    );
    let steps = forge.steps();
    assert_eq!(steps.last(), Some(&Complete), "{steps:?}");
    assert!(!steps.iter().any(|s| matches!(s, Stop { .. })), "{steps:?}");
    let updates = forge.updates.lock().unwrap().clone();
    let kept = updates
        .iter()
        .filter_map(|u| u.observed.as_ref())
        .all(|o| o.candidate.commit == cand);
    assert!(
        kept,
        "the owner observes the validated candidate, never the merged head"
    );
    let ev = evidence_of(&forge);
    assert_eq!(ev["updated_head"], head.as_str(), "{ev}");
    let rec = ev["check_recovery"][0].as_str().unwrap();
    assert_contains(rec, &["inherited", &fixed], "check_recovery");
}

fn inherited_landing_waits_for_a_later_passing_base_and_updates_again() {
    let (_sb, forge, cand, base) = forge_fixture(true, false);
    let forge = Rc::new(forge);
    let red_tip = fix_base(&forge.repo);
    inherited_jobs(
        &forge,
        &cand,
        &base,
        &red_tip,
        failed(3, 31, CLIPPY_DEAD_CODE),
    );
    forge.set_after_update(failed(4, 41, CLIPPY_DEAD_CODE));
    let (waits, cap_forge) = (Rc::new(Cell::new(0)), forge.clone());
    let cap_waits = waits.clone();
    let res = with_vcs_retry_wait(
        move |_| {
            cap_waits.set(cap_waits.get() + 1);
            if cap_waits.get() == 1 {
                let tree = git_rev(&cap_forge.repo, "main^{tree}");
                let fixed = commit_tree(&cap_forge.repo, &tree, &["main"], "the base recovers");
                git(&cap_forge.repo, &["update-ref", "refs/heads/main", &fixed]);
                cap_forge.job(&fixed, passed(5, 51));
                cap_forge.set_after_update(passed(6, 61));
            }
        },
        || {
            land_with(
                forge.as_ref(),
                json!({"max_wait_seconds": 30, "poll_interval_seconds": 5}),
            )
        },
    );
    assert_landed(res, "the later base fix resumes landing");
    assert_eq!(waits.get(), 1);
    forge.assert_sent("pr.update_branch", 2, "later recovery");
    assert!(forge.history_count(LANDING_WAITING_FOR_BASE_EVENT) > 0);
    assert_eq!(forge.history_count("pr_base_update_reserved"), 2);
}

fn landing_accepts_two_clean_operator_base_merges_and_refuses_a_dirty_hop() {
    use UpdateShape::{CleanMerge, Dirty, Foreign, NotAMerge};
    for shape in [CleanMerge, Dirty, NotAMerge, Foreign] {
        let (remote, _sb, forge, cand, _) = published_forge(false, true);
        let first = provider_update(remote.path(), &cand, shape);
        let second = provider_update(remote.path(), &first, CleanMerge);
        forge.with_state(|s| s.head = second.clone());
        forge.job(&second, passed(1, 11));
        let res = land_now(&forge);
        if matches!(shape, CleanMerge) {
            assert_landed(res, "both clean hops are the accepted candidate");
            assert_eq!(forge.sent("pr.merge")[0]["reviewed_head_sha"], second);
        } else {
            res.expect_err("a clean outer merge cannot hide an invalid inner hop");
            forge.assert_none(&["pr.merge"], "invalid inner hop");
        }
    }
}

/// [ORB-16322] [ORB-16890] Surviving inherited failures repair once, requeue if spent, or hold on red tip.
fn an_inherited_failure_that_survives_the_branch_update_gets_one_repair() {
    let run = |label: &str, spent: bool, tip_ok: bool, after: &'static str| {
        let rep = tip_ok && !spent;
        let sig = pick(
            after == CANDIDATE_ERROR,
            "cannot find value `delivered` in this scope",
            "function `stale_helper` is never used",
        );
        let (_sb, forge, cand, base) = forge_fixture(true, spent);
        let fixed = fix_base(&forge.repo);
        let tip = pick(tip_ok, passed(3, 31), failed(3, 31, CLIPPY_DEAD_CODE));
        inherited_jobs(&forge, &cand, &base, &fixed, tip);
        forge.set_after_update(failed(4, 41, after));

        let err = land_now(&forge).expect_err("the surviving failure stops the landing");
        if spent && tip_ok {
            assert_candidate_requeue_held(&err, &forge, &cand, label);
        } else if tip_ok {
            assert!(
                !matches!(&err, OrbitError::DeliveryRefused { .. }),
                "{label}: {err}"
            );
        }
        forge.assert_sent("pr.update_branch", 1, label);
        forge.assert_none(&["ci.rerun_failed", "pr.merge"], label);
        let step = pick(spent && tip_ok, StopForRequeue, Stop { repairable: rep });
        forge.assert_step(step, label);
        let r = forge.stop_reason();
        let ex = "Process completed with exit code 101";
        assert_contains(&r, &["CI / clippy", sig, ex], label);
        if rep {
            assert!(
                r.contains("automatic repair") && !r.contains("stays in review"),
                "{label}: {r}"
            );
        } else if spent {
            let p = [
                "automatic repair is already spent",
                "returns the task to the backlog",
            ];
            assert_contains(&r, &p, label);
        } else {
            let ok = r.contains("base never recovered") && r.contains("stays in review");
            assert!(ok && !r.contains("automatic repair"), "{label}: {r}");
        }
        assert!(err.to_string().contains(sig), "{label}: {err}");
    };
    run(
        "candidate diagnostic on a green tip",
        false,
        true,
        CANDIDATE_ERROR,
    );
    run("repair already spent", true, true, CANDIDATE_ERROR);
    run(
        "the base diagnostic survives onto a green tip",
        false,
        true,
        CLIPPY_DEAD_CODE,
    );
    run("the tip is red too", false, false, CLIPPY_DEAD_CODE);
}

/// [ORB-23953] [model ranking §5.1] Candidate-caused failures rerun once when citing no changed file, then requeue held.
fn a_candidate_caused_check_failure_requeues_the_candidate() {
    for (spent, base_ran) in [(false, true), (true, true), (false, false)] {
        let (_sb, forge, cand, base) = forge_fixture(false, spent);
        forge.job(&cand, failed(1, 11, CANDIDATE_ERROR));
        forge.set_after_rerun(failed(1, 12, CANDIDATE_ERROR));
        if base_ran {
            forge.job(&base, passed(2, 21));
        }

        let err = land_now(&forge).expect_err("the candidate's failure stops the landing");
        assert_candidate_requeue_held(&err, &forge, &cand, "candidate-caused");
        forge.assert_step(StopForRequeue, "candidate-caused");
        let sig = "cannot find value `delivered` in this scope";
        let ex = "Process completed with exit code 101";
        forge.assert_reason("candidate-caused", &["classified `candidate`", sig, ex]);
        assert!(err.to_string().contains("classified `candidate`"), "{err}");
        forge.assert_none(NO_MERGE_OR_UPDATE, "candidate failure");
        forge.assert_sent("ci.rerun_failed", 1, "candidate failure");
        let ledger = ci_failure_ledger(&forge.history.lock().unwrap());
        let [e] = ledger.as_slice() else {
            panic!("one recorded failure, got {ledger:?}");
        };
        assert_eq!(e.record.class, CiFailureClass::Unknown);
        assert_eq!(e.class, CiFailureClass::Code);
        assert_eq!(e.record.head_sha, cand);
        assert_eq!(e.changes.len(), 1, "candidate failure: {e:?}");
    }
}

fn untouched_failure_requires_the_same_signature_unchanged_head_rerun() {
    const UNTOUCHED: &str = "error[vulnerability]: allocator shim corrupts memory\n    ┌─ /home/runner/work/repo/repo/README.md:365:1\n";
    const CHANGED: &str = "error[vulnerability]: allocator shim corrupts memory\n    ┌─ /home/runner/work/repo/repo/work.txt:365:1\n";
    const UNCHANGED_GH: &str = "error[workflow]: invalid action pin\n    ┌─ /home/runner/work/repo/repo/.github/workflows/ci.yml:12:1\n";
    const CASES: &str =
        "stale changed fresh_green fresh_red github_stale github_changed stale_pass";
    for case in CASES.split_whitespace() {
        let (_sb, mut forge, orig, base) = forge_fixture(false, false);
        let cand = if case == "github_changed" {
            change_candidate_workflow(&mut forge)
        } else {
            orig
        };
        let forge = Arc::new(forge);
        let log = match case {
            "changed" => CHANGED,
            "github_stale" | "github_changed" => UNCHANGED_GH,
            _ => UNTOUCHED,
        };
        let reruns = matches!(case, "fresh_green" | "stale" | "github_stale");
        forge.job(&cand, failed(1, 11, log));
        let bjob = pick(
            case == "fresh_red",
            failed(2, 21, CLIPPY_DEAD_CODE),
            passed(2, 21),
        );
        forge.job(&base, bjob);
        if reruns {
            let rlog = pick(case == "github_stale", UNCHANGED_GH, UNTOUCHED);
            forge.set_after_rerun(failed(1, 12, rlog));
        } else if case == "stale_pass" {
            forge.set_after_rerun(passed(1, 12));
        }
        set_times(&forge, &[(11, "14:00", "14:01"), (21, "12:00", "12:01")]);
        let (cap, ref_base) = (forge.clone(), base.clone());
        let input = json!({"max_wait_seconds": 60, "poll_interval_seconds": 30});
        let outcome = with_vcs_retry_wait(
            move |_| {
                if case.starts_with("fresh_") {
                    cap.with_state(|s| {
                        let fg = case == "fresh_green";
                        let j = pick(fg, passed(3, 31), failed(3, 31, UNTOUCHED));
                        s.jobs.insert(ref_base.clone(), j);
                        s.job_times.insert(31, (at("14:02"), at("14:03")));
                    });
                }
            },
            || land_with(forge.as_ref(), input),
        );
        if case == "stale_pass" {
            assert_landed(outcome, "a passing same-head rerun clears the failure");
            forge.assert_sent("ci.rerun_failed", 1, case);
            assert_eq!(ledger_classes(&forge), [CiFailureClass::Flaky], "{case}");
            continue;
        }
        let err = outcome.expect_err(case);
        if case == "fresh_red" {
            assert!(is_base_pending(&err), "{case}: no job recovery: {err}");
        }
        let changed = matches!(case, "changed" | "github_changed");
        forge.assert_step(pick(reruns || changed, StopForRequeue, STOP), case);
        forge.assert_none(NO_MERGE_OR_UPDATE, case);
        forge.assert_sent("ci.rerun_failed", usize::from(reruns), case);
        if changed {
            forge.assert_reason(case, &["classified `candidate`"]);
        } else if reruns {
            forge.assert_reason(case, &["classified `candidate`"]);
            assert_eq!(
                forge.history_count("ci_base_suspect"),
                0,
                "{case}: no suspect hold"
            );
        } else {
            assert_eq!(
                forge.history_count("ci_base_suspect"),
                1,
                "{case}: suspect once"
            );
            let cj = forge.sent("ci.commit_jobs");
            assert!(
                cj.iter().filter(|i| i["commit"] == base).count() >= 2,
                "{case}"
            );
            assert_eq!(case, "fresh_red");
            forge.assert_reason(case, &["classified `inherited`"]);
        }
    }
}

fn untouched_failure_escalates_after_configured_base_ci_cycles() {
    let (_sb, forge, cand, base) = forge_fixture(false, false);
    forge.job(&cand, failed(1, 11, UNTOUCHED_TEST_FAILURE));

    let err = land_cycles(&forge, 2).expect_err("the unresolved base evidence is held");
    let esc = "escalated after 2 base CI observation cycles";
    assert!(
        is_base_pending(&err) && err.to_string().contains(esc),
        "held: {err}"
    );
    assert_eq!(evidence_of(&forge)["waiting_gate"], "base_checks_pending");
    let parts = ["CI / clippy", "shutdown test timed out", "agent-main"];
    forge.assert_reason("base cycle limit", &parts);
    forge.assert_step(STOP, "base cycle limit");
    let cj = forge.sent("ci.commit_jobs");
    let obs = cj
        .iter()
        .filter(|i| i["commit"] == base && i["latest_push_branch"] == "main")
        .count();
    assert_eq!(
        obs, 3,
        "one observation per cycle plus final base proof (ORB-25926)"
    );
    forge.assert_none(NO_RERUN_UPDATE_OR_MERGE, "base cycle limit");
}

/// ORB-25809 / ORB-25871: a proven green base at end of observation reclassifies base-suspect to candidate requeue.
fn base_suspect_failure_on_a_proven_green_base_requeues_the_candidate() {
    for case in ["proven green", "no base pass", "red tip"] {
        let (_sb, forge, cand, base) = forge_fixture(false, false);
        let mut head_job = failed(1, 11, UNTOUCHED_TEST_FAILURE);
        head_job.event = "push";
        forge.job(&cand, head_job);
        if case != "no base pass" {
            forge.job(&base, passed(2, 21));
        }
        if case == "red tip" {
            let tip = fix_base(&forge.repo);
            forge.job(&tip, failed(3, 31, UNTOUCHED_TEST_FAILURE));
        }
        let ts = [
            (11, "14:00", "14:01"),
            (21, "13:00", "13:01"),
            (31, "13:30", "13:31"),
        ];
        set_times(&forge, &ts);

        let err = land_cycles(&forge, 2).expect_err(case);
        forge.assert_none(NO_RERUN_UPDATE_OR_MERGE, case);
        let hist = forge.history.lock().unwrap();
        let reclass: Vec<_> = hist
            .iter()
            .filter(|r| r.event == CI_BASE_SUSPECT_RECLASSIFIED_EVENT)
            .filter_map(|r| CiBaseSuspectReclassification::from_history_note(r.note.as_deref()))
            .collect();
        drop(hist);
        if case == "proven green" {
            assert_candidate_requeue_held(&err, &forge, &cand, case);
            forge.assert_step(StopForRequeue, case);
            let p = ["classified `candidate`", "base-suspect observation ended"];
            forge.assert_reason(case, &p);
            let [r] = reclass.as_slice() else {
                panic!("{case}: got {reclass:?}");
            };
            assert_eq!(r.base_run_id, Some(2));
            assert_eq!(
                (r.base_sha.as_str(), r.check.as_str()),
                (base.as_str(), "CI / clippy")
            );
            assert_eq!(
                (r.head.as_str(), r.from.as_str(), r.to.as_str()),
                (cand.as_str(), "base-suspect", "candidate")
            );
        } else {
            let esc = "escalated after 2 base CI observation cycles";
            assert!(
                is_base_pending(&err) && err.to_string().contains(esc),
                "{case}: {err}"
            );
            forge.assert_step(STOP, case);
            assert!(
                reclass.is_empty(),
                "{case}: nothing is reclassified: {reclass:?}"
            );
        }
    }
}

fn untouched_failure_on_a_moved_tip_follows_the_fresh_base_check() {
    const UNTOUCHED: &str = "error[vulnerability]: allocator shim corrupts memory\n    ┌─ /home/runner/work/repo/repo/README.md:365:1\n";
    for (case, tip_ok) in [("fresh pass", true), ("fresh same failure", false)] {
        let (_sb, forge, cand, base) = forge_fixture(false, false);
        let fixed = fix_base(&forge.repo);
        forge.job(&cand, failed(1, 11, UNTOUCHED));
        forge.job(&base, passed(2, 21));
        forge.job(
            &fixed,
            pick(tip_ok, passed(3, 31), failed(3, 31, UNTOUCHED)),
        );
        if tip_ok {
            forge.set_after_rerun(failed(1, 12, UNTOUCHED));
        } else {
            forge.set_after_update(failed(4, 41, UNTOUCHED));
        }
        set_times(
            &forge,
            &[
                (11, "14:00", "14:01"),
                (21, "12:00", "12:01"),
                (31, "14:02", "14:03"),
                (41, "14:04", "14:05"),
            ],
        );

        let err = land_now(&forge).expect_err(case);

        assert!(err.to_string().contains("allocator shim"), "{case}: {err}");
        forge.assert_none(&["pr.merge"], case);
        forge.assert_sent("ci.rerun_failed", usize::from(tip_ok), case);
        if tip_ok {
            forge.assert_none(&["pr.update_branch"], case);
            forge.assert_step(StopForRequeue, case);
            forge.assert_reason(case, &["classified `candidate`"]);
            assert_eq!(
                forge.history_count("ci_base_suspect"),
                0,
                "{case}: not base-suspect"
            );
        } else {
            forge.assert_sent("pr.update_branch", 1, case);
            forge.assert_step(STOP, case);
            let parts = [
                "classified `inherited`",
                "base never recovered",
                "stays in review",
            ];
            forge.assert_reason(case, &parts);
        }
    }
}

/// ORB-21586: untouched failure with passing base repairs/requeues when its retry stays unreadable.
fn untouched_failure_with_a_passing_base_repairs_when_its_retry_is_unreadable() {
    for (case, spent) in [("repair available", false), ("repair spent", true)] {
        let (_sb, forge, cand, base) = forge_fixture(false, spent);
        forge.job(&cand, failed(1, 11, UNTOUCHED_TEST_FAILURE));
        forge.job(&base, passed(2, 21));
        forge.with_state(|s| s.rerun_plan_error = Some(RETRY_UNREADABLE));
        set_times(&forge, &[(11, "14:00", "14:01"), (21, "12:00", "12:01")]);

        let (outcome, polls) = land_counting_polls(&forge);
        let err = outcome.expect_err(case);
        assert!(
            !is_base_pending(&err),
            "{case}: must not wait out deadline: {err}"
        );
        assert_eq!(polls, 2, "{case}: escalate on the third observation cycle");
        forge.assert_sent("ci.rerun_plan", 3, case);
        forge.assert_none(NO_RERUN_UPDATE_OR_MERGE, case);
        forge.assert_step(pick(spent, StopForRequeue, Stop { repairable: true }), case);
        let parts = [
            "classified `flaky`",
            "the failure is the candidate's own",
            "retry requires a completed failed attempt",
        ];
        forge.assert_reason(case, &parts);
    }
}

/// ORB-21586: unreadable retry evidence without a base pass stops in review naming the read failure.
fn unreadable_retry_evidence_stops_the_landing_naming_the_read_failure() {
    let (_sb, forge, cand, _) = forge_fixture(false, false);
    forge.job(&cand, failed(1, 11, NETWORK_FLAKE));
    forge.with_state(|s| s.rerun_plan_error = Some(RETRY_UNREADABLE));

    let (outcome, polls) = land_counting_polls(&forge);
    let err = outcome.expect_err("unreadable retry evidence stops the landing");
    assert!(!is_base_pending(&err), "must not wait out deadline: {err}");
    assert_eq!(polls, 2, "stop on the third observation cycle");
    forge.assert_step(STOP, "unreadable retry");
    let parts = [
        "could not be read after 3 observation cycles",
        "retry requires a completed failed attempt",
    ];
    forge.assert_reason("unreadable retry", &parts);
    assert!(!forge.stop_reason().contains("waiting for"));
    forge.assert_none(NO_RERUN_UPDATE_OR_MERGE, "unreadable retry");
}

fn assert_candidate_requeue_held(error: &OrbitError, forge: &FakeForge, cand: &str, case: &str) {
    let OrbitError::DeliveryRefused { refusal, message } = error else {
        panic!("{case}: a candidate requeue holds the landing instead of failing: {error}");
    };
    assert_eq!(
        *refusal,
        DeliveryRefusal::LandingRepairRequeue,
        "{case}: {message}"
    );
    assert_eq!(refusal.error_code(), "delivery_landing_repair_requeue");
    let rec = match HeldFailure::of(Some(refusal.error_code()), Some(message)) {
        Some(HeldFailure::DeliveryRefused(actual)) => {
            assert_eq!(actual, DeliveryRefusal::LandingRepairRequeue, "{case}");
            JobRunState::Held
        }
        other => panic!("{case}: expected held outcome, got {other:?}"),
    };
    assert_eq!(rec.to_string(), "held", "{case}");
    assert_ne!(rec, JobRunState::Failed, "{case}");
    assert_ne!(rec, JobRunState::Interrupted, "{case}");
    let orbit_types::workflow::handoff::LandingStopEvidence::OwnerRefusal(ev) =
        orbit_types::workflow::handoff::LandingStopEvidence::parse(&forge.last_evidence())
    else {
        panic!("{case}: requeue evidence is the owner landing refusal");
    };
    assert_eq!(ev.requeue, Some(true), "{case}: {ev:?}");
    let phrase = "returns the task to the backlog";
    assert!(
        ev.reason.value.contains(phrase),
        "{case}: {}",
        ev.reason.value
    );
    assert_eq!(
        ev.accepted["candidate"]["value"].as_str(),
        Some(cand),
        "{case}: {}",
        ev.accepted
    );
    assert!(
        message.contains("handoff_land:") && error.to_string().contains(phrase),
        "{case}: {error}"
    );
}

fn an_unreadable_log_and_a_spent_transient_rerun_hold_the_landing() {
    let (_sb, forge, cand, _) = forge_fixture(false, false);
    forge.job(&cand, failed_reading(1, 11, FakeLog::Unreadable));
    let err = land_now(&forge).expect_err("an unreadable log stays resumable in review");
    let is_unavail = matches!(
        err,
        OrbitError::DeliveryRefused {
            refusal: DeliveryRefusal::ForgeObservationUnavailable,
            ..
        }
    );
    assert!(is_unavail, "unreadable log: {err}");
    let p1 = [
        "the failure was not classified",
        "the log of 'CI / clippy' could not be read",
    ];
    assert_contains(&err.to_string(), &p1, "unreadable log");
    forge.assert_step(STOP, "unreadable log");
    forge.assert_none(NO_RERUN_UPDATE_OR_MERGE, "unreadable log");

    let (_sb, forge, cand, _) = forge_fixture(false, false);
    forge.job(&cand, failed(1, 11, NETWORK_FLAKE));
    forge.with_state(|s| s.extra_unanswered_check = true);
    let err = land_now(&forge).expect_err("a spent transient rerun stays in review");
    let is_inc = matches!(
        err,
        OrbitError::DeliveryRefused {
            refusal: DeliveryRefusal::CheckEvidenceInconclusive,
            ..
        }
    );
    assert!(is_inc, "spent transient: {err}");
    let p2 = [
        "classified `transient`",
        "it failed again after its one rerun, so the candidate stays in review",
    ];
    assert_contains(&err.to_string(), &p2, "spent transient");
    forge.assert_step(STOP, "spent transient");
    forge.assert_none(NO_RERUN_UPDATE_OR_MERGE, "spent transient");
}

fn a_transient_check_failure_respects_the_configured_retry_limit() {
    for (rerun, lands) in [(passed(1, 12), true), (failed(1, 12, NETWORK_FLAKE), false)] {
        let (_sb, mut forge, cand, _) = forge_fixture(true, false);
        forge.ci_retry_policy.max_attempts = 1;
        forge.job(&cand, failed(1, 11, NETWORK_FLAKE));
        forge.set_after_rerun(rerun);

        let outcome = land_now(&forge);
        let sent = forge.sent("ci.rerun_failed");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["run_id"], "1");
        assert_eq!(sent[0]["plan"]["head_sha"], cand);
        assert_eq!(sent[0]["plan"]["failed_jobs"][0]["id"], 11);
        forge.assert_none(&["pr.update_branch"], "transient");
        assert_eq!(ledger_classes(&forge), [CiFailureClass::Infrastructure]);
        if lands {
            assert_landed(outcome, "the rerun passes");
        } else {
            let err = outcome.expect_err("a second transient failure stops");
            assert!(
                !matches!(&err, OrbitError::DeliveryRefused { .. }),
                "transient: {err}"
            );
            forge.assert_step(STOP, "transient");
            let parts = [
                "ci_infrastructure_retry_exhausted",
                "classification=transient",
                "attempts=1",
                "limit=1",
            ];
            forge.assert_reason("transient", &parts);
            let holds = forge.history_notes("ci_infrastructure_retry_exhausted");
            assert_eq!(holds.len(), 1);
            assert_eq!(holds[0]["classification"], "transient");
            assert_eq!(holds[0]["attempts"], 1);
        }
    }
}

#[cfg(unix)]
fn transient_non_pull_request_rerun_is_refused_without_retrying() {
    const GH: &str = "#!/bin/sh\nset -eu\ncase \"$*\" in\n *'graphql'*) cat gh-policy.json ;;\n *'pulls/42'*) cat gh-pr.json ;;\n *'rules/branches/'*) cat gh-rules.json ;;\n *'check-runs'*) cat gh-checks.json ;;\n *'/status'*) cat gh-statuses.json ;;\n *'/actions/runs?'*) cat gh-runs.json ;;\n *'/actions/runs/1'*) cat gh-run.json ;;\n *'repos/{owner}/{repo}') cat gh-repo.json ;;\n *) exit 1 ;;\nesac\n";
    for event in ["push", "workflow_dispatch"] {
        let (_sb, forge, cand, base) = forge_fixture(true, false);
        let mut failure = failed(1, 11, NETWORK_FLAKE);
        failure.event = event;
        forge.job(&cand, failure);
        forge.with_state(|s| s.real_rerun_plan = true);

        let rj = json!({"id":1,"full_name":"owner/repository","url":"https://api.github.com/repos/owner/repository"});
        let w = |n: &str, v: Value| fs::write(forge.repo.join(n), v.to_string()).unwrap();
        w("gh-repo.json", rj.clone());
        w(
            "gh-pr.json",
            json!({"number":42,"state":"open","merged":false,"head":{"sha":cand,"ref":"orbit/T1","repo":rj},"base":{"sha":base,"ref":"main","repo":rj}}),
        );
        w(
            "gh-policy.json",
            json!({"data":{"repository":{"pullRequest":{"headRefOid":cand,"baseRefName":"main","reviewDecision":null,"baseRef":{"branchProtectionRule":{"requiredStatusChecks":[{"context":"clippy","app":{"databaseId":15368}}]}}}}}}),
        );
        w("gh-rules.json", json!([]));
        w(
            "gh-checks.json",
            json!({"total_count":1,"check_runs":[{"id":100,"head_sha":cand,"name":"clippy","app":{"id":15368,"slug":"github-actions"},"status":"completed","conclusion":"failure","check_suite":{"id":10}}]}),
        );
        w(
            "gh-statuses.json",
            json!({"sha":cand,"repository":rj,"total_count":0,"statuses":[]}),
        );
        w(
            "gh-runs.json",
            json!({"total_count":1,"workflow_runs":[{"id":1,"workflow_id":1,"run_attempt":1}]}),
        );
        w(
            "gh-run.json",
            json!({"id":1,"workflow_id":1,"run_attempt":1,"repository":{"id":1},"head_repository":{"id":1},"head_sha":cand,"head_branch":"orbit/T1","event":event}),
        );
        let _env = install_fake_gh(&forge.repo, GH);

        let err = land_with(
            &forge,
            json!({"max_wait_seconds": 0, "max_base_ci_cycles": 1}),
        )
        .expect_err("refused");
        assert!(err.to_string().contains("workflow event"), "{event}: {err}");
        forge.assert_step(STOP, event);
        forge.assert_reason(
            event,
            &["workflow event", "nothing is rerun, updated or repaired"],
        );
        forge.assert_none(NO_RERUN_UPDATE_OR_MERGE, event);
        assert_eq!(
            forge.history_count("ci_check_rerun"),
            0,
            "{event}: no rerun recorded"
        );
    }
}

/// [ORB-15970] Unreadable jobs/logs or diagnostic-free logs stop in review and spend nothing.
fn a_check_failure_whose_evidence_cannot_be_read_spends_nothing() {
    use FakeLog::{Text, Unreadable, Unrecovered};
    let run = |case: &str, h_log: FakeLog, b_log: Option<FakeLog>, unrd: u8, why: &str| {
        let (_sb, forge, cand, base) = forge_fixture(true, false);
        let fixed = fix_base(&forge.repo);
        forge.job(&cand, failed_reading(1, 11, h_log));
        let bj = b_log.map_or_else(|| passed(2, 21), |l| failed_reading(2, 21, l));
        forge.job(&base, bj);
        forge.job(&fixed, passed(3, 31));
        forge.with_state(|s| {
            if unrd == 1 {
                s.unreadable.push(base);
            } else if unrd == 2 {
                s.unreadable.push(fixed);
            }
        });
        land_now(&forge).expect_err("unreadable evidence stops the landing");
        forge.assert_step(STOP, case);
        forge.assert_reason(case, &[why, "stays in review"]);
        forge.assert_none(NO_RERUN_UPDATE_OR_MERGE, case);
        assert_eq!(ledger_classes(&forge), [CiFailureClass::Unknown], "{case}");
    };
    let (dc, ce, em) = (Text(CLIPPY_DEAD_CODE), Text(CANDIDATE_ERROR), Text(""));
    run("base jobs unreadable", dc, Some(dc), 1, "the jobs on base");
    run(
        "current base jobs unreadable",
        ce,
        None,
        2,
        "the jobs on the current base",
    );
    run("base log unreadable", dc, Some(Unreadable), 0, "HTTP 502");
    run(
        "base log recovered no text",
        dc,
        Some(Unrecovered),
        0,
        "recovered no text",
    );
    run(
        "head log recovered no text",
        Unrecovered,
        Some(em),
        0,
        "recovered no text",
    );
    run("step names only", em, Some(em), 0, "names no diagnostic");
}

/// [ORB-15970] A branch update is verified against the freshly fetched landing base it merged.
fn a_branch_update_is_verified_against_the_base_it_merged() {
    use UpdateShape::{BaseGone, CleanMerge, Dirty, Foreign, NotAMerge};
    for shape in [CleanMerge, Foreign, Dirty, NotAMerge, BaseGone] {
        let lands = matches!(shape, CleanMerge);
        let (_remote, _sb, forge, cand, base) = published_forge(true, true);
        let fixed = git_rev(&forge.repo, "main");
        inherited_jobs(&forge, &cand, &base, &fixed, passed(3, 31));
        forge.with_state(|s| {
            s.after_update = Some(passed(4, 41));
            s.update = shape;
        });

        let outcome = land_now(&forge);
        forge.assert_sent("pr.update_branch", 1, "update");
        let head = head_of(&forge);
        if lands {
            assert_landed(outcome, &format!("{shape:?}"));
            assert_eq!(
                forge.sent("pr.merge")[0]["reviewed_head_sha"],
                head.as_str()
            );
        } else {
            outcome.expect_err("an unverified head stops the landing");
            forge.assert_step(STOP, "unverified");
            forge.assert_none(&["pr.merge"], "unverified");
        }
    }
}

/// [ORB-15976] A cached branch update cannot land if the initial fetch fails; restoring origin lands it.
fn a_cached_branch_update_requires_a_fresh_landing_base() {
    let (remote, _sb, forge, cand, _) = published_forge(true, false);
    let head = provider_update(remote.path(), &cand, UpdateShape::CleanMerge);
    git(&forge.repo, &["fetch", "-q", "origin"]);
    let base = git_rev(&forge.repo, "origin/main");
    let parents = git(&forge.repo, &["rev-list", "--parents", "-n", "1", &head]);
    assert_eq!(
        parents,
        format!("{head} {cand} {base}"),
        "cached before fetch fails"
    );
    forge.with_state(|s| s.head = head.clone());
    forge.job(&head, passed(1, 11));
    let bad = remote
        .path()
        .join("unreachable.git")
        .to_string_lossy()
        .into_owned();
    git(&forge.repo, &["remote", "set-url", "origin", &bad]);

    land_now(&forge).expect_err("a cached branch update cannot land after the initial fetch fails");

    forge.assert_step(STOP, "cached");
    forge.assert_none(NO_RERUN_UPDATE_OR_MERGE, "unreachable origin");
    assert_eq!(git_rev(&forge.repo, "origin/main"), base);
    assert_eq!(head_of(&forge), head);

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

    assert_landed(
        land_now(&forge),
        "the identical cached head lands once origin is reachable",
    );
    forge.assert_sent("pr.merge", 1, "cached");
    assert_eq!(
        forge.sent("pr.merge")[0]["reviewed_head_sha"],
        head.as_str(),
        "cached"
    );
    forge.assert_none(NO_RERUN_OR_UPDATE, "cached");
    assert_eq!(git_rev(&forge.repo, "origin/main"), base);
    let ev = evidence_of(&forge);
    assert_eq!(ev["updated_head"], head.as_str());
    assert!(ev.get("check_recovery").is_none());
    assert_eq!(forge.steps().last(), Some(&Complete));
}

/// [ORB-16185] Required aggregate annotations classify candidate vs inconclusive without spending recovery.
fn a_required_aggregate_failure_is_classified_from_its_source_annotations() {
    let run = |case: &str, clippy: FakeJob, annotated: &'static str, spent: bool| {
        let (_sb, forge, cand, _) = forge_fixture(false, spent);
        forge.job(&cand, clippy);
        forge.with_state(|s| s.aggregate = Some(annotated));

        let err = land_now(&forge).expect_err("a failed required aggregate stops the landing");
        let is_cand = matches!(case, "candidate" | "spent");
        forge.assert_step(pick(is_cand, StopForRequeue, STOP), case);
        let reason = forge.stop_reason();
        let class = if is_cand {
            assert_candidate_requeue_held(&err, &forge, &cand, case);
            assert_eq!(
                reason.contains("the automatic repair is already spent"),
                case == "spent",
                "{case}: {reason}"
            );
            "classified `candidate`"
        } else {
            "classified `inconclusive`"
        };
        let ann = format!("- {annotated}:1 [Uncontrolled data used in path expression]");
        assert_contains(&reason, &[class, "check run 7001 of app 57789", &ann], case);
        forge.assert_none(NO_RERUN_UPDATE_OR_MERGE, case);
    };
    run("candidate", passed(1, 11), "work.txt", false);
    run("spent", passed(1, 11), "work.txt", true);
    run("base_file", passed(1, 11), "README.md", false);
    run(
        "beside_transient",
        failed(1, 11, NETWORK_FLAKE),
        "README.md",
        false,
    );
}

fn required_aggregate_after_recovery(updated: bool) {
    let (_sb, forge, cand, base) = forge_fixture(true, false);
    if updated {
        let fixed = fix_base(&forge.repo);
        inherited_jobs(&forge, &cand, &base, &fixed, passed(3, 31));
        forge.set_after_update(passed(4, 41));
    } else {
        forge.job(&cand, failed(1, 11, NETWORK_FLAKE));
        forge.set_after_rerun(passed(1, 12));
    }
    forge.with_state(|s| s.after_recovery_aggregate = Some("work.txt"));

    land_now(&forge).expect_err("the failed aggregate stops for repair without waiting");

    forge.assert_sent("ci.rerun_failed", usize::from(!updated), "recovery");
    forge.assert_sent("pr.update_branch", usize::from(updated), "recovery");
    forge.assert_sent("pr.status", 2, "recovery");
    forge.assert_sent("ci.check_annotations", 1, "recovery");
    forge.assert_step(StopForRequeue, "recovery");
    forge.assert_reason("recovery", &["classified `candidate`", "check run 7001"]);
    forge.assert_none(&["pr.merge"], "recovery");
}

#[allow(clippy::print_stderr)] // Identify each labelled row in isolated child failure output.
fn a_required_aggregate_after_recovery_is_classified_without_waiting() {
    for (label, updated) in [("after rerun", false), ("after branch update", true)] {
        eprintln!("aggregate recovery row: {label}");
        required_aggregate_after_recovery(updated);
    }
}

fn a_transient_rerun_waits_for_queued_replacement_jobs() {
    let (_sb, forge, head, _) = forge_fixture(true, false);
    let forge = Rc::new(forge);
    forge.job(&head, failed(1, 11, NETWORK_FLAKE));
    forge.with_state(|s| {
        s.after_rerun = Some(passed(1, 12));
        s.queued_rerun = true;
    });
    let (waits, q_forge) = (Rc::new(Cell::new(0)), forge.clone());
    let cap_waits = waits.clone();
    let res = with_vcs_retry_wait(
        move |_| {
            cap_waits.set(cap_waits.get() + 1);
            assert!(
                q_forge.steps().is_empty(),
                "queued work must not stop for repair"
            );
            q_forge.assert_none(&["pr.merge"], "queued");
            q_forge.assert_sent("ci.rerun_failed", 1, "queued");
            q_forge.with_state(|s| {
                assert!(
                    !s.jobs.contains_key(&head),
                    "replacement has not registered"
                );
                let rep = s.after_rerun.take().unwrap();
                s.jobs.insert(head.clone(), rep);
                s.queued_rerun = false;
            });
        },
        || {
            land_with(
                forge.as_ref(),
                json!({"max_wait_seconds": 15, "poll_interval_seconds": 5}),
            )
        },
    );
    assert_landed(res, "the queued rerun eventually passes");
    assert_eq!(waits.get(), 1, "wait only while the replacement is queued");
    forge.assert_sent("pr.merge", 1, "queued rerun");
    forge.assert_none(&["pr.update_branch"], "queued rerun");
    assert_eq!(forge.steps().last(), Some(&Complete));
}

fn git(dir: &Path, args: &[&str]) -> String {
    let cfg = [
        "-c",
        "user.name=Orbit Test",
        "-c",
        "user.email=test@orbit.invalid",
        "-c",
        "commit.gpgsign=false",
    ];
    let out = crate::git_test_support::command()
        .arg("-C")
        .arg(dir)
        .args(cfg)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

// Behavior-distinct rows share one bounded family process.
scenario_family! {
    handoff_landing_contracts_1:
        a_local_candidate_landing_names_its_task_before_the_branch_moves,
        owner_local_landing_refuses_and_preserves_cache_shaped_work,
        a_candidate_that_cannot_fast_forward_records_no_landing_intent,
        a_mismatched_merged_pr_resolves_the_intent_before_stopping_later_attempts,
        an_already_landed_landing_holds_when_its_evidence_went_stale,
        handoff_fake_refuses_unscoped_automation_updates,
}
scenario_family! {
    handoff_landing_contracts_2:
        landing_refusals_keep_typed_holds_and_candidate_red_stays_failed,
        only_a_conflicting_or_stale_base_stops_a_pull_request_for_repair,
        an_owed_hosted_check_must_pass_before_the_owner_merges,
}
scenario_family! {
    handoff_landing_contracts_3:
        owner_landing_waits_for_exact_required_checks_before_one_verified_merge,
        owner_landing_waits_out_pending_only_block_before_checks_pass,
        owner_landing_waits_for_required_checks_the_head_has_not_reported,
        owner_landing_pending_evidence_never_extends_refusal_or_merge_authority,
        landing_poll_uses_the_completion_clock_for_elapsed_provider_time,
}
scenario_family! {
    handoff_landing_contracts_4:
        a_cached_branch_update_requires_a_fresh_landing_base,
        a_required_aggregate_after_recovery_is_classified_without_waiting,
        a_transient_rerun_waits_for_queued_replacement_jobs,
        owner_landing_waits_for_unknown_then_clean_and_retains_expired_or_blocked_evidence,
        externally_merged_base_chain_lands_after_fetching_the_retained_pull_ref,
        handoff_merge_in_progress_contracts,
}
#[cfg(unix)]
scenario_family! {
    handoff_landing_contracts_5:
        adopted_provider_head_accepts_readable_empty_policy,
        adopted_provider_head_accepts_authentic_legacy_success,
        adopted_provider_head_refuses_unavailable_or_unsatisfied_policy,
}
scenario_family! {
    handoff_landing_contracts_6:
        untouched_failure_reruns_once_after_fresh_base_pass,
        suspicion_only_history_cannot_admit_repair_from_a_non_pr_run,
        untouched_failure_uses_a_fresh_pass_behind_the_moving_tip,
        untouched_failure_requires_the_same_signature_unchanged_head_rerun,
        untouched_failure_escalates_after_configured_base_ci_cycles,
        base_suspect_failure_on_a_proven_green_base_requeues_the_candidate,
        untouched_failure_on_a_moved_tip_follows_the_fresh_base_check,
        a_candidate_caused_check_failure_requeues_the_candidate,
}
scenario_family! {
    handoff_landing_contracts_7:
        a_check_failure_inherited_from_the_base_updates_the_branch_and_lands,
        inherited_landing_waits_for_a_later_passing_base_and_updates_again,
        landing_accepts_two_clean_operator_base_merges_and_refuses_a_dirty_hop,
        an_inherited_failure_that_survives_the_branch_update_gets_one_repair,
        a_branch_update_is_verified_against_the_base_it_merged,
}
scenario_family! {
    handoff_landing_contracts_8:
        untouched_failure_with_a_passing_base_repairs_when_its_retry_is_unreadable,
        unreadable_retry_evidence_stops_the_landing_naming_the_read_failure,
        an_unreadable_log_and_a_spent_transient_rerun_hold_the_landing,
        a_transient_check_failure_respects_the_configured_retry_limit,
        a_check_failure_whose_evidence_cannot_be_read_spends_nothing,
        a_required_aggregate_failure_is_classified_from_its_source_annotations,
}
#[cfg(unix)]
scenario_family! {
    handoff_landing_contracts_9:
        untouched_failure_checks_push_history_through_the_real_adapter,
        transient_non_pull_request_rerun_is_refused_without_retrying,
}

#[cfg(unix)]
fn adopted_provider_head_accepts_readable_empty_policy() {
    crate::pr_landing::adopted_policy_cases(&["empty", "no-rule"]);
}
#[cfg(unix)]
fn adopted_provider_head_accepts_authentic_legacy_success() {
    crate::pr_landing::adopted_policy_cases(&["success"]);
}
#[cfg(unix)]
fn adopted_provider_head_refuses_unavailable_or_unsatisfied_policy() {
    const CASES: &str = "unavailable pending failed wrong-head foreign wrong-row foreign-row foreign-host shadow-pending app-bound";
    crate::pr_landing::adopted_policy_cases(&CASES.split_whitespace().collect::<Vec<_>>());
}

fn landing_poll_uses_the_completion_clock_for_elapsed_provider_time() {
    let (_sb, forge, _) = pending_forge(vec![blocked_aggregate()], |h| {
        vec![pending_policy(h, "QUEUED", true)]
    });
    let clock = Rc::new(Cell::new(1_000_u64));
    let (now, waited) = (clock.clone(), clock.clone());
    let waits = Rc::new(RefCell::new(Vec::new()));
    let rec = waits.clone();
    let res = with_prerequisite_clock(
        move || {
            let cur = now.get();
            if cur == 1_000 {
                now.set(7_000);
            }
            cur
        },
        |_, _| {},
        || {
            with_vcs_retry_wait(
                move |d| {
                    rec.borrow_mut().push(d.as_secs());
                    waited.set(waited.get() + d.as_secs() * 1_000);
                },
                || {
                    land_with(
                        &forge,
                        json!({"max_wait_seconds": 10, "poll_interval_seconds": 5}),
                    )
                },
            )
        },
    );
    assert!(res.is_err(), "a pending check must stop at the deadline");
    assert_eq!(
        *waits.borrow(),
        [4],
        "provider time consumes the same budget as waits"
    );
    forge.assert_sent("pr.merge", 0, "pending check");
}

fn owner_landing_waits_for_unknown_then_clean_and_retains_expired_or_blocked_evidence() {
    with_vcs_retry_wait(
        |_| {},
        || {
            let run = |case: &str, states: &[&'static str], review: &'static str| {
                let (_sb, forge, head, _) = forge_fixture(false, false);
                forge.job(&head, passed(1, 11));
                forge.with_state(|s| {
                    s.merge_states = states.iter().copied().collect();
                    s.review_decision = Some(review);
                });
                let max = pick(case == "settles", 15, 0);
                let res = land_with(
                    &forge,
                    json!({"max_wait_seconds": max, "poll_interval_seconds": 5}),
                );
                if case == "settles" {
                    assert_landed(res, case);
                    forge.assert_sent("pr.merge", 1, case);
                    forge.assert_sent("pr.status", 4, case);
                } else {
                    assert!(res.is_err(), "{case}");
                    forge.assert_sent("pr.status", 1, case);
                    forge.assert_none(&["pr.merge"], case);
                    forge.assert_step(STOP, case);
                    assert!(
                        !forge.last_evidence().is_empty(),
                        "retains evidence: {case}"
                    );
                }
            };
            run("settles", &["UNKNOWN", "PENDING", "CLEAN"], "");
            run("expires", &["UNKNOWN"], "");
            run("review", &["BLOCKED"], "REVIEW_REQUIRED");
            run("administrative", &["BLOCKED"], "");
        },
    );
}

fn landing_refusals_keep_typed_holds_and_candidate_red_stays_failed() {
    use DeliveryRefusal as DR;
    let run = |case: &str, st: &'static str, rev: &'static str, rollup: Value, refu: Option<DR>| {
        let mut host = OpenPullRequestHost::new(st, rollup);
        host.review_decision = rev;
        let def = json!([{"id":"land","spec":{"type":"deterministic","action":"handoff_land","config":{}},"default_input":{"handoff_id":"handoff-1","task_id":TASK_ID,"max_wait_seconds":0}}]);
        let job = super::v2_runtime::job_asset(def);
        let audit = tempdir().unwrap();
        let (writer, _, _) = super::v2_runtime::build_writer_and_sinks(audit.path(), RUN_ID);
        let err =
            execute_job_with_resume(&job, json!({}), RUN_ID, writer, &host, None).unwrap_err();
        let got = HeldFailure::of(Some(err.failure_code()), Some(&err.to_string()));
        let exp = refu.map(HeldFailure::DeliveryRefused);
        assert_eq!(got, exp, "{case}: {err}");
        if let Some(refusal) = refu {
            let ok =
                matches!(&err, DispatchError::DeliveryRefused { refusal: a, .. } if *a == refusal);
            assert!(ok, "{case}: {err}");
            assert_eq!(err.failure_code(), refusal.error_code());
            let orb = dispatch_error_to_orbit(err);
            assert!(matches!(orb, OrbitError::DeliveryRefused { refusal: a, .. } if a == refusal));
        }
        assert!(
            host.steps().iter().any(|s| matches!(s, Stop { .. })),
            "{case}"
        );
        assert!(!host.sent_ops().iter().any(|op| op == "pr.merge"), "{case}");
    };
    let (e, f) = (
        json!([]),
        json!([{"name":"test","status":"COMPLETED","conclusion":"FAILURE"}]),
    );
    run(
        "conflict",
        "DIRTY",
        "",
        e.clone(),
        Some(DR::LandingConflict),
    );
    run("draft", "DRAFT", "", e.clone(), Some(DR::PullRequestDraft));
    run(
        "review",
        "BLOCKED",
        "REVIEW_REQUIRED",
        e.clone(),
        Some(DR::RequirementsUnsatisfied),
    );
    run("candidate_red", "BLOCKED", "APPROVED", f, None);
    run("unreadable_review", "BLOCKED", "unknown", e, None);
}

fn externally_merged_base_chain_lands_after_fetching_the_retained_pull_ref() {
    let (remote, _sb, forge, cand, _) = published_forge(false, false);
    let first = provider_update(remote.path(), &cand, UpdateShape::CleanMerge);
    let head = provider_update(remote.path(), &first, UpdateShape::CleanMerge);
    let tree = git_rev(remote.path(), &format!("{head}^{{tree}}"));
    let merged = commit_tree(remote.path(), &tree, &["main"], "external squash merge");
    git(remote.path(), &["update-ref", "refs/heads/main", &merged]);
    git(remote.path(), &["update-ref", "refs/pull/42/head", &head]);
    git(remote.path(), &["update-ref", "-d", "refs/heads/orbit/T1"]);
    forge.with_state(|s| (s.head, s.merged, s.merge_commit) = (head.clone(), true, Some(merged)));
    assert_landed(
        land_now(&forge),
        "a clean externally merged chain completes",
    );
    forge.assert_none(NO_MERGE_OR_UPDATE, "already merged");
    assert_eq!(git_rev(&forge.repo, "FETCH_HEAD"), head);
    let updates = forge.updates.lock().unwrap();
    let comp = updates
        .iter()
        .find(|u| u.step == Complete)
        .expect("completes from external merge");
    let ev: Value = serde_json::from_str(&comp.evidence).unwrap();
    assert_eq!(
        ev["external_merge"], true,
        "completion records external merge"
    );
}

fn handoff_merge_in_progress_contracts() {
    for merged in [true, false] {
        let (_sb, forge, head, _) = forge_fixture(false, false);
        forge.job(&head, passed(1, 11));
        forge.with_state(|s| s.merge_in_progress = Some(merged));
        let (result, waits) = counted_land(&forge, json!({"max_wait_seconds": 30}));
        if merged {
            assert_landed(result, "merged");
        } else {
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("one re-read did not confirm it merged"),
                "{err}"
            );
            assert!(forge.steps().contains(&STOP));
        }
        assert!(
            waits.is_empty(),
            "handoff does not enter completion's polling path"
        );
        forge.assert_sent("pr.merge", 1, "no second mutation");
        forge.assert_sent("pr.status", 2, "one reconciliation read");
    }
}
