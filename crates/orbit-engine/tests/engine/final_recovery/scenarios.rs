//! [ORB-13907] The job-level final recovery hook, run through
//! `execute_job_with_resume` against a scripted host (`host.rs`). The job is
//! `setup → work → deliver` with a `failure_activity` and a deterministic
//! `final_recovery_activity` whose output is the decision.

use chrono::Utc;
use orbit_common::{ConflictedFile, RecoverableVcsConflict};
use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_engine::{CompletionObservationAdmission, DispatchError, FinalRecoveryApplication};
use orbit_types::workflow::activity_job::{ActivityV2, JobV2, V2AuditEventKind};
use orbit_types::workflow::{
    FinalRecoveryCheckpoint, FinalRecoveryCheckpointOutcome, FinalRecoveryDecision,
    FinalRecoveryKey, JobRunState, PipelineState, RecoveryFault, TransientInfrastructure,
};
use serde_json::{Value, json};

#[path = "host.rs"]
mod host;
use host::{RecoveryHost, Reply, Run, evidence_hold, run, run_with_evidence, run_with_input};

const RUN_ID: &str = "jrun-final-recovery";

#[cfg(unix)]
#[path = "prompt.rs"]
mod prompt;

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

fn det_step(id: &str, action: &str) -> Value {
    json!({ "id": id, "spec": { "type": "deterministic", "action": action, "config": {} } })
}

fn with_final_recovery(mut job: JobV2) -> JobV2 {
    job.final_recovery_activity = Some("decide".to_string());
    job.resolved_final_recovery_activity = Some(deterministic_activity("decide"));
    job
}

fn with_handoff(mut job: JobV2) -> JobV2 {
    job.failure_activity = Some("handoff".to_string());
    job.resolved_failure_activity = Some(deterministic_activity("handoff"));
    job
}

fn job_from_steps(name: &str, steps: Vec<Value>) -> JobV2 {
    let asset = json!({
        "schemaVersion": 2, "kind": "Job", "metadata": { "name": name },
        "spec": { "state": "enabled", "kind": "workflow", "steps": steps },
    });
    with_final_recovery(
        load_job_asset(&asset.to_string())
            .expect("fixture job loads")
            .spec,
    )
}

/// `setup → work → deliver`, with `handoff` as the failure activity and
/// `decide` as the final recovery activity. `work` may name `step_fix` as
/// its step recovery.
fn pipeline(step_recovery: bool) -> JobV2 {
    pipeline_acting("work", step_recovery)
}

fn three_step_job(middle_id: &str, middle_action: &str) -> JobV2 {
    with_handoff(job_from_steps(
        "final_recovery_fixture",
        vec![
            det_step("setup", "setup"),
            det_step(middle_id, middle_action),
            det_step("deliver", "deliver"),
        ],
    ))
}

/// `setup → <middle> → deliver`, otherwise exactly [`pipeline`].
fn pipeline_acting(middle: &str, step_recovery: bool) -> JobV2 {
    let mut job = three_step_job(middle, middle);
    if step_recovery {
        job.steps[1].recovery_activity = Some("step_fix".to_string());
        job.steps[1].resolved_recovery_activity = Some(deterministic_activity("step_fix"));
    }
    job
}

/// `setup → <step_id:action> → deliver`, otherwise exactly [`pipeline`].
fn pipeline_step_acting(step_id: &str, action: &str) -> JobV2 {
    three_step_job(step_id, action)
}

const FAULT_EVIDENCE: &str = "run fixture: the candidate failed its check";

fn resume_reply(step_id: &str, rationale: &str) -> Reply {
    Reply::Ok(json!({
        "decision": "resume", "fault": "implementer", "fault_evidence": FAULT_EVIDENCE,
        "step_id": step_id, "rationale": rationale,
    }))
}

fn resume_to(step_id: &str) -> Reply {
    resume_reply(step_id, "repaired the worktree")
}

fn escalate() -> Reply {
    Reply::Ok(json!({
        "decision": "escalate", "fault": "implementer", "fault_evidence": FAULT_EVIDENCE,
        "diagnosis": "the provider rejected the push",
        "human_action": "grant the token repository write access",
    }))
}

/// `setup → implement_bundle(loop of one `work` step) → deliver` with
/// final recovery; `claimed` adds the nested step's claimed default input.
fn bundle_job(claimed: Option<bool>) -> JobV2 {
    let mut step = json!({"id": "implement_one", "spec": {"type": "deterministic", "action": "work", "config": {}}});
    if let Some(claimed) = claimed {
        step["default_input"] =
            json!({"claimed": claimed, "base_sha": "0123456789abcdef0123456789abcdef01234567"});
    }
    let asset = json!({
        "schemaVersion": 2, "kind": "Job", "metadata": {"name": "implement_bundle_fixture"},
        "spec": {"state": "enabled", "kind": "workflow", "steps": [
            {"id": "setup", "spec": {"type": "deterministic", "action": "setup", "config": {}}},
            {"id": "implement_bundle", "loop": {"items": "one", "max_iterations": 1, "steps": [step]}},
            {"id": "deliver", "spec": {"type": "deterministic", "action": "deliver", "config": {}}}
        ]}
    });
    with_final_recovery(load_job_asset(&asset.to_string()).unwrap().spec)
}

fn run_pipeline(step_recovery: bool, host: &RecoveryHost) -> Run {
    run(&pipeline(step_recovery), host, None)
}

/// Host scripting `work` replies and a single `decide` reply.
fn work_decide_host(work: Vec<Reply>, decide: Reply) -> RecoveryHost {
    RecoveryHost::new([("work", work), ("decide", vec![decide])])
}

/// `work` fails once, then `decide` replies; run the standard pipeline.
fn failing_run(step_recovery: bool, decide: Reply) -> (RecoveryHost, Run) {
    let host = work_decide_host(vec![Reply::Fail], decide);
    let run = run_pipeline(step_recovery, &host);
    (host, run)
}

fn decide_input(host: &RecoveryHost) -> Value {
    host.inputs("decide")
        .into_iter()
        .next()
        .expect("decide was dispatched")
}

fn step_attempts(host: &RecoveryHost) -> Vec<Value> {
    decide_input(host)["step_recovery_attempts"]
        .as_array()
        .expect("attempts")
        .clone()
}

fn first_application(host: &RecoveryHost) -> FinalRecoveryApplication {
    host.applications()
        .into_iter()
        .next()
        .expect("one application")
}

fn assert_count(host: &RecoveryHost, action: &str, expected: usize) {
    assert_eq!(host.count(action), expected, "{action} count");
}

fn assert_actions(host: &RecoveryHost, expected: &[&str]) {
    assert_eq!(host.actions(), expected, "action sequence");
}

/// Final recovery was skipped: no dispatch, and today's failure path runs.
fn assert_skipped(run: &Run, host: &RecoveryHost) {
    assert!(run.result.is_err());
    assert_count(host, "decide", 0);
    assert_count(host, "handoff", 1);
    assert_attempts(&run.attempts, &[("skipped", None)]);
}

fn assert_attempts(actual: &[(String, Option<String>)], expected: &[(&str, Option<&str>)]) {
    let expected: Vec<_> = expected.iter().map(|&(o, d)| attempt(o, d)).collect();
    assert_eq!(actual, expected.as_slice(), "final recovery attempts");
}

fn attempt(outcome: &str, decision: Option<&str>) -> (String, Option<String>) {
    (outcome.to_string(), decision.map(str::to_string))
}

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
                "decision": "resume", "fault": "implementer", "fault_evidence": "run fixture: the candidate failed its check",
                "step_id": "work",
                "rationale": "finished the implementation",
                "response_result_fields": ["decision", "fault", "fault_evidence", "step_id", "rationale"],
                "invocation_id": "inv-1",
            }))],
        ),
    ]);
    let run = run_pipeline(false, &host);

    let outcome = run.result.expect("the resumed run completes");
    assert!(outcome.success, "the rerun from `work` must deliver");
    assert_actions(&host, &["setup", "work", "decide", "work", "deliver"]);
    assert_count(&host, "handoff", 0);
    let applications = host.applications();
    assert_eq!(applications.len(), 1);
    assert_eq!(applications[0].resume_step_index, Some(1));
    assert_attempts(&run.attempts, &[("resume", Some("resume"))]);

    let input = decide_input(&host);
    assert_eq!(input["task_id"], "T-1");
    assert_eq!(input["failed_step_id"], "work");
    assert_eq!(input["workspace_path"], "/worktrees/run");
    assert_eq!(input["base_ref"], "main");
    assert_eq!(input["crew_config_key"], "workflow.final_recovery_crews");
    assert_eq!(input["step_ids"], json!(["setup", "work", "deliver"]));
    assert_eq!(input["allowed_resume_step_ids"], json!(["work"]));
    assert_eq!(input["step_recovery_attempts"], json!([]));
    assert_eq!(input["log_tail"], "this run's failure log");
    // The in-pipeline lane applies every decision, `resume` included, but not
    // `resume_run`: the live run resumes through `resume`, never a second
    // resume of itself. It carries none of the backstop's other restrictions.
    let decisions = input["decisions"].as_array().expect("decisions list");
    assert!(decisions.contains(&json!("resume")), "{input}");
    assert!(!decisions.contains(&json!("resume_run")), "{input}");
    assert!(decisions.contains(&json!("decide")), "{input}");
    for restriction in ["lane_contract", "retained_candidate"] {
        assert!(input.get(restriction).is_none(), "{restriction}: {input}");
    }
}

fn resume_context_is_opaque_when_the_target_inherits_job_input() {
    let refusal = "The record contains an unresolved {{ input.missing }} template";
    let rationale = "Keep {{ input.missing }} literal while repairing the record";
    let host = work_decide_host(
        vec![
            Reply::Diagnostic(refusal.to_string()),
            Reply::Ok(json!({"repaired": true})),
        ],
        resume_reply("work", rationale),
    );
    let observed = run_pipeline(false, &host);
    assert!(
        observed.result.expect("the resumed run completes").success,
        "repair context is data, not another template to evaluate"
    );
    let inputs = host.inputs("work");
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[1]["final_recovery_resume"]["rationale"], rationale);
    let delivered_refusal = inputs[1]["final_recovery_resume"]["refusal"]
        .as_str()
        .unwrap();
    assert!(delivered_refusal.contains(refusal));
}

fn a_refused_deferral_reaches_the_resumed_nested_implementer_and_a_restart() {
    use orbit_types::workflow::FinalRecoveryResume;

    for claimed in [false, true] {
        let refused = json!({
            "summary": "candidate ready",
            "deferred_sandbox_validation": {
                "command": "make ci-test-affected", "exit_code": 0, "tests_run": 1,
                "base": "0123456789abcdef0123456789abcdef01234567",
                "notices": ["DEFERRED: bubblewrap unavailable: fixture: unavailable"],
                "probe": {"exit_code": 1, "output": "bwrap: No permissions to create new namespace"}
            }
        });
        let rationale = "Name only the gate summary instead of hand-writing the deferral record";
        let host = work_decide_host(
            vec![
                Reply::Ok(refused.clone()),
                Reply::Ok(json!({"repaired": true})),
            ],
            resume_reply("implement_bundle", rationale),
        );
        let job = bundle_job(Some(claimed));
        let observed = run(&job, &host, None);
        assert!(observed.result.unwrap().success);
        assert_count(&host, "work", 2);
        let inputs = host.inputs("work");
        assert!(inputs[0].get("final_recovery_resume").is_none());
        let context: FinalRecoveryResume =
            serde_json::from_value(inputs[1]["final_recovery_resume"].clone()).unwrap();
        assert_eq!(context.rationale, rationale);
        assert_eq!(context.failed_step_id, "implement_bundle");
        assert_eq!(context.refused_output["implement_one"], refused);
        assert!(
            context.refusal.contains("names no gate summary"),
            "{}",
            context.refusal
        );
        assert_eq!(inputs[1]["claimed"], claimed);
        assert!(
            host.inputs("deliver")[0]
                .get("final_recovery_resume")
                .is_none()
        );
        assert!(observed.details[0].as_ref().unwrap().contains(rationale));
        assert_eq!(
            host.applications()[0].resume_context.as_ref(),
            Some(&context)
        );

        // Re-enter from durable state: no second recovery dispatch, and the
        // nested default_input must still receive the original refusal.
        let mut state = resumed_after(
            FinalRecoveryDecision::Resume {
                step_id: context.step_id.clone(),
                rationale: context.rationale.clone(),
            },
            Some(FinalRecoveryCheckpointOutcome::Resume),
        );
        state.final_recovery.as_mut().unwrap().resume_context = Some(context.clone());
        let restarted = RecoveryHost::new([]);
        assert!(run(&job, &restarted, Some(&state)).result.unwrap().success);
        assert_count(&restarted, "decide", 0);
        assert_eq!(
            restarted.inputs("work")[0]["final_recovery_resume"],
            serde_json::to_value(context).unwrap()
        );
        assert!(
            restarted.inputs("deliver")[0]
                .get("final_recovery_resume")
                .is_none()
        );
    }
}

/// [ORB-12467] A provider refuses an oversized turn before the agent starts,
/// so the resumed step gets the bounded refusal final recovery read. The
/// context names the admitted step even when the decision's id is padded.
fn an_oversized_refusal_reaches_the_resumed_step_bounded_under_the_admitted_id() {
    let refusal = format!("probe refused: {}", "x".repeat(512 * 1024));
    let host = work_decide_host(
        vec![
            Reply::Diagnostic(refusal.clone()),
            Reply::Ok(json!({"repaired": true})),
        ],
        resume_reply(" work ", "repair the probe"),
    );
    let observed = run_pipeline(false, &host);
    assert!(observed.result.expect("the resumed run completes").success);
    let inputs = host.inputs("work");
    assert_eq!(inputs.len(), 2);
    let context = &inputs[1]["final_recovery_resume"];
    assert_eq!(context["step_id"], "work", "{context}");
    assert_eq!(context["rationale"], "repair the probe");
    let delivered = context["refusal"].as_str().expect("refusal is text");
    assert!(delivered.contains("probe refused: "));
    assert!(delivered.contains("[truncated: refusal is"));
    assert!(delivered.len() <= 64 * 1024, "{} B", delivered.len());
    assert_eq!(
        host.applications()[0]
            .resume_context
            .as_ref()
            .map(|c| c.refusal.as_str()),
        Some(delivered),
        "the persisted context is the delivered one"
    );
}

fn an_oversized_refused_output_reaches_the_resumed_implementer_bounded() {
    let summary = "y".repeat(256 * 1024);
    let host = work_decide_host(
        vec![
            Reply::Ok(
                json!({ "summary": summary, "deferred_sandbox_validation": {"probe": {"exit_code": 1}} }),
            ),
            Reply::Ok(json!({"repaired": true})),
        ],
        resume_reply(
            "implement_bundle",
            "Name only the gate summary instead of hand-writing the deferral record",
        ),
    );
    let job = bundle_job(None);
    assert!(run(&job, &host, None).result.unwrap().success);
    let inputs = host.inputs("work");
    assert_eq!(inputs.len(), 2, "one refusal and one resumed invocation");
    let refused = &inputs[1]["final_recovery_resume"]["refused_output"];
    let kept = refused["implement_one"]["summary"]
        .as_str()
        .expect("the refused record keeps its shape");
    assert!(kept.contains("[truncated: "), "{refused}");
    assert!(kept.len() < summary.len());
    assert!(refused["implement_one"]["deferred_sandbox_validation"].is_object());
    assert!(serde_json::to_string(refused).unwrap().len() <= 64 * 1024);
}

fn archive_reply(reason: &str) -> Reply {
    Reply::Ok(
        json!({ "decision": "archive", "fault": "implementer", "fault_evidence": FAULT_EVIDENCE, "reason": reason }),
    )
}

fn a_settling_decision_ends_the_run_without_the_failure_handoff() {
    let (host, run) = failing_run(false, archive_reply("superseded by the landed rewrite"));
    let error = run
        .result
        .expect_err("the original failure stays authoritative");
    assert!(error.to_string().contains("work failed"), "{error}");
    assert_count(&host, "handoff", 0);
    assert_count(&host, "deliver", 0);
    assert!(matches!(
        first_application(&host).decision,
        FinalRecoveryDecision::Archive { .. }
    ));
    assert_attempts(&run.attempts, &[("settled", Some("archive"))]);
}

fn an_escalation_still_runs_the_failure_handoff() {
    let (host, run) = failing_run(false, escalate());
    assert!(run.result.is_err());
    assert_count(&host, "handoff", 1);
    assert_actions(&host, &["setup", "work", "decide", "handoff"]);
    assert_attempts(&run.attempts, &[("escalated", Some("escalate"))]);
}

fn an_error_that_skips_step_recovery_still_reaches_final_recovery() {
    let host = work_decide_host(vec![Reply::Permanent], escalate());
    let run = run_pipeline(true, &host);
    assert!(matches!(
        run.result,
        Err(DispatchError::CliInvocationPermanent(_))
    ));
    assert_count(&host, "step_fix", 0);
    assert_count(&host, "decide", 1);
    assert_count(&host, "handoff", 1);
}

fn step_recovery_runs_first_and_final_recovery_only_once_it_is_spent() {
    let host = work_decide_host(vec![Reply::Fail, Reply::Fail], escalate());
    let run = run_pipeline(true, &host);
    assert!(run.result.is_err());
    assert_actions(
        &host,
        &["setup", "work", "step_fix", "work", "decide", "handoff"],
    );
    let input = decide_input(&host);
    let attempts = step_attempts(&host);
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

/// ORB-21581: a step that fails, admits its step recovery, and then reaches
/// a settled review awaiting evidence on the retry holds the run, exactly as
/// a direct hold does; a child found cancelled on the retry instead cancels
/// the parent rather than failing it.
fn hold_or_cancel_after_step_recovery(second: Reply, held: bool) {
    let host = work_decide_host(vec![Reply::Fail, second], escalate());
    let run = run_pipeline(true, &host);
    let outcome = run
        .result
        .expect("a hold or cancellation ends the run without an error");
    assert!(!outcome.success);
    if held {
        assert_eq!(outcome.evidence_hold, Some(evidence_hold()));
        assert!(!outcome.child_cancelled);
    } else {
        assert!(outcome.child_cancelled);
        assert!(outcome.evidence_hold.is_none());
        let message = outcome.message.expect("cancellation message");
        assert!(
            message.contains("child run jrun-child was cancelled")
                && message.contains("original error before recovery")
                && message.contains("work failed"),
            "{message}"
        );
    }
    assert_actions(&host, &["setup", "work", "step_fix", "work"]);
    assert!(run.attempts.is_empty(), "final recovery is not attempted");
    assert_eq!(run.post_recovery, [if held { "held" } else { "cancelled" }]);
}

fn an_evidence_hold_after_step_recovery_holds_the_run() {
    hold_or_cancel_after_step_recovery(Reply::EvidenceHold, true);
}

fn a_cancelled_child_after_step_recovery_cancels_the_run() {
    hold_or_cancel_after_step_recovery(Reply::ChildCancelled, false);
}

fn final_recovery_runs_at_most_once_per_run() {
    // The rerun after `resume` fails again: the hook does not run twice, and
    // the run falls through to its failure handoff.
    let host = RecoveryHost::new([
        ("work", vec![Reply::Fail, Reply::Fail]),
        ("decide", vec![resume_to("deliver"), resume_to("work")]),
    ]);
    let run = run_pipeline(false, &host);
    assert!(run.result.is_err());
    assert_count(&host, "decide", 1);
    assert_eq!(host.admissions.lock().unwrap().len(), 1);
    assert_count(&host, "handoff", 1);
    assert_attempts(
        &run.attempts,
        &[("resume", Some("resume")), ("skipped", None)],
    );
}

/// State a source run left after its final recovery recorded `decision`
/// (and `outcome`, when it got that far), resumed at `work`.
fn resumed_after(
    decision: FinalRecoveryDecision,
    outcome: Option<FinalRecoveryCheckpointOutcome>,
) -> PipelineState {
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
        repair_commit: None,
        resume_context: None,
        base_ref: Some("main".to_string()),
        admitted_at: Utc::now(),
        decision: Some(decision),
        fault: Default::default(),
        outcome,
    });
    state
}

fn a_run_resumed_after_a_final_recovery_resume_never_runs_it_again() {
    let state = resumed_after(
        FinalRecoveryDecision::Resume {
            step_id: "work".to_string(),
            rationale: "repaired the worktree".to_string(),
        },
        Some(FinalRecoveryCheckpointOutcome::Resume),
    );
    let host = work_decide_host(vec![Reply::Fail], escalate());
    let run = run(&pipeline(false), &host, Some(&state));
    assert!(run.result.is_err());
    assert_count(&host, "decide", 0);
    assert!(host.admissions.lock().unwrap().is_empty());
    assert!(host.applications().is_empty());
    assert_count(&host, "handoff", 1);
}

fn a_run_resumed_after_a_crash_mid_settlement_replays_the_recorded_decision() {
    // The source run recorded `archive` and crashed before recording what
    // applying it did. The resumed run fails again; it neither dispatches nor
    // admits final recovery, but hands the recorded decision back to the
    // host, whose application is idempotent, and opens no blocked PR.
    let archive = FinalRecoveryDecision::Archive {
        reason: "superseded by the landed rewrite".to_string(),
    };
    let state = resumed_after(archive.clone(), None);
    let host = work_decide_host(vec![Reply::Fail], escalate());
    let run = run(&pipeline(false), &host, Some(&state));
    assert!(run.result.is_err());
    assert_count(&host, "decide", 0);
    assert!(host.admissions.lock().unwrap().is_empty());
    let applications = host.applications();
    assert_eq!(applications.len(), 1);
    assert_eq!(applications[0].decision, archive);
    assert_eq!(applications[0].resume_step_index, None);
    assert_count(&host, "handoff", 0);
    assert_attempts(&run.attempts, &[("settled", Some("archive"))]);
}

fn a_provider_authentication_failure_skips_final_recovery() {
    let host = RecoveryHost::new([
        (
            "work",
            vec![Reply::Diagnostic(
                "[provider_unavailable] claude provider authentication failure (HTTP 401): \
                 Failed to authenticate: OAuth token revoked."
                    .into(),
            )],
        ),
        ("step_fix", vec![Reply::Ok(json!({ "repaired": true }))]),
        ("decide", vec![escalate()]),
    ]);
    let run = run_pipeline(true, &host);
    assert!(
        run.result.is_err(),
        "authentication is not a successful job"
    );
    assert_count(&host, "step_fix", 0);
    assert_count(&host, "decide", 0);
    assert!(
        host.admissions.lock().unwrap().is_empty(),
        "the hook is not admitted"
    );
    assert_count(&host, "handoff", 1);
    assert_attempts(&run.attempts, &[("skipped", None)]);
    assert!(
        run.details
            .first()
            .and_then(|d| d.as_deref())
            .is_some_and(|d| d.contains("could not be used on this host")),
        "the skip is recorded: {:?}",
        run.details
    );
}

/// A delivery step that lost only to a busy lock or a locked store is blocked
/// on that condition without an agent, so the cleared-infra requeue can bring
/// it back; a validation failure whose output merely contains the same text is
/// still the candidate's, and goes to the agent. `reserve_locks` stands for
/// every delivery action: the host runs it, and the classifier lists it.
fn transient_infrastructure_is_escalated_without_an_agent() {
    for (message, class) in [
        (
            "execution failed: remote tool failed (lock_busy): task write lock is held",
            TransientInfrastructure::LockBusy,
        ),
        (
            "timed out after 30000ms acquiring task lock '/repo/.orbit/locks/T-1.lock'",
            TransientInfrastructure::LockBusy,
        ),
        (
            "store error: SQLite database '/repo/.orbit/orbit.db' remained locked during task update",
            TransientInfrastructure::StoreLocked,
        ),
    ] {
        let host = RecoveryHost::new([
            ("reserve_locks", vec![Reply::Diagnostic(message.into())]),
            ("decide", vec![escalate()]),
        ]);
        let run = run(&pipeline_acting("reserve_locks", false), &host, None);
        assert!(run.result.is_err(), "{message}");
        assert_eq!(host.count("decide"), 0, "no agent judges it: {message}");
        let applications = host.applications();
        assert_eq!(applications.len(), 1, "{message}");
        assert_eq!(applications[0].infrastructure, Some(class), "{message}");
        assert!(
            matches!(
                applications[0].decision,
                FinalRecoveryDecision::Escalate { .. }
            ),
            "{message}"
        );
        assert_count(&host, "handoff", 1);
        assert_attempts(&run.attempts, &[("escalated", Some("escalate"))]);
    }

    // Each of these goes to the recovery agent as before: a lock display a
    // validation action's output merely quotes, a lock display quoted after
    // other text, and a delivery step of a claimed leaf, whose task is its
    // owner's.
    let lock = "store error: database is locked";
    for (case, action, message, input) in [
        (
            "validation output",
            "review_reconciliation_validate",
            lock.to_string(),
            json!({ "task_ids": ["T-1"] }),
        ),
        (
            "quoted mid-message",
            "reserve_locks",
            format!("required validation 'make test' did not pass on candidate head: {lock}"),
            json!({ "task_ids": ["T-1"] }),
        ),
        (
            "claimed leaf",
            "reserve_locks",
            "execution failed: remote tool failed (lock_busy): task write lock is held".into(),
            json!({ "task_ids": ["T-1"], "claimed": true }),
        ),
    ] {
        let host = RecoveryHost::new([
            (action, vec![Reply::Diagnostic(message)]),
            ("decide", vec![escalate()]),
        ]);
        let run = run_with_input(
            &pipeline_acting(action, false),
            &host,
            None,
            Vec::new(),
            input,
        );
        assert!(run.result.is_err(), "{case}");
        assert_eq!(host.count("decide"), 1, "{case}: the agent judges it");
        assert_eq!(host.applications()[0].infrastructure, None, "{case}");
    }
}

/// A `worktree_setup` failure caused by a dependency lookup `not_found` is
/// not transient: final recovery must not dispatch an agent that could requeue
/// it on an unchanged environment. The same text from another step still reaches the agent.
fn worktree_setup_dependency_not_found_skips_final_recovery() {
    // The failing step is the job's `worktree_setup` step; its action is
    // scripted here so the host, not the engine's own worktree code, fails it
    // with the owner lookup's `not_found` text.
    let job = pipeline_step_acting("worktree_setup", "worktree_fails");
    let host = RecoveryHost::new([
        (
            "worktree_fails",
            vec![Reply::Diagnostic(
                "remote tool failed (not_found): hm_test: task not found: ORB-99999".into(),
            )],
        ),
        ("decide", vec![escalate()]),
    ]);
    let skipped = run(&job, &host, None);
    assert!(skipped.result.is_err());
    assert_count(&host, "decide", 0);
    assert!(
        host.applications().is_empty(),
        "no resume or requeue decision is applied"
    );
    assert_count(&host, "handoff", 1);
    assert_attempts(&skipped.attempts, &[("skipped", None)]);
    assert!(
        skipped
            .details
            .first()
            .and_then(|d| d.as_deref())
            .is_some_and(|d| d.contains("could not resolve a dependency")),
        "the skip is recorded: {:?}",
        skipped.details
    );

    let host = work_decide_host(
        vec![Reply::Diagnostic(
            "remote tool failed (not_found): hm_test: task not found: ORB-99999".into(),
        )],
        escalate(),
    );
    let other = run_pipeline(false, &host);
    assert!(other.result.is_err());
    assert_count(&host, "decide", 1);
}

fn an_empty_crew_pool_skips_final_recovery() {
    let host = work_decide_host(vec![Reply::Fail], escalate()).with_empty_pool();
    let run = run_pipeline(false, &host);
    assert_skipped(&run, &host);
    assert!(host.applications().is_empty());
}

fn a_resume_to_a_later_step_reruns_the_failed_contract_and_records_the_downgrade() {
    let host = work_decide_host(
        vec![Reply::Fail, Reply::Ok(json!({"built": true}))],
        resume_to("deliver"),
    );
    let run = run_pipeline(false, &host);
    assert!(run.result.unwrap().success);
    assert_actions(&host, &["setup", "work", "decide", "work", "deliver"]);
    let applications = host.applications();
    assert_eq!(applications[0].resume_step_index, Some(1));
    assert_eq!(
        applications[0].decision,
        FinalRecoveryDecision::Resume {
            step_id: "work".to_string(),
            rationale: "repaired the worktree".to_string()
        }
    );
    assert_attempts(&run.attempts, &[("resume", Some("resume"))]);
    let detail = run.details[0].as_ref().unwrap();
    assert!(
        detail.contains("downgraded resume from `deliver` to failed step `work`"),
        "{detail}"
    );
    let updates = host.updates.lock().unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].0, "T-1");
    let update = &updates[0].1;
    assert_eq!(update.status, None, "a downgrade only records a comment");
    assert_eq!(
        update.job_run_id, None,
        "a comment must preserve the task's run binding"
    );
    assert_eq!(update.append_comments.len(), 1);
    assert!(update.append_comments[0].message.contains(detail));
}

fn a_resume_to_an_earlier_step_keeps_the_named_target() {
    let host = RecoveryHost::new([
        (
            "deliver",
            vec![Reply::Fail, Reply::Ok(json!({"delivered": true}))],
        ),
        ("decide", vec![resume_to("work")]),
    ]);
    let run = run_pipeline(false, &host);
    assert!(run.result.unwrap().success);
    assert_actions(
        &host,
        &["setup", "work", "deliver", "decide", "work", "deliver"],
    );
    assert_eq!(host.applications()[0].resume_step_index, Some(1));
    assert_eq!(
        decide_input(&host)["allowed_resume_step_ids"],
        json!(["work", "deliver"])
    );
    assert!(host.updates.lock().unwrap().is_empty());
}

fn a_resume_to_an_admission_or_nonexistent_step_is_escalated() {
    for target in ["setup", "no_such_step"] {
        let (host, run) = failing_run(false, resume_to(target));
        assert!(run.result.is_err(), "{target}");
        let applications = host.applications();
        let FinalRecoveryDecision::Escalate { diagnosis, .. } = &applications[0].decision else {
            panic!("resume to `{target}` must reach the host as escalate");
        };
        assert_eq!(
            diagnosis,
            &format!(
                "final recovery asked to resume from `{target}`, which is neither the failed step `work` nor an earlier step of its phase"
            ),
            "{target}"
        );
        assert_eq!(applications[0].resume_step_index, None);
        assert_eq!(host.count("deliver"), 0, "{target}");
        assert_eq!(host.count("handoff"), 1, "{target}");
    }
}

fn a_malformed_decision_is_escalated() {
    let (host, run) = failing_run(false, Reply::Ok(json!({ "decision": "resume" })));
    assert!(run.result.is_err());
    assert!(matches!(
        first_application(&host).decision,
        FinalRecoveryDecision::Escalate { .. }
    ));
    assert_count(&host, "handoff", 1);
}

fn a_decision_carries_its_fault_and_one_without_a_fault_escalates() {
    let (host, _) = failing_run(
        false,
        Reply::Ok(json!({
            "decision": "requeue", "reason": "the task was rewritten",
            "fault": "specification", "fault_evidence": "file docs/spec.md contradicts criterion 2",
            // Only Orbit's observation names the judge.
            "fault_judged_by": "claimed-by-the-agent",
        })),
    );
    let application = first_application(&host);
    assert!(matches!(
        &application.decision,
        FinalRecoveryDecision::Requeue { .. }
    ));
    assert_eq!(application.fault.fault, RecoveryFault::Specification);
    assert_eq!(
        application.fault.fault_evidence,
        "file docs/spec.md contradicts criterion 2"
    );
    assert_eq!(application.fault.judged_by, None);

    let (host, _) = failing_run(
        false,
        Reply::Ok(json!({ "decision": "requeue", "reason": "the task was rewritten" })),
    );
    let application = first_application(&host);
    assert!(
        matches!(
            &application.decision,
            FinalRecoveryDecision::Escalate { .. }
        ),
        "a decision without its fault is malformed: {application:?}"
    );
    assert_eq!(application.fault.fault, RecoveryFault::Unknown);
    assert!(application.fault.fault_evidence.contains("`fault`"));
}

fn the_recovery_packet_withholds_the_implementers_model() {
    let host = RecoveryHost::new([
        (
            "setup",
            vec![Reply::Ok(json!({
                "workspace_path": "/worktrees/run",
                "base_ref": "main",
                "base_sha": "0123456789abcdef0123456789abcdef01234567",
                "provider": "claude",
                "model": "claude-opus-5-5",
                "argv_redacted": ["claude", "--model", "claude-opus-5-5"],
                "final_message": "implemented with claude-opus-5-5",
            }))],
        ),
        (
            "work",
            vec![Reply::Diagnostic(
                "opus@high ran out of turns; the candidate fails cargo test".into(),
            )],
        ),
        ("decide", vec![escalate()]),
    ]);
    run_with_input(
        &pipeline(false),
        &host,
        None,
        Vec::new(),
        json!({ "task_ids": ["T-1"], "crew": "opus@high" }),
    );
    let packet = decide_input(&host);
    let text = packet.to_string();
    for name in [
        "claude-opus-5-5",
        "opus@high",
        "argv_redacted",
        "\"provider\"",
    ] {
        assert!(
            !text.contains(name),
            "the packet names `{name}`: {packet:#}"
        );
    }
    assert_eq!(
        packet["step_outputs"]["setup"]["final_message"],
        "implemented with [implementer model withheld]"
    );
    assert!(
        packet["error_message"]
            .as_str()
            .unwrap()
            .contains("[implementer model withheld] ran out of turns"),
        "{packet:#}"
    );
    assert_eq!(packet["workspace_path"], "/worktrees/run");
}

fn a_failure_before_the_worktree_exists_skips_final_recovery() {
    let host = RecoveryHost::new([("setup", vec![Reply::Fail]), ("decide", vec![escalate()])]);
    let run = run_pipeline(false, &host);
    assert!(run.result.is_err());
    assert_count(&host, "decide", 0);
    assert!(host.admissions.lock().unwrap().is_empty());
    assert_count(&host, "handoff", 1);
}

fn failed_recovery_dispatch_is_injected_before_final_recovery() {
    let host = RecoveryHost::new([
        ("work", vec![Reply::Fail]),
        ("step_fix", vec![Reply::Fail]),
        ("decide", vec![escalate()]),
    ]);
    let run = run_pipeline(true, &host);
    assert!(run.result.is_err());
    let input = decide_input(&host);
    let attempts = step_attempts(&host);
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
    let input = decide_input(&host);
    let attempts = step_attempts(&host);
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

fn final_recovery_dispatches_with_newest_attempts_and_failure_signatures_when_evidence_overflows() {
    for (attempt_count, distinct_failures) in [(500, 5), (500, 500), (1000, 1000)] {
        let host = work_decide_host(
            vec![Reply::Fail],
            Reply::Ok(
                json!({"decision": "requeue", "fault": "implementer", "fault_evidence": FAULT_EVIDENCE, "reason": "retry later"}),
            ),
        );
        let events = (0..attempt_count)
            .map(|index| {
                (
                    RUN_ID,
                    V2AuditEventKind::StepPostRecoveryAttempt {
                        step_id: "prior-step".to_string(),
                        recovery_activity: "older-recovery".to_string(),
                        outcome: "error".to_string(),
                        error_message: Some(format!(
                            "failure-{}: {}",
                            index % distinct_failures,
                            "failed 界\"\n".repeat(100)
                        )),
                        output: Some(json!({"attempt_index": index})),
                    },
                )
            })
            .collect();
        let run = run_with_evidence(&pipeline(false), &host, None, events);
        assert!(
            run.result.is_err(),
            "a settled recovery preserves the original job failure"
        );
        assert_count(&host, "decide", 1);
        assert!(matches!(
            &first_application(&host).decision,
            FinalRecoveryDecision::Requeue { .. }
        ));
        let attempts = step_attempts(&host);
        assert!(
            serde_json::to_vec(&attempts).unwrap().len() <= 64 * 1024,
            "the serialized evidence, including the summary, stays within its bound"
        );
        let summary = &attempts[0];
        assert_eq!(summary["truncated"], true);
        assert_eq!(summary["run_id"], RUN_ID);
        assert_eq!(summary["audit_reference"]["run_id"], RUN_ID);
        assert_eq!(
            summary["omitted_attempts"].as_u64().unwrap() as usize,
            attempt_count + 1 - attempts.len()
        );
        let signatures = summary["failure_signatures"].as_array().unwrap();
        let omitted_signatures = summary["omitted_failure_signatures"].as_u64().unwrap() as usize;
        assert_eq!(signatures.len() + omitted_signatures, distinct_failures);
        assert_eq!(
            signatures
                .iter()
                .map(|s| s["count"].as_u64().unwrap())
                .sum::<u64>()
                + summary["omitted_signature_attempts"].as_u64().unwrap(),
            attempt_count as u64
        );
        assert_eq!(
            summary["audit_reference"]["event_kinds"],
            json!(["step.recovery_attempted", "step.post_recovery_attempt"])
        );
        assert!(summary["original_serialized_bytes"].as_u64().unwrap() > 64 * 1024);
        assert_eq!(
            signatures
                .iter()
                .map(|s| s["sha256"].as_str().unwrap())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            signatures.len()
        );
        if distinct_failures <= 500 {
            assert_eq!(
                omitted_signatures, 0,
                "every attempt's signature fits even when details do not"
            );
        } else {
            assert!(
                omitted_signatures > 0,
                "unbounded signature cardinality is counted and points to the audit"
            );
        }
        if distinct_failures == 5 {
            for signature in signatures {
                assert_eq!(signature["failed_step_id"], "prior-step");
                assert_eq!(signature["activity"], "older-recovery");
                assert_eq!(signature["phase"], "post_recovery");
                assert_eq!(signature["outcome"], "error");
                assert!(
                    signature["error_message"]
                        .as_str()
                        .unwrap()
                        .starts_with("failure-")
                );
            }
        } else {
            assert!(
                signatures.iter().all(|s| s.get("error_message").is_none()),
                "many distinct failures retain compact hashes and counts"
            );
        }
        assert!(
            attempts.len() > 1,
            "retain detailed recent attempts as well as signatures"
        );
        assert!(attempts[1..].windows(2).all(|pair| {
            pair[0]["output"]["attempt_index"].as_u64().unwrap() + 1
                == pair[1]["output"]["attempt_index"].as_u64().unwrap()
        }));
        assert_eq!(
            attempts.last().unwrap()["output"]["attempt_index"],
            attempt_count - 1
        );
        assert_count(&host, "handoff", 0);
    }
}

fn final_recovery_keeps_post_recovery_output_when_a_later_step_fails() {
    let host = RecoveryHost::new([
        (
            "work",
            vec![Reply::Fail, Reply::Ok(json!({"repaired": true}))],
        ),
        ("deliver", vec![Reply::Fail]),
        ("decide", vec![escalate()]),
    ]);
    let run = run_pipeline(true, &host);
    assert!(run.result.is_err());
    assert_eq!(decide_input(&host)["failed_step_id"], "deliver");
    let attempts = step_attempts(&host);
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[1]["phase"], "post_recovery");
    assert_eq!(attempts[1]["outcome"], "success");
    assert_eq!(attempts[1]["output"], json!({"repaired": true}));
}

fn conflict(paths: &[&str], resolvable: bool) -> RecoverableVcsConflict {
    RecoverableVcsConflict {
        operation: "git_rebase".into(),
        original_base_sha: "original".into(),
        target_base_sha: "target".into(),
        conflicting_paths: paths.iter().map(|path| (*path).to_string()).collect(),
        conflicting_files: paths
            .iter()
            .map(|path| ConflictedFile {
                path: (*path).to_string(),
                deterministically_resolvable: resolvable,
            })
            .collect(),
        diagnostic: "stopped rebase".into(),
    }
}

fn conflict_resume_job() -> JobV2 {
    let mut job = job_from_steps(
        "conflict_diagnostic_resume",
        vec![
            det_step("setup", "setup"),
            det_step("sync_base", "sync_base"),
            det_step("deliver", "deliver"),
        ],
    );
    let orbit_types::workflow::activity_job::JobV2StepBody::Target(target) = &mut job.steps[2].body
    else {
        panic!("deliver is a target step");
    };
    // The next step renders the success output. `failed_only` from the
    // incomplete checkpoint must not be visible through that template.
    target.default_input = Some(json!({"rendered": "{{ steps.sync_base.output }}"}));
    job
}

fn old_conflict_resume() -> PipelineState {
    let setup = json!({ "workspace_path": "/worktrees/run", "base_ref": "main", "base_sha": "0123456789abcdef0123456789abcdef01234567" });
    let mut state = PipelineState::new(
        RUN_ID.into(),
        "conflict_diagnostic_resume".into(),
        json!({ "task_ids": ["T-1"] }),
    );
    state.record_step(0, JobRunState::Success, Some(setup.clone()), None);
    state.record_pipeline_output("setup", setup);
    state.record_pipeline_output(
        "sync_base",
        json!({
            "conflicting_paths": ["old.rs"],
            "conflicting_files": [{ "path": "old.rs", "deterministically_resolvable": false }],
            "failed_only": "discard me",
        }),
    );
    state
}

/// [ORB-15889] Final recovery clears the pipeline entry a newer conflict
/// recorded. The success checkpoint must keep that newer evidence, including
/// an explicit empty list, and must not restore the resumed `old.rs` paths.
/// (Ordinary resume and step recovery are the controls in
/// `incomplete_conflict_diagnostics_survive_resumed_execution_and_success_checkpoint`.)
fn final_recovery_resume_checkpoints_the_latest_conflict_diagnostics() {
    enum Latest {
        Newer(RecoverableVcsConflict),
        Seeded,
    }

    let resume = old_conflict_resume();
    let job = conflict_resume_job();
    for (name, latest) in [
        ("newer conflict", Latest::Newer(conflict(&["new.rs"], true))),
        ("explicit empty lists", Latest::Newer(conflict(&[], false))),
        ("no newer conflict", Latest::Seeded),
    ] {
        let (first, paths, files) = match &latest {
            Latest::Newer(conflict) => (
                Reply::Conflict(conflict.clone()),
                serde_json::to_value(&conflict.conflicting_paths).expect("paths"),
                serde_json::to_value(&conflict.conflicting_files).expect("files"),
            ),
            Latest::Seeded => (
                Reply::Fail,
                json!(["old.rs"]),
                json!([{
                    "path": "old.rs",
                    "deterministically_resolvable": false,
                }]),
            ),
        };
        let host = RecoveryHost::new([
            (
                "sync_base",
                vec![first, Reply::Ok(json!({"rebased": true}))],
            ),
            (
                "decide",
                vec![resume_reply("sync_base", "resolved the stopped rebase")],
            ),
            ("deliver", vec![Reply::Ok(json!({"delivered": true}))]),
        ]);
        let observed = run(&job, &host, Some(&resume));
        let outcome = observed
            .result
            .unwrap_or_else(|error| panic!("{name}: resumed run completes, got {error}"));
        assert!(outcome.success, "{name}: the rerun from sync_base delivers");
        assert_eq!(
            host.actions(),
            ["sync_base", "decide", "sync_base", "deliver"],
            "{name}: an incomplete sync_base executes, final recovery resumes it, and diagnostics do not skip it"
        );
        assert_attempts(&observed.attempts, &[("resume", Some("resume"))]);
        let expected = json!({ "rebased": true, "conflicting_paths": paths.clone(), "conflicting_files": files.clone() });
        let output = &outcome.pipeline["sync_base"];
        assert!(
            output.get("conflicting_paths").is_some() && output.get("conflicting_files").is_some(),
            "{name}: both diagnostic keys stay present: {output}"
        );
        assert_eq!(output, &expected, "{name}");
        assert!(
            output.get("failed_only").is_none(),
            "{name}: unrelated failed output stays out of the success checkpoint"
        );
        if matches!(latest, Latest::Newer(_)) {
            assert_ne!(
                output["conflicting_paths"],
                json!(["old.rs"]),
                "{name}: final recovery must not restore the resumed conflict"
            );
        }
        let checkpoints = host.checkpoints.lock().unwrap();
        let synced: Vec<_> = checkpoints
            .iter()
            .filter(|(step_id, _)| step_id == "sync_base")
            .map(|(_, output)| output)
            .collect();
        assert_eq!(
            synced,
            vec![&expected],
            "{name}: the success checkpoint is the output final recovery left in place"
        );
        drop(checkpoints);
        let diagnostics = host.diagnostics.lock().unwrap();
        match &latest {
            Latest::Newer(_) => {
                assert_eq!(diagnostics.len(), 1, "{name}");
                assert_eq!(diagnostics[0].0, "sync_base");
                assert_eq!(diagnostics[0].1["conflicting_paths"], paths, "{name}");
                assert_eq!(diagnostics[0].1["conflicting_files"], files, "{name}");
            }
            Latest::Seeded => assert!(
                diagnostics.is_empty(),
                "{name}: no new conflict means no replacement diagnostic"
            ),
        }
        drop(diagnostics);
        let deliver_inputs = host.inputs("deliver");
        let rendered = &deliver_inputs[0]["rendered"];
        assert_eq!(
            rendered, &expected,
            "{name}: the next step's template sees the success output"
        );
        assert!(
            rendered.get("failed_only").is_none(),
            "{name}: the next step's template does not see the failed field"
        );
    }
}

/// Behavior-distinct rows share one bounded family process; the row name
/// is the scenario function name, so failures still name the case.
macro_rules! recovery_contracts {
    ($name:ident: $($row:ident),+ $(,)?) => {
        #[test]
        fn $name() {
            crate::git_fixture::isolated(module_path!(), stringify!($name), || {
                crate::git_fixture::run_scenarios(&[$((stringify!($row), $row as fn())),+]);
            });
        }
    };
}
recovery_contracts!(recovery_contracts_1: a_resume_reruns_from_the_named_step_and_the_run_completes, resume_context_is_opaque_when_the_target_inherits_job_input, a_refused_deferral_reaches_the_resumed_nested_implementer_and_a_restart, an_oversized_refusal_reaches_the_resumed_step_bounded_under_the_admitted_id, an_oversized_refused_output_reaches_the_resumed_implementer_bounded, a_settling_decision_ends_the_run_without_the_failure_handoff);
recovery_contracts!(recovery_contracts_2: an_escalation_still_runs_the_failure_handoff, an_error_that_skips_step_recovery_still_reaches_final_recovery, step_recovery_runs_first_and_final_recovery_only_once_it_is_spent, final_recovery_runs_at_most_once_per_run, a_run_resumed_after_a_final_recovery_resume_never_runs_it_again, a_run_resumed_after_a_crash_mid_settlement_replays_the_recorded_decision, an_evidence_hold_after_step_recovery_holds_the_run, a_cancelled_child_after_step_recovery_cancels_the_run);
recovery_contracts!(recovery_contracts_3: a_provider_authentication_failure_skips_final_recovery, an_empty_crew_pool_skips_final_recovery, a_resume_to_a_later_step_reruns_the_failed_contract_and_records_the_downgrade, a_resume_to_an_earlier_step_keeps_the_named_target, a_resume_to_an_admission_or_nonexistent_step_is_escalated, a_malformed_decision_is_escalated, a_decision_carries_its_fault_and_one_without_a_fault_escalates, the_recovery_packet_withholds_the_implementers_model);
recovery_contracts!(recovery_contracts_4: completion_observation_classifies_retry_policy_and_candidate_refusal, a_failure_before_the_worktree_exists_skips_final_recovery, failed_recovery_dispatch_is_injected_before_final_recovery, final_recovery_injects_only_its_runs_bounded_evidence_in_order, final_recovery_dispatches_with_newest_attempts_and_failure_signatures_when_evidence_overflows, final_recovery_keeps_post_recovery_output_when_a_later_step_fails, final_recovery_resume_checkpoints_the_latest_conflict_diagnostics, transient_infrastructure_is_escalated_without_an_agent, worktree_setup_dependency_not_found_skips_final_recovery);

/// The executor cannot turn a policy refusal into completion authority, and
/// the deterministic retry spends the existing once-only recovery budget.
fn completion_observation_classifies_retry_policy_and_candidate_refusal() {
    for (admission, kind, classification, count) in [
        (
            CompletionObservationAdmission::Retry,
            "resume",
            "transient-observation",
            2,
        ),
        (
            CompletionObservationAdmission::PolicyChanged {
                reason: "review now required".into(),
            },
            "requeue",
            "policy-changed",
            1,
        ),
        (
            CompletionObservationAdmission::Unavailable,
            "escalate",
            "candidate-changed",
            1,
        ),
    ] {
        let mut host = RecoveryHost::new([("work", vec![Reply::Observation, Reply::Observation])]);
        host.completion_admission = admission;
        let job = pipeline(false);
        let observed =
            orbit_engine::test_support::with_vcs_retry_wait(|_| {}, || run(&job, &host, None));
        assert!(observed.result.is_err());
        assert_eq!(host.count("work"), count);
        assert_count(&host, "decide", 0);
        assert_count(&host, "setup", 1);
        let applications = host.applications();
        assert_eq!(
            applications.len(),
            1,
            "a second observation failure cannot renew recovery"
        );
        let decision = &applications[0].decision;
        assert_eq!(decision.kind(), kind);
        let text = match decision {
            FinalRecoveryDecision::Resume { rationale, .. } => rationale,
            FinalRecoveryDecision::Requeue { reason } => reason,
            FinalRecoveryDecision::Escalate { diagnosis, .. } => diagnosis,
            other => panic!("unexpected {other:?}"),
        };
        assert!(text.contains(classification), "{text}");
    }
}
