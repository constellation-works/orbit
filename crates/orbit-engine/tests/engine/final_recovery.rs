//! [ORB-13907] The job-level final recovery hook, run through
//! `execute_job_with_resume` against a scripted host.
//!
//! The job is `setup → work → deliver` with a `failure_activity` and a
//! deterministic `final_recovery_activity` whose output is the decision. The
//! host stands in for Core: it admits (or skips) the hook and acts on each
//! decision the way the applier's outcome maps — `resume` back to the engine,
//! `escalate` as escalated, anything else as settled.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_agent::loop_engine::InMemorySink;
use orbit_common::OrbitError;
use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_engine::{
    DispatchError, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
    FinalRecoveryApplied, JobOutcome, RuntimeHost, V2AuditWriter, V2SqliteSink,
    execute_job_with_resume,
};
use orbit_types::workflow::activity_job::{ActivityV2, JobV2, V2AuditEventKind};
use orbit_types::workflow::{
    FinalRecoveryCheckpoint, FinalRecoveryDecision, FinalRecoveryKey, JobRunState, PipelineState,
};
use serde_json::{Value, json};

const RUN_ID: &str = "jrun-final-recovery";

/// How a scripted deterministic call ends.
#[derive(Clone)]
enum Reply {
    Ok(Value),
    Fail,
    Permanent,
}

struct RecoveryHost {
    replies: Mutex<HashMap<String, VecDeque<Reply>>>,
    calls: Mutex<Vec<(String, Value)>>,
    admission: FinalRecoveryAdmission,
    admissions: Mutex<Vec<FinalRecoveryAdmissionRequest>>,
    applications: Mutex<Vec<FinalRecoveryApplication>>,
}

impl RecoveryHost {
    fn new<const N: usize>(replies: [(&str, Vec<Reply>); N]) -> Self {
        Self {
            replies: Mutex::new(
                replies
                    .into_iter()
                    .map(|(action, queue)| (action.to_string(), queue.into()))
                    .collect(),
            ),
            calls: Mutex::new(Vec::new()),
            admission: FinalRecoveryAdmission::Admitted,
            admissions: Mutex::new(Vec::new()),
            applications: Mutex::new(Vec::new()),
        }
    }

    /// Answer admission like a host whose `workflow.final_recovery_crews` is
    /// `[]`.
    fn with_empty_pool(mut self) -> Self {
        self.admission = FinalRecoveryAdmission::Skipped {
            reason: "final recovery is disabled: `workflow.final_recovery_crews` is []".to_string(),
        };
        self
    }

    fn inputs(&self, action: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| name == action)
            .map(|(_, input)| input.clone())
            .collect()
    }

    fn count(&self, action: &str) -> usize {
        self.inputs(action).len()
    }

    fn actions(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(name, _)| name.clone())
            .collect()
    }

    fn applications(&self) -> Vec<FinalRecoveryApplication> {
        self.applications.lock().unwrap().clone()
    }
}

impl RuntimeHost for RecoveryHost {
    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        self.calls
            .lock()
            .unwrap()
            .push((action.to_string(), input.clone()));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .get_mut(action)
            .and_then(VecDeque::pop_front);
        match reply {
            Some(Reply::Ok(value)) => Ok(value),
            Some(Reply::Fail) => Err(DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!("{action} failed"),
            }),
            Some(Reply::Permanent) => Err(DispatchError::CliInvocationPermanent(format!(
                "{action}: sandbox unavailable"
            ))),
            None if action == "setup" => Ok(json!({
                "workspace_path": "/worktrees/run",
                "base_ref": "main",
                "base_sha": "0123456789abcdef0123456789abcdef01234567",
            })),
            None => Ok(json!({ "action": action })),
        }
    }

    fn admit_final_recovery(
        &self,
        _run_id: &str,
        request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError> {
        self.admissions.lock().unwrap().push(request.clone());
        Ok(self.admission.clone())
    }

    fn apply_final_recovery(
        &self,
        _run_id: &str,
        application: &FinalRecoveryApplication,
    ) -> Result<FinalRecoveryApplied, OrbitError> {
        self.applications.lock().unwrap().push(application.clone());
        Ok(match &application.decision {
            FinalRecoveryDecision::Resume { .. } => FinalRecoveryApplied::Resume,
            FinalRecoveryDecision::Escalate { .. } => FinalRecoveryApplied::Escalated {
                outcome: "blocked for a human".to_string(),
            },
            other => FinalRecoveryApplied::Settled {
                outcome: other.kind().to_string(),
            },
        })
    }
}

fn deterministic_activity(name: &str) -> ActivityV2 {
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Activity",
        "metadata": { "name": name },
        "spec": {
            "type": "deterministic",
            "description": name,
            "action": name,
            "config": {},
        },
    });
    load_activity_asset(&asset.to_string())
        .expect("fixture activity loads")
        .spec
}

/// `setup → work → deliver`, with `handoff` as the failure activity and
/// `decide` as the final recovery activity. `work` may name `step_fix` as
/// its step recovery.
fn pipeline(step_recovery: bool) -> JobV2 {
    let step = |id: &str| {
        json!({
            "id": id,
            "spec": { "type": "deterministic", "action": id, "config": {} },
        })
    };
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Job",
        "metadata": { "name": "final_recovery_fixture" },
        "spec": {
            "state": "enabled",
            "kind": "workflow",
            "steps": [step("setup"), step("work"), step("deliver")],
        },
    });
    let mut job = load_job_asset(&asset.to_string())
        .expect("fixture job loads")
        .spec;
    job.failure_activity = Some("handoff".to_string());
    job.resolved_failure_activity = Some(deterministic_activity("handoff"));
    job.final_recovery_activity = Some("decide".to_string());
    job.resolved_final_recovery_activity = Some(deterministic_activity("decide"));
    if step_recovery {
        job.steps[1].recovery_activity = Some("step_fix".to_string());
        job.steps[1].resolved_recovery_activity = Some(deterministic_activity("step_fix"));
    }
    job
}

struct Run {
    result: Result<JobOutcome, DispatchError>,
    /// `(outcome, decision)` of every `job.final_recovery_attempted` event.
    attempts: Vec<(String, Option<String>)>,
}

fn run(job: &JobV2, host: &RecoveryHost, resume: Option<&PipelineState>) -> Run {
    let audit_root = tempfile::tempdir().expect("audit tempdir");
    let inner = Arc::new(InMemorySink::new(audit_root.path().join("blobs")));
    let envelope = Arc::new(V2SqliteSink::for_audit_root(
        Arc::new(orbit_store::Store::open_in_memory().expect("open sqlite sink")),
        "ws_final_recovery",
        RUN_ID,
        "test-agent",
        None,
        audit_root.path(),
    ));
    let writer =
        Arc::new(V2AuditWriter::new(RUN_ID, "test-agent", inner).with_envelope_sink(envelope));
    let result = execute_job_with_resume(
        job,
        json!({ "task_ids": ["T-1"] }),
        RUN_ID,
        writer.clone(),
        host,
        resume,
    );
    let attempts = writer
        .events_snapshot()
        .expect("audit events")
        .into_iter()
        .filter_map(|event| match event.kind {
            V2AuditEventKind::FinalRecoveryAttempted {
                outcome, decision, ..
            } => Some((outcome, decision)),
            _ => None,
        })
        .collect();
    Run { result, attempts }
}

fn resume_to(step_id: &str) -> Reply {
    Reply::Ok(json!({
        "decision": "resume",
        "step_id": step_id,
        "rationale": "repaired the worktree",
    }))
}

fn escalate() -> Reply {
    Reply::Ok(json!({
        "decision": "escalate",
        "diagnosis": "the provider rejected the push",
        "human_action": "grant the token repository write access",
    }))
}

fn attempt(outcome: &str, decision: Option<&str>) -> (String, Option<String>) {
    (outcome.to_string(), decision.map(str::to_string))
}

#[test]
fn a_resume_reruns_from_the_named_step_and_the_run_completes() {
    // The decision arrives the way an agent activity reports it: its own keys
    // listed in `response_result_fields`, beside Orbit's invocation metadata.
    let host = RecoveryHost::new([
        (
            "work",
            vec![Reply::Fail, Reply::Ok(json!({ "built": true }))],
        ),
        (
            "decide",
            vec![Reply::Ok(json!({
                "decision": "resume",
                "step_id": "work",
                "rationale": "finished the implementation",
                "response_result_fields": ["decision", "step_id", "rationale"],
                "invocation_id": "inv-1",
            }))],
        ),
    ]);
    let run = run(&pipeline(false), &host, None);

    let outcome = run.result.expect("the resumed run completes");
    assert!(outcome.success, "the rerun from `work` must deliver");
    assert_eq!(
        host.actions(),
        ["setup", "work", "decide", "work", "deliver"],
        "only `work` and what follows it run again; setup is not repeated"
    );
    assert_eq!(
        host.count("handoff"),
        0,
        "a resumed run opens no failure handoff"
    );
    let applications = host.applications();
    assert_eq!(applications.len(), 1);
    assert_eq!(applications[0].resume_step_index, Some(1));
    assert_eq!(run.attempts, [attempt("resume", Some("resume"))]);

    let input = &host.inputs("decide")[0];
    assert_eq!(input["task_id"], "T-1");
    assert_eq!(input["failed_step_id"], "work");
    assert_eq!(input["workspace_path"], "/worktrees/run");
    assert_eq!(input["base_ref"], "main");
    assert_eq!(input["crew_config_key"], "workflow.final_recovery_crews");
    assert_eq!(input["step_ids"], json!(["setup", "work", "deliver"]));
}

#[test]
fn a_settling_decision_ends_the_run_without_the_failure_handoff() {
    let host = RecoveryHost::new([
        ("work", vec![Reply::Fail]),
        (
            "decide",
            vec![Reply::Ok(json!({
                "decision": "archive",
                "reason": "superseded by the landed rewrite",
            }))],
        ),
    ]);
    let run = run(&pipeline(false), &host, None);

    let error = run
        .result
        .expect_err("the original failure stays authoritative");
    assert!(error.to_string().contains("work failed"), "{error}");
    assert_eq!(host.count("handoff"), 0, "settled work gets no blocked PR");
    assert_eq!(host.count("deliver"), 0);
    let applications = host.applications();
    assert!(matches!(
        applications[0].decision,
        FinalRecoveryDecision::Archive { .. }
    ));
    assert_eq!(run.attempts, [attempt("settled", Some("archive"))]);
}

#[test]
fn an_escalation_still_runs_the_failure_handoff() {
    let host = RecoveryHost::new([("work", vec![Reply::Fail]), ("decide", vec![escalate()])]);
    let run = run(&pipeline(false), &host, None);

    assert!(run.result.is_err());
    assert_eq!(
        host.count("handoff"),
        1,
        "escalation preserves today's handoff"
    );
    assert_eq!(
        host.actions(),
        ["setup", "work", "decide", "handoff"],
        "final recovery runs before the failure activity"
    );
    assert_eq!(run.attempts, [attempt("escalated", Some("escalate"))]);
}

#[test]
fn an_error_that_skips_step_recovery_still_reaches_final_recovery() {
    let host = RecoveryHost::new([
        ("work", vec![Reply::Permanent]),
        ("decide", vec![escalate()]),
    ]);
    let run = run(&pipeline(true), &host, None);

    assert!(matches!(
        run.result,
        Err(DispatchError::CliInvocationPermanent(_))
    ));
    assert_eq!(
        host.count("step_fix"),
        0,
        "a permanent error bypasses step recovery"
    );
    assert_eq!(host.count("decide"), 1, "but not the job's final recovery");
    assert_eq!(host.count("handoff"), 1);
}

#[test]
fn step_recovery_runs_first_and_final_recovery_only_once_it_is_spent() {
    let host = RecoveryHost::new([
        ("work", vec![Reply::Fail, Reply::Fail]),
        ("decide", vec![escalate()]),
    ]);
    let run = run(&pipeline(true), &host, None);

    assert!(run.result.is_err());
    assert_eq!(
        host.actions(),
        ["setup", "work", "step_fix", "work", "decide", "handoff"]
    );
}

#[test]
fn final_recovery_runs_at_most_once_per_run() {
    // The rerun after `resume` fails again: the hook does not run twice, and
    // the run falls through to its failure handoff.
    let host = RecoveryHost::new([
        ("work", vec![Reply::Fail, Reply::Fail]),
        ("decide", vec![resume_to("work"), resume_to("work")]),
    ]);
    let run = run(&pipeline(false), &host, None);

    assert!(run.result.is_err());
    assert_eq!(host.count("decide"), 1);
    assert_eq!(host.admissions.lock().unwrap().len(), 1);
    assert_eq!(host.count("handoff"), 1);
    assert_eq!(
        run.attempts,
        [attempt("resume", Some("resume")), attempt("skipped", None),]
    );
}

/// State a source run left after its final recovery recorded `decision`
/// (and `outcome`, when it got that far), resumed at `work`.
fn resumed_after(decision: FinalRecoveryDecision, outcome: Option<&str>) -> PipelineState {
    let mut state = PipelineState::new(
        "jrun-source".to_string(),
        "final_recovery_fixture".to_string(),
        json!({ "task_ids": ["T-1"] }),
    );
    let setup = json!({ "workspace_path": "/worktrees/run", "base_ref": "main" });
    state.record_step(0, JobRunState::Success, Some(setup.clone()), None);
    state.sync_pipeline(json!({ "setup": setup }));
    state.final_recovery = Some(FinalRecoveryCheckpoint {
        key: FinalRecoveryKey {
            run_id: "jrun-source".to_string(),
            attempt: 1,
        },
        failed_step_id: "work".to_string(),
        task_id: "T-1".to_string(),
        observed: None,
        base_ref: Some("main".to_string()),
        admitted_at: Utc::now(),
        decision: Some(decision),
        outcome: outcome.map(str::to_string),
    });
    state
}

#[test]
fn a_run_resumed_after_a_final_recovery_resume_never_runs_it_again() {
    let state = resumed_after(
        FinalRecoveryDecision::Resume {
            step_id: "work".to_string(),
            rationale: "repaired the worktree".to_string(),
        },
        Some("resume"),
    );
    let host = RecoveryHost::new([("work", vec![Reply::Fail]), ("decide", vec![escalate()])]);
    let run = run(&pipeline(false), &host, Some(&state));

    assert!(run.result.is_err());
    assert_eq!(host.count("decide"), 0);
    assert!(host.admissions.lock().unwrap().is_empty());
    assert!(host.applications().is_empty());
    assert_eq!(host.count("handoff"), 1);
}

#[test]
fn a_run_resumed_after_a_crash_mid_settlement_replays_the_recorded_decision() {
    // The source run recorded `archive` and crashed before recording what
    // applying it did. The resumed run fails again; it neither dispatches nor
    // admits final recovery, but hands the recorded decision back to the
    // host, whose application is idempotent, and opens no blocked PR.
    let archive = FinalRecoveryDecision::Archive {
        reason: "superseded by the landed rewrite".to_string(),
    };
    let state = resumed_after(archive.clone(), None);
    let host = RecoveryHost::new([("work", vec![Reply::Fail]), ("decide", vec![escalate()])]);
    let run = run(&pipeline(false), &host, Some(&state));

    assert!(run.result.is_err());
    assert_eq!(
        host.count("decide"),
        0,
        "the decision is not asked for twice"
    );
    assert!(host.admissions.lock().unwrap().is_empty());
    let applications = host.applications();
    assert_eq!(applications.len(), 1);
    assert_eq!(applications[0].decision, archive);
    assert_eq!(applications[0].resume_step_index, None);
    assert_eq!(host.count("handoff"), 0, "settled work gets no blocked PR");
    assert_eq!(run.attempts, [attempt("settled", Some("archive"))]);
}

#[test]
fn an_empty_crew_pool_skips_final_recovery() {
    let host = RecoveryHost::new([("work", vec![Reply::Fail]), ("decide", vec![escalate()])])
        .with_empty_pool();
    let run = run(&pipeline(false), &host, None);

    assert!(run.result.is_err());
    assert_eq!(host.count("decide"), 0, "a skipped hook dispatches nothing");
    assert!(host.applications().is_empty());
    assert_eq!(host.count("handoff"), 1, "and today's failure path follows");
    assert_eq!(run.attempts, [attempt("skipped", None)]);
}

#[test]
fn a_resume_outside_the_failed_phase_is_escalated() {
    // `deliver` is after the failure and `setup` is the job's own first phase;
    // neither is a step the failed one may resume from.
    for target in ["deliver", "setup", "no_such_step"] {
        let host = RecoveryHost::new([
            ("work", vec![Reply::Fail]),
            ("decide", vec![resume_to(target)]),
        ]);
        let run = run(&pipeline(false), &host, None);

        assert!(run.result.is_err(), "{target}");
        let applications = host.applications();
        assert!(
            matches!(
                applications[0].decision,
                FinalRecoveryDecision::Escalate { .. }
            ),
            "resume to `{target}` must reach the host as escalate"
        );
        assert_eq!(applications[0].resume_step_index, None);
        assert_eq!(host.count("deliver"), 0, "{target}");
        assert_eq!(host.count("handoff"), 1, "{target}");
    }
}

#[test]
fn a_malformed_decision_is_escalated() {
    let host = RecoveryHost::new([
        ("work", vec![Reply::Fail]),
        ("decide", vec![Reply::Ok(json!({ "decision": "resume" }))]),
    ]);
    let run = run(&pipeline(false), &host, None);

    assert!(run.result.is_err());
    assert!(matches!(
        host.applications()[0].decision,
        FinalRecoveryDecision::Escalate { .. }
    ));
    assert_eq!(host.count("handoff"), 1);
}

#[test]
fn a_failure_before_the_worktree_exists_skips_final_recovery() {
    let host = RecoveryHost::new([("setup", vec![Reply::Fail]), ("decide", vec![escalate()])]);
    let run = run(&pipeline(false), &host, None);

    assert!(run.result.is_err());
    assert_eq!(host.count("decide"), 0);
    assert!(host.admissions.lock().unwrap().is_empty());
    assert_eq!(host.count("handoff"), 1);
}
