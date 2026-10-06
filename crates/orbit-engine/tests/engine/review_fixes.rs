#![allow(missing_docs)]

//! The shipped PR pipeline's before-PR review: one fresh reviewer that fixes
//! its own findings [ORB-13989].
//!
//! Every activity the shipped `task_pr_pipeline` names resolves to a scripted
//! deterministic stand-in, so the job graph itself — the review steps, their
//! `when:` guards, owner revalidation of a reviewer commit, the template
//! wiring into the PR steps, final recovery and the terminal failure handoff
//! — runs exactly as shipped. The gate's own decisions (the two-commit shape,
//! the findings comment, the certificate) are covered at the gate boundary in
//! `orbit-core`; here the settlement stub answers from a script.
//!
//! Runs under `cargo nextest run -p orbit-engine --test engine -E 'test(/^review_fixes::/)'`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use orbit_agent::loop_engine::InMemorySink;
use orbit_common::OrbitError;
use orbit_engine::activity_job::{V2ActivityCatalog, load_job_asset};
use orbit_engine::{
    DispatchError, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
    FinalRecoveryApplied, JobOutcome, RuntimeHost, V2AuditWriter, V2SqliteSink,
    execute_job_with_resume, resolve_job_catalog_refs_for_execution,
};
use orbit_types::workflow::FinalRecoveryDecision;
use orbit_types::workflow::activity_job::{ActivityV2, ActivityV2Spec, DeterministicSpec};
use serde_json::{Value, json};

#[test]
fn a_reviewer_timeout_retains_its_output_and_stops_before_retry_or_publication() {
    let host = ScriptedHost::new(Settlement::Timeout, Revalidation::Passes);
    let result = run_shipped_pipeline(&host);
    assert!(
        !matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    assert_eq!(host.inputs("agent_review_repair").len(), 1);
    assert!(host.inputs("review_gate_settle").is_empty());
    assert!(host.inputs("pr_open").is_empty());
    let handoff = host.inputs("pr_failure_handoff");
    assert_eq!(handoff.len(), 1);
    assert!(
        handoff[0]["error_message"]
            .as_str()
            .unwrap()
            .contains("review_timeout_incomplete:")
    );
    assert_eq!(handoff[0]["pipeline"]["review"]["timed_out"], true);
    assert!(matches!(
        host.reviewer_events.lock().unwrap().as_slice(),
        [
            orbit_types::workflow::ReviewerInvocationEvent::Started { .. },
            orbit_types::workflow::ReviewerInvocationEvent::TimedOut { .. },
        ]
    ));
}

/// No findings: the reviewed head is the implementation head, nothing is
/// revalidated, and the PR opens with no "Review fixes" section.
#[test]
fn accept_publishes_the_implementation_head_without_revalidation() {
    let host = ScriptedHost::new(Settlement::Accept, Revalidation::Passes);
    let result = run_shipped_pipeline(&host);

    assert!(
        matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    assert_eq!(
        host.actions(),
        [
            "worktree_setup",
            "review_gate_admit",
            "candidate_resume",
            "agent_implement",
            "git_commit",
            "pr_prepare",
            "git_rebase",
            "candidate_validate",
            "review_gate_admit",
            "agent_review_repair",
            "review_gate_settle",
            "git_push",
            "pr_open",
            "pr_promote",
            "pr_complete",
            "review_gate_admit",
            "review_gate_settle",
            "review_gate_admit",
            "review_gate_settle",
        ],
        "one review, no revalidation, then delivery; both completion re-review \
         rounds only admit and settle as not applicable"
    );
    let pr_open = &host.inputs("pr_open")[0];
    assert_eq!(pr_open["reviewed_head_sha"], "candidate");
    assert_eq!(pr_open["review_fixes"], "");
    assert!(host.inputs("pr_failure_handoff").is_empty());
}

/// Fixable findings: the reviewer commit becomes the reviewed head. Owner
/// validation reruns on it with the implementation head as the ownership
/// base, and only then is it pushed and opened with the "Review fixes"
/// section the settlement produced.
#[test]
fn accept_with_fixes_revalidates_the_reviewer_commit_before_publishing_it() {
    let host = ScriptedHost::new(Settlement::AcceptWithFixes, Revalidation::Passes);
    let result = run_shipped_pipeline(&host);

    assert!(
        matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    let actions = host.actions();
    let settle = position(&actions, "review_gate_settle");
    assert_eq!(
        &actions[settle..settle + 4],
        [
            "review_gate_settle",
            "candidate_validate",
            "git_push",
            "pr_open"
        ],
        "revalidation sits between settlement and publication: {actions:?}"
    );
    assert_eq!(
        host.inputs("agent_review_repair").len(),
        1,
        "one reviewer, no second round"
    );

    let validations = host.inputs("candidate_validate");
    assert_eq!(validations.len(), 2);
    assert_eq!(validations[0].get("ownership_base_sha"), None);
    assert_eq!(validations[1]["ownership_base_sha"], "candidate");
    assert_eq!(validations[1]["base_sha"], "base-sha");
    assert_eq!(validations[1]["workspace_path"], WORKSPACE);

    let pr_open = &host.inputs("pr_open")[0];
    assert_eq!(pr_open["reviewed_head_sha"], "reviewer-fixes");
    assert_eq!(pr_open["review_fixes"], REVIEW_FIXES);
    assert_eq!(host.inputs("git_push")[0]["workspace_path"], WORKSPACE);
    let complete = &host.inputs("pr_complete")[0];
    assert_eq!(complete["reviewed_head_sha"], "reviewer-fixes");
    assert_eq!(complete["published_head_sha"], "reviewer-fixes");
    assert!(host.inputs("pr_failure_handoff").is_empty());
}

/// A reviewer commit that fails owner revalidation rejects the candidate:
/// no step recovery, no second review, nothing pushed or opened, and the
/// failure handoff blocks the task naming the revalidation step.
#[test]
fn a_failed_revalidation_rejects_without_a_second_review() {
    let host = ScriptedHost::new(Settlement::AcceptWithFixes, Revalidation::Fails);
    let result = run_shipped_pipeline(&host);

    assert!(
        !matches!(&result, Ok(outcome) if outcome.success),
        "a reviewer commit that fails revalidation must not deliver: {result:?}"
    );
    assert_eq!(host.inputs("agent_review_repair").len(), 1);
    assert_eq!(
        host.inputs("review_gate_admit")
            .iter()
            .filter(|input| input["preflight"] != true)
            .count(),
        1,
        "no second reviewer start"
    );
    for step in ["git_push", "pr_open", "pr_promote", "pr_complete"] {
        assert!(host.inputs(step).is_empty(), "{step} ran after a rejection");
    }
    assert!(
        host.inputs("step_failure_recovery").is_empty(),
        "revalidation has no step recovery"
    );
    let handoff = host.inputs("pr_failure_handoff");
    assert_eq!(handoff.len(), 1);
    assert_eq!(handoff[0]["failed_step_id"], "review_validate");
    assert_eq!(
        handoff[0]["pipeline"]["review_gate_settle"]["implementation_head_sha"], "candidate",
        "the handoff sees both commits"
    );
}

/// An unfixable finding: settlement refuses, nothing is published, final
/// recovery gets its one look, and the failure handoff blocks the task with
/// the candidate preserved.
#[test]
fn reject_blocks_the_task_after_final_recovery() {
    let host = ScriptedHost::new(Settlement::Reject, Revalidation::Passes);
    let result = run_shipped_pipeline(&host);

    assert!(
        !matches!(&result, Ok(outcome) if outcome.success),
        "a rejected candidate must not deliver: {result:?}"
    );
    for step in [
        "candidate_validate",
        "git_push",
        "pr_open",
        "pr_promote",
        "pr_complete",
    ] {
        let after_review = host
            .actions()
            .iter()
            .skip_while(|action| *action != "review_gate_settle")
            .any(|action| action == step);
        assert!(!after_review, "{step} ran after a rejection");
    }
    assert!(
        host.inputs("step_failure_recovery").is_empty(),
        "a settled refusal is a decision, not a fault to recover"
    );

    let admissions = host.final_recovery_admissions();
    assert_eq!(admissions.len(), 1, "final recovery is eligible");
    assert_eq!(admissions[0].failed_step_id, "review_gate_settle");
    let recovery_inputs = host.inputs("final_recovery");
    assert_eq!(recovery_inputs.len(), 1, "final recovery dispatches once");
    assert_eq!(recovery_inputs[0]["task_id"], "T-1");
    assert_eq!(recovery_inputs[0]["run_id"], RUN_ID);
    assert_eq!(recovery_inputs[0]["failed_step_id"], "review_gate_settle");
    assert_eq!(recovery_inputs[0]["activity_name"], "review_gate_settle");
    assert_eq!(recovery_inputs[0]["workspace_path"], WORKSPACE);
    assert_eq!(recovery_inputs[0]["log_tail"], "");
    assert_eq!(recovery_inputs[0]["step_recovery_attempts"], json!([]));
    assert_eq!(host.log_tail_run_ids(), [RUN_ID]);

    let actions = host.actions();
    let review_settled = position(&actions, "review_gate_settle");
    assert_eq!(
        &actions[review_settled + 1..review_settled + 3],
        ["final_recovery", "pr_failure_handoff"],
        "recovery escalates before failure handoff: {actions:?}"
    );

    let handoff = host.inputs("pr_failure_handoff");
    assert_eq!(handoff.len(), 1);
    assert_eq!(handoff[0]["failed_step_id"], "review_gate_settle");
    let message = handoff[0]["error_message"].as_str().unwrap_or_default();
    assert!(
        message.contains("review_gate_blocked"),
        "the handoff carries the gate's refusal: {message}"
    );
}

#[test]
fn reject_supplies_fixture_log_tail_to_final_recovery() {
    let host = ScriptedHost::new(Settlement::Reject, Revalidation::Passes)
        .with_log_tail("fixture-owned recovery evidence");
    let result = run_shipped_pipeline(&host);

    assert!(
        !matches!(&result, Ok(outcome) if outcome.success),
        "a rejected candidate must not deliver: {result:?}"
    );
    let recovery_inputs = host.inputs("final_recovery");
    assert_eq!(recovery_inputs.len(), 1, "final recovery dispatches once");
    assert_eq!(recovery_inputs[0]["run_id"], RUN_ID);
    assert_eq!(
        recovery_inputs[0]["log_tail"],
        "fixture-owned recovery evidence"
    );
    assert_eq!(host.log_tail_run_ids(), [RUN_ID]);
}

const STUB_PREFIX: &str = "test_stub_";
const RUN_ID: &str = "review-fixes-run";
const WORKSPACE: &str = "/worktrees/review-fixes-run";
const REVIEW_FIXES: &str = "## Review fixes\n\n- `F1` [high] Missing guard — added the guard";

/// Every activity the shipped PR pipeline dispatches, as scripted stand-ins.
const ACTIVITIES: &[&str] = &[
    "worktree_setup",
    "candidate_resume",
    "agent_implement",
    "step_failure_recovery",
    "git_commit",
    "pr_prepare",
    "git_rebase",
    "pr_conflict_recovery",
    "candidate_validate",
    "review_gate_admit",
    "agent_review_repair",
    "review_gate_settle",
    "git_push",
    "pr_open",
    "pr_promote",
    "pr_complete",
    "pr_failure_handoff",
    "final_recovery",
    "claim_validate",
    "claim_handoff",
];

pub(super) fn position(actions: &[String], action: &str) -> usize {
    actions
        .iter()
        .position(|candidate| candidate == action)
        .unwrap_or_else(|| panic!("{action} never ran: {actions:?}"))
}

pub(super) fn run_shipped_pipeline(host: &ScriptedHost) -> Result<JobOutcome, DispatchError> {
    run_shipped_job(
        host,
        "task_pr_pipeline",
        json!({
            "task_ids": ["T-1"],
            "base_branch": "main",
            "base_sync": "remote",
            "completion": "done",
        }),
    )
}

/// Run the shipped job `name` over `input`, every activity scripted.
pub(super) fn run_shipped_job(
    host: &ScriptedHost,
    name: &str,
    input: Value,
) -> Result<JobOutcome, DispatchError> {
    let audit_root = tempfile::tempdir().expect("audit tempdir");
    let inner = Arc::new(InMemorySink::new(audit_root.path().join("blobs")));
    let store = Arc::new(orbit_store::Store::open_in_memory().expect("open sqlite sink"));
    let envelope = Arc::new(V2SqliteSink::for_audit_root(
        store,
        "ws_review_fixes",
        RUN_ID,
        "review-fixes-agent",
        None,
        audit_root.path(),
    ));
    let writer = Arc::new(
        V2AuditWriter::new(RUN_ID, "review-fixes-agent", inner).with_envelope_sink(envelope),
    );
    execute_job_with_resume(&shipped_job(name), input, RUN_ID, writer, host, None)
}

/// The shipped job `name`, every activity resolved to a scripted
/// deterministic action named after it. The prefix keeps the engine's own
/// built-in actions of the same names out of the way.
fn shipped_job(name: &str) -> orbit_types::workflow::JobV2 {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(format!("crates/orbit-core/assets/jobs/{name}.yaml"));
    let shipped = std::fs::read_to_string(root).expect("read the shipped job");
    let mut job = load_job_asset(&shipped)
        .expect("the shipped job loads")
        .spec;
    let mut catalog = V2ActivityCatalog::new();
    for &name in ACTIVITIES {
        catalog.insert(
            name,
            ActivityV2 {
                description: format!("scripted `{name}`"),
                input_schema_json: Value::Null,
                output_schema_json: Value::Null,
                fs_profile: None,
                spec: ActivityV2Spec::Deterministic(DeterministicSpec {
                    action: format!("{STUB_PREFIX}{name}"),
                    config: Value::Null,
                }),
            },
        );
    }
    resolve_job_catalog_refs_for_execution(&mut job, &catalog)
        .expect("every shipped activity resolves to a stand-in");
    job
}

/// What the scripted gate settles the admitted attempt as.
#[derive(Clone, Copy)]
pub(super) enum Settlement {
    Accept,
    /// The reviewer committed fixes over the implementation head.
    AcceptWithFixes,
    /// An open finding: the gate refuses.
    Reject,
    Timeout,
}

/// Whether owner revalidation of a reviewer commit passes.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Revalidation {
    Passes,
    Fails,
}

/// Plays the shipped pipeline's activities, keeping the candidate head a
/// reviewer commit advances.
pub(super) struct ScriptedHost {
    settlement: Settlement,
    revalidation: Revalidation,
    /// What `candidate_resume` decides; by default, a fresh implementation.
    resume: Value,
    head: Mutex<String>,
    calls: Mutex<Vec<(String, Value)>>,
    reviewer_events: Mutex<Vec<orbit_types::workflow::ReviewerInvocationEvent>>,
    admissions: Mutex<Vec<FinalRecoveryAdmissionRequest>>,
    log_tail: Option<String>,
    log_tail_run_ids: Mutex<Vec<String>>,
}

impl ScriptedHost {
    pub(super) fn new(settlement: Settlement, revalidation: Revalidation) -> Self {
        Self {
            settlement,
            revalidation,
            resume: json!({
                "phase": "candidate_resume",
                "outcome": "fresh",
                "implement": true,
                "reason": "no earlier run is linked to the task",
                "repair": null,
            }),
            head: Mutex::new("candidate".to_string()),
            calls: Mutex::default(),
            reviewer_events: Mutex::default(),
            admissions: Mutex::default(),
            log_tail: None,
            log_tail_run_ids: Mutex::default(),
        }
    }

    pub(super) fn with_log_tail(mut self, log_tail: impl Into<String>) -> Self {
        self.log_tail = Some(log_tail.into());
        self
    }

    /// `candidate_resume` answers `resume` instead.
    pub(super) fn resuming(mut self, resume: Value) -> Self {
        self.resume = resume;
        self
    }

    pub(super) fn actions(&self) -> Vec<String> {
        let calls = self.calls.lock().expect("call log");
        calls.iter().map(|(action, _)| action.clone()).collect()
    }

    pub(super) fn inputs(&self, action: &str) -> Vec<Value> {
        let calls = self.calls.lock().expect("call log");
        calls
            .iter()
            .filter(|(name, _)| name == action)
            .map(|(_, input)| input.clone())
            .collect()
    }

    fn final_recovery_admissions(&self) -> Vec<FinalRecoveryAdmissionRequest> {
        self.admissions.lock().expect("admissions").clone()
    }

    fn log_tail_run_ids(&self) -> Vec<String> {
        self.log_tail_run_ids
            .lock()
            .expect("log tail lookups")
            .clone()
    }

    fn settle(&self, input: &Value) -> Result<Value, DispatchError> {
        if input.pointer("/admission/applies") != Some(&json!(true)) {
            return Ok(json!({
                "gate": "not_required",
                "reviewed_head_sha": "",
                "reviewed_base_sha": "",
                "reviewer_fixed": false,
                "review_fixes": "",
                "handoff_evidence": null,
            }));
        }
        let attempt = input["admission"]["attempt_id"].clone();
        let mut head = self.head.lock().expect("candidate");
        let implementation = head.clone();
        match self.settlement {
            Settlement::Accept => Ok(json!({
                "gate": "passed",
                "verdict": "accept",
                "attempt_id": attempt,
                "reviewed_head_sha": implementation,
                "reviewed_base_sha": "base-sha",
                "implementation_head_sha": implementation,
                "reviewer_fixed": false,
                "review_fixes": "",
                "handoff_evidence": null,
            })),
            Settlement::AcceptWithFixes => {
                *head = "reviewer-fixes".to_string();
                Ok(json!({
                    "gate": "passed",
                    "verdict": "accept_with_fixes",
                    "attempt_id": attempt,
                    "reviewed_head_sha": *head,
                    "reviewed_base_sha": "base-sha",
                    "implementation_head_sha": implementation,
                    "reviewer_fixed": true,
                    "review_fixes": REVIEW_FIXES,
                    "handoff_evidence": null,
                }))
            }
            Settlement::Reject | Settlement::Timeout => {
                Err(DispatchError::DeterministicActionRefused {
                    action: "review_gate_settle".to_string(),
                    message: format!("review_gate_blocked: attempt {attempt} settled reject"),
                })
            }
        }
    }
}

impl RuntimeHost for ScriptedHost {
    fn record_reviewer_invocation(
        &self,
        request: &orbit_engine::ReviewerInvocationRequest,
    ) -> Result<Option<u64>, OrbitError> {
        self.reviewer_events.lock().unwrap().push(request.event);
        Ok(None)
    }

    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        let action = action.strip_prefix(STUB_PREFIX).unwrap_or(action);
        self.calls
            .lock()
            .expect("call log")
            .push((action.to_string(), input.clone()));
        let head = self.head.lock().expect("candidate").clone();
        let output = match action {
            "worktree_setup" => json!({
                "job_run_id": RUN_ID,
                "workspace_path": WORKSPACE,
                "base_ref": "origin/main",
                "base_sha": "base-sha",
                "prior_job_run_id": null,
            }),
            "candidate_resume" => self.resume.clone(),
            "agent_implement" => json!({ "summary": "implemented" }),
            "git_commit" => json!({ "skipped_no_diff_expected": false }),
            "pr_prepare" | "git_rebase" => json!({
                "head": "candidate-branch",
                "head_sha": head,
                "base": "main",
                "base_ref": "origin/main",
                "base_sha": "base-sha",
                "remote_sha": head,
                "remote_sha_before": head,
                "head_sha_before": head,
                "commits_behind": 0,
                "sync_required": false,
                "rewritten": false,
            }),
            "candidate_validate"
                if input.get("ownership_base_sha").is_some()
                    && self.revalidation == Revalidation::Fails =>
            {
                return Err(DeterministicFailure::validation(&head));
            }
            "candidate_validate" => json!({ "decision": "passed", "tested_head": head }),
            "review_gate_admit" if input["preflight"] == true => {
                json!({ "applies": true, "decision": "preflight_passed" })
            }
            "review_gate_admit" if input.get("re_review_after").is_some() => {
                json!({ "applies": false, "reason": "re_review_not_required" })
            }
            "review_gate_admit" => json!({
                "applies": true,
                "first_task_id": "T-1",
                "attempt_id": "rvw-1",
                "lineage_key": "lineage-1",
                "manifest_artifact": "review-manifest.json",
                "report_artifact": "review-report.json",
                "reviewer": { "crew": "reviewers" },
            }),
            "agent_review_repair" => {
                json!({ "summary": "partial review", "verdict": "accept", "timed_out": matches!(self.settlement, Settlement::Timeout) })
            }
            "review_gate_settle" => return self.settle(input),
            "git_push" => json!({ "local_sha": head, "remote_sha_before": null }),
            "pr_open" => json!({ "pr_number": "41", "pr_url": "https://example.invalid/41" }),
            "pr_promote" => json!({ "promoted": true }),
            "pr_complete" => json!({
                "re_review_required": false,
                "merge": { "merged": true },
                "completed_task_ids": ["T-1"],
                "skipped_task_ids": [],
            }),
            "final_recovery" => json!({
                "decision": "escalate",
                "diagnosis": "the reviewer left an open finding",
                "human_action": "decide the finding",
            }),
            "pr_failure_handoff" => json!({ "decision": "blocked_review_gate" }),
            "claim_validate" => json!({
                "decision": "passed",
                "tested_head": head,
                "candidate": { "commit": head },
                "validation": [],
            }),
            "claim_handoff" => json!({ "decision": "handed_off" }),
            other => {
                return Err(DispatchError::DeterministicActionFailed {
                    action: other.to_string(),
                    message: "not scripted".to_string(),
                });
            }
        };
        Ok(output)
    }

    fn admit_final_recovery(
        &self,
        _run_id: &str,
        request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError> {
        self.admissions
            .lock()
            .expect("admissions")
            .push(request.clone());
        Ok(FinalRecoveryAdmission::Admitted)
    }

    fn final_recovery_log_tail(&self, run_id: &str) -> Result<Option<String>, OrbitError> {
        self.log_tail_run_ids
            .lock()
            .expect("log tail lookups")
            .push(run_id.to_string());
        Ok((run_id == RUN_ID).then(|| self.log_tail.clone()).flatten())
    }

    fn apply_final_recovery(
        &self,
        _run_id: &str,
        application: &FinalRecoveryApplication,
    ) -> Result<FinalRecoveryApplied, OrbitError> {
        Ok(match &application.decision {
            FinalRecoveryDecision::Escalate { .. } => FinalRecoveryApplied::Escalated {
                outcome: "blocked for a human".to_string(),
            },
            other => FinalRecoveryApplied::Settled {
                outcome: other.kind().to_string(),
            },
        })
    }
}

/// The failure a required command on the reviewer commit reports.
struct DeterministicFailure;

impl DeterministicFailure {
    fn validation(head: &str) -> DispatchError {
        DispatchError::DeterministicActionFailed {
            action: "candidate_validate".to_string(),
            message: format!("required validation 'make test' did not pass on candidate {head}"),
        }
    }
}
