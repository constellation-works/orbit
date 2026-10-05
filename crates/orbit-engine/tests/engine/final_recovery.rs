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
    Diagnostic(String),
}

struct RecoveryHost {
    replies: Mutex<HashMap<String, VecDeque<Reply>>>,
    calls: Mutex<Vec<(String, Value)>>,
    admission: FinalRecoveryAdmission,
    admissions: Mutex<Vec<FinalRecoveryAdmissionRequest>>,
    applications: Mutex<Vec<FinalRecoveryApplication>>,
    logs: HashMap<String, String>,
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
            logs: HashMap::from([(RUN_ID.to_string(), "this run's failure log".to_string())]),
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
            Some(Reply::Diagnostic(message)) => Err(DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message,
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

    fn final_recovery_log_tail(&self, run_id: &str) -> Result<Option<String>, OrbitError> {
        Ok(self.logs.get(run_id).cloned())
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
    run_with_evidence(job, host, resume, Vec::new())
}

fn run_with_evidence(
    job: &JobV2,
    host: &RecoveryHost,
    resume: Option<&PipelineState>,
    prior_events: Vec<(&str, V2AuditEventKind)>,
) -> Run {
    let audit_root = tempfile::tempdir().expect("audit tempdir");
    let inner = Arc::new(InMemorySink::new(audit_root.path().join("blobs")));
    let store = Arc::new(orbit_store::Store::open_in_memory().expect("open sqlite sink"));
    let mut own_events = Vec::new();
    for (run_id, event) in prior_events {
        if run_id == RUN_ID {
            own_events.push(event);
            continue;
        }
        let sink = Arc::new(V2SqliteSink::for_audit_root(
            store.clone(),
            "ws_final_recovery",
            run_id,
            "test-agent",
            None,
            audit_root.path(),
        ));
        let writer =
            V2AuditWriter::new(run_id, "test-agent", inner.clone()).with_envelope_sink(sink);
        writer.emit(event).expect("persist prior event");
    }
    let envelope = Arc::new(V2SqliteSink::for_audit_root(
        store,
        "ws_final_recovery",
        RUN_ID,
        "test-agent",
        None,
        audit_root.path(),
    ));
    let writer =
        Arc::new(V2AuditWriter::new(RUN_ID, "test-agent", inner).with_envelope_sink(envelope));
    for event in own_events {
        writer.emit(event).expect("persist own prior event");
    }
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
    assert_eq!(input["step_recovery_attempts"], json!([]));
    assert_eq!(input["log_tail"], "this run's failure log");
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
    let input = &host.inputs("decide")[0];
    let attempts = input["step_recovery_attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["activity"], "step_fix");
    assert_eq!(attempts[0]["phase"], "recovery");
    assert_eq!(attempts[0]["outcome"], "success");
    assert_eq!(attempts[0]["output"]["action"], "step_fix");
    assert_eq!(attempts[1]["phase"], "post_recovery");
    assert_eq!(attempts[1]["outcome"], "error");
    assert!(
        attempts[1]["error_message"]
            .as_str()
            .unwrap()
            .contains("work failed")
    );
    assert_eq!(input["log_tail"], "this run's failure log");
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

#[test]
fn failed_recovery_dispatch_is_injected_before_final_recovery() {
    let host = RecoveryHost::new([
        ("work", vec![Reply::Fail]),
        ("step_fix", vec![Reply::Fail]),
        ("decide", vec![escalate()]),
    ]);
    let run = run(&pipeline(true), &host, None);
    assert!(run.result.is_err());
    let input = &host.inputs("decide")[0];
    let attempts = input["step_recovery_attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["activity"], "step_fix");
    assert_eq!(attempts[0]["outcome"], "failed");
    assert_eq!(attempts[0]["failure_phase"], "dispatch");
    assert!(
        attempts[0]["error_message"]
            .as_str()
            .unwrap()
            .contains("step_fix failed")
    );
    assert_eq!(attempts[0]["output"], Value::Null);
    assert_eq!(input["log_tail"], "this run's failure log");
}

#[test]
fn final_recovery_injects_only_its_runs_bounded_evidence_in_order() {
    let oversized = format!("start:{}:end", "界".repeat(100_000));
    let mut host = RecoveryHost::new([
        (
            "work",
            vec![Reply::Fail, Reply::Diagnostic(oversized.clone())],
        ),
        ("step_fix", vec![Reply::Ok(json!({"diagnosis": oversized}))]),
        ("decide", vec![escalate()]),
    ]);
    host.logs.insert(
        RUN_ID.to_string(),
        format!("{}own-log-end", "界".repeat(100_000)),
    );
    host.logs
        .insert("other-run".to_string(), "foreign-log".to_string());
    let event = |activity: &str| V2AuditEventKind::StepPostRecoveryAttempt {
        step_id: "prior-step".to_string(),
        recovery_activity: activity.to_string(),
        outcome: "error".to_string(),
        error_message: Some("prior failure".to_string()),
        output: None,
    };
    let run = run_with_evidence(
        &pipeline(true),
        &host,
        None,
        vec![
            ("other-run", event("foreign-recovery")),
            (RUN_ID, event("older-recovery")),
        ],
    );
    assert!(run.result.is_err());
    let input = &host.inputs("decide")[0];
    let attempts = input["step_recovery_attempts"].as_array().unwrap();
    assert_eq!(
        attempts.len(),
        3,
        "include every own recovery record and no foreign record"
    );
    assert_eq!(attempts[0]["activity"], "older-recovery");
    assert_eq!(attempts[1]["activity"], "step_fix");
    assert_eq!(attempts[2]["phase"], "post_recovery");
    assert!(
        attempts
            .windows(2)
            .all(|pair| pair[0]["attempted_at"].as_str() <= pair[1]["attempted_at"].as_str())
    );
    let output = attempts[1]["output"]["diagnosis"].as_str().unwrap();
    assert!(
        output.len() <= 8 * 1024,
        "oversized output leaf stays within the existing recovery bound"
    );
    assert!(output.starts_with("start:") && output.ends_with(":end"));
    let error = attempts[2]["error_message"].as_str().unwrap();
    assert!(
        error.len() <= 64 * 1024,
        "oversized diagnostic stays within the existing recovery input bound"
    );
    assert!(
        serde_json::to_vec(&input["step_recovery_attempts"])
            .unwrap()
            .len()
            <= 64 * 1024,
        "the entire attempt collection must fit the existing recovery input bound"
    );
    let log = input["log_tail"].as_str().unwrap();
    assert!(
        log.len() <= 64 * 1024,
        "the host cannot supply an unbounded log to final recovery"
    );
    assert!(log.ends_with("own-log-end"));
    assert!(!log.contains("foreign-log"));
}

#[test]
fn final_recovery_escalates_instead_of_dropping_records_that_cannot_fit() {
    let host = RecoveryHost::new([("work", vec![Reply::Fail]), ("decide", vec![escalate()])]);
    let events = (0..500)
        .map(|_| {
            (
                RUN_ID,
                V2AuditEventKind::StepPostRecoveryAttempt {
                    step_id: "prior-step".to_string(),
                    recovery_activity: "older-recovery".to_string(),
                    outcome: "error".to_string(),
                    error_message: Some("failed ".repeat(100)),
                    output: None,
                },
            )
        })
        .collect();
    let run = run_with_evidence(&pipeline(false), &host, None, events);
    assert!(run.result.is_err());
    assert_eq!(
        host.count("decide"),
        0,
        "never dispatch an unbounded or incomplete attempt array"
    );
    let applications = host.applications();
    let FinalRecoveryDecision::Escalate { diagnosis, .. } = &applications[0].decision else {
        panic!("oversized audit evidence must escalate");
    };
    assert!(
        diagnosis.contains("step-recovery evidence exceeds"),
        "{diagnosis}"
    );
    assert_eq!(host.count("handoff"), 1);
}

#[test]
fn final_recovery_keeps_post_recovery_output_when_a_later_step_fails() {
    let host = RecoveryHost::new([
        (
            "work",
            vec![Reply::Fail, Reply::Ok(json!({"repaired": true}))],
        ),
        ("deliver", vec![Reply::Fail]),
        ("decide", vec![escalate()]),
    ]);
    let run = run(&pipeline(true), &host, None);
    assert!(run.result.is_err());
    let input = &host.inputs("decide")[0];
    assert_eq!(input["failed_step_id"], "deliver");
    let attempts = input["step_recovery_attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[1]["phase"], "post_recovery");
    assert_eq!(attempts[1]["outcome"], "success");
    assert_eq!(attempts[1]["output"], json!({"repaired": true}));
}
